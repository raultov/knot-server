use anyhow::Context;
use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use neo4rs::query;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use utoipa::{IntoParams, ToSchema};

use crate::models::{RegisterRepoRequest, RegisterRepoResponse, RepoEntry, RepoListResponse};
use crate::state::AppState;

#[derive(Debug, Serialize, ToSchema)]
struct ErrorResponse {
    error: String,
}

fn error_response(status: StatusCode, message: impl Into<String>) -> Response {
    (
        status,
        Json(ErrorResponse {
            error: message.into(),
        }),
    )
        .into_response()
}

// ── Read endpoints ──────────────────────────────────────────────

#[derive(Debug, Deserialize, IntoParams)]
pub struct SearchParams {
    /// The search query string
    #[param(example = "authentication logic")]
    pub q: Option<String>,
    /// Maximum number of results to return
    #[param(example = 5)]
    pub max_results: Option<usize>,
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

#[derive(Debug, Deserialize, IntoParams)]
pub struct CallersParams {
    /// Name of the entity to find callers for
    #[param(example = "handleRequest")]
    pub entity: Option<String>,
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

#[derive(Debug, Deserialize, IntoParams)]
pub struct ExploreParams {
    /// Relative file path within the repository
    #[param(example = "src/main.rs")]
    pub path: Option<String>,
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
        let registry = state.registry.lock().unwrap();
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

#[derive(Debug, Deserialize, IntoParams)]
pub struct DepsParams {
    /// Reverse the dependency lookup (who depends on this repo vs what this repo depends on)
    #[param(example = false)]
    pub reverse: Option<bool>,
    /// Maximum traversal depth for transitive dependencies
    #[param(example = 3)]
    pub max_depth: Option<u32>,
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

// ── Graph (subgraph) endpoints ───────────────────────────────────

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
struct GraphNodeResponse {
    id: String,
    name: String,
    kind: Option<String>,
    language: Option<String>,
    fqn: Option<String>,
    signature: Option<String>,
    file_path: Option<String>,
    start_line: Option<i64>,
}

#[derive(Debug, Serialize, ToSchema)]
struct GraphEdgeResponse {
    source: String,
    target: String,
    #[serde(rename = "type")]
    edge_type: String,
}

#[derive(Debug, Serialize, ToSchema)]
struct GraphResponse {
    root_id: Option<String>,
    nodes: Vec<GraphNodeResponse>,
    edges: Vec<GraphEdgeResponse>,
    truncated: bool,
    total_nodes_found: usize,
}

const VALID_RELATIONSHIPS: &[&str] = &[
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
];

const DEFAULT_RELATIONSHIPS_OVERVIEW: &str = "CALLS,EXTENDS,IMPLEMENTS";
const DEFAULT_RELATIONSHIPS_SUBGRAPH: &str = "CALLS,REFERENCES,CONTAINS";

const KIND_CATEGORY_CLASSES: &[&str] = &[
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

const KIND_CATEGORY_INTERFACES: &[&str] = &[
    "interface",
    "kotlin_interface",
    "rust_trait",
    "groovy_interface",
    "groovy_trait",
];

const KIND_CATEGORY_FUNCTIONS: &[&str] = &[
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

/// All valid kind category names accepted by the `kinds` query parameter.
const VALID_KIND_CATEGORIES: &[&str] = &["classes", "interfaces", "functions", "other"];

/// Default visible kinds for the overview graph.
const DEFAULT_VISIBLE_KINDS: &str = "classes,interfaces";

/// Parse the `kinds` query parameter into a flat list of entity kind strings.
///
/// Returns the list of kind strings that should be visible, or an error if any
/// category name is invalid.
fn parse_kinds(kinds: &str) -> Result<Vec<&str>, String> {
    let cats: Vec<&str> = if kinds.trim().is_empty() {
        DEFAULT_VISIBLE_KINDS.split(',').map(str::trim).collect()
    } else {
        kinds.split(',').map(str::trim).collect()
    };

    for cat in &cats {
        if !VALID_KIND_CATEGORIES.contains(cat) {
            return Err(format!(
                "Invalid kind category '{}'. Valid values: {}",
                cat,
                VALID_KIND_CATEGORIES.join(", ")
            ));
        }
    }

    let mut visible = Vec::new();
    for cat in cats {
        match cat {
            "classes" => visible.extend_from_slice(KIND_CATEGORY_CLASSES),
            "interfaces" => visible.extend_from_slice(KIND_CATEGORY_INTERFACES),
            "functions" => visible.extend_from_slice(KIND_CATEGORY_FUNCTIONS),
            "other" => {
                // "other" means: any kind not explicitly in classes, interfaces, or functions.
                // We don't expand it here — the Cypher query will handle it via exclusion.
            }
            _ => {}
        }
    }

    Ok(visible)
}

/// Returns true if the "other" category is explicitly listed in `kinds`.
fn includes_other(kinds: &str) -> bool {
    if kinds.trim().is_empty() {
        return DEFAULT_VISIBLE_KINDS.contains("other");
    }
    kinds.split(',').any(|c| c.trim() == "other")
}

fn subgraph_to_response(result: knot::models::SubgraphResult) -> GraphResponse {
    GraphResponse {
        root_id: result.root_id,
        nodes: result
            .nodes
            .into_iter()
            .map(|n| {
                let language = n
                    .kind
                    .as_ref()
                    .and_then(|k| k.split('_').next().map(|s| s.to_string()));
                GraphNodeResponse {
                    id: n.uuid,
                    name: n.name,
                    kind: n.kind,
                    language,
                    fqn: n.fqn,
                    signature: n.signature,
                    file_path: n.file_path,
                    start_line: n.start_line,
                }
            })
            .collect(),
        edges: result
            .edges
            .into_iter()
            .map(|e| GraphEdgeResponse {
                source: e.source_uuid,
                target: e.target_uuid,
                edge_type: e.relationship,
            })
            .filter(|e| e.source != e.target)
            .collect(),
        truncated: result.truncated,
        total_nodes_found: result.total_nodes_found,
    }
}

fn parse_direction(direction: &str) -> knot::models::SubgraphDirection {
    match direction {
        "incoming" => knot::models::SubgraphDirection::Incoming,
        "outgoing" => knot::models::SubgraphDirection::Outgoing,
        _ => knot::models::SubgraphDirection::Both,
    }
}

fn parse_relationships(relationships: &str) -> Result<Vec<&str>, String> {
    let parsed: Vec<&str> = if relationships.trim().is_empty() {
        vec!["CALLS"]
    } else {
        relationships.split(',').map(|s| s.trim()).collect()
    };

    for rel in &parsed {
        if !VALID_RELATIONSHIPS.contains(rel) {
            return Err(format!(
                "Invalid relationship type '{}'. Valid types: {}",
                rel,
                VALID_RELATIONSHIPS.join(", ")
            ));
        }
    }

    Ok(parsed)
}

#[utoipa::path(
    get,
    path = "/api/repos/{id}/graph",
    tag = "Graph",
    params(
        ("id" = String, Path, description = "Repository ID"),
        GraphParams,
    ),
    responses(
        (status = 200, description = "Graph overview or subgraph results", body = GraphResponse),
        (status = 400, description = "Missing or invalid query parameter", body = ErrorResponse),
        (status = 404, description = "Repository or entity not found", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse),
    ),
    description = "Query entity relationship graph. Without entity/entity_id returns an overview; with one returns a subgraph centered on that entity.",
)]
pub async fn graph_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(params): Query<GraphParams>,
) -> Response {
    {
        let registry = state.registry.lock().unwrap();
        if registry.get(&id).is_none() {
            return error_response(
                StatusCode::NOT_FOUND,
                format!("Repository '{}' not found", id),
            );
        }
    }

    // Determine entity_name and entity_uuid from params
    let (entity_name, entity_uuid) = if let Some(uuid) = &params.entity_id
        && !uuid.trim().is_empty()
    {
        // UUID provided — pass it directly to the subgraph query.
        // We still need a name for backward compatibility, so resolve it.
        match resolve_uuid_to_name(&state, uuid, &id).await {
            Ok(Some(name)) => (Some(name), Some(uuid.clone())),
            Ok(None) => {
                return error_response(
                    StatusCode::NOT_FOUND,
                    format!("Entity with UUID '{}' not found", uuid),
                );
            }
            Err(e) => {
                return error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Failed to resolve entity UUID: {e}"),
                );
            }
        }
    } else {
        match &params.entity {
            Some(e) if !e.trim().is_empty() => (Some(e.clone()), None),
            _ => (None, None),
        }
    };

