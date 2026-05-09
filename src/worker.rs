use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::locking::acquire_file_lock;
use crate::models::{IndexJob, RepoEntry};

pub async fn worker_loop(
    mut rx: tokio::sync::mpsc::Receiver<IndexJob>,
    state: Arc<crate::state::AppState>,
) {
    while let Some(job) = rx.recv().await {
        let repo_id = job.repo_id().to_string();
        tracing::info!("Worker picked up job: {:?} for {}", job, repo_id);

        let repo = {
            let registry = state.registry.lock().unwrap();
            match registry.get(&repo_id) {
                Some(entry) => entry.clone(),
                None => {
                    tracing::error!(
                        "Repository '{}' not found in registry, skipping job",
                        repo_id
                    );
                    continue;
                }
            }
        };

        if let Err(e) = process_repository(&repo, &state).await {
            tracing::error!("Indexing failed for {}: {e:#}", repo.id);
            let mut registry = state.registry.lock().unwrap();
            let _ = registry.update_status(&repo.id, crate::models::RepoStatus::Error);
        }
    }
}

async fn process_repository(
    repo: &RepoEntry,
    state: &crate::state::AppState,
) -> anyhow::Result<()> {
    // 1. Acquire exclusive file lock
    let lock_path = PathBuf::from(&repo.local_path).join(".knot.lock");
    let _lock = match acquire_file_lock(&lock_path) {
        Ok(lock) => {
            tracing::info!("Worker: acquired file lock for '{}'", repo.id);
            lock
        }
        Err(_) => {
            tracing::info!("Worker: '{}' locked by another node, skipping", repo.id);
            return Ok(());
        }
    };

    // 2. Update status to indexing
    {
        let mut registry = state.registry.lock().unwrap();
        registry.update_status(&repo.id, crate::models::RepoStatus::Indexing)?;
        tracing::info!("Worker: status=indexing for '{}'", repo.id);
    }

    // 3. Git operation
    if Path::new(&repo.local_path).join(".git").exists() {
        tracing::info!("Worker: pulling '{}' from {}", repo.id, repo.url);
        crate::git::run_git_pull(repo).await?;
        tracing::info!("Worker: pull complete for '{}'", repo.id);
    } else {
        tracing::info!("Worker: cloning '{}' from {}", repo.id, repo.url);
        crate::git::run_git_clone(repo).await?;
        tracing::info!("Worker: clone complete for '{}'", repo.id);
    }

    // 4. Build knot Config programmatically
    let knot_cfg = knot::config::Config {
        repo_path: repo.local_path.clone(),
        repo_name: repo.id.clone(),
        qdrant_url: state.qdrant_url.clone(),
        qdrant_collection: state.qdrant_collection.clone(),
        neo4j_uri: state.neo4j_uri.clone(),
        neo4j_user: state.neo4j_user.clone(),
        neo4j_password: state.neo4j_password.clone(),
        custom_queries_path: None,
        embed_dim: state.embed_dim,
        batch_size: 64,
        clean: false,
        dependency_repos: Vec::new(),
        watch: false,
        dry_run: false,
        custom_ca_certs: None,
        output_format: knot::config::OutputFormat::Markdown,
        ingest_concurrency: 4,
        rayon_threads: state.rayon_threads,
        include_config_files: false,
    };

    // 5. Load IndexState
    let mut index_state = knot::pipeline::state::IndexState::load(&repo.local_path)?;

    // 6. Run the indexing pipeline
    tracing::info!("Worker: starting indexing pipeline for '{}'", repo.id);
    knot::pipeline::runner::run_indexing_pipeline(
        &knot_cfg,
        &state.vector_db,
        &state.graph_db,
        &mut index_state,
    )
    .await?;
    tracing::info!("Worker: indexing pipeline complete for '{}'", repo.id);

    // 7. Update registry
    {
        let mut registry = state.registry.lock().unwrap();
        registry.update_status(&repo.id, crate::models::RepoStatus::Idle)?;
        registry.update_last_indexed(&repo.id)?;
        tracing::info!("Worker: status=idle for '{}'", repo.id);
    }

    tracing::info!("Worker: job completed for '{}'", repo.id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{AuthType, RepoStatus};
    use crate::registry::Registry;
    use knot::db::graph::ConnectExt;
    use knot::db::vector::VectorConnectExt;
    use std::sync::Mutex;
    use tempfile::TempDir;

    async fn create_test_state(workspace: &Path) -> Arc<crate::state::AppState> {
        let registry = Registry::load_or_create(workspace).unwrap();

        let graph_db =
            knot::db::graph::GraphDb::connect("bolt://localhost:9999", "neo4j", "badpassword")
                .await
                .expect("connect for test db");
        let vector_db =
            knot::db::vector::VectorDb::connect("http://localhost:9999", "test_collection", 384)
                .await
                .expect("connect for test vector db");
        let embedder = knot::pipeline::embed::Embedder::init().expect("init embedder");

        Arc::new(crate::state::AppState {
            vector_db: Arc::new(vector_db),
            graph_db: Arc::new(graph_db),
            embedder: Arc::new(Mutex::new(embedder)),
            workspace_dir: workspace.to_string_lossy().into(),
            registry: Arc::new(Mutex::new(registry)),
            job_tx: tokio::sync::mpsc::channel(16).0,
            qdrant_url: "http://localhost:6334".into(),
            qdrant_collection: "knot_entities".into(),
            neo4j_uri: "bolt://localhost:7687".into(),
            neo4j_user: "neo4j".into(),
            neo4j_password: "secret".into(),
            embed_dim: 384,
            rayon_threads: None,
        })
    }

    #[tokio::test]
    async fn test_job_queue_processes_sequentially() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<IndexJob>(16);
        let order = Arc::new(Mutex::new(Vec::new()));

        let order_clone = order.clone();
        let handle = tokio::spawn(async move {
            while let Some(job) = rx.recv().await {
                order_clone.lock().unwrap().push(job.repo_id().to_string());
            }
        });

        tx.send(IndexJob::Pull {
            repo_id: "a".into(),
        })
        .await
        .unwrap();
        tx.send(IndexJob::Pull {
            repo_id: "b".into(),
        })
        .await
        .unwrap();
        tx.send(IndexJob::Pull {
            repo_id: "c".into(),
        })
        .await
        .unwrap();
        drop(tx);

        handle.await.unwrap();
        let processed = order.lock().unwrap();
        assert_eq!(*processed, vec!["a", "b", "c"]);
    }

    #[tokio::test]
    async fn test_job_queue_skips_locked_repos() {
        let dir = TempDir::new().unwrap();
        let lock_path = dir.path().join(".knot.lock");

        // Acquire a lock first
        let _held_lock = acquire_file_lock(&lock_path).unwrap();

        // Try to acquire again — should fail gracefully
        let result = acquire_file_lock(&lock_path);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_process_repository_nonexistent_skips() {
        let dir = TempDir::new().unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();

        let state = create_test_state(&workspace).await;

        let repo = RepoEntry {
            id: "nonexistent".into(),
            url: "https://invalid.example.com/nonexistent.git".into(),
            local_path: workspace.join("nonexistent").to_string_lossy().into(),
            auth_type: AuthType::Ssh,
            branch: "main".into(),
            webhook_secret: None,
            last_indexed: None,
            status: RepoStatus::Idle,
        };

        // Should fail during git clone but not panic
        let result = process_repository(&repo, &state).await;
        // The error is expected — we don't have a real git remote
        // The test verifies the function runs without panicking
        assert!(result.is_err());
    }
}
