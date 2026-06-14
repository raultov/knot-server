use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use std::sync::Arc;

use crate::handlers::models::*;
use crate::models::{RegisterRepoRequest, RegisterRepoResponse, RepoEntry, RepoListResponse};
use crate::models::AppState;

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
    let registry = state.registry.lock().unwrap();
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
    let registry = state.registry.lock().unwrap();
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
        (status = 202, description = "Repository registered and clone job enqueued", body = RegisterRepoResponse),
        (status = 409, description = "Repository already exists", body = ErrorResponse),
        (status = 429, description = "Indexing queue is full", body = ErrorResponse),
    ),
    description = "Register a new Git repository. The server clones it and queues it for indexing.",
)]
pub async fn register_repo_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<RegisterRepoRequest>,
) -> Response {
    let id = body.generate_id();
    let local_path = crate::models::repo_local_path(&state.workspace_dir, &id);

    let entry = crate::models::RepoEntry {
        id: id.clone(),
        url: body.url.clone(),
        auth_type: body.auth_type.clone(),
        local_path,
        branch: body.branch.clone(),
        webhook_secret: body.webhook_secret.clone(),
        last_indexed: None,
        status: crate::models::RepoStatus::Pending,
    };

    let mut registry = state.registry.lock().unwrap();
    match registry.add(entry) {
        Ok(()) => {
            // Enqueue Clone job for the new repository
            let job = crate::models::IndexJob::Clone {
                repo_id: id.clone(),
            };
            match state.job_tx.try_send(job) {
                Ok(()) => {}
                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                    let _ = registry.remove(&id);
                    return error_response(
                        StatusCode::TOO_MANY_REQUESTS,
                        "Server is at maximum capacity: indexing queue is full",
                    );
                }
                Err(e) => {
                    tracing::error!("Failed to enqueue Clone job for {}: {e}", id);
                }
            }

            tracing::info!(
                "Registered repository '{}' (url: {}, auth: {:?})",
                id,
                body.url,
                body.auth_type
            );

            let response = RegisterRepoResponse {
                id,
                message: "Repository registered successfully".into(),
            };
            (StatusCode::ACCEPTED, Json(response)).into_response()
        }
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("already exists") {
                error_response(StatusCode::CONFLICT, msg)
            } else {
                error_response(StatusCode::INTERNAL_SERVER_ERROR, msg)
            }
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
    let mut registry = state.registry.lock().unwrap();

    match registry.remove(&id) {
        Ok(()) => {
            // Clean up databases in background (fire-and-forget)
            let graph_db = state.graph_db.clone();
            let vector_db = state.vector_db.clone();
            let rid = id.clone();
            tokio::spawn(async move {
                crate::cleanup::delete_repo_from_databases(&rid, &graph_db, &vector_db).await;
            });

            // Clean up local directory
            let repo_path = crate::models::repo_local_path(&state.workspace_dir, &id);
            if std::path::Path::new(&repo_path).exists()
                && let Err(e) = std::fs::remove_dir_all(&repo_path)
            {
                tracing::warn!("Failed to remove repo directory {}: {e}", repo_path);
            }
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