    match entity_name {
        Some(entity_name) => {
            let depth = params.depth.unwrap_or(2).clamp(1, 5);
            let direction_str = params.direction.as_deref().unwrap_or("both");
            let direction = parse_direction(direction_str);
            let rels_str = params
                .relationships
                .as_deref()
                .unwrap_or(DEFAULT_RELATIONSHIPS_SUBGRAPH);
            let relationships = match parse_relationships(rels_str) {
                Ok(rels) => rels,
                Err(msg) => {
                    return error_response(StatusCode::BAD_REQUEST, msg);
                }
            };

            let kinds_str = params.kinds.as_deref().unwrap_or(DEFAULT_VISIBLE_KINDS);
            let visible_kinds = match parse_kinds(kinds_str) {
                Ok(kinds) => kinds,
                Err(msg) => return error_response(StatusCode::BAD_REQUEST, msg),
            };
            let include_oth = includes_other(kinds_str);
            let kind_filter: Option<&[&str]> = if include_oth {
                None
            } else {
                Some(visible_kinds.as_slice())
            };

            match knot::cli_tools::run_get_subgraph(
                &entity_name,
                &id,
                depth,
                &relationships,
                direction,
                None,
                &state.graph_db,
                entity_uuid.as_deref(),
                kind_filter,
            )
            .await
            {
                Ok(result) => {
                    let mut response = subgraph_to_response(result);

                    let connected_uuids: std::collections::HashSet<&str> = response
                        .edges
                        .iter()
                        .flat_map(|e| vec![e.source.as_str(), e.target.as_str()])
                        .collect();

                    response.nodes.retain(|n| {
                        Some(&n.id) == response.root_id.as_ref()
                            || connected_uuids.contains(n.id.as_str())
                    });

                    (StatusCode::OK, Json(response)).into_response()
                }
                Err(e) => error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Graph query failed: {e}"),
                ),
            }
        }
        None => {
            let depth = params.depth.unwrap_or(2).clamp(1, 5);
            let rels_str = params
                .relationships
                .as_deref()
                .unwrap_or(DEFAULT_RELATIONSHIPS_OVERVIEW);
            let relationships = match parse_relationships(rels_str) {
                Ok(rels) => rels,
                Err(msg) => {
                    return error_response(StatusCode::BAD_REQUEST, msg);
                }
            };

            let kinds_str = params.kinds.as_deref().unwrap_or(DEFAULT_VISIBLE_KINDS);
            let visible_kinds = match parse_kinds(kinds_str) {
                Ok(kinds) => kinds,
                Err(msg) => {
                    return error_response(StatusCode::BAD_REQUEST, msg);
                }
            };
            let other = includes_other(kinds_str);

            match fetch_all_entities(&state, &id, depth, &relationships, &visible_kinds, other)
                .await
            {
                Ok(response) => (StatusCode::OK, Json(response)).into_response(),
                Err(e) => error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Graph overview query failed: {e}"),
                ),
            }
        }
    }
}

async fn resolve_uuid_to_name(
    state: &AppState,
    uuid: &str,
    repo_id: &str,
) -> anyhow::Result<Option<String>> {
    let graph = neo4rs::Graph::new(&state.neo4j_uri, &state.neo4j_user, &state.neo4j_password)
        .context("Failed to connect to Neo4j")?;
    let q = query("MATCH (e:Entity {uuid: $uuid, repo_name: $repo_name}) RETURN e.name LIMIT 1")
        .param("uuid", uuid)
        .param("repo_name", repo_id);
    let mut rows = graph.execute(q).await.context("Neo4j query failed")?;
    if let Ok(Some(row)) = rows.next().await {
        Ok(row.get::<String>("e.name").ok())
    } else {
        Ok(None)
    }
}

