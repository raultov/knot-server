use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use std::sync::Arc;

use crate::handlers::models::*;
use crate::state::AppState;

#[utoipa::path(
    post,
    path = "/api/webhook/{id}",
    tag = "Webhooks",
    request_body(content_type = "application/json", description = "Raw webhook payload (validated via HMAC signature)"),
    params(
        ("id" = String, Path, description = "Repository ID"),
    ),
    responses(
        (status = 202, description = "Webhook accepted, indexing job enqueued"),
        (status = 401, description = "Invalid or missing webhook signature", body = ErrorResponse),
        (status = 404, description = "Repository not found", body = ErrorResponse),
    ),
    description = "Endpoint for Git provider webhooks (GitHub, GitLab, Bitbucket). Validates payload signatures and triggers incremental re-indexing on push events.",
)]
pub async fn webhook_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    // Check repo exists and get webhook secret
    let webhook_secret = {
        let registry = state.registry.lock().unwrap();
        match registry.get(&id) {
            Some(entry) => entry.webhook_secret.clone(),
            None => {
                return error_response(
                    StatusCode::NOT_FOUND,
                    format!("Repository '{}' not found", id),
                );
            }
        }
    };

    let Some(secret) = webhook_secret else {
        return error_response(
            StatusCode::UNAUTHORIZED,
            "Webhook secret not configured for this repository",
        );
    };

    // Try GitLab token validation
    if let Some(token) = headers.get("X-Gitlab-Token").and_then(|v| v.to_str().ok()) {
        if crate::webhook::validate_gitlab_token(token, &secret) {
            return enqueue_pull_job(&state, &id).await;
        }
        return error_response(StatusCode::UNAUTHORIZED, "Invalid GitLab webhook token");
    }

    // Try GitHub signature validation
    if let Some(sig) = headers
        .get("X-Hub-Signature-256")
        .and_then(|v| v.to_str().ok())
    {
        if crate::webhook::validate_github_signature(sig, &body, &secret) {
            return enqueue_pull_job(&state, &id).await;
        }
        return error_response(StatusCode::UNAUTHORIZED, "Invalid GitHub webhook signature");
    }

    // Try Bitbucket signature validation
    if let Some(sig) = headers.get("X-Hub-Signature").and_then(|v| v.to_str().ok()) {
        if crate::webhook::validate_bitbucket_signature(sig, &body, &secret) {
            return enqueue_pull_job(&state, &id).await;
        }
        return error_response(
            StatusCode::UNAUTHORIZED,
            "Invalid Bitbucket webhook signature",
        );
    }

    error_response(
        StatusCode::UNAUTHORIZED,
        "Missing webhook signature header (X-Gitlab-Token, X-Hub-Signature-256, or X-Hub-Signature)",
    )
}

async fn enqueue_pull_job(state: &Arc<AppState>, repo_id: &str) -> Response {
    let job = crate::models::IndexJob::Pull {
        repo_id: repo_id.to_string(),
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
            tracing::error!("Failed to enqueue webhook Pull job for {}: {e}", repo_id);
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to enqueue indexing job",
            );
        }
    }
    tracing::info!("Webhook validated for '{}', enqueued Pull job", repo_id);
    (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({
            "message": "Webhook received, indexing job enqueued",
            "repo_id": repo_id
        })),
    )
        .into_response()
}
