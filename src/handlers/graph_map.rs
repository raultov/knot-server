use crate::handlers::models::*;

/// Maps a knot Neo4j subgraph node to a GraphNodeResponse.
pub fn map_node(n: knot::models::SubgraphNode) -> GraphNodeResponse {
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
}

/// Maps a knot Neo4j subgraph edge to a GraphEdgeResponse.
pub fn map_edge(e: knot::models::SubgraphEdge) -> GraphEdgeResponse {
    GraphEdgeResponse {
        source: e.source_uuid,
        target: e.target_uuid,
        edge_type: e.relationship,
    }
}

/// Converts a knot Neo4j SubgraphResult into a GraphResponse.
pub fn subgraph_to_response(result: knot::models::SubgraphResult) -> GraphResponse {
    GraphResponse {
        root_id: result.root_id,
        nodes: result.nodes.into_iter().map(map_node).collect(),
        edges: result
            .edges
            .into_iter()
            .map(map_edge)
            .filter(|e| e.source != e.target)
            .collect(),
        truncated: result.truncated,
        total_nodes_found: result.total_nodes_found,
    }
}
