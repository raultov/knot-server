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
}

impl ServerConfig {
    pub fn from_env() -> Self {
        Self::parse()
    }
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct KnotConfigParams {
    pub repo_path: String,
    pub repo_name: String,
    pub qdrant_url: String,
    pub qdrant_collection: String,
    pub neo4j_uri: String,
    pub neo4j_user: String,
    pub neo4j_password: String,
    pub embed_dim: u64,
}

#[allow(dead_code)]
pub fn build_knot_config(params: &KnotConfigParams) -> knot::config::Config {
    knot::config::Config {
        repo_path: params.repo_path.clone(),
        repo_name: params.repo_name.clone(),
        qdrant_url: params.qdrant_url.clone(),
        qdrant_collection: params.qdrant_collection.clone(),
        neo4j_uri: params.neo4j_uri.clone(),
        neo4j_user: params.neo4j_user.clone(),
        neo4j_password: params.neo4j_password.clone(),
        custom_queries_path: None,
        embed_dim: params.embed_dim,
        batch_size: 64,
        clean: false,
        dependency_repos: Vec::new(),
        watch: false,
        dry_run: false,
        custom_ca_certs: None,
        output_format: knot::config::OutputFormat::Markdown,
        ingest_concurrency: 4,
        rayon_threads: None,
        include_config_files: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_knot_config_from_server_config() {
        let params = KnotConfigParams {
            qdrant_url: "http://localhost:6334".into(),
            qdrant_collection: "knot_entities".into(),
            neo4j_uri: "bolt://localhost:7687".into(),
            neo4j_user: "neo4j".into(),
            neo4j_password: "secret".into(),
            repo_path: "/repos/my-app".into(),
            repo_name: "my-app".into(),
            embed_dim: 384,
        };
        let knot_cfg = build_knot_config(&params);

        assert_eq!(knot_cfg.repo_path, "/repos/my-app");
        assert_eq!(knot_cfg.repo_name, "my-app");
        assert!(!knot_cfg.clean);
        assert!(!knot_cfg.watch);
        assert!(!knot_cfg.dry_run);
        assert_eq!(knot_cfg.embed_dim, 384);
    }

    #[test]
    fn test_knot_config_always_incremental() {
        let params = KnotConfigParams {
            qdrant_url: "http://localhost:6334".into(),
            qdrant_collection: "knot_entities".into(),
            neo4j_uri: "bolt://localhost:7687".into(),
            neo4j_user: "neo4j".into(),
            neo4j_password: "secret".into(),
            repo_path: "/repos/x".into(),
            repo_name: "x".into(),
            embed_dim: 384,
        };
        let knot_cfg = build_knot_config(&params);
        assert!(
            !knot_cfg.clean,
            "knot-server must always index incrementally"
        );
        assert!(!knot_cfg.watch, "knot-server manages scheduling, not knot");
    }

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
}
