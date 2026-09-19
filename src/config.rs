use clap::parser::ValueSource;
use clap::{CommandFactory, FromArgMatches, Parser};

/// Base Qdrant collection name. The default embedding model uses it verbatim;
/// a non-default model derives a suffixed name from it via
/// [`knot::pipeline::embed::EmbedModelChoice::default_collection`].
pub const DEFAULT_COLLECTION: &str = "knot_entities";

#[derive(Debug, Parser)]
#[command(
    name = "knot-server",
    version,
    about = "Distributed REST API server for knot codebase indexing"
)]
pub struct ServerConfig {
    #[arg(long, env = "KNOT_SERVER_PORT", default_value_t = 3000)]
    pub port: u16,

    #[arg(long, env = "KNOT_SERVER_BIND_ADDR", default_value = "0.0.0.0")]
    pub bind_addr: String,

    #[arg(
        long,
        env = "KNOT_SERVER_QDRANT_URL",
        default_value = "http://localhost:6334"
    )]
    pub qdrant_url: String,

    /// Default `knot_entities`; a different embedding model derives a suffixed
    /// collection automatically (`ServerConfig::resolved_collection`). An
    /// explicitly supplied value always wins.
    #[arg(
        long,
        env = "KNOT_SERVER_QDRANT_COLLECTION",
        default_value = DEFAULT_COLLECTION
    )]
    pub qdrant_collection: String,

    /// Whether `qdrant_collection` was supplied explicitly (command line or
    /// environment). Not a CLI argument: `from_env` fills it from clap's
    /// `ValueSource`, because clap materializes the default value into the
    /// `String` above and thus erases the distinction.
    #[arg(skip)]
    pub qdrant_collection_explicit: bool,

    #[arg(
        long,
        env = "KNOT_SERVER_NEO4J_URI",
        default_value = "bolt://localhost:7687"
    )]
    pub neo4j_uri: String,

    #[arg(long, env = "KNOT_SERVER_NEO4J_USER", default_value = "neo4j")]
    pub neo4j_user: String,

    #[arg(long, env = "KNOT_NEO4J_PASSWORD")]
    pub neo4j_password: String,

    #[arg(
        long,
        env = "KNOT_WORKSPACE_DIR",
        default_value = "/var/lib/knot/repos"
    )]
    pub workspace_dir: String,

    /// Deprecated: the dimension is derived from `KNOT_EMBED_MODEL`.
    /// Hidden and optional; `resolve_embed_dim` validates it.
    #[arg(long, env = "KNOT_SERVER_EMBED_DIM", hide = true)]
    pub embed_dim: Option<u64>,

    #[arg(long, env = "KNOT_SERVER_RAYON_THREADS")]
    pub rayon_threads: Option<usize>,

    #[arg(long, env = "KNOT_SERVER_BATCH_SIZE", default_value_t = 128)]
    pub batch_size: usize,

    #[arg(long, env = "KNOT_SERVER_INGEST_CONCURRENCY", default_value_t = 4)]
    pub ingest_concurrency: usize,

    #[arg(long, env = "KNOT_SERVER_POLL_INTERVAL_SECS", default_value_t = 86400)]
    pub poll_interval_secs: u64,

    #[arg(
        long,
        env = "KNOT_SERVER_STALE_LOCK_TIMEOUT_SECS",
        default_value_t = 3600
    )]
    pub stale_lock_timeout_secs: u64,

    #[arg(long, env = "KNOT_SERVER_MAX_INDEX_AGE_SECS", default_value_t = 86400)]
    pub max_index_age_secs: u64,

    #[arg(long, env = "KNOT_SERVER_QUEUE_CAPACITY", default_value_t = 16)]
    pub queue_capacity: usize,

    #[arg(long, env = "KNOT_SERVER_METRICS_ENABLED", default_value_t = true)]
    pub metrics_enabled: bool,

    #[arg(
        long,
        env = "KNOT_SERVER_MCP_ENABLED",
        default_value_t = true,
        default_missing_value = "true",
        action = clap::ArgAction::Set,
        num_args = 0..=1,
        require_equals = false,
        help = "Serve the knot MCP tool surface at /mcp (stateless JSON-RPC over HTTP)"
    )]
    pub mcp_enabled: bool,

    #[arg(long, env = "KNOT_SERVER_TRACING_ENABLED", default_value_t = false)]
    pub tracing_enabled: bool,

    #[arg(
        long,
        env = "KNOT_SERVER_OTLP_ENDPOINT",
        default_value = "http://localhost:4317"
    )]
    pub otlp_endpoint: String,

    #[arg(long, env = "KNOT_SERVER_TRACE_SAMPLE_RATIO", default_value_t = 1.0)]
    pub trace_sample_ratio: f64,
}

