//! DTOs and pure projection helpers for the `/api/repos/{id}/deps` REST endpoint.

use serde::Serialize;
use utoipa::ToSchema;

/// Dependency lookup response object for `/api/repos/{id}/deps`.
#[derive(Debug, Serialize, ToSchema)]
pub struct DepsResponse {
    /// Array of dependency objects, e.g. `[{"repo_name": "..."}]`.
    pub dependencies: Vec<serde_json::Value>,
    /// Diagnostic details explaining why an empty result set occurred, or `null` when dependencies were found.
    pub diagnostics: Option<DepsDiagnosticsDto>,
    /// Information about traversal depth requested and effective clamping.
    pub depth: DepthReport,
}

/// Report on requested vs effective traversal depth.
#[derive(Debug, Serialize, ToSchema)]
pub struct DepthReport {
    /// Requested `max_depth` parameter (defaults to 3 if omitted).
    pub requested: u32,
    /// Effective `max_depth` enforced by knot core.
    pub effective: u32,
    /// Whether requested depth was clamped or floored.
    pub clamped: bool,
    /// Maximum ceiling allowed for `max_depth`.
    pub ceiling: u32,
}

/// Diagnostic details for an empty dependency lookup.
#[derive(Debug, Serialize, ToSchema)]
pub struct DepsDiagnosticsDto {
    /// Direction of the lookup (`"forward"` or `"reverse"`).
    pub direction: &'static str,
    /// Whether the target repository is indexed in knot.
    pub repo_indexed: bool,
    /// Whether the lookup question is answerable (false when repo has no build identity in reverse).
    pub answerable: bool,
    /// Discriminator key describing the cause of the empty result.
    pub reason: &'static str,
    /// Human-readable suggestion for resolving the empty result.
    pub remedy: String,
    /// Repository identity details if indexed.
    pub identity: Option<RepoIdentityDto>,
    /// Forward lookup: declared build dependencies with resolution status.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub declared_dependencies: Option<Vec<DeclaredDependencyDto>>,
    /// Forward lookup: count of declared dependencies.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub declared_count: Option<usize>,
    /// Forward lookup: count of declared dependencies that resolve to an indexed repo.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_count: Option<usize>,
    /// Reverse lookup: indexed repositories declaring this repo without an edge yet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub declaring_consumers: Option<Vec<DeclaringConsumerDto>>,
}

/// Repository build identity stored on `:Repository` node.
#[derive(Debug, Serialize, ToSchema)]
pub struct RepoIdentityDto {
    pub build_system: String,
    pub group_id: String,
    pub artifact_id: String,
    pub version: String,
}

/// One declared build dependency and its resolution status.
#[derive(Debug, Serialize, ToSchema)]
pub struct DeclaredDependencyDto {
    /// Verbatim declared dependency name.
    pub name: String,
    /// Resolved repository name if indexed, or `null` if unindexed.
    pub resolved_repo: Option<String>,
}

/// An indexed consumer repository declaring the target repo.
#[derive(Debug, Serialize, ToSchema)]
pub struct DeclaringConsumerDto {
    pub repo_name: String,
    pub declared_as: String,
}

/// Build depth report using knot core constants and resolver.
pub fn build_depth_report(requested: u32) -> DepthReport {
    let effective = knot::cli_tools::resolve_max_depth(requested);
    DepthReport {
        requested,
        effective,
        clamped: requested != effective,
        ceiling: knot::cli_tools::MAX_DEPTH_CEILING,
    }
}

/// Convert knot's `DepsDiagnostics` into a REST `DepsDiagnosticsDto`.
pub fn project_diagnostics(
    diag: &knot::cli_tools::DepsDiagnostics,
    reverse: bool,
) -> DepsDiagnosticsDto {
    let repo_indexed = diag.identity.is_some();
    let identity_dto = diag.identity.as_ref().map(|id| RepoIdentityDto {
        build_system: id.build_system.clone(),
        group_id: id.group_id.clone(),
        artifact_id: id.artifact_id.clone(),
        version: id.version.clone(),
    });

    if reverse {
        project_reverse_diagnostics(diag, repo_indexed, identity_dto)
    } else {
        project_forward_diagnostics(diag, repo_indexed, identity_dto)
    }
}

