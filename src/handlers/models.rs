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
