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

    // 2. Git operation
    let is_local = crate::local_sync::is_local_path(&repo.url);
    let exists = Path::new(&repo.local_path).join(".git").exists();
    {
        let mut registry = state.registry.lock().unwrap();
        if is_local {
            registry.update_status(&repo.id, crate::models::RepoStatus::Pulling)?;
            tracing::info!("Worker: status=pulling (local) for '{}'", repo.id);
        } else if exists {
            registry.update_status(&repo.id, crate::models::RepoStatus::Pulling)?;
            tracing::info!("Worker: status=pulling for '{}'", repo.id);
        } else {
            registry.update_status(&repo.id, crate::models::RepoStatus::Cloning)?;
            tracing::info!("Worker: status=cloning for '{}'", repo.id);
        }
    }

    if is_local {
        tracing::info!(
            "Worker: syncing local working tree for '{}' from {}",
            repo.id,
            repo.url
        );
        let src = repo.url.clone();
        let dst = repo.local_path.clone();
        tokio::task::spawn_blocking(move || crate::local_sync::sync_local_working_tree(&src, &dst))
            .await??;
        tracing::info!("Worker: local sync complete for '{}'", repo.id);
    } else if exists {
        tracing::info!("Worker: pulling '{}' from {}", repo.id, repo.url);
        crate::git::run_git_pull(repo).await?;
        tracing::info!("Worker: pull complete for '{}'", repo.id);
    } else {
        tracing::info!("Worker: cloning '{}' from {}", repo.id, repo.url);
        crate::git::run_git_clone(repo).await?;
        tracing::info!("Worker: clone complete for '{}'", repo.id);
    }

    // 3. Update status to indexing
    {
        let mut registry = state.registry.lock().unwrap();
        registry.update_status(&repo.id, crate::models::RepoStatus::Indexing)?;
        tracing::info!("Worker: status=indexing for '{}'", repo.id);
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
        batch_size: state.batch_size,
        clean: false,
        dependency_repos: Vec::new(),
        watch: false,
        dry_run: false,
        custom_ca_certs: None,
        output_format: knot::config::OutputFormat::Markdown,
        ingest_concurrency: state.ingest_concurrency,
        rayon_threads: state.rayon_threads,
        include_config_files: false,
    };

    // 5. Load IndexState
    //    For local paths, defend against a stale on-disk state file from an
    //    older `knot` version (no `version` field → version=0 < current).
    //    `local_sync` preserves `.knot/` across syncs because it is the
    //    indexer's incremental state, so a knot-version transition would
    //    otherwise block every future sync. Clear the stale file and, if
    //    load still fails for any other reason, fall back to a fresh state
    //    rather than failing the whole local sync job.
    let mut index_state = if is_local {
        if crate::local_sync::clear_stale_index_state(&repo.local_path) {
            tracing::warn!(
                "Removed stale .knot/index_state.json for local repo '{}' \
                 (older knot format); forcing a clean re-index",
                repo.id
            );
        }
        match knot::pipeline::state::IndexState::load(&repo.local_path) {
            Ok(state) => state,
            Err(e) => {
                tracing::warn!(
                    "IndexState::load failed for local repo '{}': {e:#}; \
                     starting from a fresh state",
                    repo.id
                );
                let state_file = Path::new(&repo.local_path)
                    .join(".knot")
                    .join("index_state.json");
                let _ = std::fs::remove_file(&state_file);
                knot::pipeline::state::IndexState::default()
            }
        }
    } else {
        knot::pipeline::state::IndexState::load(&repo.local_path)?
    };

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
        registry.update_status(&repo.id, crate::models::RepoStatus::Indexed)?;
        registry.update_last_indexed(&repo.id)?;
        tracing::info!("Worker: status=indexed for '{}'", repo.id);
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
        Arc::new(crate::state::AppState {
            vector_db: Arc::new(vector_db),
            graph_db: Arc::new(graph_db),
            embedder: None,
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
            batch_size: 64,
            ingest_concurrency: 4,
            start_time: std::time::Instant::now(),
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
            status: RepoStatus::Indexed,
        };

        // Should fail during git clone but not panic
        let result = process_repository(&repo, &state).await;
        // The error is expected — we don't have a real git remote
        // The test verifies the function runs without panicking
        assert!(result.is_err());
    }
}