async fn fetch_all_entities(
    state: &AppState,
    repo_id: &str,
    depth: u32,
    relationships: &[&str],
    visible_kinds: &[&str],
    include_other: bool,
) -> anyhow::Result<GraphResponse> {
    let graph = neo4rs::Graph::new(&state.neo4j_uri, &state.neo4j_user, &state.neo4j_password)
        .context("Failed to connect to Neo4j")?;

    let rel_filter = relationships.join("|");

    let node_q_str = format!(
        "MATCH (root:Entity {{repo_name: $repo_name}})
         WHERE NOT ()-[:CONTAINS]->(root)
         MATCH (root)-[:{rel_filter}*0..{depth}]->(e:Entity)
         RETURN DISTINCT e.uuid, e.name, e.kind, e.fqn, e.signature, e.file_path, e.start_line"
    );

    let node_q = query(&node_q_str).param("repo_name", repo_id);

    let mut rows = graph
        .execute(node_q)
        .await
        .context("Neo4j node query failed")?;

    let visible_set: std::collections::HashSet<&str> = visible_kinds.iter().copied().collect();

    let mut nodes = Vec::new();
    while let Ok(Some(row)) = rows.next().await {
        let uuid = row.get::<String>("e.uuid").unwrap_or_default();
        let name = row.get::<String>("e.name").unwrap_or_default();
        if uuid.is_empty() || name.is_empty() {
            continue;
        }
        let kind: Option<String> = row.get::<String>("e.kind").ok();
        if let Some(ref k) = kind
            && !include_other
            && !visible_set.contains(k.as_str())
        {
            continue;
        }
        let language = kind
            .as_ref()
            .and_then(|k| k.split('_').next().map(|s| s.to_string()));
        nodes.push(GraphNodeResponse {
            id: uuid,
            name,
            kind,
            language,
            fqn: row.get::<String>("e.fqn").ok(),
            signature: row.get::<String>("e.signature").ok(),
            file_path: row.get::<String>("e.file_path").ok(),
            start_line: row.get::<i64>("e.start_line").ok(),
        });
    }

    let node_uuids: std::collections::HashSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();

    let total = nodes.len();

    let edge_q = if visible_set.is_empty() && !include_other {
        String::from("RETURN DISTINCT '' AS source, '' AS target, '' AS rel LIMIT 0")
    } else {
        let visible_list = visible_kinds
            .iter()
            .map(|k| format!("'{}'", k))
            .collect::<Vec<_>>()
            .join(", ");

        if include_other {
            format!(
                "MATCH (a:Entity {{repo_name: $repo_name}})-[r:{rel_filter}]->(b:Entity {{repo_name: $repo_name}})
                 RETURN DISTINCT a.uuid AS source, b.uuid AS target, type(r) AS rel"
            )
        } else {
            format!(
                "MATCH (a:Entity {{repo_name: $repo_name}})-[r:{rel_filter}]->(b:Entity {{repo_name: $repo_name}})
                 WHERE a.kind IN [{visible_list}] AND b.kind IN [{visible_list}]
                 RETURN DISTINCT a.uuid AS source, b.uuid AS target, type(r) AS rel

                 UNION

                 MATCH (m1:Entity {{repo_name: $repo_name}})-[r:{rel_filter}]->(m2:Entity {{repo_name: $repo_name}})
                 WHERE NOT m1.kind IN [{visible_list}]
                   AND NOT m2.kind IN [{visible_list}]
                   AND m1.enclosing_class <> ''
                   AND m2.enclosing_class <> ''
                 MATCH (c1:Entity {{name: m1.enclosing_class, repo_name: $repo_name}})
                 MATCH (c2:Entity {{name: m2.enclosing_class, repo_name: $repo_name}})
                 WHERE c1.kind IN [{visible_list}]
                   AND c2.kind IN [{visible_list}]
                   AND c1.uuid <> c2.uuid
                 RETURN DISTINCT c1.uuid AS source, c2.uuid AS target, type(r) AS rel"
            )
        }
    };

    let edge_q = query(&edge_q).param("repo_name", repo_id);

    let mut edge_rows = graph
        .execute(edge_q)
        .await
        .context("Failed to query entity edges")?;
    let mut edges = Vec::new();
    while let Ok(Some(row)) = edge_rows.next().await {
        if let (Ok(source), Ok(target), Ok(rel)) = (
            row.get::<String>("source"),
            row.get::<String>("target"),
            row.get::<String>("rel"),
        ) {
            if source.is_empty()
                || target.is_empty()
                || rel.is_empty()
                || source == target
                || !node_uuids.contains(source.as_str())
                || !node_uuids.contains(target.as_str())
            {
                continue;
            }
            edges.push(GraphEdgeResponse {
                source,
                target,
                edge_type: rel,
            });
        }
    }

    Ok(GraphResponse {
        root_id: None,
        nodes,
        edges,
        truncated: false,
        total_nodes_found: total,
    })
}

