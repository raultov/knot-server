use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use std::sync::Arc;

use crate::handlers::models::*;
use crate::models::AppState;

#[utoipa::path(
    post,
    path = "/api/repos/{id}/sync",
    tag = "Indexing",
    params(
        ("id" = String, Path, description = "Repository ID"),
    ),
    responses(
        (status = 202, description = "Sync job enqueued", body = serde_json::Value),
        (status = 404, description = "Repository not found", body = ErrorResponse),
        (status = 429, description = "Indexing queue is full", body = ErrorResponse),
    ),
    description = "Manually trigger a sync (pull + re-index) for a repository.",
)]
pub async fn sync_repo_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    // Check repo exists
    {
        let registry = state.registry.lock().unwrap();
        if registry.get(&id).is_none() {
            return error_response(
                StatusCode::NOT_FOUND,
                format!("Repository '{}' not found", id),
            );
        }
    }

    // Enqueue Pull job
    let job = crate::models::IndexJob::Pull {
        repo_id: id.clone(),
    };
    match state.job_tx.try_send(job) {
        Ok(()) => {}
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
            return error_response(
                StatusCode::TOO_MANY_REQUESTS,
                "Server is at maximum capacity: indexing queue is full",
            );
        }
        Err(e) => {
            tracing::error!("Failed to enqueue Pull job for {}: {e}", id);
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to enqueue sync job",
            );
        }
    }

    tracing::info!("Enqueued sync job for '{}'", id);

    (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({
            "message": "Sync job enqueued",
            "repo_id": id
        })),
    )
        .into_response()
}
