use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use neo4rs::{Graph, query};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;
use utoipa::ToSchema;

use crate::handlers::models::{
    ErrorResponse, GraphEdgeResponse, RepoGraphNode, RepoGraphParams, RepoGraphResponse,
    RepoRelation, error_response,
};
use crate::models::AppState;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum RepoDirection {
    Outgoing,
    Incoming,
    Both,
}

pub fn parse_repo_direction(direction: &str) -> Result<RepoDirection, String> {
    match direction.to_lowercase().as_str() {
        "outgoing" => Ok(RepoDirection::Outgoing),
        "incoming" => Ok(RepoDirection::Incoming),
        "both" => Ok(RepoDirection::Both),
        _ => Err(format!(
            "Invalid direction '{}'. Valid values: outgoing, incoming, both",
            direction
        )),
    }
}

pub fn clamp_repo_depth(depth: Option<u32>) -> u32 {
    depth.unwrap_or(3).clamp(1, 5)
}

pub fn build_repo_node_query(direction: RepoDirection, depth: u32) -> String {
    let select_part = "RETURN DISTINCT d.name AS name, d.build_system AS build_system, \
                       d.group_id AS group_id, d.artifact_id AS artifact_id, d.version AS version";
    match direction {
        RepoDirection::Outgoing => format!(
            "MATCH (root:Repository {{name: $repo_name}})-[:DEPENDS_ON*1..{}]->(d:Repository) {}",
            depth, select_part
        ),
        RepoDirection::Incoming => format!(
            "MATCH (d:Repository)-[:DEPENDS_ON*1..{}]->(root:Repository {{name: $repo_name}}) {}",
            depth, select_part
        ),
        RepoDirection::Both => format!(
            "MATCH (root:Repository {{name: $repo_name}})-[:DEPENDS_ON*1..{}]->(d:Repository) {} \
             UNION \
             MATCH (d:Repository)-[:DEPENDS_ON*1..{}]->(root:Repository {{name: $repo_name}}) {}",
            depth, select_part, depth, select_part
        ),
    }
}

pub fn build_repo_edge_query() -> &'static str {
    "MATCH (a:Repository)-[:DEPENDS_ON]->(b:Repository) \
     WHERE a.name IN $names AND b.name IN $names \
     RETURN DISTINCT a.name AS source, b.name AS target"
}

pub struct RawRepoNode {
    pub name: String,
    pub build_system: Option<String>,
    pub group_id: Option<String>,
    pub artifact_id: Option<String>,
    pub version: Option<String>,
}

pub fn map_repo_graph(
    root: Option<RawRepoNode>,
    outgoing_deps: Vec<RawRepoNode>,
    incoming_deps: Vec<RawRepoNode>,
    registered_ids: &HashSet<String>,
    edges: Vec<(String, String)>,
) -> RepoGraphResponse {
    let root_name = root.as_ref().map(|r| r.name.clone());
    let mut nodes = Vec::new();
    let mut seen = HashSet::new();

    if let Some(r) = root {
        let is_reg = registered_ids.contains(&r.name);
        seen.insert(r.name.clone());
        nodes.push(RepoGraphNode {
            id: r.name.clone(),
            name: r.name.clone(),
            build_system: r.build_system,
            group_id: r.group_id,
            artifact_id: r.artifact_id,
            version: r.version,
            is_root: true,
            registered: is_reg,
            relation: RepoRelation::Root,
        });
    }

    let mut push_deps = |deps: Vec<RawRepoNode>, relation: RepoRelation| {
        for dep in deps {
            if seen.insert(dep.name.clone()) {
                let is_reg = registered_ids.contains(&dep.name);
                nodes.push(RepoGraphNode {
                    id: dep.name.clone(),
                    name: dep.name.clone(),
                    build_system: dep.build_system,
                    group_id: dep.group_id,
                    artifact_id: dep.artifact_id,
                    version: dep.version,
                    is_root: false,
                    registered: is_reg,
                    relation,
                });
            }
        }
    };

    push_deps(outgoing_deps, RepoRelation::Dependency);
    push_deps(incoming_deps, RepoRelation::Dependent);

    let edge_responses = edges
        .into_iter()
        .map(|(src, tgt)| GraphEdgeResponse {
            source: src,
            target: tgt,
            edge_type: "DEPENDS_ON".to_string(),
        })
        .collect();

    RepoGraphResponse {
        root_id: root_name,
        nodes,
        edges: edge_responses,
        total_nodes_found: seen.len(),
    }
}

