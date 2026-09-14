use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;
use std::sync::Arc;

use crate::handlers::models::*;
use crate::handlers::scope::{
    ResolvedScope, clamp_max_results, clamp_max_targets, scope_fields, scope_or_error,
    unknown_repos_error,
};
use crate::models::AppState;

fn extract_required_param(param: Option<&String>) -> Option<&str> {
    param.map(String::as_str).filter(|s| !s.trim().is_empty())
}

/// Read the explicit truncation contract out of a `find_callers` payload:
/// `(true_total, returned, truncated)`.
///
/// `true_total` is knot's pre-truncation target count — never the number of
/// entries actually returned — so a caller can quantify how partial the
/// relationship buckets are. `returned` is the number of `resolution.targets[]`
/// present in the payload. Returns `None` when the resolution block is absent.
fn callers_target_metadata(value: &serde_json::Value) -> Option<(u64, u64, bool)> {
    let resolution = value.get("resolution")?;
    let true_total = resolution.get("total_targets")?.as_u64()?;
    let returned = resolution.get("targets")?.as_array()?.len() as u64;
    let truncated = resolution
        .get("truncated")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    Some((true_total, returned, truncated))
}

/// Record the true/returned/truncated triple on the current span so a partial
/// impact set is visible in traces, not just in the JSON body. Malformed or
/// partial payloads are ignored (the response still passes through verbatim).
fn record_callers_truncation(value: &serde_json::Value) {
    if let Some((true_total, returned, truncated)) = callers_target_metadata(value) {
        let span = tracing::Span::current();
        span.record("total_targets", true_total);
        span.record("returned_targets", returned);
        span.record("truncated", truncated);
    }
}

/// Empty-registry body for `GET /api/search` (CROSS_REPO_SEARCH_PLAN §3):
/// status 200 with a bare JSON array — the caller asked for "all registered
/// repositories" and there are none.
fn empty_search_response() -> Response {
    (StatusCode::OK, Json(json!([]))).into_response()
}

