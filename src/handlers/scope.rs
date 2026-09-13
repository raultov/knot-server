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

/// Default and clamp bounds for `max_results` on the search routes. Both
/// mirror knot's own advertised contract ([`knot::cli_tools::DEFAULT_MAX_RESULTS`]
/// and [`knot::cli_tools::MAX_RESULTS_CEILING`]) so REST can never widen (or
/// lag behind) the bound the MCP tool advertises, and cannot drift if knot
/// retunes them. There is no pagination on either surface: past the ceiling,
/// callers narrow the search with `kinds` / `path` / `repo` or refine the
/// query instead of raising the limit.
pub(crate) const DEFAULT_MAX_RESULTS: usize = knot::cli_tools::DEFAULT_MAX_RESULTS;
pub(crate) const MIN_MAX_RESULTS: usize = 1;
pub(crate) const MAX_MAX_RESULTS: usize = knot::cli_tools::MAX_RESULTS_CEILING;

/// Clamp bounds for `max_targets` on the callers routes. Both numeric bounds
/// mirror knot's own target-resolution cap ([`knot::db::graph::DEFAULT_MAX_TARGETS`])
/// and hard ceiling ([`knot::db::graph::MAX_TARGETS_CEILING`]) so the REST
/// surface can never widen the contract knot enforces internally, and cannot
/// drift if knot retunes them.
pub(crate) const DEFAULT_MAX_TARGETS: usize = knot::db::graph::DEFAULT_MAX_TARGETS;
pub(crate) const MIN_MAX_TARGETS: usize = 1;
pub(crate) const MAX_MAX_TARGETS: usize = knot::db::graph::MAX_TARGETS_CEILING;

/// Single clamping implementation behind every capped query parameter, so the
/// search (`max_results`) and callers (`max_targets`) routes cannot drift
/// apart.
fn clamp_in_range(requested: Option<usize>, default: usize, min: usize, max: usize) -> usize {
    requested.unwrap_or(default).clamp(min, max)
}

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

/// Clamp a caller-supplied `max_results` into knot's accepted range
/// (`[1, 100]`, default 5). The routes are unauthenticated and unfiltered by
/// default, so an unbounded cap over the whole corpus would be a cheap way
/// to exhaust the server (CROSS_REPO_SEARCH_PLAN D4). The bounds are knot's
/// ([`knot::cli_tools::resolve_max_results`]), so REST and `/mcp` enforce the
/// same default and ceiling by construction.
pub fn clamp_max_results(requested: Option<usize>) -> usize {
    clamp_in_range(
        requested,
        DEFAULT_MAX_RESULTS,
        MIN_MAX_RESULTS,
        MAX_MAX_RESULTS,
    )
}

/// Clamp a caller-supplied `max_targets` into knot's accepted range
/// (`[1, 500]`, default 25).
///
/// `max_targets` is the opt-in path to the full impact set when a callers
/// response reports `resolution.truncated: true`: the response always carries
/// the true pre-truncation count in `resolution.total_targets`, and raising
/// `max_targets` (up to the ceiling) widens `resolution.targets[]`. Because
/// the bounds are knot's, the server can never ask knot for more than knot's
/// own ceiling allows.
pub fn clamp_max_targets(requested: Option<usize>) -> usize {
    clamp_in_range(
        requested,
        DEFAULT_MAX_TARGETS,
        MIN_MAX_TARGETS,
        MAX_MAX_TARGETS,
    )
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

    // ---- max_results (search) --------------------------------------------

    #[test]
    fn result_bounds_mirror_knot() {
        // The server must never widen (or lag behind) the bound knot's MCP
        // tool advertises: same default, same ceiling.
        assert_eq!(DEFAULT_MAX_RESULTS, knot::cli_tools::DEFAULT_MAX_RESULTS);
        assert_eq!(MAX_MAX_RESULTS, knot::cli_tools::MAX_RESULTS_CEILING);
    }

    #[test]
    fn rest_and_mcp_agree_on_the_clamp() {
        // For every request knot's `resolve_max_results` and the REST clamp
        // must land on the same enforced value, so a client sees identical
        // behavior on `/mcp` and on both search routes.
        for requested in [0usize, 1, 5, 20, 99, 100, 1000, usize::MAX] {
            let rest = clamp_max_results(Some(requested));
            let mcp = knot::cli_tools::resolve_max_results(requested).value;
            assert_eq!(rest, mcp, "REST and /mcp disagree for {requested}");
        }
    }

    #[test]
    fn clamp_acceptance_matrix() {
        // The exact contract from the search-limit spec: floor, default,
        // in-range pass-through, ceiling.
        assert_eq!(clamp_max_results(Some(0)), 1);
        assert_eq!(clamp_max_results(Some(5)), 5);
        assert_eq!(clamp_max_results(Some(1000)), 100);
        assert_eq!(clamp_max_results(None), DEFAULT_MAX_RESULTS);
    }

    #[test]
    fn knot_clamp_notice_mentions_ceiling_and_no_pagination() {
        // REST relies on knot's notice wording for the clamped-MCP case; the
        // reply must state the ceiling and the no-pagination rule so callers
        // learn to narrow instead of raising the limit.
        let notice = knot::cli_tools::resolve_max_results(1000)
            .notice()
            .expect("a clamped request must carry a notice");
        assert!(
            notice.contains("100"),
            "notice must state the ceiling: {notice}"
        );
        assert!(
            notice.to_lowercase().contains("no pagination"),
            "notice must state the no-pagination rule: {notice}"
        );
        assert!(
            knot::cli_tools::resolve_max_results(100).notice().is_none(),
            "an in-range request must not carry a notice"
        );
    }

    // ---- max_targets (callers) -------------------------------------------

    #[test]
    fn target_bounds_mirror_knot() {
        // The server must never widen knot's own resolution contract.
        assert_eq!(DEFAULT_MAX_TARGETS, knot::db::graph::DEFAULT_MAX_TARGETS);
        assert_eq!(MAX_MAX_TARGETS, knot::db::graph::MAX_TARGETS_CEILING);
    }

    #[test]
    fn clamp_max_targets_defaults_to_knot_default() {
        assert_eq!(clamp_max_targets(None), DEFAULT_MAX_TARGETS);
        assert_eq!(
            clamp_max_targets(Some(DEFAULT_MAX_TARGETS)),
            DEFAULT_MAX_TARGETS
        );
    }

    #[test]
    fn clamp_max_targets_floor_is_one() {
        // `0` would mean "no targets at all"; it must not disable the query.
        assert_eq!(clamp_max_targets(Some(0)), MIN_MAX_TARGETS);
        assert_eq!(clamp_max_targets(Some(1)), 1);
    }

    #[test]
    fn clamp_max_targets_ceiling_is_knot_ceiling() {
        assert_eq!(clamp_max_targets(Some(usize::MAX)), MAX_MAX_TARGETS);
        assert_eq!(clamp_max_targets(Some(MAX_MAX_TARGETS)), MAX_MAX_TARGETS);
        assert_eq!(
            clamp_max_targets(Some(MAX_MAX_TARGETS + 1)),
            MAX_MAX_TARGETS
        );
    }

    #[test]
    fn clamp_max_targets_honours_in_range_values() {
        for requested in [2usize, 25, 100, 499] {
            assert_eq!(clamp_max_targets(Some(requested)), requested);
        }
    }
}
