use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use std::sync::Arc;

use crate::models::AppState;

#[utoipa::path(
    get,
    path = "/api/health",
    tag = "Health",
    responses(
        (status = 200, description = "Server health status"),
    ),
    description = "Check server health, uptime, queue capacity, and repository statistics.",
)]
pub async fn health_handler(State(state): State<Arc<AppState>>) -> Response {
    let mut registry = state.registry.lock().unwrap();
    let repos = registry.list();
    let cloning_count = repos
        .iter()
        .filter(|r| r.status == crate::models::RepoStatus::Cloning)
        .count();
    let pulling_count = repos
        .iter()
        .filter(|r| r.status == crate::models::RepoStatus::Pulling)
        .count();
    let indexing_count = repos
        .iter()
        .filter(|r| r.status == crate::models::RepoStatus::Indexing)
        .count();

    let uptime = state.start_time.elapsed().as_secs();

    let health = serde_json::json!({
        "status": "ok",
        "uptime_seconds": uptime,
        "queue_capacity": state.job_tx.capacity(),
        "repositories_total": repos.len(),
        "repositories_cloning": cloning_count,
        "repositories_pulling": pulling_count,
        "repositories_indexing": indexing_count,
        "workspace_dir": state.workspace_dir,
        "metrics_endpoint": "/metrics",
    });

    (StatusCode::OK, Json(health)).into_response()
}