impl ServerConfig {
    pub fn from_env() -> Self {
        // `Self::parse()` loses whether an argument was explicitly supplied:
        // `get_matches` + `from_arg_matches` keeps the `ValueSource`, which
        // `resolved_collection` needs to distinguish "not set" from
        // "explicitly set to the base name".
        let matches = Self::command().get_matches();
        let mut cfg =
            Self::from_arg_matches(&matches).expect("clap-validated arguments must re-parse");
        cfg.qdrant_collection_explicit =
            collection_explicit(matches.value_source("qdrant_collection"));
        cfg
    }

    /// The effective Qdrant collection for `choice`: an explicitly supplied
    /// value always wins; otherwise the model's suffix is applied to
    /// [`DEFAULT_COLLECTION`] (MiniLM keeps `knot_entities` byte-for-byte, BGE
    /// lands on `knot_entities_bge768`).
    ///
    /// Always go through this method rather than calling [`resolve_collection`]
    /// with `Some(self.qdrant_collection)`: the raw field is *always* `Some`
    /// because clap fills the default value, which would make the derivation
    /// dead code.
    pub fn resolved_collection(&self, choice: &knot::pipeline::embed::EmbedModelChoice) -> String {
        let supplied = self
            .qdrant_collection_explicit
            .then_some(self.qdrant_collection.as_str());
        resolve_collection(supplied, DEFAULT_COLLECTION, choice)
    }
}

/// Whether `qdrant_collection` was supplied explicitly. `EnvVariable` counts:
/// an operator who sets `KNOT_SERVER_QDRANT_COLLECTION` has made an explicit
/// choice, and treating that as "not set" would silently override it with a
/// derived name.
fn collection_explicit(source: Option<ValueSource>) -> bool {
    matches!(
        source,
        Some(ValueSource::CommandLine | ValueSource::EnvVariable)
    )
}

/// Embedding model knot's indexing pipeline and knot-server's query embedder
/// both resolve from `KNOT_EMBED_MODEL` (falling back to knot's default).
pub fn resolved_embed_model() -> String {
    std::env::var("KNOT_EMBED_MODEL")
        .unwrap_or_else(|_| knot::pipeline::embed::DEFAULT_EMBED_MODEL.to_owned())
}

/// `KNOT_SERVER_EMBED_DIM` / `--embed-dim` are deprecated: the dimension is
/// derived from `KNOT_EMBED_MODEL`. An agreeing value warns; a contradicting
/// one aborts, because it means the operator believes a different model is
/// active.
///
/// This check is local (mirroring upstreams `resolve_embed_dim`) rather than
/// reusing knot's `resolve_embed_and_collection`: that function is private and
/// its error text names `KNOT_EMBED_DIM` / `--embed-dim`, the wrong variable
/// to tell a knot-server operator to unset. The shared logic that matters
/// (model → dimension) is public upstream and used here — not duplicated.
pub fn resolve_embed_dim(embed_model: &str, supplied: Option<u64>) -> anyhow::Result<u64> {
    use std::str::FromStr;

    let choice = knot::pipeline::embed::EmbedModelChoice::from_str(embed_model)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    match supplied {
        None => Ok(choice.dim),
        Some(v) if v == choice.dim => {
            tracing::warn!(
                "KNOT_SERVER_EMBED_DIM={v} is deprecated and removed in the next major: the \
                 dimension is derived from the embedding model '{embed_model}' (native \
                 dimension {native}). Unset KNOT_SERVER_EMBED_DIM / --embed-dim.",
                native = choice.dim
            );
            Ok(choice.dim)
        }
        Some(v) => anyhow::bail!(
            "KNOT_SERVER_EMBED_DIM ({v}) does not match the selected embedding model \
             '{embed_model}' (native dimension {native}). \
             Unset KNOT_SERVER_EMBED_DIM / --embed-dim: the dimension is derived from \
             KNOT_EMBED_MODEL. Changing the model also requires a full re-index.",
            native = choice.dim
        ),
    }
}

/// The native dimension of knot's default embedding model. Fixtures use this
/// instead of a literal dimension so a future default flip cannot leave a
/// fixture lying about the active model.
#[cfg(test)]
pub fn default_embed_dim() -> u64 {
    resolve_embed_dim(knot::pipeline::embed::DEFAULT_EMBED_MODEL, None)
        .expect("knot's DEFAULT_EMBED_MODEL must be a known model")
}

