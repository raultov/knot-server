mod cleanup;
mod config;
mod git;
mod handlers;
mod locking;
mod models;
mod registry;
mod scheduler;
mod state;
mod webhook;
mod worker;

use std::path::Path;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::routing::{get, post};
use knot::db::graph::{ConnectExt, GraphDb};
use knot::db::vector::{VectorConnectExt, VectorDb};
use knot::pipeline::embed::Embedder;
use registry::Registry;
use state::AppState;
use tokio::signal;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cfg = config::ServerConfig::from_env();
    tracing::info!("Starting knot-server v{}", env!("CARGO_PKG_VERSION"));
    tracing::info!("Binding to {}:{}", cfg.bind_addr, cfg.port);

    // Configure Rayon thread pool once per process
    if let Some(threads) = cfg.rayon_threads {
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build_global()
            .expect("Failed to configure Rayon thread pool");
        tracing::info!("Rayon thread pool configured: {} threads", threads);
    }

    tracing::info!("Connecting to Neo4j at {}...", cfg.neo4j_uri);
    let graph_db = loop {
        match GraphDb::connect(&cfg.neo4j_uri, &cfg.neo4j_user, &cfg.neo4j_password).await {
            Ok(db) => break db,
            Err(e) => {
                tracing::warn!("Neo4j connection attempt failed: {e}");
                tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
            }
        }
    };
    loop {
        match graph_db.ensure_indexes().await {
            Ok(()) => break,
            Err(e) => {
                tracing::warn!("Neo4j index creation failed: {e}");
                tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
            }
        }
    }
    tracing::info!("Neo4j connection established");

    tracing::info!("Connecting to Qdrant at {}...", cfg.qdrant_url);
    let vector_db =
        VectorDb::connect(&cfg.qdrant_url, &cfg.qdrant_collection, cfg.embed_dim).await?;
    vector_db.ensure_collection().await?;
    tracing::info!("Qdrant connection established");

    tracing::info!("Initializing embedding model...");
    let embedder = Embedder::init()?;
    tracing::info!("Embedding model ready");

    tracing::info!("Loading repository registry from {}...", cfg.workspace_dir);
    let registry = Registry::load_or_create(Path::new(&cfg.workspace_dir))?;
    tracing::info!("Registry loaded: {} repositories", registry.list().len());

    // Create job queue channel
    let (job_tx, job_rx) = tokio::sync::mpsc::channel::<models::IndexJob>(cfg.queue_capacity);

    let start_time = std::time::Instant::now();

    let state = Arc::new(AppState {
        vector_db: Arc::new(vector_db),
        graph_db: Arc::new(graph_db),
        embedder: Arc::new(Mutex::new(embedder)),
        workspace_dir: cfg.workspace_dir.clone(),
        registry: Arc::new(Mutex::new(registry)),
        job_tx: job_tx.clone(),
        qdrant_url: cfg.qdrant_url.clone(),
        qdrant_collection: cfg.qdrant_collection.clone(),
        neo4j_uri: cfg.neo4j_uri.clone(),
        neo4j_user: cfg.neo4j_user.clone(),
        neo4j_password: cfg.neo4j_password.clone(),
        embed_dim: cfg.embed_dim,
        rayon_threads: cfg.rayon_threads,
        start_time,
    });

    // Spawn the worker loop
    let worker_state = state.clone();
    tokio::spawn(async move {
        worker::worker_loop(job_rx, worker_state).await;
    });
    tracing::info!("Indexing worker started (concurrency: 1)");

    // Spawn the background scheduler
    let scheduler_state = state.clone();
    let poll_interval = cfg.poll_interval_secs;
    let stale_lock_timeout = cfg.stale_lock_timeout_secs;
    let max_index_age = cfg.max_index_age_secs;
    tokio::spawn(async move {
        scheduler::scheduler_loop(
            scheduler_state,
            poll_interval,
            stale_lock_timeout,
            max_index_age,
        )
        .await;
    });
    tracing::info!(
        "Background scheduler started (poll: {}s, stale lock timeout: {}s, max index age: {}s)",
        poll_interval,
        stale_lock_timeout,
        max_index_age
    );

    let app = Router::new()
        .route(
            "/api/repos",
            get(handlers::list_repos_handler).post(handlers::register_repo_handler),
        )
        .route(
            "/api/repos/{id}",
            get(handlers::get_repo_handler).delete(handlers::delete_repo_handler),
        )
        .route("/api/repos/{id}/sync", post(handlers::sync_repo_handler))
        .route("/api/repos/{id}/search", get(handlers::search_handler))
        .route("/api/repos/{id}/callers", get(handlers::callers_handler))
        .route("/api/repos/{id}/explore", get(handlers::explore_handler))
        .route("/api/repos/{id}/deps", get(handlers::deps_handler))
        .route("/api/webhook/{id}", post(handlers::webhook_handler))
        .route("/api/health", get(handlers::health_handler))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(format!("{}:{}", cfg.bind_addr, cfg.port)).await?;
    tracing::info!("knot-server listening on {}:{}", cfg.bind_addr, cfg.port);

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    tracing::info!("knot-server shut down gracefully");
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {
            tracing::info!("Received SIGINT, shutting down...");
        },
        () = terminate => {
            tracing::info!("Received SIGTERM, shutting down...");
        },
    }
}
