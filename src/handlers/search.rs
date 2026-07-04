use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use std::sync::Arc;

use crate::handlers::models::*;
use crate::models::AppState;

#[utoipa::path(
    get,
    path = "/api/repos/{id}/search",
    tag = "Search",
    params(
        ("id" = String, Path, description = "Repository ID"),
        SearchParams,
    ),
    responses(
        (status = 200, description = "Search results", body = serde_json::Value),
        (status = 400, description = "Missing or invalid query parameter", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse),
    ),
    description = "Semantic + structural search. Find code by meaning, class name, method signature, or docstrings.",
)]
pub async fn search_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(params): Query<SearchParams>,
) -> Response {
    let query = match &params.q {
        Some(q) if !q.trim().is_empty() => q.as_str(),
        _ => return error_response(StatusCode::BAD_REQUEST, "Missing required parameter 'q'"),
    };

    let max_results = params.max_results.unwrap_or(5);

    let embedder = match &state.embedder {
        Some(e) => e,
        None => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Embedding model not initialized",
            );
        }
    };

    match knot::cli_tools::run_search_hybrid_context(
        query,
        max_results,
        Some(&id),
        &state.vector_db,
        &state.graph_db,
        embedder,
    )
    .await
    {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err(e) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Search failed: {e}"),
        ),
    }
}

#[utoipa::path(
    get,
    path = "/api/repos/{id}/callers",
    tag = "Search",
    params(
        ("id" = String, Path, description = "Repository ID"),
        CallersParams,
    ),
    responses(
        (status = 200, description = "Caller analysis results", body = serde_json::Value),
        (status = 400, description = "Missing or invalid query parameter", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse),
    ),
    description = "Find all callers referencing a specific entity. Returns reverse dependency graph.",
)]
pub async fn callers_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(params): Query<CallersParams>,
) -> Response {
    let entity_name = match &params.entity {
        Some(e) if !e.trim().is_empty() => e.as_str(),
        _ => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "Missing required parameter 'entity'",
            );
        }
    };

    match knot::cli_tools::run_find_callers(entity_name, Some(&id), &state.graph_db).await {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err(e) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Find callers failed: {e}"),
        ),
    }
}

#[utoipa::path(
    get,
    path = "/api/repos/{id}/explore",
    tag = "Search",
    params(
        ("id" = String, Path, description = "Repository ID"),
        ExploreParams,
    ),
    responses(
        (status = 200, description = "File exploration results", body = serde_json::Value),
        (status = 400, description = "Missing or invalid query parameter", body = ErrorResponse),
        (status = 404, description = "Repository not found", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse),
    ),
    description = "Explore a file's architecture. Returns all classes, methods, and properties with signatures.",
)]
pub async fn explore_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(params): Query<ExploreParams>,
) -> Response {
    let relative = match &params.path {
        Some(p) if !p.trim().is_empty() => p.as_str(),
        _ => return error_response(StatusCode::BAD_REQUEST, "Missing required parameter 'path'"),
    };

    // Neo4j stores absolute file paths from the indexer. Build the full path
    // by prepending the repo's local_path.
    let full_path = {
        let mut registry = state.registry.lock().unwrap();
        match registry.get(&id) {
            Some(entry) => {
                let trimmed = relative.trim_start_matches('/');
                format!("{}/{}", entry.local_path.trim_end_matches('/'), trimmed)
            }
            None => {
                return error_response(
                    StatusCode::NOT_FOUND,
                    format!("Repository '{}' not found", id),
                );
            }
        }
    };

    match knot::cli_tools::run_explore_file(&full_path, Some(&id), &state.graph_db).await {
        Ok((_display_path, entities_json)) => (StatusCode::OK, Json(entities_json)).into_response(),
        Err(e) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Explore file failed: {e}"),
        ),
    }
}

#[utoipa::path(
    get,
    path = "/api/repos/{id}/deps",
    tag = "Search",
    params(
        ("id" = String, Path, description = "Repository ID"),
        DepsParams,
    ),
    responses(
        (status = 200, description = "Dependency lookup results", body = serde_json::Value),
        (status = 400, description = "Missing or invalid query parameter", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse),
    ),
    description = "Cross-repository dependency lookup. Shows which repos depend on this one or vice versa.",
)]
pub async fn deps_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(params): Query<DepsParams>,
) -> Response {
    let max_depth = params.max_depth.unwrap_or(3);
    let reverse = params.reverse.unwrap_or(false);

    match knot::cli_tools::run_deps(&id, max_depth, reverse, &state.graph_db).await {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err(e) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Deps lookup failed: {e}"),
        ),
    }
}