/// Derive the effective Qdrant collection: **an explicitly supplied collection
/// always wins**; otherwise the model's suffix is applied to the base default
/// (MiniLM keeps `knot_entities` byte-for-byte; BGE lands on
/// `knot_entities_bge768`). Mirrors upstreams `resolve_collection`.
pub fn resolve_collection(
    supplied: Option<&str>,
    base_default: &str,
    choice: &knot::pipeline::embed::EmbedModelChoice,
) -> String {
    match supplied {
        Some(c) => c.to_owned(),
        None => choice.default_collection(base_default),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn parse_args(args: &[&str]) -> ServerConfig {
        let mut full = vec!["knot-server", "--neo4j-password", "secret"];
        full.extend_from_slice(args);
        ServerConfig::try_parse_from(full).expect("Failed to parse")
    }

    #[test]
    fn test_default_port() {
        let cfg = parse_args(&[]);
        assert_eq!(cfg.port, 3000);
    }

    #[test]
    fn test_custom_port() {
        let cfg = parse_args(&["--port", "8080"]);
        assert_eq!(cfg.port, 8080);
    }

    #[test]
    fn test_default_values() {
        let cfg = parse_args(&[]);
        assert_eq!(cfg.bind_addr, "0.0.0.0");
        assert_eq!(cfg.qdrant_url, "http://localhost:6334");
        assert_eq!(cfg.qdrant_collection, DEFAULT_COLLECTION);
        // `parse_args` uses `try_parse_from`, so the explicit flag is untouched
        // (only `from_env` fills it).
        assert!(!cfg.qdrant_collection_explicit);
        assert_eq!(cfg.neo4j_uri, "bolt://localhost:7687");
        assert_eq!(cfg.neo4j_user, "neo4j");
        assert_eq!(cfg.embed_dim, None);
        assert_eq!(cfg.batch_size, 128);
    }

    #[test]
    fn test_embed_dim_flag_still_parses() {
        // Backward-compat pin: `--embed-dim` exists in the published v0.7.0,
        // so removing it outright would break operator scripts. It stays
        // hidden, deprecated and `Option<u64>`.
        let cfg = parse_args(&["--embed-dim", "384"]);
        assert_eq!(cfg.embed_dim, Some(384));
    }

    #[test]
    fn test_custom_workspace_dir() {
        let cfg = parse_args(&["--workspace-dir", "/custom/path"]);
        assert_eq!(cfg.workspace_dir, "/custom/path");
    }

    #[test]
    fn test_metrics_enabled_default_true() {
        assert!(parse_args(&[]).metrics_enabled);
    }

    #[test]
    fn test_mcp_enabled_default_true() {
        assert!(parse_args(&[]).mcp_enabled);
    }

    #[test]
    fn test_mcp_can_be_disabled() {
        assert!(!parse_args(&["--mcp-enabled", "false"]).mcp_enabled);
    }

    #[test]
    fn test_tracing_defaults() {
        let cfg = parse_args(&[]);
        // Off by default: exporting spans requires a running OTLP collector.
        assert!(!cfg.tracing_enabled);
        assert_eq!(cfg.otlp_endpoint, "http://localhost:4317");
        assert_eq!(cfg.trace_sample_ratio, 1.0);
    }

    #[test]
    fn test_tracing_custom_values() {
        let cfg = parse_args(&[
            "--tracing-enabled",
            "--otlp-endpoint",
            "http://jaeger:4317",
            "--trace-sample-ratio",
            "0.25",
        ]);
        assert!(cfg.tracing_enabled);
        assert_eq!(cfg.otlp_endpoint, "http://jaeger:4317");
        assert_eq!(cfg.trace_sample_ratio, 0.25);
    }

    #[test]
    fn test_default_derived_dim_matches_knot_default_model() {
        // Drift guard: if knot flips the default model again (or changes a
        // dimension) this fails in CI, not at startup.
        let choice = knot::pipeline::embed::EmbedModelChoice::from_str(
            knot::pipeline::embed::DEFAULT_EMBED_MODEL,
        )
        .expect("knot's DEFAULT_EMBED_MODEL must be a known model");
        assert_eq!(
            resolve_embed_dim(knot::pipeline::embed::DEFAULT_EMBED_MODEL, None).unwrap(),
            choice.dim
        );
    }

    #[test]
    fn test_resolve_embed_dim_accepts_none_and_matching() {
        assert_eq!(resolve_embed_dim("AllMiniLML6V2", None).unwrap(), 384);
        assert_eq!(resolve_embed_dim("BGEBaseENV15", None).unwrap(), 768);
        assert_eq!(resolve_embed_dim("AllMiniLML6V2", Some(384)).unwrap(), 384);
        assert_eq!(resolve_embed_dim("BGEBaseENV15", Some(768)).unwrap(), 768);
    }

    #[test]
    fn test_resolve_embed_dim_rejects_contradicting() {
        let err = resolve_embed_dim("BGEBaseENV15", Some(384))
            .unwrap_err()
            .to_string();
        assert!(err.contains("KNOT_SERVER_EMBED_DIM (384)"), "{err}");
        assert!(err.contains("BGEBaseENV15"), "{err}");
        assert!(err.contains("768"), "{err}");
    }

    #[test]
    fn test_resolve_embed_dim_rejects_unknown_model() {
        let err = resolve_embed_dim("GPT5", None).unwrap_err().to_string();
        assert!(err.contains("Unknown embedding model 'GPT5'"), "{err}");
        assert!(err.contains("AllMiniLML6V2"), "{err}");
        assert!(err.contains("BGEBaseENV15"), "{err}");
    }

    #[test]
    fn test_resolve_collection_default_minilm_keeps_base_name() {
        let choice = knot::pipeline::embed::EmbedModelChoice::from_str("AllMiniLML6V2").unwrap();
        assert_eq!(
            resolve_collection(None, "knot_entities", &choice),
            "knot_entities"
        );
    }

    #[test]
    fn test_resolve_collection_default_bge_derives_suffixed_name() {
        let choice = knot::pipeline::embed::EmbedModelChoice::from_str("BGEBaseENV15").unwrap();
        assert_eq!(
            resolve_collection(None, "knot_entities", &choice),
            "knot_entities_bge768"
        );
    }

    #[test]
    fn test_resolve_collection_explicit_value_wins_for_either_model() {
        let minilm = knot::pipeline::embed::EmbedModelChoice::from_str("AllMiniLML6V2").unwrap();
        let bge = knot::pipeline::embed::EmbedModelChoice::from_str("BGEBaseENV15").unwrap();
        assert_eq!(
            resolve_collection(Some("my_collection"), "knot_entities", &minilm),
            "my_collection"
        );
        assert_eq!(
            resolve_collection(Some("my_collection"), "knot_entities", &bge),
            "my_collection"
        );
    }

    #[test]
    fn test_value_source_distinguishes_default_from_explicit() {
        // The plumbing resolve_collection relies on: an explicitly supplied
        // collection that equals the base name must be distinguishable from
        // the untouched default (both carry the same string; only the source
        // differs).
        let matches = ServerConfig::command()
            .try_get_matches_from(vec![
                "knot-server",
                "--neo4j-password",
                "secret",
                "--qdrant-collection",
                "knot_entities",
            ])
            .unwrap();
        assert_eq!(
            matches.value_source("qdrant_collection"),
            Some(ValueSource::CommandLine)
        );

        let default_matches = ServerConfig::command()
            .try_get_matches_from(vec!["knot-server", "--neo4j-password", "secret"])
            .unwrap();
        assert_eq!(
            default_matches.value_source("qdrant_collection"),
            Some(ValueSource::DefaultValue)
        );
    }

    #[test]
    fn test_collection_explicit_classifies_sources() {
        assert!(!collection_explicit(None));
        assert!(!collection_explicit(Some(ValueSource::DefaultValue)));
        assert!(collection_explicit(Some(ValueSource::CommandLine)));
        // An env-supplied collection is an explicit choice too; misclassifying
        // it would silently override the operator with a derived name.
        assert!(collection_explicit(Some(ValueSource::EnvVariable)));
    }

    #[test]
    fn test_resolved_collection_derives_only_when_not_explicit() {
        // C2 regression: with the default (not explicit) collection, BGE must
        // derive the suffixed name. Passing `Some(cfg.qdrant_collection)`
        // instead would return the base name and this assertion fails.
        let bge = knot::pipeline::embed::EmbedModelChoice::from_str("BGEBaseENV15").unwrap();
        let matches = ServerConfig::command()
            .try_get_matches_from(vec!["knot-server", "--neo4j-password", "secret"])
            .unwrap();
        let mut cfg = ServerConfig::from_arg_matches(&matches).unwrap();
        cfg.qdrant_collection_explicit =
            collection_explicit(matches.value_source("qdrant_collection"));
        assert_eq!(cfg.resolved_collection(&bge), "knot_entities_bge768");

        // Explicitly naming the base collection still wins, even for BGE.
        cfg.qdrant_collection_explicit = true;
        cfg.qdrant_collection = DEFAULT_COLLECTION.to_owned();
        assert_eq!(cfg.resolved_collection(&bge), DEFAULT_COLLECTION);

        // An arbitrary explicit collection wins too.
        cfg.qdrant_collection = "my_collection".to_owned();
        assert_eq!(cfg.resolved_collection(&bge), "my_collection");
    }
}
