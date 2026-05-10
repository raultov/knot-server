use std::sync::Arc;

use knot::db::graph::{DeleteExt, GraphDb};
use knot::db::vector::{VectorDb, VectorDeleteExt};

/// Delete all data for a repository from both Neo4j and Qdrant.
pub async fn delete_repo_from_databases(
    repo_id: &str,
    graph_db: &Arc<GraphDb>,
    vector_db: &Arc<VectorDb>,
) {
    if let Err(e) = graph_db.delete_by_repo(repo_id).await {
        tracing::error!("Failed to delete Neo4j entities for '{}': {e}", repo_id);
    }
    if let Err(e) = graph_db.delete_repository(repo_id).await {
        tracing::error!(
            "Failed to delete Neo4j Repository node for '{}': {e}",
            repo_id
        );
    }
    if let Err(e) = vector_db.delete_by_repo(repo_id).await {
        tracing::error!("Failed to delete Qdrant vectors for '{}': {e}", repo_id);
    }
}