fn project_reverse_diagnostics(
    diag: &knot::cli_tools::DepsDiagnostics,
    repo_indexed: bool,
    identity_dto: Option<RepoIdentityDto>,
) -> DepsDiagnosticsDto {
    match &diag.direction {
        knot::cli_tools::DepsDirection::Reverse(Some(consumers)) => {
            let remedy = if consumers.is_empty() {
                "No action needed: no indexed repository declares this repository as a build dependency.".to_string()
            } else {
                "The graph is stale: re-index either side to create the edge(s) (`knot-indexer --repo-path <path>`).".to_string()
            };
            let reason = if consumers.is_empty() {
                "no_declaring_consumers"
            } else {
                "declared_without_edge"
            };
            let consumer_dtos = consumers
                .iter()
                .map(|c| DeclaringConsumerDto {
                    repo_name: c.repo_name.clone(),
                    declared_as: c.declared_as.clone(),
                })
                .collect();

            DepsDiagnosticsDto {
                direction: "reverse",
                repo_indexed,
                answerable: true,
                reason,
                remedy,
                identity: identity_dto,
                declared_dependencies: None,
                declared_count: None,
                resolved_count: None,
                declaring_consumers: Some(consumer_dtos),
            }
        }
        knot::cli_tools::DepsDirection::Reverse(None) => {
            let reason = if repo_indexed {
                "no_matchable_build_identity"
            } else {
                "repository_not_indexed"
            };
            let remedy = if repo_indexed {
                "Re-index the repository with a build manifest (pom.xml, build.gradle, Cargo.toml, package.json, or .csproj) so its identity can be matched.".to_string()
            } else {
                "Run `knot-indexer --repo-path <path>` first, then retry.".to_string()
            };

            DepsDiagnosticsDto {
                direction: "reverse",
                repo_indexed,
                answerable: false,
                reason,
                remedy,
                identity: identity_dto,
                declared_dependencies: None,
                declared_count: None,
                resolved_count: None,
                declaring_consumers: None,
            }
        }
        knot::cli_tools::DepsDirection::Forward(_) => DepsDiagnosticsDto {
            direction: "reverse",
            repo_indexed,
            answerable: false,
            reason: "repository_not_indexed",
            remedy: "Run `knot-indexer --repo-path <path>` first, then retry.".to_string(),
            identity: identity_dto,
            declared_dependencies: None,
            declared_count: None,
            resolved_count: None,
            declaring_consumers: None,
        },
    }
}

fn project_forward_diagnostics(
    diag: &knot::cli_tools::DepsDiagnostics,
    repo_indexed: bool,
    identity_dto: Option<RepoIdentityDto>,
) -> DepsDiagnosticsDto {
    match &diag.direction {
        knot::cli_tools::DepsDirection::Forward(declared) => {
            if !repo_indexed {
                return DepsDiagnosticsDto {
                    direction: "forward",
                    repo_indexed: false,
                    answerable: false,
                    reason: "repository_not_indexed",
                    remedy: "Run `knot-indexer --repo-path <path>` first, then retry.".to_string(),
                    identity: None,
                    declared_dependencies: None,
                    declared_count: None,
                    resolved_count: None,
                    declaring_consumers: None,
                };
            }

            let declared_count = declared.len();
            let resolved_count = declared
                .iter()
                .filter(|d| d.resolved_repo.is_some())
                .count();

            let reason = if declared.is_empty() {
                "no_declared_dependencies"
            } else if resolved_count == 0 {
                "declared_but_unresolved"
            } else {
                "resolved_but_no_edge"
            };

            let remedy = if declared.is_empty() {
                "No action needed: repository declares no build dependencies.".to_string()
            } else if resolved_count == 0 {
                "Index the dependency's own repository with `knot-indexer --repo-path <path>` — the DEPENDS_ON edge is created by that run.".to_string()
            } else {
                "The graph is stale: re-index either side to create the edge(s) (`knot-indexer --repo-path <path>`).".to_string()
            };

            let dep_dtos = declared
                .iter()
                .map(|d| DeclaredDependencyDto {
                    name: d.name.clone(),
                    resolved_repo: d.resolved_repo.clone(),
                })
                .collect();

            DepsDiagnosticsDto {
                direction: "forward",
                repo_indexed: true,
                answerable: true,
                reason,
                remedy,
                identity: identity_dto,
                declared_dependencies: Some(dep_dtos),
                declared_count: Some(declared_count),
                resolved_count: Some(resolved_count),
                declaring_consumers: None,
            }
        }
        knot::cli_tools::DepsDirection::Reverse(_) => DepsDiagnosticsDto {
            direction: "forward",
            repo_indexed,
            answerable: false,
            reason: "repository_not_indexed",
            remedy: "Run `knot-indexer --repo-path <path>` first, then retry.".to_string(),
            identity: identity_dto,
            declared_dependencies: None,
            declared_count: None,
            resolved_count: None,
            declaring_consumers: None,
        },
    }
}

