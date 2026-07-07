use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use std::sync::Arc;

use crate::handlers::models::*;
use crate::models::AppState;
use crate::models::{RegisterRepoRequest, RegisterRepoResponse, RepoEntry, RepoListResponse};

#[utoipa::path(
    get,
    path = "/api/repos",
    tag = "Repositories",
    responses(
        (status = 200, description = "List of all registered repositories", body = RepoListResponse),
    ),
    description = "List all registered Git repositories with their current status and metadata.",
)]
pub async fn list_repos_handler(State(state): State<Arc<AppState>>) -> Response {
    let mut registry = state.registry.lock().unwrap();
    let repos = registry.list().to_vec();
    let response = RepoListResponse {
        repositories: repos,
    };
    (StatusCode::OK, Json(response)).into_response()
}

#[utoipa::path(
    get,
    path = "/api/repos/{id}",
    tag = "Repositories",
    params(
        ("id" = String, Path, description = "Repository ID"),
    ),
    responses(
        (status = 200, description = "Repository details", body = RepoEntry),
        (status = 404, description = "Repository not found", body = ErrorResponse),
    ),
    description = "Get detailed information about a single registered repository.",
)]
pub async fn get_repo_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let mut registry = state.registry.lock().unwrap();
    match registry.get(&id) {
        Some(entry) => (StatusCode::OK, Json(entry.clone())).into_response(),
        None => error_response(
            StatusCode::NOT_FOUND,
            format!("Repository '{}' not found", id),
        ),
    }
}

#[utoipa::path(
    post,
    path = "/api/repos",
    tag = "Repositories",
    request_body = RegisterRepoRequest,
    responses(
        (status = 202, description = "Repository registered (or re-registered) and clone job enqueued", body = RegisterRepoResponse),
        (status = 429, description = "Indexing queue is full", body = ErrorResponse),
    ),
    description = "Register a new Git repository, or re-register an existing one. The endpoint is idempotent: if a repository with the same derived id already exists, it is re-registered and a fresh Clone job is enqueued. The destructive cleanup (wiping the Neo4j/Qdrant data and the local directory) is performed by the indexing worker under its file lock — not by this endpoint — so a re-registration never races with in-flight indexing. The response message indicates whether the call was a fresh registration or a re-registration."
)]
pub async fn register_repo_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<RegisterRepoRequest>,
) -> Response {
    if body.url.trim().is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "Repository URL cannot be empty");
    }

    let id = body.generate_id();
    if id.is_empty() {
        return error_response(
            StatusCode::BAD_REQUEST,
            "Generated repository ID is empty (invalid URL format)",
        );
    }

    let local_path = crate::models::repo_local_path(&state.workspace_dir, &id);

    let entry = RepoEntry {
        id: id.clone(),
        url: body.url.clone(),
        auth_type: body.auth_type.clone(),
        local_path,
        branch: body.branch.clone(),
        webhook_secret: body.webhook_secret.clone(),
        last_indexed: None,
        status: crate::models::RepoStatus::Queued,
    };

    let mut registry = state.registry.lock().unwrap();
    match registry.add_or_replace(entry) {
        Ok(was_replaced) => {
            // Enqueue a Clone job. The worker is the sole owner of destructive
            // cleanup: when it picks up this job it wipes the databases and the
            // local directory *under the file lock*, then clones from scratch
            // (see `worker::decide_job_plan`). The handler deliberately does NOT
            // touch `local_path` here — doing so in a background task raced with
            // the worker and left previously-healthy repos corrupted (Bug A).
            let job = crate::models::IndexJob::Clone {
                repo_id: id.clone(),
            };
            match state.job_tx.try_send(job) {
                Ok(()) => {}
                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                    let _ = registry.update_status(&id, crate::models::RepoStatus::Pending);
                    return error_response(
                        StatusCode::TOO_MANY_REQUESTS,
                        "Server is at maximum capacity: indexing queue is full",
                    );
                }
                Err(e) => {
                    tracing::error!("Failed to enqueue Clone job for {}: {e}", id);
                    let _ = registry.update_status(&id, crate::models::RepoStatus::Pending);
                }
            }

            if was_replaced {
                tracing::info!(
                    "Re-registered repository '{}' (url: {}, auth: {:?})",
                    id,
                    body.url,
                    body.auth_type
                );
            } else {
                tracing::info!(
                    "Registered repository '{}' (url: {}, auth: {:?})",
                    id,
                    body.url,
                    body.auth_type
                );
            }

            let response = RegisterRepoResponse {
                id,
                message: if was_replaced {
                    "Repository re-registered successfully".into()
                } else {
                    "Repository registered successfully".into()
                },
            };
            (StatusCode::ACCEPTED, Json(response)).into_response()
        }
        Err(e) => {
            let msg = e.to_string();
            error_response(StatusCode::INTERNAL_SERVER_ERROR, msg)
        }
    }
}

