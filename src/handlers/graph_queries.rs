use anyhow::Context;
use neo4rs::{Graph, query};
use std::collections::HashSet;

use crate::handlers::models::*;
use crate::models::AppState;

/// Neo4j query shape shared by the node, edge and overview queries.
///
/// `visible_set` is a `HashSet` view over `visible_kinds`, kept alongside it so
/// the per-row membership checks in `fetch_nodes`/`fetch_edges` stay O(1)
/// without rebuilding the set per call.
pub struct GraphQuerySpec<'a> {
    pub repo_id: &'a str,
    pub depth: u32,
    pub rel_filter: &'a str,
    pub visible_kinds: &'a [&'a str],
    pub visible_set: &'a HashSet<&'a str>,
    pub include_other: bool,
}

/// Builds the overview node query: roots (no incoming CONTAINS) expanded by the
/// user-selected relationship filter up to `depth`. `$repo_name` stays a bound
/// parameter — never interpolated.
fn build_overview_node_query(rel_filter: &str, depth: u32) -> String {
    format!(
        "MATCH (root:Entity {{repo_name: $repo_name}})
         WHERE NOT ()-[:CONTAINS]->(root)
         MATCH (root)-[:{rel_filter}*0..{depth}]->(e:Entity)
         RETURN DISTINCT e.uuid, e.name, e.kind, e.fqn, e.signature, e.file_path, e.start_line",
        rel_filter = rel_filter,
        depth = depth,
    )
}

/// One-hop query for entities that are contained by any other entity — the
/// nested declarations the root closure cannot reach.
fn build_nested_node_query() -> &'static str {
    "MATCH ()-[:CONTAINS]->(e:Entity {repo_name: $repo_name})
     RETURN DISTINCT e.uuid, e.name, e.kind, e.fqn, e.signature, e.file_path, e.start_line"
}

/// Fetches graph nodes from Neo4j starting from the root repository node.
pub async fn fetch_nodes(
    graph: &Graph,
    spec: &GraphQuerySpec<'_>,
) -> anyhow::Result<Vec<GraphNodeResponse>> {
    let node_q_str = build_overview_node_query(spec.rel_filter, spec.depth);
    let node_q = query(&node_q_str).param("repo_name", spec.repo_id);

    let mut rows = graph
        .execute(node_q)
        .await
        .context("Neo4j node query failed")?;

    let nested_q_str = build_nested_node_query();
    let nested_q = query(nested_q_str).param("repo_name", spec.repo_id);

    let mut nested_rows = graph
        .execute(nested_q)
        .await
        .context("Neo4j nested-node query failed")?;

    let mut nodes = Vec::new();
    let mut seen_uuids = HashSet::new();

    let mut process_row = |row: neo4rs::Row| {
        let uuid = row.get::<String>("e.uuid").unwrap_or_default();
        let name = row.get::<String>("e.name").unwrap_or_default();
        if uuid.is_empty() || name.is_empty() {
            return;
        }
        if !seen_uuids.insert(uuid.clone()) {
            return;
        }
        let kind: Option<String> = row.get::<String>("e.kind").ok();
        if let Some(ref k) = kind
            && !spec.include_other
            && !spec.visible_set.contains(k.as_str())
        {
            return;
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
    };

    while let Ok(Some(row)) = rows.next().await {
        process_row(row);
    }
    while let Ok(Some(row)) = nested_rows.next().await {
        process_row(row);
    }

    Ok(nodes)
}

/// Fetches graph edges from Neo4j between the identified node UUIDs.
pub async fn fetch_edges(
    graph: &Graph,
    spec: &GraphQuerySpec<'_>,
    node_uuids: &HashSet<&str>,
) -> anyhow::Result<Vec<GraphEdgeResponse>> {
    let edge_q = if spec.visible_set.is_empty() && !spec.include_other {
        String::from("RETURN DISTINCT '' AS source, '' AS target, '' AS rel LIMIT 0")
    } else {
        let visible_list = spec
            .visible_kinds
            .iter()
            .map(|k| format!("'{}'", k))
            .collect::<Vec<_>>()
            .join(", ");

        if spec.include_other {
            format!(
                "MATCH (a:Entity {{repo_name: $repo_name}})-[r:{rel_filter}]->(b:Entity {{repo_name: $repo_name}})
                 RETURN DISTINCT a.uuid AS source, b.uuid AS target, type(r) AS rel",
                rel_filter = spec.rel_filter,
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
                 RETURN DISTINCT c1.uuid AS source, c2.uuid AS target, type(r) AS rel",
                rel_filter = spec.rel_filter,
            )
        }
    };

    let edge_q = query(&edge_q).param("repo_name", spec.repo_id);
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

    Ok(edges)
}

/// Fetches all nodes and edges for the overview graph.
pub async fn fetch_all_entities(
    state: &AppState,
    spec: &GraphQuerySpec<'_>,
) -> anyhow::Result<GraphResponse> {
    let graph = Graph::new(&state.neo4j_uri, &state.neo4j_user, &state.neo4j_password)
        .context("Failed to connect to Neo4j")?;

    let nodes = fetch_nodes(&graph, spec).await?;
    let node_uuids: HashSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
    let total = nodes.len();

    let edges = fetch_edges(&graph, spec, &node_uuids).await?;

    Ok(GraphResponse {
        root_id: None,
        nodes,
        edges,
        truncated: false,
        total_nodes_found: total,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_overview_node_query_contains_root_predicate() {
        let q = build_overview_node_query("CALLS", 2);
        assert!(q.contains("WHERE NOT ()-[:CONTAINS]->(root)"));
    }

    #[test]
    fn test_build_overview_node_query_interpolates_rel_filter_and_depth() {
        let q = build_overview_node_query("CALLS|EXTENDS", 3);
        assert!(q.contains("-[:CALLS|EXTENDS*0..3]->"));
        // Assert $repo_name is not interpolated (stays a parameter)
        assert!(q.contains("{repo_name: $repo_name}"));
        assert!(!q.contains("{repo_name: \""));
    }

    #[test]
    fn test_build_overview_node_query_no_limit() {
        let q = build_overview_node_query("CALLS", 2);
        assert!(!q.to_uppercase().contains("LIMIT"));
    }

    #[test]
    fn test_build_nested_node_query_is_one_hop() {
        let q = build_nested_node_query();
        assert!(q.contains("MATCH ()-[:CONTAINS]->(e:Entity {repo_name: $repo_name})"));
        assert!(!q.contains("*0.."));
        assert!(!q.contains("*1.."));
        // Projection columns check
        assert!(q.contains(
            "RETURN DISTINCT e.uuid, e.name, e.kind, e.fqn, e.signature, e.file_path, e.start_line"
        ));
    }
}