/// Construct a full REST response object.
pub fn build_deps_response(
    dependencies: serde_json::Value,
    diagnostics: Option<&knot::cli_tools::DepsDiagnostics>,
    reverse: bool,
    requested_depth: u32,
) -> DepsResponse {
    let deps_array = match dependencies {
        serde_json::Value::Array(arr) => arr,
        _ => Vec::new(),
    };

    let diagnostics_dto = diagnostics.map(|d| project_diagnostics(d, reverse));
    let depth_report = build_depth_report(requested_depth);

    DepsResponse {
        dependencies: deps_array,
        diagnostics: diagnostics_dto,
        depth: depth_report,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use knot::cli_tools::{
        DEFAULT_MAX_DEPTH, DeclaredDependency, DepsDiagnostics, DepsDirection, MAX_DEPTH_CEILING,
    };
    use knot::db::graph::RepoIdentity;
    use serde_json::json;

    fn sample_identity() -> RepoIdentity {
        RepoIdentity {
            build_system: "npm".to_string(),
            group_id: "".to_string(),
            artifact_id: "my-app".to_string(),
            version: "1.0.0".to_string(),
        }
    }

    #[test]
    fn test_non_empty_deps_returns_null_diagnostics() {
        let deps = json!([{"repo_name": "auth-lib"}]);
        let resp = build_deps_response(deps, None, false, 3);
        assert_eq!(resp.dependencies.len(), 1);
        assert!(resp.diagnostics.is_none());
        assert_eq!(resp.depth.effective, 3);
        assert!(!resp.depth.clamped);

        let json_val = serde_json::to_value(&resp).unwrap();
        assert!(json_val["diagnostics"].is_null());
    }

    #[test]
    fn test_empty_forward_unresolved_dependencies_preserves_order_and_null_field() {
        let diag = DepsDiagnostics {
            identity: Some(sample_identity()),
            direction: DepsDirection::Forward(vec![
                DeclaredDependency {
                    name: "react".to_string(),
                    resolved_repo: None,
                },
                DeclaredDependency {
                    name: "lodash".to_string(),
                    resolved_repo: None,
                },
            ]),
        };

        let resp = build_deps_response(json!([]), Some(&diag), false, 3);
        assert!(resp.dependencies.is_empty());
        let diag_dto = resp.diagnostics.unwrap();
        assert_eq!(diag_dto.reason, "declared_but_unresolved");
        assert_eq!(diag_dto.declared_count, Some(2));
        assert_eq!(diag_dto.resolved_count, Some(0));
        assert_eq!(diag_dto.direction, "forward");
        assert!(diag_dto.answerable);

        let json_val = serde_json::to_value(&diag_dto).unwrap();
        let deps_arr = json_val["declared_dependencies"].as_array().unwrap();
        assert_eq!(deps_arr.len(), 2);
        assert_eq!(deps_arr[0]["name"], "react");
        assert!(deps_arr[0]["resolved_repo"].is_null());
        assert_eq!(deps_arr[1]["name"], "lodash");
        assert!(deps_arr[1]["resolved_repo"].is_null());
    }

    #[test]
    fn test_reverse_unanswerable_vs_zero_consumers_are_distinguishable() {
        // Reverse(None) - unanswerable
        let diag_unanswerable = DepsDiagnostics {
            identity: Some(RepoIdentity {
                build_system: "none".to_string(),
                group_id: "".to_string(),
                artifact_id: "".to_string(),
                version: "".to_string(),
            }),
            direction: DepsDirection::Reverse(None),
        };
        let resp_unanswerable = build_deps_response(json!([]), Some(&diag_unanswerable), true, 3);
        let dto1 = resp_unanswerable.diagnostics.unwrap();
        assert!(!dto1.answerable);
        assert_eq!(dto1.reason, "no_matchable_build_identity");
        assert!(dto1.declaring_consumers.is_none());

        // Reverse(Some([])) - answerable, zero consumers
        let diag_zero = DepsDiagnostics {
            identity: Some(sample_identity()),
            direction: DepsDirection::Reverse(Some(vec![])),
        };
        let resp_zero = build_deps_response(json!([]), Some(&diag_zero), true, 3);
        let dto2 = resp_zero.diagnostics.unwrap();
        assert!(dto2.answerable);
        assert_eq!(dto2.reason, "no_declaring_consumers");
        assert_eq!(dto2.declaring_consumers.as_ref().unwrap().len(), 0);

        // Assert JSON structures are explicitly distinct
        let json1 = serde_json::to_value(&dto1).unwrap();
        let json2 = serde_json::to_value(&dto2).unwrap();
        assert_eq!(json1["answerable"], false);
        assert_eq!(json2["answerable"], true);
        assert!(json1.get("declaring_consumers").is_none());
        assert_eq!(json2["declaring_consumers"], json!([]));
    }

    #[test]
    fn test_depth_report_clamping_and_flooring() {
        let r_clamp = build_depth_report(99);
        assert_eq!(r_clamp.requested, 99);
        assert_eq!(r_clamp.effective, MAX_DEPTH_CEILING);
        assert!(r_clamp.clamped);
        assert_eq!(r_clamp.ceiling, MAX_DEPTH_CEILING);

        let r_floor = build_depth_report(0);
        assert_eq!(r_floor.requested, 0);
        assert_eq!(r_floor.effective, 1);
        assert!(r_floor.clamped);

        let r_valid = build_depth_report(DEFAULT_MAX_DEPTH);
        assert_eq!(r_valid.requested, DEFAULT_MAX_DEPTH);
        assert_eq!(r_valid.effective, DEFAULT_MAX_DEPTH);
        assert!(!r_valid.clamped);
    }

    #[test]
    fn test_empty_with_diagnostics_failure_degrades_gracefully() {
        let resp = build_deps_response(json!([]), None, false, 3);
        assert!(resp.dependencies.is_empty());
        assert!(resp.diagnostics.is_none());
        assert_eq!(resp.depth.effective, 3);

        let json_val = serde_json::to_value(&resp).unwrap();
        assert_eq!(json_val["dependencies"], json!([]));
        assert!(json_val["diagnostics"].is_null());
    }

    #[test]
    fn test_stale_graph_forward_and_not_indexed_cases() {
        // Forward resolved but no edge
        let diag_stale = DepsDiagnostics {
            identity: Some(sample_identity()),
            direction: DepsDirection::Forward(vec![DeclaredDependency {
                name: "auth-lib".to_string(),
                resolved_repo: Some("auth-lib-repo".to_string()),
            }]),
        };
        let resp_stale = build_deps_response(json!([]), Some(&diag_stale), false, 3);
        let dto_stale = resp_stale.diagnostics.unwrap();
        assert_eq!(dto_stale.reason, "resolved_but_no_edge");
        assert_eq!(dto_stale.resolved_count, Some(1));

        // Repo not indexed
        let diag_not_indexed = DepsDiagnostics {
            identity: None,
            direction: DepsDirection::Forward(vec![]),
        };
        let resp_not_indexed = build_deps_response(json!([]), Some(&diag_not_indexed), false, 3);
        let dto_not_indexed = resp_not_indexed.diagnostics.unwrap();
        assert_eq!(dto_not_indexed.reason, "repository_not_indexed");
        assert!(!dto_not_indexed.repo_indexed);
        assert!(!dto_not_indexed.answerable);
    }

    #[test]
    fn test_coercion_of_non_array_dependency_input() {
        let resp = build_deps_response(json!({"error": "bad"}), None, false, 3);
        assert!(resp.dependencies.is_empty());
    }
}
