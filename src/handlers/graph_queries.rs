use anyhow::Context;
use neo4rs::{Graph, query};
use std::collections::HashSet;

use crate::handlers::models::*;
use crate::models::AppState;

/// Fetches graph nodes from Neo4j starting from the root repository node.
pub async fn fetch_nodes(
    graph: &Graph,
    repo_id: &str,
    depth: u32,
    rel_filter: &str,
    visible_set: &HashSet<&str>,
    include_other: bool,
) -> anyhow::Result<Vec<GraphNodeResponse>> {
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

    Ok(nodes)
}

/// Fetches graph edges from Neo4j between the identified node UUIDs.
pub async fn fetch_edges(
    graph: &Graph,
    repo_id: &str,
    rel_filter: &str,
    visible_kinds: &[&str],
    visible_set: &HashSet<&str>,
    include_other: bool,
    node_uuids: &HashSet<&str>,
) -> anyhow::Result<Vec<GraphEdgeResponse>> {
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

    Ok(edges)
}

/// Fetches all nodes and edges for the overview graph.
pub async fn fetch_all_entities(
    state: &AppState,
    repo_id: &str,
    depth: u32,
    relationships: &[&str],
    visible_kinds: &[&str],
    include_other: bool,
) -> anyhow::Result<GraphResponse> {
    let graph = Graph::new(&state.neo4j_uri, &state.neo4j_user, &state.neo4j_password)
        .context("Failed to connect to Neo4j")?;

    let rel_filter = relationships.join("|");
    let visible_set: HashSet<&str> = visible_kinds.iter().copied().collect();

    let nodes = fetch_nodes(
        &graph,
        repo_id,
        depth,
        &rel_filter,
        &visible_set,
        include_other,
    )
    .await?;
    let node_uuids: HashSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
    let total = nodes.len();

    let edges = fetch_edges(
        &graph,
        repo_id,
        &rel_filter,
        visible_kinds,
        &visible_set,
        include_other,
        &node_uuids,
    )
    .await?;

    Ok(GraphResponse {
        root_id: None,
        nodes,
        edges,
        truncated: false,
        total_nodes_found: total,
    })
}
