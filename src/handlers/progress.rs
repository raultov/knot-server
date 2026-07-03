use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use std::sync::Arc;
use utoipa::ToSchema;

use crate::handlers::models::*;
use crate::models::{AppState, RepoStatus};

#[derive(Debug, Serialize, ToSchema)]
pub struct ProgressResponse {
    pub repo_id: String,
    pub status: RepoStatus,
    pub stage: String,
    pub total_files: u64,
    pub parsed_files: u64,
    pub percent_complete: f32,
    pub entities_ingested: u64,
    pub batches_ingested: u64,
    pub error: Option<String>,
}

pub fn build_progress_response(
    repo_id: &str,
    status: RepoStatus,
    snapshot: Option<knot::pipeline::progress::IndexingProgress>,
) -> ProgressResponse {
    match snapshot {
        Some(snap) => {
            let stage_str = serde_json::to_value(snap.stage)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_else(|| "idle".to_string());

            ProgressResponse {
                repo_id: repo_id.to_string(),
                status,
                stage: stage_str,
                total_files: snap.total_files,
                parsed_files: snap.parsed_files,
                percent_complete: snap.percent_complete,
                entities_ingested: snap.entities_ingested,
                batches_ingested: snap.batches_ingested,
                error: snap.error,
            }
        }
        None => ProgressResponse {
            repo_id: repo_id.to_string(),
            status,
            stage: "idle".to_string(),
            total_files: 0,
            parsed_files: 0,
            percent_complete: 0.0,
            entities_ingested: 0,
            batches_ingested: 0,
            error: None,
        },
    }
}

