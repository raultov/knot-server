use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;
use std::sync::Arc;

use crate::handlers::models::*;
use crate::handlers::scope::{
    ResolvedScope, clamp_max_results, scope_fields, scope_or_error, unknown_repos_error,
};
use crate::models::AppState;

fn extract_required_param(param: Option<&String>) -> Option<&str> {
    param.map(String::as_str).filter(|s| !s.trim().is_empty())
}

/// Empty-registry body for `GET /api/search` (CROSS_REPO_SEARCH_PLAN §3):
/// status 200 with a bare JSON array — the caller asked for "all registered
/// repositories" and there are none.
fn empty_search_response() -> Response {
    (StatusCode::OK, Json(json!([]))).into_response()
}

/// Empty-registry body for `GET /api/callers` (CROSS_REPO_SEARCH_PLAN §3):
/// six empty buckets plus a neutral `resolution` block, shaped byte-for-byte
/// like knot's natural empty response (pinned by E2E scenario G6).
fn empty_callers_response(entity_name: &str) -> Response {
    (
        StatusCode::OK,
        Json(json!({
            "calls": [],
            "extends": [],
            "implements": [],
            "overridden_by": [],
            "overrides": [],
            "references": [],
            "resolution": {
                "fuzzy": false,
                "query": entity_name,
                "targets": [],
                "tier": "none",
                "truncated": false,
            }
        })),
    )
        .into_response()
}

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
#[tracing::instrument(
    name = "search",
    skip_all,
    fields(
        repo_id = %id,
        query_len = tracing::field::Empty,
        max_results = tracing::field::Empty,
    )
)]
pub async fn search_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(params): Query<SearchParams>,
) -> Response {
    let query = match extract_required_param(params.q.as_ref()) {
        Some(q) => q,
        None => return error_response(StatusCode::BAD_REQUEST, "Missing required parameter 'q'"),
    };

    let max_results = params.max_results.unwrap_or(5);

    // Record only the query *length* — never the query text itself, which for a
    // code search may contain proprietary source.
    let span = tracing::Span::current();
    span.record("query_len", query.len());
    span.record("max_results", max_results);

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
        &knot::models::RepoScope::One(id.clone()),
        &knot::cli_tools::SearchContext {
            vector_db: &state.vector_db,
            graph_db: &state.graph_db,
            embedder,
        },
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
#[tracing::instrument(
    name = "callers",
    skip_all,
    fields(repo_id = %id, entity = tracing::field::Empty)
)]
pub async fn callers_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(params): Query<CallersParams>,
) -> Response {
    let entity_name = match extract_required_param(params.entity.as_ref()) {
        Some(e) => e,
        None => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "Missing required parameter 'entity'",
            );
        }
    };
    tracing::Span::current().record("entity", entity_name);

    match knot::cli_tools::run_find_callers(
        entity_name,
        &knot::models::RepoScope::One(id.clone()),
        &state.graph_db,
    )
    .await
    {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err(e) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Find callers failed: {e}"),
        ),
    }
}