#[utoipa::path(
    get,
    path = "/api/repos/{id}/graph/repos",
    tag = "Graph",
    params(
        ("id" = String, Path, description = "Repository ID"),
        RepoGraphParams,
    ),
    responses(
        (status = 200, description = "Repository dependency graph results", body = RepoGraphResponse),
        (status = 400, description = "Invalid query parameter", body = ErrorResponse),
        (status = 404, description = "Repository not found", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse),
    ),
    description = "Query repository-level dependency graph (DEPENDS_ON relations).",
)]
#[tracing::instrument(
    name = "repo_graph",
    skip_all,
    fields(repo_id = %id, depth = ?params.depth, direction = ?params.direction)
)]
#[expect(clippy::too_many_lines, reason = "deferred refactoring")]
pub async fn repo_graph_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(params): Query<RepoGraphParams>,
) -> Result<Response, Response> {
    // 1. Verify repository exists in registry
    let registered_ids = {
        let mut registry = state.registry.lock().unwrap();
        if registry.get(&id).is_none() {
            return Err(error_response(
                StatusCode::NOT_FOUND,
                format!("Repository '{}' not found", id),
            ));
        }
        registry
            .list()
            .iter()
            .map(|r| r.id.clone())
            .collect::<HashSet<_>>()
    };

    // 2. Parse and clamp params
    let depth = clamp_repo_depth(params.depth);
    let direction_str = params.direction.as_deref().unwrap_or("both");
    let direction = match parse_repo_direction(direction_str) {
        Ok(d) => d,
        Err(msg) => return Err(error_response(StatusCode::BAD_REQUEST, msg)),
    };

    // 3. Query Neo4j
    let graph =
        Graph::new(&state.neo4j_uri, &state.neo4j_user, &state.neo4j_password).map_err(|e| {
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to connect to Neo4j: {e}"),
            )
        })?;

    // Query root node
    let root_query = query("MATCH (r:Repository {name: $repo_name}) \
                            RETURN r.name AS name, r.build_system AS build_system, \
                                   r.group_id AS group_id, r.artifact_id AS artifact_id, r.version AS version")
        .param("repo_name", id.as_str());

    let mut root_rows = graph.execute(root_query).await.map_err(|e| {
        error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Neo4j query failed: {e}"),
        )
    })?;

    let root_node = if let Ok(Some(row)) = root_rows.next().await {
        Some(RawRepoNode {
            name: row.get::<String>("name").unwrap_or_default(),
            build_system: row.get::<String>("build_system").ok(),
            group_id: row.get::<String>("group_id").ok(),
            artifact_id: row.get::<String>("artifact_id").ok(),
            version: row.get::<String>("version").ok(),
        })
    } else {
        None
    };

    // If root does not exist in Neo4j, return empty graph
    if root_node.is_none() {
        return Ok((
            StatusCode::OK,
            Json(RepoGraphResponse {
                root_id: None,
                nodes: Vec::new(),
                edges: Vec::new(),
                total_nodes_found: 0,
            }),
        )
            .into_response());
    }

    // Query neighbors
    let neighbors_query_str = build_repo_node_query(direction, depth);
    let neighbors_query = query(&neighbors_query_str).param("repo_name", id.as_str());
    let mut neighbor_rows = graph.execute(neighbors_query).await.map_err(|e| {
        error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Neo4j neighbor query failed: {e}"),
        )
    })?;

    let mut outgoing_deps = Vec::new();
    let mut incoming_deps = Vec::new();

    while let Ok(Some(row)) = neighbor_rows.next().await {
        let name = row.get::<String>("name").unwrap_or_default();
        if name.is_empty() {
            continue;
        }
        let node = RawRepoNode {
            name,
            build_system: row.get::<String>("build_system").ok(),
            group_id: row.get::<String>("group_id").ok(),
            artifact_id: row.get::<String>("artifact_id").ok(),
            version: row.get::<String>("version").ok(),
        };

        // For outgoing (or incoming), categorize by query type or fallback
        match direction {
            RepoDirection::Outgoing => outgoing_deps.push(node),
            RepoDirection::Incoming => incoming_deps.push(node),
            RepoDirection::Both => {
                // If both, let's query relationship direction or place in outgoing as default
                outgoing_deps.push(node);
            }
        }
    }

    // Identify all node names for edge query
    let mut names = vec![id.clone()];
    for dep in &outgoing_deps {
        names.push(dep.name.clone());
    }
    for dep in &incoming_deps {
        names.push(dep.name.clone());
    }

    let mut edges = Vec::new();
    if names.len() > 1 {
        let edge_query_str = build_repo_edge_query();
        let edge_query = query(edge_query_str).param("names", names);
        let mut edge_rows = graph.execute(edge_query).await.map_err(|e| {
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Neo4j edge query failed: {e}"),
            )
        })?;
        while let Ok(Some(row)) = edge_rows.next().await {
            if let (Ok(source), Ok(target)) =
                (row.get::<String>("source"), row.get::<String>("target"))
            {
                edges.push((source, target));
            }
        }
    }

    let response = map_repo_graph(
        root_node,
        outgoing_deps,
        incoming_deps,
        &registered_ids,
        edges,
    );
    Ok((StatusCode::OK, Json(response)).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_repo_direction() {
        assert_eq!(
            parse_repo_direction("outgoing").unwrap(),
            RepoDirection::Outgoing
        );
        assert_eq!(
            parse_repo_direction("INCOMING").unwrap(),
            RepoDirection::Incoming
        );
        assert_eq!(parse_repo_direction("Both").unwrap(), RepoDirection::Both);
        assert!(parse_repo_direction("sideways").is_err());
    }

    #[test]
    fn test_clamp_repo_depth() {
        assert_eq!(clamp_repo_depth(None), 3);
        assert_eq!(clamp_repo_depth(Some(0)), 1);
        assert_eq!(clamp_repo_depth(Some(2)), 2);
        assert_eq!(clamp_repo_depth(Some(6)), 5);
    }

    #[test]
    fn test_build_repo_node_query() {
        let q_out = build_repo_node_query(RepoDirection::Outgoing, 3);
        assert!(q_out.contains("-[:DEPENDS_ON*1..3]->(d:Repository)"));
        assert!(q_out.contains("(root:Repository {name: $repo_name})"));

        let q_in = build_repo_node_query(RepoDirection::Incoming, 5);
        assert!(q_in.contains("(d:Repository)-[:DEPENDS_ON*1..5]->"));

        let q_both = build_repo_node_query(RepoDirection::Both, 2);
        assert!(q_both.contains("UNION"));
    }

    #[test]
    fn test_map_repo_graph() {
        let root = Some(RawRepoNode {
            name: "app".to_string(),
            build_system: Some("cargo".to_string()),
            group_id: None,
            artifact_id: Some("app".to_string()),
            version: Some("1.0.0".to_string()),
        });
        let outgoing = vec![RawRepoNode {
            name: "lib1".to_string(),
            build_system: Some("cargo".to_string()),
            group_id: None,
            artifact_id: Some("lib1".to_string()),
            version: Some("0.1.0".to_string()),
        }];
        let incoming = vec![RawRepoNode {
            name: "dependent-service".to_string(),
            build_system: Some("cargo".to_string()),
            group_id: None,
            artifact_id: Some("dependent-service".to_string()),
            version: Some("2.0.0".to_string()),
        }];
        let mut registered = HashSet::new();
        registered.insert("app".to_string());
        registered.insert("lib1".to_string());

        let edges = vec![("app".to_string(), "lib1".to_string())];

        let result = map_repo_graph(root, outgoing, incoming, &registered, edges);
        assert_eq!(result.root_id.unwrap(), "app");
        assert_eq!(result.total_nodes_found, 3);
        assert_eq!(result.nodes[0].name, "app");
        assert_eq!(result.nodes[0].relation, RepoRelation::Root);
        assert!(result.nodes[0].registered);
        assert_eq!(result.nodes[1].name, "lib1");
        assert_eq!(result.nodes[1].relation, RepoRelation::Dependency);
        assert!(result.nodes[1].registered);
        assert_eq!(result.nodes[2].name, "dependent-service");
        assert_eq!(result.nodes[2].relation, RepoRelation::Dependent);
        assert!(!result.nodes[2].registered);
        assert_eq!(result.edges[0].source, "app");
        assert_eq!(result.edges[0].target, "lib1");
        assert_eq!(result.edges[0].edge_type, "DEPENDS_ON");
    }
}