#[utoipa::path(
    get,
    path = "/api/repos/{id}/graph/expand",
    tag = "Graph",
    params(
        ("id" = String, Path, description = "Repository ID"),
        GraphExpandParams,
    ),
    responses(
        (status = 200, description = "Expanded subgraph results", body = GraphResponse),
        (status = 400, description = "Missing or invalid query parameter", body = ErrorResponse),
        (status = 404, description = "Repository or entity not found", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse),
    ),
    description = "Expand a graph node to discover its neighbors. Supports excluding already-visible UUIDs for incremental graph expansion.",
)]
pub async fn graph_expand_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(params): Query<GraphExpandParams>,
) -> Response {
    {
        let registry = state.registry.lock().unwrap();
        if registry.get(&id).is_none() {
            return error_response(
                StatusCode::NOT_FOUND,
                format!("Repository '{}' not found", id),
            );
        }
    }

    let (entity_name, entity_uuid) = if let Some(uuid) = &params.entity_id
        && !uuid.trim().is_empty()
    {
        match resolve_uuid_to_name(&state, uuid, &id).await {
            Ok(Some(name)) => (Some(name), Some(uuid.clone())),
            Ok(None) => {
                return error_response(
                    StatusCode::NOT_FOUND,
                    format!("Entity with UUID '{}' not found", uuid),
                );
            }
            Err(e) => {
                return error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Failed to resolve entity UUID: {e}"),
                );
            }
        }
    } else {
        match &params.entity {
            Some(e) if !e.trim().is_empty() => (Some(e.clone()), None),
            _ => (None, None),
        }
    };

    let entity_name = match entity_name {
        Some(name) => name,
        None => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "Missing required parameter 'entity' or 'entity_id'",
            );
        }
    };

    let direction_str = params.direction.as_deref().unwrap_or("both");
    let direction = parse_direction(direction_str);
    let rels_str = params
        .relationships
        .as_deref()
        .unwrap_or(DEFAULT_RELATIONSHIPS_SUBGRAPH);
    let relationships = match parse_relationships(rels_str) {
        Ok(rels) => rels,
        Err(msg) => {
            return error_response(StatusCode::BAD_REQUEST, msg);
        }
    };

    let exclude_uuids: std::collections::HashSet<String> = params
        .exclude
        .as_deref()
        .unwrap_or("")
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    let depth = params.depth.unwrap_or(2).clamp(1, 5);

    let kinds_str = params.kinds.as_deref().unwrap_or(DEFAULT_VISIBLE_KINDS);
    let visible_kinds = match parse_kinds(kinds_str) {
        Ok(kinds) => kinds,
        Err(msg) => return error_response(StatusCode::BAD_REQUEST, msg),
    };
    let include_oth = includes_other(kinds_str);
    let kind_filter: Option<&[&str]> = if include_oth {
        None
    } else {
        Some(visible_kinds.as_slice())
    };

    match knot::cli_tools::run_get_subgraph(
        &entity_name,
        &id,
        depth,
        &relationships,
        direction,
        None,
        &state.graph_db,
        entity_uuid.as_deref(),
        kind_filter,
    )
    .await
    {
        Ok(mut result) => {
            if !exclude_uuids.is_empty() {
                result.nodes.retain(|n| !exclude_uuids.contains(&n.uuid));
            }

            let mut response = subgraph_to_response(result);

            let connected_uuids: std::collections::HashSet<&str> = response
                .edges
                .iter()
                .flat_map(|e| vec![e.source.as_str(), e.target.as_str()])
                .collect();

            response.nodes.retain(|n| {
                Some(&n.id) == response.root_id.as_ref() || connected_uuids.contains(n.id.as_str())
            });

            (StatusCode::OK, Json(response)).into_response()
        }
        Err(e) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Graph expand failed: {e}"),
        ),
    }
}

// ── Graph viewer endpoint ────────────────────────────────────────

const GRAPH_VIEWER_HTML_TEMPLATE: &str = include_str!("../assets/graph-viewer.html");

static GRAPH_VIEWER_HTML: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    GRAPH_VIEWER_HTML_TEMPLATE.replace("{{KNOT_SERVER_VERSION}}", env!("CARGO_PKG_VERSION"))
});

pub async fn graph_viewer_handler() -> Response {
    (
        StatusCode::OK,
        [("content-type", "text/html; charset=utf-8")],
        GRAPH_VIEWER_HTML.as_str(),
    )
        .into_response()
}

pub async fn favicon_handler() -> Response {
    const FAVICON_BYTES: &[u8] = include_bytes!("../assets/favicon.png");

    Response::builder()
        .header("content-type", "image/png")
        .body(axum::body::Body::from(FAVICON_BYTES))
        .unwrap()
}

// ── Repository management endpoints ─────────────────────────────

#[utoipa::path(
    get,
    path = "/api/repos",
    tag = "Repositories",
    responses(
        (status = 200, description = "List of all registered repositories", body = RepoListResponse),
    ),
    description = "List all registered Git repositories with their current status and metadata.",
)]
pub async fn list_repos_handler(State(state): State<Arc<AppState>>) -> Response {
    let registry = state.registry.lock().unwrap();
    let repos = registry.list().to_vec();
    let response = RepoListResponse {
        repositories: repos,
    };
    (StatusCode::OK, Json(response)).into_response()
}

#[utoipa::path(
    get,
    path = "/api/repos/{id}",
    tag = "Repositories",
    params(
        ("id" = String, Path, description = "Repository ID"),
    ),
    responses(
        (status = 200, description = "Repository details", body = RepoEntry),
        (status = 404, description = "Repository not found", body = ErrorResponse),
    ),
    description = "Get detailed information about a single registered repository.",
)]
pub async fn get_repo_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let registry = state.registry.lock().unwrap();
    match registry.get(&id) {
        Some(entry) => (StatusCode::OK, Json(entry.clone())).into_response(),
        None => error_response(
            StatusCode::NOT_FOUND,
            format!("Repository '{}' not found", id),
        ),
    }
}

#[utoipa::path(
    post,
    path = "/api/repos",
    tag = "Repositories",
    request_body = RegisterRepoRequest,
    responses(
        (status = 202, description = "Repository registered and clone job enqueued", body = RegisterRepoResponse),
        (status = 409, description = "Repository already exists", body = ErrorResponse),
        (status = 429, description = "Indexing queue is full", body = ErrorResponse),
    ),
    description = "Register a new Git repository. The server clones it and queues it for indexing.",
)]
pub async fn register_repo_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<RegisterRepoRequest>,
) -> Response {
    let id = body.generate_id();
    let local_path = crate::models::repo_local_path(&state.workspace_dir, &id);

    let entry = crate::models::RepoEntry {
        id: id.clone(),
        url: body.url.clone(),
        auth_type: body.auth_type.clone(),
        local_path,
        branch: body.branch.clone(),
        webhook_secret: body.webhook_secret.clone(),
        last_indexed: None,
        status: crate::models::RepoStatus::Pending,
    };

    let mut registry = state.registry.lock().unwrap();
    match registry.add(entry) {
        Ok(()) => {
            // Enqueue Clone job for the new repository
            let job = crate::models::IndexJob::Clone {
                repo_id: id.clone(),
            };
            match state.job_tx.try_send(job) {
                Ok(()) => {}
                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                    let _ = registry.remove(&id);
                    return error_response(
                        StatusCode::TOO_MANY_REQUESTS,
                        "Server is at maximum capacity: indexing queue is full",
                    );
                }
                Err(e) => {
                    tracing::error!("Failed to enqueue Clone job for {}: {e}", id);
                }
            }

            tracing::info!(
                "Registered repository '{}' (url: {}, auth: {:?})",
                id,
                body.url,
                body.auth_type
            );

            let response = RegisterRepoResponse {
                id,
                message: "Repository registered successfully".into(),
            };
            (StatusCode::ACCEPTED, Json(response)).into_response()
        }
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("already exists") {
                error_response(StatusCode::CONFLICT, msg)
            } else {
                error_response(StatusCode::INTERNAL_SERVER_ERROR, msg)
            }
        }
    }
}

