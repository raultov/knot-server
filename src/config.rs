use clap::Parser;

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

    #[arg(
        long,
        env = "KNOT_SERVER_QDRANT_COLLECTION",
        default_value = "knot_entities"
    )]
    pub qdrant_collection: String,

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

    #[arg(long, env = "KNOT_SERVER_EMBED_DIM", default_value_t = 384)]
    pub embed_dim: u64,

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
        Self::parse()
    }
}

/// Embedding model knot's indexing pipeline and knot-server's query embedder
/// both resolve from `KNOT_EMBED_MODEL` (falling back to knot's default).
///
/// knot-server exposes the *dimension* as its own `KNOT_SERVER_EMBED_DIM` but
/// lets knot own the model name, so this mirrors the exact lookup performed by
/// `knot::pipeline::embed::Embedder::init`. Keeping that value and
/// [`validate_embed_pair`] in one module means the manually-built
/// `knot::config::Config` and the startup guard can never disagree.
pub fn resolved_embed_model() -> String {
    std::env::var("KNOT_EMBED_MODEL")
        .unwrap_or_else(|_| knot::pipeline::embed::DEFAULT_EMBED_MODEL.to_owned())
}

/// Fail fast when `KNOT_SERVER_EMBED_DIM` does not match the native dimension
/// of the model selected via `KNOT_EMBED_MODEL`.
///
/// Mirrors knot's own `validate_embed_pair`. Without it a mismatch (for
/// example `KNOT_EMBED_MODEL=BGEBaseENV15` while `KNOT_SERVER_EMBED_DIM`
/// stays at the 384 default) only surfaces later as a confusing Qdrant
/// "wrong vector size" error during collection setup or ingestion. An unknown
/// model name is rejected with knot's list of accepted names.
pub fn validate_embed_pair(embed_model: &str, embed_dim: u64) -> anyhow::Result<()> {
    use std::str::FromStr;

    let choice = knot::pipeline::embed::EmbedModelChoice::from_str(embed_model)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    if choice.dim != embed_dim {
        anyhow::bail!(
            "KNOT_SERVER_EMBED_DIM ({embed_dim}) does not match the selected embedding model \
             '{embed_model}' (native dimension {native}). \
             Set KNOT_SERVER_EMBED_DIM to {native}, or unset KNOT_EMBED_MODEL and re-run. \
             Changing the model also requires a full re-index.",
            native = choice.dim
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_port() {
        let args = vec!["knot-server", "--neo4j-password", "secret"];
        let cfg = ServerConfig::try_parse_from(args).expect("Failed to parse");
        assert_eq!(cfg.port, 3000);
    }

    #[test]
    fn test_custom_port() {
        let args = vec![
            "knot-server",
            "--neo4j-password",
            "secret",
            "--port",
            "8080",
        ];
        let cfg = ServerConfig::try_parse_from(args).expect("Failed to parse");
        assert_eq!(cfg.port, 8080);
    }

    #[test]
    fn test_default_values() {
        let args = vec!["knot-server", "--neo4j-password", "secret"];
        let cfg = ServerConfig::try_parse_from(args).expect("Failed to parse");
        assert_eq!(cfg.bind_addr, "0.0.0.0");
        assert_eq!(cfg.qdrant_url, "http://localhost:6334");
        assert_eq!(cfg.qdrant_collection, "knot_entities");
        assert_eq!(cfg.neo4j_uri, "bolt://localhost:7687");
        assert_eq!(cfg.neo4j_user, "neo4j");
        assert_eq!(cfg.embed_dim, 384);
        assert_eq!(cfg.batch_size, 128);
    }

    #[test]
    fn test_custom_workspace_dir() {
        let args = vec![
            "knot-server",
            "--neo4j-password",
            "secret",
            "--workspace-dir",
            "/custom/path",
        ];
        let cfg = ServerConfig::try_parse_from(args).expect("Failed to parse");
        assert_eq!(cfg.workspace_dir, "/custom/path");
    }

    #[test]
    fn test_metrics_enabled_default_true() {
        let args = vec!["knot-server", "--neo4j-password", "secret"];
        let cfg = ServerConfig::try_parse_from(args).expect("Failed to parse");
        assert!(cfg.metrics_enabled);
    }

    #[test]
    fn test_mcp_enabled_default_true() {
        let args = vec!["knot-server", "--neo4j-password", "secret"];
        let cfg = ServerConfig::try_parse_from(args).expect("Failed to parse");
        assert!(cfg.mcp_enabled);
    }

    #[test]
    fn test_mcp_can_be_disabled() {
        let args = vec![
            "knot-server",
            "--neo4j-password",
            "secret",
            "--mcp-enabled",
            "false",
        ];
        let cfg = ServerConfig::try_parse_from(args).expect("Failed to parse");
        assert!(!cfg.mcp_enabled);
    }

    #[test]
    fn test_tracing_defaults() {
        let args = vec!["knot-server", "--neo4j-password", "secret"];
        let cfg = ServerConfig::try_parse_from(args).expect("Failed to parse");
        // Off by default: exporting spans requires a running OTLP collector.
        assert!(!cfg.tracing_enabled);
        assert_eq!(cfg.otlp_endpoint, "http://localhost:4317");
        assert_eq!(cfg.trace_sample_ratio, 1.0);
    }

    #[test]
    fn test_tracing_custom_values() {
        let args = vec![
            "knot-server",
            "--neo4j-password",
            "secret",
            "--tracing-enabled",
            "--otlp-endpoint",
            "http://jaeger:4317",
            "--trace-sample-ratio",
            "0.25",
        ];
        let cfg = ServerConfig::try_parse_from(args).expect("Failed to parse");
        assert!(cfg.tracing_enabled);
        assert_eq!(cfg.otlp_endpoint, "http://jaeger:4317");
        assert_eq!(cfg.trace_sample_ratio, 0.25);
    }

    #[test]
    fn test_validate_embed_pair_accepts_matching_model() {
        validate_embed_pair("AllMiniLML6V2", 384).expect("384 matches MiniLM");
        validate_embed_pair("BGEBaseENV15", 768).expect("768 matches BGE-base");
    }

    #[test]
    fn test_validate_embed_pair_is_case_insensitive() {
        validate_embed_pair("jinaembeddingsv2basecode", 768)
            .expect("names parse case-insensitively");
    }

    #[test]
    fn test_validate_embed_pair_rejects_mismatched_dimension() {
        let err = validate_embed_pair("BGEBaseENV15", 384)
            .unwrap_err()
            .to_string();
        assert!(err.contains("KNOT_SERVER_EMBED_DIM (384)"), "{err}");
        assert!(err.contains("BGEBaseENV15"), "{err}");
        assert!(err.contains("native dimension 768"), "{err}");
    }

    #[test]
    fn test_validate_embed_pair_rejects_unknown_model() {
        let err = validate_embed_pair("GPT5", 384).unwrap_err().to_string();
        assert!(err.contains("Unknown embedding model 'GPT5'"), "{err}");
        assert!(err.contains("AllMiniLML6V2"), "{err}");
    }
}
