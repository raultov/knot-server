use anyhow::Context;
use neo4rs::query;

use crate::models::AppState;

/// Resolves a UUID to an entity name by querying Neo4j.
pub async fn resolve_uuid_to_name(
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
