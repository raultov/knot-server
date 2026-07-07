use std::path::{Path, PathBuf};
use std::sync::Arc;

use knot::db::graph::{DeleteExt, GraphDb};
use knot::db::vector::{VectorDb, VectorDeleteExt};

use crate::models::AppState;

/// Delete all data for a repository from both Neo4j and Qdrant.
pub async fn delete_repo_from_databases(
    repo_id: &str,
    graph_db: &Arc<GraphDb>,
    vector_db: &Arc<VectorDb>,
) {
    if let Err(e) = graph_db.delete_by_repo(repo_id).await {
        tracing::error!("Failed to delete Neo4j entities for '{}': {e}", repo_id);
    }
    if let Err(e) = graph_db.delete_repository(repo_id).await {
        tracing::error!(
            "Failed to delete Neo4j Repository node for '{}': {e}",
            repo_id
        );
    }
    if let Err(e) = vector_db.delete_by_repo(repo_id).await {
        tracing::error!("Failed to delete Qdrant vectors for '{}': {e}", repo_id);
    }
}

/// Which artifacts of a repository a cleanup pass should destroy.
///
/// Every field is independent so the different call-sites can express their
/// exact intent:
/// - the `Clone` job wipes `databases` + `local_dir` (keeps the progress
///   snapshot: the fresh run will overwrite it),
/// - the worker error-path wipes `progress` always and `databases`/`local_dir`
///   only for repos that never indexed successfully (see
///   [`crate::worker::should_wipe_on_failure`]),
/// - `delete_repo_handler` wipes everything.
#[derive(Debug, Clone, Copy)]
pub struct CleanupScope {
    pub databases: bool,
    pub local_dir: bool,
    pub progress: bool,
}

/// Best-effort removal of a repository's artifacts according to `scope`.
///
/// Every step is independent and non-fatal: a failure in one (e.g. a database
/// that is down) only logs and never prevents the others from running, so a
/// caller can rely on "as much as possible was cleaned". The database part is
/// self-healing on the next full index run (knot's pipeline `delete_by_repo`s
/// before re-ingesting), which is why best-effort is acceptable here.
///
/// The local directory is derived from `repo_id` via
/// [`crate::models::repo_local_path`] so it matches the path the worker and the
/// registry use. Note (see the design doc §2.5): when this runs under the
/// worker's file lock, removing the directory that holds `.knot.lock` does not
/// invalidate the already-acquired lock on Linux (the fd survives the unlink).
pub async fn cleanup_repo_artifacts(state: &Arc<AppState>, repo_id: &str, scope: CleanupScope) {
    if scope.databases {
        delete_repo_from_databases(repo_id, &state.graph_db, &state.vector_db).await;
    }

    if scope.local_dir {
        let repo_path = crate::models::repo_local_path(&state.workspace_dir, repo_id);
        if Path::new(&repo_path).exists()
            && let Err(e) = std::fs::remove_dir_all(&repo_path)
        {
            tracing::warn!("Failed to remove repo directory {}: {e}", repo_path);
        }
    }

    if scope.progress {
        crate::progress_store::remove_snapshot(&PathBuf::from(&state.workspace_dir), repo_id);
        if let Ok(mut trackers) = state.progress_trackers.lock() {
            trackers.remove(repo_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::RepoStatus;
    use crate::registry::Registry;
    use knot::db::graph::ConnectExt;
    use knot::db::vector::VectorConnectExt;
    use knot::pipeline::progress::ProgressTracker;
    use std::collections::HashMap;
    use std::sync::Mutex;
    use tempfile::TempDir;

    async fn create_test_state(workspace: &Path) -> Arc<AppState> {
        let registry = Registry::load_or_create(workspace).unwrap();
        let graph_db =
            knot::db::graph::GraphDb::connect("bolt://localhost:9999", "neo4j", "badpassword")
                .await
                .expect("connect for test db");
        let vector_db =
            knot::db::vector::VectorDb::connect("http://localhost:9999", "test_collection", 384)
                .await
                .expect("connect for test vector db");
        Arc::new(AppState {
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
            progress_trackers: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    #[tokio::test]
    async fn test_cleanup_artifacts_removes_local_dir_and_progress_snapshot() {
        let dir = TempDir::new().unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let state = create_test_state(&workspace).await;

        let repo_id = "gamma";

        // Populate repos/<id>/ with some content.
        let repo_path = crate::models::repo_local_path(&state.workspace_dir, repo_id);
        std::fs::create_dir_all(&repo_path).unwrap();
        std::fs::write(Path::new(&repo_path).join("file.txt"), b"data").unwrap();

        // Write a progress snapshot and register an in-memory tracker.
        let persisted = crate::progress_store::PersistedProgress {
            repo_id: repo_id.into(),
            status: RepoStatus::Indexing,
            stage: "parsing".into(),
            total_files: 1,
            parsed_files: 0,
            percent_complete: 0.0,
            entities_ingested: 0,
            batches_ingested: 0,
            error: None,
            updated_at: crate::time_utils::chrono_now(),
        };
        crate::progress_store::write_snapshot(&workspace, &persisted).unwrap();
        state
            .progress_trackers
            .lock()
            .unwrap()
            .insert(repo_id.into(), Arc::new(ProgressTracker::new()));

        let snapshot = crate::progress_store::snapshot_path(&workspace, repo_id);
        assert!(Path::new(&repo_path).exists());
        assert!(snapshot.exists());

        cleanup_repo_artifacts(
            &state,
            repo_id,
            CleanupScope {
                databases: false,
                local_dir: true,
                progress: true,
            },
        )
        .await;

        assert!(!Path::new(&repo_path).exists(), "local dir must be removed");
        assert!(!snapshot.exists(), "progress snapshot must be removed");
        assert!(
            !state
                .progress_trackers
                .lock()
                .unwrap()
                .contains_key(repo_id),
            "in-memory tracker must be removed"
        );
    }

    #[tokio::test]
    async fn test_cleanup_artifacts_is_idempotent_when_nothing_exists() {
        let dir = TempDir::new().unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let state = create_test_state(&workspace).await;

        let scope = CleanupScope {
            databases: false,
            local_dir: true,
            progress: true,
        };

        // First call on a repo with no artifacts at all.
        cleanup_repo_artifacts(&state, "ghost", scope).await;
        // Second call must not panic either.
        cleanup_repo_artifacts(&state, "ghost", scope).await;
    }
}
