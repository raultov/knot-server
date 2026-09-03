//! Repository scope resolution for the cross-repo routes
//! (`GET /api/search`, `GET /api/callers`).
//!
//! The pure core (`resolve_scope`, `clamp_max_results`) is kept free of
//! axum, DB and registry types so it is unit-testable in isolation
//! (CROSS_REPO_SEARCH_PLAN §4.2). Parsing of the `repo` parameter is
//! delegated verbatim to `knot::models::RepoScope::parse` — knot-server
//! must not reimplement trimming, deduping or sentinel precedence.
//!
//! `RepoScope::All` no longer bypasses the registry: it expands to the
//! registry id list, so `repo=all` — and an omitted `repo` — mean "all
//! *registered* repositories" (CROSS_REPO_SEARCH_PLAN D1/D2).

use axum::http::StatusCode;
use axum::response::Response;
use knot::models::RepoScope;

use crate::handlers::models::error_response;
use crate::models::AppState;

/// Default and clamp bounds for `max_results` on the cross-repo search route.
pub(crate) const DEFAULT_MAX_RESULTS: usize = 5;
pub(crate) const MIN_MAX_RESULTS: usize = 1;
pub(crate) const MAX_MAX_RESULTS: usize = 100;

/// The outcome of resolving the `repo` parameter against the registry.
///
/// `NoRepositories` exists because `RepoScope::Many(vec![])` is **not** a
/// representable "nothing": `filter_names()` returns an empty vec for it, and
/// knot's DB layer treats an empty filter list as *unfiltered*
/// (`knot::models::RepoScope::filter_names`). Expanding `All` over an empty
/// registry therefore cannot be expressed as a `RepoScope` at all — the caller
/// must skip the query entirely.
#[derive(Debug, PartialEq, Eq)]
pub enum ResolvedScope {
    Scope(RepoScope),
    NoRepositories,
}

