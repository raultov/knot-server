//! Startup model-consistency guard — thin wiring around knot's guard ladder.
//!
//! The six classification rules, the closed-set dimension→model inference and
//! their unit tests all live upstream in `knot::startup_guard` (`classify_startup`);
//! nothing is re-derived here. This module only supplies the inputs:
//! `probe_collection_dim` for the collection's real vector size, and
//! `repo_embed_markers` for the per-repository embedding markers persisted on
//! the Neo4j `:Repository` nodes.
//!
//! D-S2 of the plan: knot's `verify_startup` wire-up takes a
//! `&knot::config::Config`, whose `repo_path` / `repo_name` are meaningless at
//! server scope (knot-server builds a per-repo `Config` in the worker), so
//! knot-server wires the inputs itself — the *decision logic stays upstream*.

use anyhow::Result;
use knot::db::graph::{ConnectExt as _, GraphDb, RepoQueryExt as _};
use knot::db::vector::probe_collection_dim;
use knot::pipeline::embed::EmbedModelChoice;
use knot::startup_guard::{GuardContext, GuardHints, StartupVerdict, classify_startup_with_hints};

/// H1: knot's guard ladder is shared, but its remediation advice must speak
/// knot-server's language. knot's own binaries read `KNOT_QDRANT_COLLECTION`
/// and re-index with `knot-indexer --clean`; a server operator reads
/// `KNOT_SERVER_QDRANT_COLLECTION` and re-indexes through the REST API. The
/// model variable is shared (`KNOT_EMBED_MODEL` drives both).
const SERVER_GUARD_HINTS: GuardHints<'static> = GuardHints {
    collection_var: "KNOT_SERVER_QDRANT_COLLECTION",
    embed_model_var: "KNOT_EMBED_MODEL",
    reindex_cmd: "`POST /api/repos/{id}/sync`",
};

/// Run the guard ladder for the server's collection.
///
/// * `Ok` → proceed silently.
/// * `WarnPartial` → `tracing::warn!` naming every invisible repository.
/// * `Abort` → return `Err` so `main` exits non-zero.
///
/// Failures reading the marker data degrade to an empty marker list — the
/// guard protects against silent model mixing, not against reachable
/// databases going away (the normal connection path reports those).
pub async fn verify_server_startup(
    qdrant_url: &str,
    collection: &str,
    neo4j: (&str, &str, &str),
    embed_model: &str,
    choice: &EmbedModelChoice,
) -> Result<()> {
    let collection_dim = match probe_collection_dim(qdrant_url, collection).await {
        Ok(dim) => dim,
        Err(e) => {
            tracing::warn!(
                "Embed-model guard skipped: could not probe collection '{collection}' ({e})"
            );
            return Ok(());
        }
    };

    let (uri, user, password) = neo4j;
    let markers = match GraphDb::connect(uri, user, password).await {
        Ok(db) => db.repo_embed_markers(&[]).await.unwrap_or_else(|e| {
            tracing::warn!("Embed markers unreadable ({e}) — model check skipped");
            Vec::new()
        }),
        Err(e) => {
            tracing::warn!("Neo4j unreachable for the embed-marker check ({e}) — skipped");
            Vec::new()
        }
    };

    let ctx = GuardContext {
        configured: choice,
        configured_name: embed_model,
        collection,
        collection_dim,
    };

    match classify_startup_with_hints(ctx, &markers, &SERVER_GUARD_HINTS) {
        StartupVerdict::Ok => Ok(()),
        StartupVerdict::WarnPartial {
            invisible,
            their_model,
        } => {
            tracing::warn!(
                "Repositories indexed with embedding model '{their_model}' exist in the graph \
                 but are invisible to semantic search from '{collection}': {}. \
                 Re-index them (`POST /api/repos/{{id}}/sync`) with '{embed_model}' to include them.",
                invisible.join(", ")
            );
            Ok(())
        }
        StartupVerdict::Abort(msg) => Err(anyhow::anyhow!(msg)),
    }
}