#[utoipa::path(
    delete,
    path = "/api/repos/{id}",
    tag = "Repositories",
    params(
        ("id" = String, Path, description = "Repository ID"),
    ),
    responses(
        (status = 200, description = "Repository deleted successfully", body = serde_json::Value),
        (status = 404, description = "Repository not found", body = ErrorResponse),
    ),
    description = "Delete a repository and clean up its databases and local files.",
)]
pub async fn delete_repo_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    // Scope the (non-Send) registry guard so it is fully released before the
    // `.await` below — otherwise the handler future is not `Send`/`Sync` and
    // fails axum's `Handler` bound.
    let removed = {
        let mut registry = state.registry.lock().unwrap();
        registry.remove(&id)
    };

    match removed {
        Ok(()) => {
            // Same cleanup the worker performs on a wipe, but for a full
            // delete: databases + local directory + progress snapshot/tracker.
            crate::cleanup::cleanup_repo_artifacts(
                &state,
                &id,
                crate::cleanup::CleanupScope {
                    databases: true,
                    local_dir: true,
                    progress: true,
                },
            )
            .await;

            tracing::info!("Deleted repository '{}'", id);
            (
                StatusCode::OK,
                Json(serde_json::json!({"message": "Repository deleted"})),
            )
                .into_response()
        }
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("not found") {
                error_response(StatusCode::NOT_FOUND, msg)
            } else {
                error_response(StatusCode::INTERNAL_SERVER_ERROR, msg)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::AuthType;
    use knot::db::graph::ConnectExt;
    use knot::db::vector::VectorConnectExt;
    use knot::pipeline::progress::ProgressTracker;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use tempfile::TempDir;

    async fn create_test_state(workspace: &std::path::Path) -> Arc<AppState> {
        let registry = crate::registry::Registry::load_or_create(workspace)
            .expect("Failed to create test registry");

        let graph_db =
            knot::db::graph::GraphDb::connect("bolt://localhost:9999", "neo4j", "badpassword")
                .await
                .expect("connect for test db");

        let vector_db =
            knot::db::vector::VectorDb::connect("http://localhost:9999", "test_collection", 384)
                .await
                .expect("connect for test vector db");

        let (job_tx, _job_rx) = tokio::sync::mpsc::channel::<crate::models::IndexJob>(16);

        Arc::new(AppState {
            vector_db: Arc::new(vector_db),
            graph_db: Arc::new(graph_db),
            embedder: None,
            workspace_dir: workspace.to_string_lossy().into(),
            registry: Arc::new(Mutex::new(registry)),
            job_tx,
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

    async fn create_test_state_with_rx(
        workspace: &std::path::Path,
    ) -> (
        Arc<AppState>,
        tokio::sync::mpsc::Receiver<crate::models::IndexJob>,
    ) {
        let registry = crate::registry::Registry::load_or_create(workspace)
            .expect("Failed to create test registry");
        let graph_db =
            knot::db::graph::GraphDb::connect("bolt://localhost:9999", "neo4j", "badpassword")
                .await
                .expect("connect for test db");
        let vector_db =
            knot::db::vector::VectorDb::connect("http://localhost:9999", "test_collection", 384)
                .await
                .expect("connect for test vector db");
        let (job_tx, job_rx) = tokio::sync::mpsc::channel::<crate::models::IndexJob>(16);
        (
            Arc::new(AppState {
                vector_db: Arc::new(vector_db),
                graph_db: Arc::new(graph_db),
                embedder: None,
                workspace_dir: workspace.to_string_lossy().into(),
                registry: Arc::new(Mutex::new(registry)),
                job_tx,
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
            }),
            job_rx,
        )
    }

    #[tokio::test]
    async fn test_reregister_does_not_delete_local_dir_in_handler() {
        let dir = TempDir::new().unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let (state, mut job_rx) = create_test_state_with_rx(&workspace).await;

        let url = "git@github.com:org/reg-test.git";
        let id = crate::models::RegisterRepoRequest {
            url: url.into(),
            auth_type: AuthType::Ssh,
            branch: "main".into(),
            webhook_secret: None,
        }
        .generate_id();

        // Pre-existing, healthy local checkout: a .git dir and a file that a
        // stray background cleanup would have deleted.
        let local_path = crate::models::repo_local_path(&state.workspace_dir, &id);
        std::fs::create_dir_all(std::path::Path::new(&local_path).join(".git")).unwrap();
        std::fs::write(std::path::Path::new(&local_path).join("keep.txt"), b"keep").unwrap();

        // Register the same repo once so the second call is a re-registration.
        let entry = RepoEntry {
            id: id.clone(),
            url: url.into(),
            auth_type: AuthType::Ssh,
            local_path: local_path.clone(),
            branch: "main".into(),
            webhook_secret: None,
            last_indexed: Some("2026-07-06T00:00:00Z".into()),
            status: crate::models::RepoStatus::Indexed,
        };
        state
            .registry
            .lock()
            .unwrap()
            .add_or_replace(entry)
            .unwrap();

        let body = crate::models::RegisterRepoRequest {
            url: url.into(),
            auth_type: AuthType::Ssh,
            branch: "main".into(),
            webhook_secret: None,
        };
        let response = register_repo_handler(State(state.clone()), Json(body)).await;
        assert_eq!(response.status(), StatusCode::ACCEPTED);

        // Give any (now-removed) background task a chance to run.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        assert!(
            std::path::Path::new(&local_path).join("keep.txt").exists(),
            "handler must NOT delete the local dir on re-registration (worker does it)"
        );

        // Exactly one Clone job was enqueued.
        match job_rx.try_recv() {
            Ok(crate::models::IndexJob::Clone { repo_id }) => assert_eq!(repo_id, id),
            other => panic!("expected exactly one Clone job, got {other:?}"),
        }
        assert!(job_rx.try_recv().is_err(), "no extra jobs expected");
    }

    #[tokio::test]
    async fn test_delete_removes_tracker_from_state() {
        let dir = TempDir::new().unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();

        let state = create_test_state(&workspace).await;

        let entry = RepoEntry {
            id: "delete-test".into(),
            url: "git@github.com:org/delete-test.git".into(),
            local_path: "/tmp/delete-test".into(),
            auth_type: AuthType::Ssh,
            branch: "main".into(),
            webhook_secret: None,
            last_indexed: None,
            status: crate::models::RepoStatus::Indexed,
        };
        state
            .registry
            .lock()
            .unwrap()
            .add_or_replace(entry)
            .unwrap();

        state
            .progress_trackers
            .lock()
            .unwrap()
            .insert("delete-test".into(), Arc::new(ProgressTracker::new()));

        let response = delete_repo_handler(State(state.clone()), Path("delete-test".into())).await;

        assert_eq!(response.status(), StatusCode::OK);

        let map = state.progress_trackers.lock().unwrap();
        assert!(
            !map.contains_key("delete-test"),
            "tracker should be removed after delete"
        );
    }
}