/// Empty-registry body for `GET /api/callers` (CROSS_REPO_SEARCH_PLAN §3):
/// six empty buckets plus a neutral `resolution` block, shaped byte-for-byte
/// like knot's natural empty response (pinned by E2E scenario G6). The
/// `total_targets: 0` field mirrors knot's pre-truncation count so REST and
/// MCP agree on the total even for a trivially-empty result.
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
                "total_targets": 0,
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
    description = "Semantic + structural search. Find code by meaning, class name, method signature, or docstrings. \
                   `max_results` is enforced at 1..=100 (default 5): requests above 100 are clamped to 100 — there is \
                   no pagination or cursor, so to look past the bound narrow the search with `kinds` / `path` or refine \
                   the query. The optional `path` filter accepts a repo-relative directory prefix or a glob \
                   (e.g. `src/api` or `src/**/*_test.rs`).",
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

    // Enforced, not advisory: knot clamps again internally, but clamping here
    // keeps the REST contract (and the tracing span) honest about what was
    // actually served — 1..=100, default 5, no pagination.
    let max_results = clamp_max_results(params.max_results);

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
        knot::cli_tools::SearchFilters {
            kinds: params
                .kinds
                .as_deref()
                .map(str::trim)
                .filter(|k| !k.is_empty()),
            path: params
                .path
                .as_deref()
                .map(str::trim)
                .filter(|p| !p.is_empty()),
        },
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
    description = "Find all callers referencing a specific entity. Returns the reverse dependency graph. \
                   `resolution.total_targets` is the true pre-truncation target count and \
                   `resolution.truncated` says whether `resolution.targets[]` (and therefore the \
                   per-bucket counts) is a sample; raise `max_targets` (default 25, max 500) for the \
                   full set.",
)]
#[tracing::instrument(
    name = "callers",
    skip_all,
    fields(
        repo_id = %id,
        entity = tracing::field::Empty,
        max_targets = tracing::field::Empty,
        total_targets = tracing::field::Empty,
        returned_targets = tracing::field::Empty,
        truncated = tracing::field::Empty,
    )
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
    let max_targets = clamp_max_targets(params.max_targets);
    let span = tracing::Span::current();
    span.record("entity", entity_name);
    span.record("max_targets", max_targets);

    match knot::cli_tools::run_find_callers(
        entity_name,
        &knot::models::RepoScope::One(id.clone()),
        &state.graph_db,
        Some(max_targets),
        None,
    )
    .await
    {
        Ok(value) => {
            record_callers_truncation(&value);
            (StatusCode::OK, Json(value)).into_response()
        }
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
                    `repo_name`. `max_results` is a global cap across the scope, enforced at 1..=100 \
                    (default 5): requests above 100 are clamped to 100 — there is no pagination or \
                    cursor, so to look past the bound narrow the scope with `repo` / `kinds` / `path` \
                    or refine the query. The optional `path` filter accepts a repo-relative directory \
                    prefix or a glob (e.g. `src/api` or `src/**/*_test.rs`), applied within every \
                    repository of the scope.",
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
        knot::cli_tools::SearchFilters {
            kinds: params
                .kinds
                .as_deref()
                .map(str::trim)
                .filter(|k| !k.is_empty()),
            path: params
                .path
                .as_deref()
                .map(str::trim)
                .filter(|p| !p.is_empty()),
        },
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
                   `resolution.targets[]` is labeled too. `resolution.total_targets` is the true \
                   pre-truncation target count and `resolution.truncated` says whether the bucket \
                   counts are a sample; raise `max_targets` (default 25, max 500) for the full set. \
                   There is no `max_results` here.",
)]
#[tracing::instrument(
    name = "callers_all",
    skip_all,
    fields(
        entity = tracing::field::Empty,
        repo_scope = tracing::field::Empty,
        repo_count = tracing::field::Empty,
        max_targets = tracing::field::Empty,
        total_targets = tracing::field::Empty,
        returned_targets = tracing::field::Empty,
        truncated = tracing::field::Empty,
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
    let max_targets = clamp_max_targets(params.max_targets);
    span.record("max_targets", max_targets);

    match knot::cli_tools::run_find_callers(
        entity_name,
        &scope,
        &state.graph_db,
        Some(max_targets),
        None,
    )
    .await
    {
        Ok(value) => {
            record_callers_truncation(&value);
            (StatusCode::OK, Json(value)).into_response()
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A knot `find_callers` payload whose target resolution was truncated:
    /// 1 target shown out of 112, with 2 caller rows. The bucket counts are a
    /// sample; `total_targets` must stay 112 (the true total), never 1 or 2.
    fn truncated_payload() -> serde_json::Value {
        json!({
            "calls": [
                {"name": "a", "kind": "function", "file_path": "a.rs", "start_line": 1},
                {"name": "b", "kind": "function", "file_path": "b.rs", "start_line": 2}
            ],
            "extends": [],
            "implements": [],
            "references": [],
            "resolution": {
                "query": "delete",
                "tier": "exact_name",
                "fuzzy": false,
                "truncated": true,
                "total_targets": 112,
                "targets": [{"fqn": "repo::delete"}]
            }
        })
    }

    #[test]
    fn metadata_reports_true_total_not_returned_entries() {
        let (true_total, returned, truncated) =
            callers_target_metadata(&truncated_payload()).expect("resolution present");
        assert_eq!(
            true_total, 112,
            "true total must be the pre-truncation count"
        );
        assert_eq!(
            returned, 1,
            "returned must count resolution.targets[], not bucket rows"
        );
        assert!(truncated);
        // The bucket row count (2) is irrelevant to the resolution metadata.
        assert_ne!(true_total, 2);
    }

    #[test]
    fn metadata_marks_complete_resolution_as_not_truncated() {
        let payload = json!({
            "calls": [],
            "resolution": {
                "tier": "exact_name",
                "truncated": false,
                "total_targets": 3,
                "targets": [{}, {}, {}]
            }
        });
        let (true_total, returned, truncated) =
            callers_target_metadata(&payload).expect("resolution present");
        assert_eq!(true_total, 3);
        assert_eq!(returned, 3);
        assert!(!truncated);
    }

    #[test]
    fn metadata_is_none_without_a_resolution_block() {
        assert!(callers_target_metadata(&json!({"calls": []})).is_none());
    }

    #[test]
    fn metadata_is_none_when_total_targets_is_absent() {
        // Pre-fix knot payloads omitted `total_targets`; the helper must not
        // silently substitute the returned count for the true total.
        let legacy = json!({
            "resolution": {"tier": "exact_name", "truncated": true, "targets": [{}]}
        });
        assert!(callers_target_metadata(&legacy).is_none());
    }

    #[tokio::test]
    async fn empty_callers_response_carries_zero_total_and_no_truncation() {
        let response = empty_callers_response("Ghost");
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["resolution"]["total_targets"], 0);
        assert_eq!(value["resolution"]["truncated"], false);
        assert_eq!(value["resolution"]["query"], "Ghost");
        for bucket in [
            "calls",
            "extends",
            "implements",
            "overridden_by",
            "overrides",
            "references",
        ] {
            assert_eq!(value[bucket], json!([]), "bucket {bucket} must be empty");
        }
    }
}
