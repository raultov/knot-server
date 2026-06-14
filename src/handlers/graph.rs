use anyhow::Context;
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use neo4rs::query;
use std::sync::Arc;

use crate::handlers::models::*;
use crate::state::AppState;

// ── Read endpoints ──────────────────────────────────────────────

// ── Graph (subgraph) endpoints ───────────────────────────────────

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

/// All valid kind category names accepted by the `kinds` query parameter.
const VALID_KIND_CATEGORIES: &[&str] = &["classes", "interfaces", "functions", "other"];

/// Default visible kinds for the overview graph.
const DEFAULT_VISIBLE_KINDS: &str = "classes,interfaces";

/// Parse the `kinds` query parameter into a flat list of entity kind strings.
///
/// Returns the list of kind strings that should be visible, or an error if any
/// category name is invalid.
pub fn parse_kinds(kinds: &str) -> Result<Vec<&str>, String> {
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
pub fn includes_other(kinds: &str) -> bool {
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

pub fn parse_direction(direction: &str) -> knot::models::SubgraphDirection {
    match direction {
        "incoming" => knot::models::SubgraphDirection::Incoming,
        "outgoing" => knot::models::SubgraphDirection::Outgoing,
        _ => knot::models::SubgraphDirection::Both,
    }
}

pub fn parse_relationships(relationships: &str) -> Result<Vec<&str>, String> {
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

// ── Repository management endpoints ─────────────────────────────

// ── Trigger endpoints ───────────────────────────────────────────

// ── Webhook endpoint ─────────────────────────────────────────────

// ── Health endpoint ──────────────────────────────────────────────
