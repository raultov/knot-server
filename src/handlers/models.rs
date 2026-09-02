use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorResponse {
    pub error: String,
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct SearchParams {
    /// The search query string
    #[param(example = "authentication logic")]
    pub q: Option<String>,
    /// Maximum number of results to return
    #[param(example = 5)]
    pub max_results: Option<usize>,
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct CallersParams {
    /// Name of the entity to find callers for
    #[param(example = "handleRequest")]
    pub entity: Option<String>,
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct GlobalSearchParams {
    /// The search query string
    #[param(example = "authentication logic")]
    pub q: Option<String>,
    /// Repository scope: one name, a comma-separated list, or `all` / `*`.
    /// Omit to search every indexed repository.
    #[param(example = "repo-a,repo-b")]
    pub repo: Option<String>,
    /// Maximum number of results (global across the scope), clamped to 1..=100
    #[param(example = 5)]
    pub max_results: Option<usize>,
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct GlobalCallersParams {
    /// Name of the entity to find callers for
    #[param(example = "handleRequest")]
    pub entity: Option<String>,
    /// Repository scope: one name, a comma-separated list, or `all` / `*`.
    /// Omit to analyze every indexed repository.
    #[param(example = "repo-a,repo-b")]
    pub repo: Option<String>,
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct ExploreParams {
    /// Relative file path within the repository
    #[param(example = "src/main.rs")]
    pub path: Option<String>,
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct DepsParams {
    /// Reverse the dependency lookup (who depends on this repo vs what this repo depends on)
    #[param(example = false)]
    pub reverse: Option<bool>,
    /// Maximum traversal depth for transitive dependencies
    #[param(example = 3)]
    pub max_depth: Option<u32>,
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct GraphParams {
    /// Entity name to center the graph on (optional; omit for overview)
    #[param(example = "handleRequest")]
    pub entity: Option<String>,
    /// Entity UUID to center the graph on (optional; alternative to entity name)
    pub entity_id: Option<String>,
    /// Graph traversal depth (1-5)
    #[param(example = 2)]
    pub depth: Option<u32>,
    /// Comma-separated relationship types to include
    #[param(example = "CALLS,EXTENDS,IMPLEMENTS")]
    pub relationships: Option<String>,
    /// Graph traversal direction: incoming, outgoing, or both
    #[param(example = "both")]
    pub direction: Option<String>,
    /// Comma-separated kind categories: classes, interfaces, functions, other
    #[param(example = "classes,interfaces")]
    pub kinds: Option<String>,
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct GraphExpandParams {
    /// Entity name to expand from
    #[param(example = "handleRequest")]
    pub entity: Option<String>,
    /// Entity UUID to expand from (alternative to entity name)
    pub entity_id: Option<String>,
    /// Graph traversal depth (1-5)
    #[param(example = 2)]
    pub depth: Option<u32>,
    /// Comma-separated relationship types to include
    #[param(example = "CALLS,REFERENCES,CONTAINS")]
    pub relationships: Option<String>,
    /// Graph traversal direction: incoming, outgoing, or both
    #[param(example = "both")]
    pub direction: Option<String>,
    /// Comma-separated entity UUIDs to exclude from results
    pub exclude: Option<String>,
    /// Comma-separated kind categories: classes, interfaces, functions, other
    #[param(example = "classes,interfaces")]
    pub kinds: Option<String>,
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct RepoGraphParams {
    /// Traversal depth (1-5)
    #[param(example = 3)]
    pub depth: Option<u32>,
    /// Traversal direction: incoming, outgoing, or both
    #[param(example = "both")]
    pub direction: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum RepoRelation {
    Root,
    Dependency,
    Dependent,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RepoGraphNode {
    pub id: String,
    pub name: String,
    pub build_system: Option<String>,
    pub group_id: Option<String>,
    pub artifact_id: Option<String>,
    pub version: Option<String>,
    pub is_root: bool,
    pub registered: bool,
    pub relation: RepoRelation,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RepoGraphResponse {
    pub root_id: Option<String>,
    pub nodes: Vec<RepoGraphNode>,
    pub edges: Vec<GraphEdgeResponse>,
    pub total_nodes_found: usize,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct GraphNodeResponse {
    pub id: String,
    pub name: String,
    pub kind: Option<String>,
    pub language: Option<String>,
    pub fqn: Option<String>,
    pub signature: Option<String>,
    pub file_path: Option<String>,
    pub start_line: Option<i64>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct GraphEdgeResponse {
    pub source: String,
    pub target: String,
    #[serde(rename = "type")]
    pub edge_type: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct GraphResponse {
    pub root_id: Option<String>,
    pub nodes: Vec<GraphNodeResponse>,
    pub edges: Vec<GraphEdgeResponse>,
    pub truncated: bool,
    pub total_nodes_found: usize,
}

pub fn error_response(status: StatusCode, message: impl Into<String>) -> Response {
    (
        status,
        Json(ErrorResponse {
            error: message.into(),
        }),
    )
        .into_response()
}

pub const VALID_RELATIONSHIPS: &[&str] = &[
    "CALLS",
    "EXTENDS",
    "IMPLEMENTS",
    "REFERENCES",
    "REFERENCES_DOM",
    "USES_CSS_CLASS",
    "IMPORTS_SCRIPT",
    "IMPORTS_STYLESHEET",
    "MACRO_CALLS",
    "CONTAINS",
    "GENERIC_BOUND",
    "DEPENDS_ON",
    "OVERRIDES",
    "USES_BACKEND",
    "USES_PROBE",
    "USES_ACL",
    "INCLUDES",
    "IMPORTS_VMOD",
    "DECLARED_UNUSED",
];

pub const DEFAULT_RELATIONSHIPS_OVERVIEW: &str = "CALLS,EXTENDS,IMPLEMENTS";

pub const DEFAULT_RELATIONSHIPS_SUBGRAPH: &str = "CALLS,REFERENCES,CONTAINS";

pub const KIND_CATEGORY_CLASSES: &[&str] = &[
    "class",
    "kotlin_class",
    "kotlin_object",
    "kotlin_companion_object",
    "kotlin_enum",
    "rust_struct",
    "rust_enum",
    "rust_union",
    "rust_impl",
    "rust_module",
    "python_class",
    "cpp_class",
    "c_struct",
    "cpp_namespace",
    "groovy_class",
    "groovy_enum",
    "csharp_class",
    "csharp_struct",
    "csharp_record",
    "csharp_enum",
    "csharp_delegate",
    "enum",
    "vcl_backend",
    "vcl_probe",
    "vcl_acl",
    "vcc_module",
    "vcc_object",
    "vtc_test_case",
    "vtc_server",
    "vtc_client",
    "vtc_varnish_instance",
];

pub const KIND_CATEGORY_INTERFACES: &[&str] = &[
    "interface",
    "kotlin_interface",
    "rust_trait",
    "groovy_interface",
    "groovy_trait",
    "csharp_interface",
];

pub const KIND_CATEGORY_FUNCTIONS: &[&str] = &[
    "method",
    "function",
    "kotlin_function",
    "kotlin_method",
    "kotlin_property",
    "rust_function",
    "rust_method",
    "rust_macro_def",
    "rust_type_alias",
    "rust_constant",
    "rust_static",
    "rust_macro_invoke",
    "python_function",
    "python_method",
    "python_module",
    "python_constant",
    "c_function",
    "cpp_method",
    "macro_definition",
    "scss_function",
    "scss_mixin",
    "scss_variable",
    "groovy_method",
    "groovy_function",
    "groovy_property",
    "csharp_method",
    "csharp_constructor",
    "csharp_local_function",
    "csharp_operator",
    "csharp_indexer",
    "csharp_property",
    "csharp_field",
    "csharp_event",
    "csharp_constant",
    "constant",
    "vcl_subroutine",
    "vcl_builtin_sub",
    "vcc_function",
    "vcc_method",
];

pub const DEFAULT_VISIBLE_KINDS: &str = "classes,interfaces";

pub const VALID_KIND_CATEGORIES: &[&str] = &["classes", "interfaces", "functions", "other"];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn valid_relationships_has_no_duplicates_and_is_upper_case() {
        let mut seen = HashSet::new();
        for rel in VALID_RELATIONSHIPS {
            assert!(seen.insert(*rel), "duplicate relationship: {rel}");
            assert_eq!(
                *rel,
                rel.to_uppercase(),
                "{rel} must match knot's wire format"
            );
        }
    }

    #[test]
    fn every_enum_kind_is_categorised_as_a_class() {
        for k in ["enum", "groovy_enum", "kotlin_enum"] {
            assert!(
                KIND_CATEGORY_CLASSES.contains(&k),
                "{k} must be in KIND_CATEGORY_CLASSES so it is visible by default"
            );
        }
    }

    #[test]
    fn kind_categories_are_disjoint_and_have_no_duplicates() {
        let mut seen = HashSet::new();
        for k in KIND_CATEGORY_CLASSES {
            assert!(seen.insert(*k), "duplicate kind in CLASSES: {k}");
        }
        for k in KIND_CATEGORY_INTERFACES {
            assert!(
                seen.insert(*k),
                "duplicate kind in INTERFACES (or already in CLASSES): {k}"
            );
        }
        for k in KIND_CATEGORY_FUNCTIONS {
            assert!(
                seen.insert(*k),
                "duplicate kind in FUNCTIONS (or already in another category): {k}"
            );
        }
    }

    #[test]
    fn varnish_relationships_accepted() {
        for rel in [
            "USES_BACKEND",
            "USES_PROBE",
            "USES_ACL",
            "INCLUDES",
            "IMPORTS_VMOD",
            "DECLARED_UNUSED",
        ] {
            assert!(
                VALID_RELATIONSHIPS.contains(&rel),
                "{rel} must be in VALID_RELATIONSHIPS"
            );
        }
    }

    #[test]
    fn varnish_kinds_are_categorised() {
        let in_classes: &[&str] = &[
            "vcl_backend",
            "vcl_probe",
            "vcl_acl",
            "vcc_module",
            "vcc_object",
            "vtc_test_case",
            "vtc_server",
            "vtc_client",
            "vtc_varnish_instance",
        ];
        let in_functions: &[&str] = &[
            "vcl_subroutine",
            "vcl_builtin_sub",
            "vcc_function",
            "vcc_method",
        ];
        let uncategorised: &[&str] = &[
            "vcl_version",
            "vcl_import",
            "vcl_object_instance",
            "vtc_logexpect",
            "vtc_barrier",
        ];

        for k in in_classes {
            assert!(
                KIND_CATEGORY_CLASSES.contains(k),
                "{k} must be in KIND_CATEGORY_CLASSES"
            );
        }
        for k in in_functions {
            assert!(
                KIND_CATEGORY_FUNCTIONS.contains(k),
                "{k} must be in KIND_CATEGORY_FUNCTIONS"
            );
        }
        for k in uncategorised {
            assert!(
                !KIND_CATEGORY_CLASSES.contains(k)
                    && !KIND_CATEGORY_INTERFACES.contains(k)
                    && !KIND_CATEGORY_FUNCTIONS.contains(k),
                "{k} must not be in any category"
            );
        }
    }

    /// Every declaration kind knot's C# extractor emits must land in a
    /// category, otherwise the graph overview renders empty for C# repos under
    /// the default `classes,interfaces` filter. `csharp_namespace` is the sole
    /// exception: it is a container, not a declaration, and every namespace node
    /// is isolated under CALLS/EXTENDS/IMPLEMENTS, so it stays in `other`.
    #[test]
    fn every_csharp_kind_is_categorised() {
        const CSHARP_KINDS: &[&str] = &[
            "csharp_class",
            "csharp_constant",
            "csharp_constructor",
            "csharp_delegate",
            "csharp_enum",
            "csharp_event",
            "csharp_field",
            "csharp_indexer",
            "csharp_interface",
            "csharp_local_function",
            "csharp_method",
            "csharp_operator",
            "csharp_property",
            "csharp_record",
            "csharp_struct",
        ];

        for k in CSHARP_KINDS {
            assert!(
                KIND_CATEGORY_CLASSES.contains(k)
                    || KIND_CATEGORY_INTERFACES.contains(k)
                    || KIND_CATEGORY_FUNCTIONS.contains(k),
                "{k} must belong to a kind category"
            );
        }
    }

    #[test]
    fn csharp_namespace_is_only_reachable_via_other() {
        assert!(
            !KIND_CATEGORY_CLASSES.contains(&"csharp_namespace")
                && !KIND_CATEGORY_INTERFACES.contains(&"csharp_namespace")
                && !KIND_CATEGORY_FUNCTIONS.contains(&"csharp_namespace"),
            "csharp_namespace must stay uncategorised so the default overview \
             is not flooded with isolated container nodes"
        );
    }

    #[test]
    fn csharp_type_kinds_are_visible_by_default() {
        for k in [
            "csharp_class",
            "csharp_struct",
            "csharp_record",
            "csharp_enum",
        ] {
            assert!(
                KIND_CATEGORY_CLASSES.contains(&k),
                "{k} must be in KIND_CATEGORY_CLASSES so it is visible by default"
            );
        }
        assert!(
            KIND_CATEGORY_INTERFACES.contains(&"csharp_interface"),
            "csharp_interface must be in KIND_CATEGORY_INTERFACES"
        );
    }

    #[test]
    fn csharp_member_kinds_are_functions() {
        for k in [
            "csharp_method",
            "csharp_constructor",
            "csharp_local_function",
            "csharp_operator",
            "csharp_indexer",
            "csharp_property",
            "csharp_field",
            "csharp_event",
            "csharp_constant",
        ] {
            assert!(
                KIND_CATEGORY_FUNCTIONS.contains(&k),
                "{k} must be in KIND_CATEGORY_FUNCTIONS"
            );
        }
    }
}