#[utoipa::path(
    delete,
    path = "/api/repos/{id}",
    tag = "Repositories",
    params(
        ("id" = String, Path, description = "Repository ID"),
    ),
    responses(
        (status = 200, description = "Repository deleted successfully", body = serde_json::Value),
        (status = 404, description = "Repository not found", body = ErrorResponse),
    ),
    description = "Delete a repository and clean up its databases and local files.",
)]
pub async fn delete_repo_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let mut registry = state.registry.lock().unwrap();

    match registry.remove(&id) {
        Ok(()) => {
            // Clean up databases in background (fire-and-forget)
            let graph_db = state.graph_db.clone();
            let vector_db = state.vector_db.clone();
            let rid = id.clone();
            tokio::spawn(async move {
                crate::cleanup::delete_repo_from_databases(&rid, &graph_db, &vector_db).await;
            });

            // Clean up local directory
            let repo_path = crate::models::repo_local_path(&state.workspace_dir, &id);
            if std::path::Path::new(&repo_path).exists()
                && let Err(e) = std::fs::remove_dir_all(&repo_path)
            {
                tracing::warn!("Failed to remove repo directory {}: {e}", repo_path);
            }
            tracing::info!("Deleted repository '{}'", id);
            (
                StatusCode::OK,
                Json(serde_json::json!({"message": "Repository deleted"})),
            )
                .into_response()
        }
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("not found") {
                error_response(StatusCode::NOT_FOUND, msg)
            } else {
                error_response(StatusCode::INTERNAL_SERVER_ERROR, msg)
            }
        }
    }
}

// ── Trigger endpoints ───────────────────────────────────────────

#[utoipa::path(
    post,
    path = "/api/repos/{id}/sync",
    tag = "Indexing",
    params(
        ("id" = String, Path, description = "Repository ID"),
    ),
    responses(
        (status = 202, description = "Sync job enqueued", body = serde_json::Value),
        (status = 404, description = "Repository not found", body = ErrorResponse),
        (status = 429, description = "Indexing queue is full", body = ErrorResponse),
    ),
    description = "Manually trigger a sync (pull + re-index) for a repository.",
)]
pub async fn sync_repo_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    // Check repo exists
    {
        let registry = state.registry.lock().unwrap();
        if registry.get(&id).is_none() {
            return error_response(
                StatusCode::NOT_FOUND,
                format!("Repository '{}' not found", id),
            );
        }
    }

    // Enqueue Pull job
    let job = crate::models::IndexJob::Pull {
        repo_id: id.clone(),
    };
    match state.job_tx.try_send(job) {
        Ok(()) => {}
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
            return error_response(
                StatusCode::TOO_MANY_REQUESTS,
                "Server is at maximum capacity: indexing queue is full",
            );
        }
        Err(e) => {
            tracing::error!("Failed to enqueue Pull job for {}: {e}", id);
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to enqueue sync job",
            );
        }
    }

    tracing::info!("Enqueued sync job for '{}'", id);

    (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({
            "message": "Sync job enqueued",
            "repo_id": id
        })),
    )
        .into_response()
}

// ── Webhook endpoint ─────────────────────────────────────────────

#[utoipa::path(
    post,
    path = "/api/webhook/{id}",
    tag = "Webhooks",
    request_body(content_type = "application/json", description = "Raw webhook payload (validated via HMAC signature)"),
    params(
        ("id" = String, Path, description = "Repository ID"),
    ),
    responses(
        (status = 202, description = "Webhook accepted, indexing job enqueued"),
        (status = 401, description = "Invalid or missing webhook signature", body = ErrorResponse),
        (status = 404, description = "Repository not found", body = ErrorResponse),
    ),
    description = "Endpoint for Git provider webhooks (GitHub, GitLab, Bitbucket). Validates payload signatures and triggers incremental re-indexing on push events.",
)]
pub async fn webhook_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    // Check repo exists and get webhook secret
    let webhook_secret = {
        let registry = state.registry.lock().unwrap();
        match registry.get(&id) {
            Some(entry) => entry.webhook_secret.clone(),
            None => {
                return error_response(
                    StatusCode::NOT_FOUND,
                    format!("Repository '{}' not found", id),
                );
            }
        }
    };

    let Some(secret) = webhook_secret else {
        return error_response(
            StatusCode::UNAUTHORIZED,
            "Webhook secret not configured for this repository",
        );
    };

    // Try GitLab token validation
    if let Some(token) = headers.get("X-Gitlab-Token").and_then(|v| v.to_str().ok()) {
        if crate::webhook::validate_gitlab_token(token, &secret) {
            return enqueue_pull_job(&state, &id).await;
        }
        return error_response(StatusCode::UNAUTHORIZED, "Invalid GitLab webhook token");
    }

    // Try GitHub signature validation
    if let Some(sig) = headers
        .get("X-Hub-Signature-256")
        .and_then(|v| v.to_str().ok())
    {
        if crate::webhook::validate_github_signature(sig, &body, &secret) {
            return enqueue_pull_job(&state, &id).await;
        }
        return error_response(StatusCode::UNAUTHORIZED, "Invalid GitHub webhook signature");
    }

    // Try Bitbucket signature validation
    if let Some(sig) = headers.get("X-Hub-Signature").and_then(|v| v.to_str().ok()) {
        if crate::webhook::validate_bitbucket_signature(sig, &body, &secret) {
            return enqueue_pull_job(&state, &id).await;
        }
        return error_response(
            StatusCode::UNAUTHORIZED,
            "Invalid Bitbucket webhook signature",
        );
    }

    error_response(
        StatusCode::UNAUTHORIZED,
        "Missing webhook signature header (X-Gitlab-Token, X-Hub-Signature-256, or X-Hub-Signature)",
    )
}

async fn enqueue_pull_job(state: &Arc<AppState>, repo_id: &str) -> Response {
    let job = crate::models::IndexJob::Pull {
        repo_id: repo_id.to_string(),
    };
    match state.job_tx.try_send(job) {
        Ok(()) => {}
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
            return error_response(
                StatusCode::TOO_MANY_REQUESTS,
                "Server is at maximum capacity: indexing queue is full",
            );
        }
        Err(e) => {
            tracing::error!("Failed to enqueue webhook Pull job for {}: {e}", repo_id);
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to enqueue indexing job",
            );
        }
    }
    tracing::info!("Webhook validated for '{}', enqueued Pull job", repo_id);
    (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({
            "message": "Webhook received, indexing job enqueued",
            "repo_id": repo_id
        })),
    )
        .into_response()
}

