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

#[derive(Debug, Serialize, ToSchema)]
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
];

pub const DEFAULT_RELATIONSHIPS_OVERVIEW: &str = "CALLS,EXTENDS,IMPLEMENTS";

pub const DEFAULT_RELATIONSHIPS_SUBGRAPH: &str = "CALLS,REFERENCES,CONTAINS";

pub const KIND_CATEGORY_CLASSES: &[&str] = &[
    "class",
    "kotlin_class",
    "kotlin_object",
    "kotlin_companion_object",
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
    "enum",
];

pub const KIND_CATEGORY_INTERFACES: &[&str] = &[
    "interface",
    "kotlin_interface",
    "rust_trait",
    "groovy_interface",
    "groovy_trait",
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
    "constant",
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
}