/// Resolve the `repo` query parameter against the set of known repository ids.
///
/// `RepoScope::All` — also produced by an omitted, empty or sentinel `repo`
/// value — expands to the registry id list (sorted and deduped): an empty
/// registry yields [`ResolvedScope::NoRepositories`], a single id
/// `Scope(One)`, two or more `Scope(Many)`. Every other scope is
/// membership-checked against `known`; unknown names are returned **sorted
/// and deduped** in `Err`.
pub fn resolve_scope(raw: Option<&str>, known: &[String]) -> Result<ResolvedScope, Vec<String>> {
    let scope = RepoScope::parse_optional(raw);
    if matches!(scope, RepoScope::All) {
        let mut ids: Vec<String> = known.to_vec();
        ids.sort();
        ids.dedup();
        return Ok(match ids.len() {
            0 => ResolvedScope::NoRepositories,
            1 => ResolvedScope::Scope(RepoScope::One(ids.remove(0))),
            _ => ResolvedScope::Scope(RepoScope::Many(ids)),
        });
    }

    let mut unknown: Vec<String> = scope
        .filter_names()
        .into_iter()
        .filter(|name| !known.contains(name))
        .collect();
    unknown.sort();
    unknown.dedup();
    if unknown.is_empty() {
        Ok(ResolvedScope::Scope(scope))
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
) -> Result<ResolvedScope, Vec<String>> {
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
/// (`"all" | "one" | "many" | "none"`) and the number of named repositories
/// (`0` for `All` and for `NoRepositories`). Names are deliberately not
/// recorded — the kind + count keeps the span payload bounded for `all` over
/// a large cluster.
pub(crate) fn scope_fields(scope: &ResolvedScope) -> (&'static str, usize) {
    match scope {
        ResolvedScope::NoRepositories => ("none", 0),
        ResolvedScope::Scope(RepoScope::All) => ("all", 0),
        ResolvedScope::Scope(RepoScope::One(_)) => ("one", 1),
        ResolvedScope::Scope(RepoScope::Many(names)) => ("many", names.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known() -> Vec<String> {
        vec!["repo-a".to_string(), "repo-b".to_string()]
    }

    // D2: `All` expands to the registry id list, sorted.
    #[test]
    fn all_expands_to_registered_repos() {
        let unsorted = vec!["repo-b".to_string(), "repo-a".to_string()];
        assert_eq!(
            resolve_scope(None, &unsorted),
            Ok(ResolvedScope::Scope(RepoScope::Many(vec![
                "repo-a".to_string(),
                "repo-b".to_string()
            ])))
        );
        // Empty and whitespace-only values parse to `All` too.
        assert_eq!(
            resolve_scope(Some(""), &known()),
            resolve_scope(None, &known())
        );
        assert_eq!(
            resolve_scope(Some("  "), &known()),
            resolve_scope(None, &known())
        );
    }

    #[test]
    fn sentinel_expands_to_registered_repos() {
        for raw in ["all", "ALL", "All", "*"] {
            assert_eq!(
                resolve_scope(Some(raw), &known()),
                Ok(ResolvedScope::Scope(RepoScope::Many(vec![
                    "repo-a".to_string(),
                    "repo-b".to_string()
                ]))),
                "sentinel '{raw}' must expand to the registry"
            );
        }
    }

    #[test]
    fn all_with_single_registered_repo_is_one() {
        let single = vec!["repo-a".to_string()];
        assert_eq!(
            resolve_scope(None, &single),
            Ok(ResolvedScope::Scope(RepoScope::One("repo-a".to_string())))
        );
    }

    // D3: an empty registry must never become `Many([])`, which knot's DB
    // layer reads as *unfiltered* — the exact inverse of the intent.
    #[test]
    fn all_with_empty_registry_is_no_repositories() {
        assert_eq!(resolve_scope(None, &[]), Ok(ResolvedScope::NoRepositories));
    }

    #[test]
    fn sentinel_with_empty_registry_is_no_repositories() {
        for raw in ["all", "ALL", "*"] {
            assert_eq!(
                resolve_scope(Some(raw), &[]),
                Ok(ResolvedScope::NoRepositories),
                "sentinel '{raw}' over an empty registry"
            );
        }
    }

    #[test]
    fn expansion_never_yields_an_empty_filter_list() {
        // For every expansion outcome the resulting scope (when present)
        // carries a non-empty filter list — the invariant that makes the
        // `NoRepositories` variant necessary (D3).
        for known in [
            vec![],
            vec!["repo-a".to_string()],
            vec!["repo-b".to_string(), "repo-a".to_string()],
        ] {
            match resolve_scope(None, &known).unwrap() {
                ResolvedScope::NoRepositories => assert!(known.is_empty()),
                ResolvedScope::Scope(scope) => {
                    assert!(!scope.filter_names().is_empty());
                }
            }
        }
    }

    // The sentinel wins over a named list in knot's parse rules
    // ("all,ghost" parses to `All`), so the input now expands to the
    // registry instead of skipping the membership check: `ghost` is not
    // reachable through it.
    #[test]
    fn sentinel_no_longer_skips_membership_check() {
        let resolved = resolve_scope(Some("all,ghost"), &known()).unwrap();
        let ResolvedScope::Scope(scope) = resolved else {
            panic!("a non-empty registry must resolve to a scope");
        };
        let names = scope.filter_names();
        assert_eq!(names, vec!["repo-a".to_string(), "repo-b".to_string()]);
        assert!(!names.contains(&"ghost".to_string()));
    }

    #[test]
    fn named_scopes_are_unchanged() {
        assert_eq!(
            resolve_scope(Some("repo-a"), &known()),
            Ok(ResolvedScope::Scope(RepoScope::One("repo-a".to_string())))
        );
        assert_eq!(
            resolve_scope(Some("repo-a,repo-b"), &known()),
            Ok(ResolvedScope::Scope(RepoScope::Many(vec![
                "repo-a".to_string(),
                "repo-b".to_string()
            ])))
        );
        // Trimming/deduping is knot's authority; order is first-occurrence.
        assert_eq!(
            resolve_scope(Some(" repo-a , repo-a , repo-b "), &known()),
            Ok(ResolvedScope::Scope(RepoScope::Many(vec![
                "repo-a".to_string(),
                "repo-b".to_string()
            ])))
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
    fn empty_registry_still_rejects_named_unknowns() {
        assert_eq!(
            resolve_scope(Some("ghost"), &[]),
            Err(vec!["ghost".to_string()])
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