// ── Health endpoint ──────────────────────────────────────────────

#[utoipa::path(
    get,
    path = "/api/health",
    tag = "Health",
    responses(
        (status = 200, description = "Server health status"),
    ),
    description = "Check server health, uptime, queue capacity, and repository statistics.",
)]
pub async fn health_handler(State(state): State<Arc<AppState>>) -> Response {
    let registry = state.registry.lock().unwrap();
    let repos = registry.list();
    let cloning_count = repos
        .iter()
        .filter(|r| r.status == crate::models::RepoStatus::Cloning)
        .count();
    let pulling_count = repos
        .iter()
        .filter(|r| r.status == crate::models::RepoStatus::Pulling)
        .count();
    let indexing_count = repos
        .iter()
        .filter(|r| r.status == crate::models::RepoStatus::Indexing)
        .count();

    let uptime = state.start_time.elapsed().as_secs();

    let health = serde_json::json!({
        "status": "ok",
        "uptime_seconds": uptime,
        "queue_capacity": state.job_tx.capacity(),
        "repositories_total": repos.len(),
        "repositories_cloning": cloning_count,
        "repositories_pulling": pulling_count,
        "repositories_indexing": indexing_count,
        "workspace_dir": state.workspace_dir,
    });

    (StatusCode::OK, Json(health)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::{get, post};
    use knot::db::graph::ConnectExt;
    use knot::db::vector::VectorConnectExt;
    use std::sync::{Arc, Mutex};
    use tempfile::TempDir;
    use tower::ServiceExt;

    use crate::registry::Registry;

    fn build_test_app(state: Arc<AppState>) -> Router {
        Router::new()
            .route(
                "/api/repos",
                get(list_repos_handler).post(register_repo_handler),
            )
            .route(
                "/api/repos/{id}",
                get(get_repo_handler).delete(delete_repo_handler),
            )
            .route("/api/repos/{id}/search", get(search_handler))
            .route("/api/repos/{id}/callers", get(callers_handler))
            .route("/api/repos/{id}/explore", get(explore_handler))
            .route("/api/repos/{id}/deps", get(deps_handler))
            .route("/api/repos/{id}/graph", get(graph_handler))
            .route("/api/repos/{id}/graph/expand", get(graph_expand_handler))
            .route("/api/repos/{id}/sync", post(sync_repo_handler))
            .route("/api/webhook/{id}", post(webhook_handler))
            .route("/api/health", get(health_handler))
            .route("/graph", get(graph_viewer_handler))
            .with_state(state)
    }

    async fn create_test_state_with_tempdir(
        temp_dir: &TempDir,
    ) -> (
        Arc<AppState>,
        tokio::sync::mpsc::Receiver<crate::models::IndexJob>,
    ) {
        let workspace = temp_dir.path().to_path_buf();
        let registry =
            Registry::load_or_create(&workspace).expect("Failed to create test registry");

        let graph_db =
            knot::db::graph::GraphDb::connect("bolt://localhost:9999", "neo4j", "badpassword")
                .await
                .expect("connect for test db should work");

        let vector_db =
            knot::db::vector::VectorDb::connect("http://localhost:9999", "test_collection", 384)
                .await
                .expect("connect for test vector db should work");

        let (job_tx, job_rx) = tokio::sync::mpsc::channel::<crate::models::IndexJob>(16);

        (
            Arc::new(AppState {
                vector_db: Arc::new(vector_db),
                graph_db: Arc::new(graph_db),
                embedder: None,
                workspace_dir: workspace.to_string_lossy().into(),
                registry: Arc::new(Mutex::new(registry)),
                job_tx,
                qdrant_url: "http://localhost:6334".into(),
                qdrant_collection: "knot_entities".into(),
                neo4j_uri: "bolt://localhost:7687".into(),
                neo4j_user: "neo4j".into(),
                neo4j_password: "secret".into(),
                embed_dim: 384,
                rayon_threads: None,
                batch_size: 64,
                ingest_concurrency: 4,
                start_time: std::time::Instant::now(),
            }),
            job_rx,
        )
    }

    #[tokio::test]
    async fn test_register_duplicate_returns_409() {
        let dir = TempDir::new().unwrap();
        let (state, _job_rx) = create_test_state_with_tempdir(&dir).await;
        let app = build_test_app(state);

        let body = serde_json::json!({
            "url": "git@github.com:org/repo.git",
            "auth_type": "ssh"
        });

        // First registration succeeds
        let _ = app
            .clone()
            .oneshot(
                Request::post("/api/repos")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        // Second returns conflict
        let response = app
            .oneshot(
                Request::post("/api/repos")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn test_delete_nonexistent_repo_returns_404() {
        let dir = TempDir::new().unwrap();
        let (state, _job_rx) = create_test_state_with_tempdir(&dir).await;
        let app = build_test_app(state);

        let response = app
            .oneshot(
                Request::delete("/api/repos/nonexistent")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_get_nonexistent_repo_returns_404() {
        let dir = TempDir::new().unwrap();
        let (state, _job_rx) = create_test_state_with_tempdir(&dir).await;
        let app = build_test_app(state);

        let response = app
            .oneshot(
                Request::get("/api/repos/ghost")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_sync_nonexistent_repo_returns_404() {
        let dir = TempDir::new().unwrap();
        let (state, _job_rx) = create_test_state_with_tempdir(&dir).await;
        let app = build_test_app(state);

        let response = app
            .oneshot(
                Request::post("/api/repos/ghost/sync")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_sync_existing_repo_returns_202() {
        let dir = TempDir::new().unwrap();
        let (state, _job_rx) = create_test_state_with_tempdir(&dir).await;
        let app = build_test_app(state);

        // Register a repo first
        let body = serde_json::json!({
            "url": "git@github.com:org/sync-test.git",
            "auth_type": "ssh"
        });
        let reg_response = app
            .clone()
            .oneshot(
                Request::post("/api/repos")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(reg_response.status(), StatusCode::ACCEPTED);

        let body_bytes = axum::body::to_bytes(reg_response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let reg_json: RegisterRepoResponse = serde_json::from_slice(&body_bytes).unwrap();

        // Trigger sync
        let response = app
            .oneshot(
                Request::post(format!("/api/repos/{}/sync", reg_json.id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let status = response.status();
        let _body_bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        assert_eq!(status, StatusCode::ACCEPTED);
    }

    #[tokio::test]
    async fn test_webhook_nonexistent_repo_no_auth_returns_404() {
        let dir = TempDir::new().unwrap();
        let (state, _job_rx) = create_test_state_with_tempdir(&dir).await;
        let app = build_test_app(state);

        let response = app
            .oneshot(
                Request::post("/api/webhook/test-repo")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_webhook_nonexistent_repo_returns_404() {
        let dir = TempDir::new().unwrap();
        let (state, _job_rx) = create_test_state_with_tempdir(&dir).await;
        let app = build_test_app(state);

        let response = app
            .oneshot(
                Request::post("/api/webhook/ghost")
                    .header("content-type", "application/json")
                    .header("X-Gitlab-Token", "test-token")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_health_returns_ok() {
        let dir = TempDir::new().unwrap();
        let (state, _job_rx) = create_test_state_with_tempdir(&dir).await;
        let app = build_test_app(state);

        let response = app
            .oneshot(Request::get("/api/health").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body_bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let health: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(health["status"], "ok");
        assert!(health["uptime_seconds"].as_u64().is_some());
        assert!(health["repositories_total"].as_u64().is_some());
    }

    #[tokio::test]
    async fn test_graph_missing_entity_returns_overview_error() {
        let dir = TempDir::new().unwrap();
        let (state, _job_rx) = create_test_state_with_tempdir(&dir).await;
        let app = build_test_app(state);

        let body = serde_json::json!({
            "url": "git@github.com:org/repo.git",
            "auth_type": "ssh"
        });
        let res = app
            .clone()
            .oneshot(
                Request::post("/api/repos")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        let body_bytes = axum::body::to_bytes(res.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        let repo_id = json["id"].as_str().unwrap();

        let response = app
            .oneshot(
                Request::get(format!("/api/repos/{repo_id}/graph"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn test_graph_expand_missing_entity_returns_400() {
        let dir = TempDir::new().unwrap();
        let (state, _job_rx) = create_test_state_with_tempdir(&dir).await;
        let app = build_test_app(state);

        let body = serde_json::json!({
            "url": "git@github.com:org/repo.git",
            "auth_type": "ssh"
        });
        let res = app
            .clone()
            .oneshot(
                Request::post("/api/repos")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        let body_bytes = axum::body::to_bytes(res.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        let repo_id = json["id"].as_str().unwrap();

        let response = app
            .oneshot(
                Request::get(format!("/api/repos/{repo_id}/graph/expand"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_graph_viewer_returns_200() {
        let dir = TempDir::new().unwrap();
        let (state, _job_rx) = create_test_state_with_tempdir(&dir).await;
        let app = build_test_app(state);

        let response = app
            .oneshot(Request::get("/graph").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("content-type")
                .unwrap()
                .to_str()
                .unwrap(),
            "text/html; charset=utf-8"
        );

        let body_bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let body = String::from_utf8(body_bytes.to_vec()).unwrap();
        assert!(body.contains("<!DOCTYPE html>"));
        assert!(body.contains("ForceGraph3D"));
        assert!(
            !body.contains("{{KNOT_SERVER_VERSION}}"),
            "version placeholder was not substituted"
        );
        assert!(
            body.contains(env!("CARGO_PKG_VERSION")),
            "package version absent from HTML"
        );
        assert!(body.contains("knot-server v"), "version badge malformed");
    }

    #[tokio::test]
    async fn test_graph_nonexistent_repo_returns_404() {
        let dir = TempDir::new().unwrap();
        let (state, _job_rx) = create_test_state_with_tempdir(&dir).await;
        let app = build_test_app(state);

        let response = app
            .oneshot(
                Request::get("/api/repos/nonexistent/graph?entity=some_function")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_graph_expand_nonexistent_repo_returns_404() {
        let dir = TempDir::new().unwrap();
        let (state, _job_rx) = create_test_state_with_tempdir(&dir).await;
        let app = build_test_app(state);

        let response = app
            .oneshot(
                Request::get("/api/repos/nonexistent/graph/expand?entity=some_function")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_register_returns_429_when_queue_full() {
        let dir = TempDir::new().unwrap();

        let workspace = dir.path().to_owned();
        let workspace2 = workspace.join("ws2");
        std::fs::create_dir_all(&workspace2).unwrap();
        let registry2 = crate::registry::Registry::load_or_create(&workspace2).expect("registry");

        let graph_db2 = knot::db::graph::GraphDb::connect("bolt://localhost:9999", "neo4j", "bad")
            .await
            .expect("connect");
        let vector_db2 = knot::db::vector::VectorDb::connect("http://localhost:9999", "test", 384)
            .await
            .expect("connect");

        let (small_tx, mut small_rx) = tokio::sync::mpsc::channel::<crate::models::IndexJob>(1);
        let state2 = Arc::new(AppState {
            vector_db: Arc::new(vector_db2),
            graph_db: Arc::new(graph_db2),
            embedder: None,
            workspace_dir: workspace2.to_string_lossy().into(),
            registry: Arc::new(Mutex::new(registry2)),
            job_tx: small_tx,
            qdrant_url: "http://localhost:6334".into(),
            qdrant_collection: "knot_entities".into(),
            neo4j_uri: "bolt://localhost:7687".into(),
            neo4j_user: "neo4j".into(),
            neo4j_password: "secret".into(),
            embed_dim: 384,
            rayon_threads: None,
            batch_size: 64,
            ingest_concurrency: 4,
            start_time: std::time::Instant::now(),
        });

        let app = build_test_app(state2);

        let body = serde_json::json!({
            "url": "git@github.com:org/foo.git",
            "auth_type": "ssh"
        });

        // First registration: fills the queue (capacity=1)
        let resp1 = app
            .clone()
            .oneshot(
                Request::post("/api/repos")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp1.status(), StatusCode::ACCEPTED);

        let body2 = serde_json::json!({
            "url": "git@github.com:org/bar.git",
            "auth_type": "ssh"
        });

        // Second registration: queue is full → 429
        let resp2 = app
            .oneshot(
                Request::post("/api/repos")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&body2).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp2.status(), StatusCode::TOO_MANY_REQUESTS);

        // Drain the queue to avoid channel drop warnings
        let _ = small_rx.try_recv();
    }

    #[tokio::test]
    async fn test_search_missing_query_returns_400() {
        let dir = TempDir::new().unwrap();
        let (state, _job_rx) = create_test_state_with_tempdir(&dir).await;
        let app = build_test_app(state);

        let response = app
            .oneshot(
                Request::get("/api/repos/any-repo/search")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_callers_missing_entity_returns_400() {
        let dir = TempDir::new().unwrap();
        let (state, _job_rx) = create_test_state_with_tempdir(&dir).await;
        let app = build_test_app(state);

        let response = app
            .oneshot(
                Request::get("/api/repos/any-repo/callers")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_explore_missing_path_returns_400() {
        let dir = TempDir::new().unwrap();
        let (state, _job_rx) = create_test_state_with_tempdir(&dir).await;
        let app = build_test_app(state);

        let response = app
            .oneshot(
                Request::get("/api/repos/any-repo/explore")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_list_repos_returns_empty() {
        let dir = TempDir::new().unwrap();
        let (state, _job_rx) = create_test_state_with_tempdir(&dir).await;
        let app = build_test_app(state);

        let response = app
            .oneshot(Request::get("/api/repos").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body_bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        assert!(json["repositories"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_graph_invalid_relationship_returns_400() {
        let dir = TempDir::new().unwrap();
        let (state, _job_rx) = create_test_state_with_tempdir(&dir).await;
        let app = build_test_app(state);

        let body = serde_json::json!({
            "url": "git@github.com:org/repo.git",
            "auth_type": "ssh"
        });
        let res = app
            .clone()
            .oneshot(
                Request::post("/api/repos")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        let body_bytes = axum::body::to_bytes(res.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        let repo_id = json["id"].as_str().unwrap();

        let response = app
            .oneshot(
                Request::get(format!("/api/repos/{repo_id}/graph?relationships=INVALID"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_graph_invalid_kind_returns_400() {
        let dir = TempDir::new().unwrap();
        let (state, _job_rx) = create_test_state_with_tempdir(&dir).await;
        let app = build_test_app(state);

        let body = serde_json::json!({
            "url": "git@github.com:org/repo.git",
            "auth_type": "ssh"
        });
        let res = app
            .clone()
            .oneshot(
                Request::post("/api/repos")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        let body_bytes = axum::body::to_bytes(res.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        let repo_id = json["id"].as_str().unwrap();

        let response = app
            .oneshot(
                Request::get(format!("/api/repos/{repo_id}/graph?kinds=INVALID"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_graph_expand_invalid_kind_returns_400() {
        let dir = TempDir::new().unwrap();
        let (state, _job_rx) = create_test_state_with_tempdir(&dir).await;
        let app = build_test_app(state);

        let body = serde_json::json!({
            "url": "git@github.com:org/repo.git",
            "auth_type": "ssh"
        });
        let res = app
            .clone()
            .oneshot(
                Request::post("/api/repos")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        let body_bytes = axum::body::to_bytes(res.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        let repo_id = json["id"].as_str().unwrap();

        let response = app
            .oneshot(
                Request::get(format!(
                    "/api/repos/{repo_id}/graph/expand?entity=test&kinds=INVALID"
                ))
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_graph_subgraph_invalid_kind_returns_400() {
        let dir = TempDir::new().unwrap();
        let (state, _job_rx) = create_test_state_with_tempdir(&dir).await;
        let app = build_test_app(state);

        let body = serde_json::json!({
            "url": "git@github.com:org/repo.git",
            "auth_type": "ssh"
        });
        let res = app
            .clone()
            .oneshot(
                Request::post("/api/repos")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        let body_bytes = axum::body::to_bytes(res.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        let repo_id = json["id"].as_str().unwrap();

        let response = app
            .oneshot(
                Request::get(format!(
                    "/api/repos/{repo_id}/graph?entity=some_func&kinds=INVALID"
                ))
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn test_parse_kinds_empty_uses_default() {
        let result = parse_kinds("").unwrap();
        assert!(!result.is_empty());
        assert!(result.contains(&"class"));
        assert!(result.contains(&"interface"));
        assert!(result.contains(&"rust_struct"));
        assert!(result.contains(&"rust_trait"));
        assert!(!result.contains(&"function"));
    }

    #[test]
    fn test_parse_kinds_invalid_returns_error() {
        let result = parse_kinds("INVALID");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Invalid kind category"));
    }

    #[test]
    fn test_parse_kinds_classes_expands() {
        let result = parse_kinds("classes").unwrap();
        assert!(!result.is_empty());
        for k in KIND_CATEGORY_CLASSES {
            assert!(result.contains(k), "missing kind: {}", k);
        }
    }

    #[test]
    fn test_parse_kinds_interfaces_expands() {
        let result = parse_kinds("interfaces").unwrap();
        assert!(!result.is_empty());
        for k in KIND_CATEGORY_INTERFACES {
            assert!(result.contains(k), "missing kind: {}", k);
        }
    }

    #[test]
    fn test_parse_kinds_functions_expands() {
        let result = parse_kinds("functions").unwrap();
        assert!(!result.is_empty());
        for k in KIND_CATEGORY_FUNCTIONS {
            assert!(result.contains(k), "missing kind: {}", k);
        }
    }

    #[test]
    fn test_parse_kinds_other_returns_empty_visible() {
        let result = parse_kinds("other").unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_parse_kinds_multiple_categories() {
        let result = parse_kinds("classes,interfaces,other").unwrap();
        assert!(result.contains(&"class"));
        assert!(result.contains(&"interface"));
        assert!(!result.contains(&"function"));
    }

    #[test]
    fn test_includes_other_empty_default() {
        assert!(!includes_other(""));
    }

    #[test]
    fn test_includes_other_explicit() {
        assert!(includes_other("other"));
    }

    #[test]
    fn test_includes_other_not_present() {
        assert!(!includes_other("classes"));
        assert!(!includes_other("classes,interfaces"));
        assert!(!includes_other("functions"));
    }

    #[test]
    fn test_includes_other_mixed() {
        assert!(includes_other("classes,other"));
        assert!(includes_other("classes,interfaces,other"));
        assert!(includes_other("other,functions"));
    }
}
