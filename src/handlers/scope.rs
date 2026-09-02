//! Repository scope resolution for the cross-repo routes
//! (`GET /api/search`, `GET /api/callers`).
//!
//! The pure core (`resolve_scope`, `clamp_max_results`) is kept free of
//! axum, DB and registry types so it is unit-testable in isolation
//! (CROSS_REPO_SEARCH_PLAN §4.2). Parsing of the `repo` parameter is
//! delegated verbatim to `knot::models::RepoScope::parse` — knot-server
//! must not reimplement trimming, deduping or sentinel precedence.

use axum::http::StatusCode;
use axum::response::Response;
use knot::models::RepoScope;

use crate::handlers::models::error_response;
use crate::models::AppState;

/// Default and clamp bounds for `max_results` on the cross-repo search route.
pub(crate) const DEFAULT_MAX_RESULTS: usize = 5;
pub(crate) const MIN_MAX_RESULTS: usize = 1;
pub(crate) const MAX_MAX_RESULTS: usize = 100;

/// Resolve the `repo` query parameter against the set of known repository ids.
///
/// `Ok(scope)` when every named repository exists (or the scope is `All`);
/// `Err(unknown)` with the unknown names **sorted and deduped** otherwise.
/// `RepoScope::All` short-circuits to `Ok` — the membership check only
/// applies to explicitly named repositories.
pub fn resolve_scope(raw: Option<&str>, known: &[String]) -> Result<RepoScope, Vec<String>> {
    let scope = RepoScope::parse_optional(raw);
    if matches!(scope, RepoScope::All) {
        return Ok(scope);
    }

    let mut unknown: Vec<String> = scope
        .filter_names()
        .into_iter()
        .filter(|name| !known.contains(name))
        .collect();
    unknown.sort();
    unknown.dedup();
    if unknown.is_empty() {
        Ok(scope)
    } else {
        Err(unknown)
    }
}

/// Clamp a caller-supplied `max_results` into the accepted range
/// (`[1, 100]`, default 5). The route is unauthenticated and unfiltered by
/// default, so an unbounded cap over the whole corpus would be a cheap way
/// to exhaust the server (CROSS_REPO_SEARCH_PLAN D4).
pub fn clamp_max_results(requested: Option<usize>) -> usize {
    requested
        .unwrap_or(DEFAULT_MAX_RESULTS)
        .clamp(MIN_MAX_RESULTS, MAX_MAX_RESULTS)
}

/// Snapshot the registry ids and resolve the scope. On failure, returns the
/// unknown repository names (sorted and deduped) for the caller to render
/// via [`unknown_repos_error`]. Shared verbatim by both cross-repo handlers
/// so their error shape is byte-identical (CROSS_REPO_SEARCH_PLAN §4.4).
///
/// The registry snapshot happens inside a block that ends before the first
/// `.await` in the caller — a `std::sync::MutexGuard` is not `Send` and
/// would not compile across an await point.
pub(crate) fn scope_or_error(
    state: &AppState,
    raw: Option<&str>,
) -> Result<RepoScope, Vec<String>> {
    let known: Vec<String> = {
        let mut registry = state.registry.lock().unwrap();
        registry.list().iter().map(|r| r.id.clone()).collect()
    };
    resolve_scope(raw, &known)
}

/// Render the 400 response for unknown repository names:
/// `{"error":"Unknown repository ids: a, b"}` (names sorted, comma-separated).
pub(crate) fn unknown_repos_error(unknown: &[String]) -> Response {
    error_response(
        StatusCode::BAD_REQUEST,
        format!("Unknown repository ids: {}", unknown.join(", ")),
    )
}

/// Span-friendly description of a resolved scope: its kind
/// (`"all" | "one" | "many"`) and the number of named repositories
/// (`0` for `All`). Names are deliberately not recorded — the kind +
/// count keeps the span payload bounded for `all` over a large cluster.
pub(crate) fn scope_fields(scope: &RepoScope) -> (&'static str, usize) {
    match scope {
        RepoScope::All => ("all", 0),
        RepoScope::One(_) => ("one", 1),
        RepoScope::Many(names) => ("many", names.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known() -> Vec<String> {
        vec!["repo-a".to_string(), "repo-b".to_string()]
    }

    #[test]
    fn absent_repo_param_is_all() {
        assert_eq!(resolve_scope(None, &known()), Ok(RepoScope::All));
    }

    #[test]
    fn empty_repo_param_is_all() {
        assert_eq!(resolve_scope(Some(""), &known()), Ok(RepoScope::All));
        assert_eq!(resolve_scope(Some("  "), &known()), Ok(RepoScope::All));
    }

    #[test]
    fn sentinel_all_is_case_insensitive() {
        for raw in ["all", "ALL", "All", "*"] {
            assert_eq!(
                resolve_scope(Some(raw), &known()),
                Ok(RepoScope::All),
                "sentinel '{raw}' must resolve to All"
            );
        }
    }

    #[test]
    fn single_known_repo_is_one() {
        assert_eq!(
            resolve_scope(Some("repo-a"), &known()),
            Ok(RepoScope::One("repo-a".to_string()))
        );
    }

    #[test]
    fn list_of_known_repos_is_many() {
        assert_eq!(
            resolve_scope(Some("repo-a,repo-b"), &known()),
            Ok(RepoScope::Many(vec![
                "repo-a".to_string(),
                "repo-b".to_string()
            ]))
        );
    }

    #[test]
    fn whitespace_and_duplicates_are_normalised() {
        // Trimming/deduping is knot's authority; order is first-occurrence.
        assert_eq!(
            resolve_scope(Some(" repo-a , repo-a , repo-b "), &known()),
            Ok(RepoScope::Many(vec![
                "repo-a".to_string(),
                "repo-b".to_string()
            ]))
        );
    }

    #[test]
    fn unknown_single_name_is_rejected() {
        assert_eq!(
            resolve_scope(Some("ghost"), &known()),
            Err(vec!["ghost".to_string()])
        );
    }

    #[test]
    fn unknown_names_are_sorted_and_deduped() {
        assert_eq!(
            resolve_scope(Some("z,ghost,ghost"), &known()),
            Err(vec!["ghost".to_string(), "z".to_string()])
        );
    }

    #[test]
    fn partially_unknown_list_is_rejected_whole() {
        assert_eq!(
            resolve_scope(Some("repo-a,ghost"), &known()),
            Err(vec!["ghost".to_string()])
        );
    }

    #[test]
    fn sentinel_skips_membership_check() {
        assert_eq!(resolve_scope(Some("all"), &[]), Ok(RepoScope::All));
        assert_eq!(resolve_scope(Some("*"), &[]), Ok(RepoScope::All));
    }

    #[test]
    fn sentinel_wins_over_unknown_name_in_list() {
        assert_eq!(
            resolve_scope(Some("all,ghost"), &known()),
            Ok(RepoScope::All)
        );
    }

    #[test]
    fn repo_names_are_case_sensitive() {
        assert_eq!(
            resolve_scope(Some("REPO-A"), &known()),
            Err(vec!["REPO-A".to_string()])
        );
    }

    #[test]
    fn clamp_defaults_to_five() {
        assert_eq!(clamp_max_results(None), 5);
        assert_eq!(clamp_max_results(Some(5)), 5);
    }

    #[test]
    fn clamp_floor_is_one() {
        assert_eq!(clamp_max_results(Some(0)), 1);
    }

    #[test]
    fn clamp_ceiling_is_hundred() {
        assert_eq!(clamp_max_results(Some(99999)), 100);
        assert_eq!(clamp_max_results(Some(100)), 100);
    }
}