#[utoipa::path(
    get,
    path = "/api/search",
    tag = "Search",
    params(GlobalSearchParams),
    responses(
        (status = 200, description = "Search results across the requested repositories (null when there are no hits)", body = serde_json::Value),
        (status = 400, description = "Missing query or unknown repository ids", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse),
    ),
    description = "Semantic + structural search across one, several, or all registered repositories. \
                   `repo` accepts a single id, a comma-separated list, or the sentinel `all` / `*`; \
                   omit it (or use the sentinel) to search every registered repository — the scope \
                   expands to the registry id list, so unregistered repositories are never queried \
                   and an empty registry returns an empty result with 200. Each entity carries \
                   `repo_name`. `max_results` is a global cap across the scope, clamped to 1..=100.",
)]
#[tracing::instrument(
    name = "search_all",
    skip_all,
    fields(
        query_len = tracing::field::Empty,
        max_results = tracing::field::Empty,
        repo_scope = tracing::field::Empty,
        repo_count = tracing::field::Empty,
    )
)]
pub async fn search_all_handler(
    State(state): State<Arc<AppState>>,
    Query(params): Query<GlobalSearchParams>,
) -> Response {
    let query = match extract_required_param(params.q.as_ref()) {
        Some(q) => q,
        None => return error_response(StatusCode::BAD_REQUEST, "Missing required parameter 'q'"),
    };

    let max_results = clamp_max_results(params.max_results);

    // Record only the query *length* — never the query text itself, which for a
    // code search may contain proprietary source.
    let span = tracing::Span::current();
    span.record("query_len", query.len());
    span.record("max_results", max_results);

    let resolved = match scope_or_error(&state, params.repo.as_deref()) {
        Ok(resolved) => resolved,
        Err(unknown) => return unknown_repos_error(&unknown),
    };
    let (scope_kind, repo_count) = scope_fields(&resolved);
    span.record("repo_scope", scope_kind);
    span.record("repo_count", repo_count);
    let scope = match resolved {
        ResolvedScope::Scope(scope) => scope,
        ResolvedScope::NoRepositories => return empty_search_response(),
    };

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
        &scope,
        &knot::cli_tools::SearchContext {
            vector_db: &state.vector_db,
            graph_db: &state.graph_db,
            embedder,
        },
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
    path = "/api/callers",
    tag = "Search",
    params(GlobalCallersParams),
    responses(
        (status = 200, description = "Caller analysis results across the requested repositories", body = serde_json::Value),
        (status = 400, description = "Missing entity or unknown repository ids", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse),
    ),
    description = "Find all callers referencing an entity across one, several, or all registered \
                   repositories (`repo=all` / omitted `repo` expand to the registry id list, so \
                   unregistered repositories are never queried; an empty registry returns empty \
                   buckets with 200 without querying). Every row identifies the repository of the \
                   caller (`repo_name`) and of the referenced entity (`target_repo_name`); \
                   `resolution.targets[]` is labeled too. There is no `max_results`: the response \
                   is bounded by knot's 25-target resolution cap, surfaced as `resolution.truncated`.",
)]
#[tracing::instrument(
    name = "callers_all",
    skip_all,
    fields(
        entity = tracing::field::Empty,
        repo_scope = tracing::field::Empty,
        repo_count = tracing::field::Empty,
    )
)]
pub async fn callers_all_handler(
    State(state): State<Arc<AppState>>,
    Query(params): Query<GlobalCallersParams>,
) -> Response {
    let entity_name = match extract_required_param(params.entity.as_ref()) {
        Some(e) => e,
        None => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "Missing required parameter 'entity'",
            );
        }
    };
    // An entity name is a public identifier, not user prose, so it is
    // recorded (unlike the search query, whose text stays out of spans).
    tracing::Span::current().record("entity", entity_name);

    let resolved = match scope_or_error(&state, params.repo.as_deref()) {
        Ok(resolved) => resolved,
        Err(unknown) => return unknown_repos_error(&unknown),
    };
    let (scope_kind, repo_count) = scope_fields(&resolved);
    let span = tracing::Span::current();
    span.record("repo_scope", scope_kind);
    span.record("repo_count", repo_count);
    let scope = match resolved {
        ResolvedScope::Scope(scope) => scope,
        ResolvedScope::NoRepositories => return empty_callers_response(entity_name),
    };

    match knot::cli_tools::run_find_callers(entity_name, &scope, &state.graph_db).await {
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
#[tracing::instrument(
    name = "explore",
    skip_all,
    fields(repo_id = %id, path = tracing::field::Empty)
)]
pub async fn explore_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(params): Query<ExploreParams>,
) -> Response {
    let relative = match extract_required_param(params.path.as_ref()) {
        Some(p) => p,
        None => {
            return error_response(StatusCode::BAD_REQUEST, "Missing required parameter 'path'");
        }
    };
    tracing::Span::current().record("path", relative);

    // knot 1.5.1+ stores repo-relative file paths in Neo4j (see the knot
    // `relative_file_paths` spec). Pass the caller-supplied relative path
    // straight through — `run_explore_file` normalizes it (POSIX separators,
    // strips a leading "./"). We still look up the repo so unknown ids return
    // 404 instead of an empty result.
    let relative_path = {
        let mut registry = state.registry.lock().unwrap();
        if registry.get(&id).is_none() {
            return error_response(
                StatusCode::NOT_FOUND,
                format!("Repository '{}' not found", id),
            );
        }
        relative.trim_start_matches('/').to_string()
    };

    match knot::cli_tools::run_explore_file(
        &relative_path,
        &knot::models::RepoScope::One(id.clone()),
        &state.graph_db,
    )
    .await
    {
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
#[tracing::instrument(
    name = "deps",
    skip_all,
    fields(repo_id = %id, max_depth = tracing::field::Empty, reverse = tracing::field::Empty)
)]
pub async fn deps_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(params): Query<DepsParams>,
) -> Response {
    let max_depth = params.max_depth.unwrap_or(3);
    let reverse = params.reverse.unwrap_or(false);
    let span = tracing::Span::current();
    span.record("max_depth", max_depth);
    span.record("reverse", reverse);

    match knot::cli_tools::run_deps(&id, max_depth, reverse, &state.graph_db).await {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err(e) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Deps lookup failed: {e}"),
        ),
    }
}