#[utoipa::path(
    get,
    path = "/api/repos/{id}/progress",
    tag = "Indexing",
    params(
        ("id" = String, Path, description = "Repository ID"),
    ),
    responses(
        (status = 200, description = "Indexing progress for the repository", body = ProgressResponse),
        (status = 404, description = "Repository not found", body = ErrorResponse),
    ),
    description = "Get live indexing progress (stage, percent complete, counters) for a repository.",
)]
pub async fn progress_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let status = {
        let registry = state.registry.lock().unwrap();
        match registry.get(&id) {
            Some(entry) => entry.status.clone(),
            None => {
                return error_response(
                    StatusCode::NOT_FOUND,
                    format!("Repository '{}' not found", id),
                );
            }
        }
    };

    let snapshot = {
        let map = state.progress_trackers.lock().unwrap();
        map.get(&id).map(|tracker| tracker.snapshot())
    };

    let response = build_progress_response(&id, status, snapshot);
    (StatusCode::OK, Json(response)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::AuthType;
    use knot::pipeline::progress::{IndexingProgress, IndexingStage};

    #[test]
    fn test_build_progress_response_none_snapshot_returns_idle() {
        let resp = build_progress_response("my-repo", RepoStatus::Indexed, None);

        assert_eq!(resp.repo_id, "my-repo");
        assert_eq!(resp.status, RepoStatus::Indexed);
        assert_eq!(resp.stage, "idle");
        assert_eq!(resp.total_files, 0);
        assert_eq!(resp.parsed_files, 0);
        assert_eq!(resp.percent_complete, 0.0);
        assert_eq!(resp.entities_ingested, 0);
        assert_eq!(resp.batches_ingested, 0);
        assert_eq!(resp.error, None);
    }

    #[test]
    fn test_build_progress_response_parsing_stage() {
        let snap = IndexingProgress {
            repo_name: "my-repo".into(),
            stage: IndexingStage::Parsing,
            total_files: 120,
            parsed_files: 55,
            percent_complete: 45.8,
            entities_ingested: 900,
            batches_ingested: 14,
            error: None,
        };

        let resp = build_progress_response("my-repo", RepoStatus::Indexing, Some(snap));

        assert_eq!(resp.repo_id, "my-repo");
        assert_eq!(resp.status, RepoStatus::Indexing);
        assert_eq!(resp.stage, "parsing");
        assert_eq!(resp.total_files, 120);
        assert_eq!(resp.parsed_files, 55);
        assert!((resp.percent_complete - 45.8).abs() < 0.1);
        assert_eq!(resp.entities_ingested, 900);
        assert_eq!(resp.batches_ingested, 14);
        assert_eq!(resp.error, None);
    }

    #[test]
    fn test_build_progress_response_completed_stage() {
        let snap = IndexingProgress {
            repo_name: "my-repo".into(),
            stage: IndexingStage::Completed,
            total_files: 120,
            parsed_files: 120,
            percent_complete: 100.0,
            entities_ingested: 5000,
            batches_ingested: 50,
            error: None,
        };

        let resp = build_progress_response("my-repo", RepoStatus::Indexed, Some(snap));

        assert_eq!(resp.stage, "completed");
        assert_eq!(resp.percent_complete, 100.0);
        assert_eq!(resp.error, None);
    }

    #[test]
    fn test_build_progress_response_failed_stage_with_error() {
        let snap = IndexingProgress {
            repo_name: "my-repo".into(),
            stage: IndexingStage::Failed,
            total_files: 100,
            parsed_files: 42,
            percent_complete: 42.0,
            entities_ingested: 300,
            batches_ingested: 10,
            error: Some("connection refused".into()),
        };

        let resp = build_progress_response("my-repo", RepoStatus::Error, Some(snap));

        assert_eq!(resp.stage, "failed");
        assert_eq!(resp.error, Some("connection refused".into()));
    }

    mod handler_tests {
        use super::*;
        use crate::registry::Registry;
        use axum::Router;
        use axum::body::Body;
        use axum::http::Request;
        use axum::http::StatusCode;
        use axum::routing::get;
        use knot::db::graph::ConnectExt;
        use knot::db::vector::VectorConnectExt;
        use std::collections::HashMap;
        use std::sync::{Arc, Mutex};
        use tempfile::TempDir;
        use tower::ServiceExt;

        fn build_progress_test_app(state: Arc<AppState>) -> Router {
            Router::new()
                .route("/api/repos/{id}/progress", get(super::progress_handler))
                .with_state(state)
        }

        async fn create_progress_test_state(temp_dir: &TempDir) -> Arc<AppState> {
            let workspace = temp_dir.path().to_path_buf();
            let registry =
                Registry::load_or_create(&workspace).expect("Failed to create test registry");

            let graph_db =
                knot::db::graph::GraphDb::connect("bolt://localhost:9999", "neo4j", "badpassword")
                    .await
                    .expect("connect for test db should work");

            let vector_db = knot::db::vector::VectorDb::connect(
                "http://localhost:9999",
                "test_collection",
                384,
            )
            .await
            .expect("connect for test vector db should work");

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

        #[tokio::test]
        async fn test_progress_nonexistent_repo_returns_404() {
            let dir = TempDir::new().unwrap();
            let state = create_progress_test_state(&dir).await;
            let app = build_progress_test_app(state);

            let response = app
                .oneshot(
                    Request::get("/api/repos/nonexistent/progress")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();

            assert_eq!(response.status(), StatusCode::NOT_FOUND);
        }

        #[tokio::test]
        async fn test_progress_idle_for_registered_unindexed_repo() {
            let dir = TempDir::new().unwrap();
            let state = create_progress_test_state(&dir).await;

            let entry = crate::models::RepoEntry {
                id: "test-repo".into(),
                url: "git@github.com:org/test-repo.git".into(),
                local_path: "/tmp/test-repo".into(),
                auth_type: AuthType::Ssh,
                branch: "main".into(),
                webhook_secret: None,
                last_indexed: None,
                status: RepoStatus::Indexed,
            };
            state
                .registry
                .lock()
                .unwrap()
                .add_or_replace(entry)
                .unwrap();

            let app = build_progress_test_app(state);

            let response = app
                .oneshot(
                    Request::get("/api/repos/test-repo/progress")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();

            assert_eq!(response.status(), StatusCode::OK);

            let body_bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
                .await
                .unwrap();
            let json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();

            assert_eq!(json["repo_id"], "test-repo");
            assert_eq!(json["stage"], "idle");
            assert_eq!(json["percent_complete"], 0.0);
            assert_eq!(json["total_files"], 0);
            assert_eq!(json["parsed_files"], 0);
            assert_eq!(json["error"], serde_json::Value::Null);
        }
    }
}
