mod cleanup;
mod config;
mod git;
mod handlers;
mod local_sync;
mod locking;
mod models;
mod registry;
mod scheduler;
mod time_utils;
mod webhook;
mod worker;

use std::path::Path;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::Request;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use knot::db::graph::{ConnectExt, GraphDb};
use knot::db::vector::{VectorConnectExt, VectorDb};
use knot::pipeline::embed::Embedder;
use registry::Registry;
use models::AppState;
use tokio::signal;
use tracing_subscriber::EnvFilter;
use utoipa::OpenApi;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use utoipa_swagger_ui::SwaggerUi;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    setup_tracing();

    let cfg = config::ServerConfig::from_env();
    tracing::info!("Starting knot-server v{}", env!("CARGO_PKG_VERSION"));
    tracing::info!("Binding to {}:{}", cfg.bind_addr, cfg.port);

    setup_rayon(cfg.rayon_threads);

    let graph_db = setup_neo4j(&cfg.neo4j_uri, &cfg.neo4j_user, &cfg.neo4j_password).await;

    let fastembed_cache_dir = setup_fastembed_cache(&cfg.workspace_dir)?;

    let vector_db = setup_qdrant(&cfg.qdrant_url, &cfg.qdrant_collection, cfg.embed_dim).await?;

    let embedder = setup_embedder(fastembed_cache_dir)?;

    tracing::info!("Loading repository registry from {}...", cfg.workspace_dir);
    let mut registry = Registry::load_or_create(Path::new(&cfg.workspace_dir))?;
    tracing::info!("Registry loaded: {} repositories", registry.list().len());

    // Create job queue channel
    let (job_tx, job_rx) = tokio::sync::mpsc::channel::<models::IndexJob>(cfg.queue_capacity);

    recover_stuck_repos(&mut registry, &job_tx);

    let start_time = std::time::Instant::now();

    let state = Arc::new(AppState {
        vector_db: Arc::new(vector_db),
        graph_db: Arc::new(graph_db),
        embedder: Some(Arc::new(Mutex::new(embedder))),
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
        batch_size: cfg.batch_size,
        ingest_concurrency: cfg.ingest_concurrency,
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

    #[derive(OpenApi)]
    #[openapi(
        info(
            title = "knot-server",
            version = env!("CARGO_PKG_VERSION"),
            description = "REST API for managing and indexing Git repositories. Provides semantic search, caller analysis, file exploration, dependency graphs, and interactive 3D visualization.",
            license(name = "MIT", url = "https://github.com/raultov/knot-server/blob/master/LICENSE"),
        ),
        tags(
            (name = "Repositories", description = "Register, list, inspect, and delete Git repositories"),
            (name = "Search", description = "Semantic search, caller analysis, file exploration, and dependency lookup"),
            (name = "Graph", description = "Entity relationship subgraph queries"),
            (name = "Indexing", description = "Trigger manual sync and re-indexing"),
            (name = "Webhooks", description = "Git provider webhook endpoints (GitHub, GitLab, Bitbucket)"),
            (name = "Health", description = "Server health and statistics"),
        ),
    )]
    struct ApiDoc;

    // Build API routes with automatic OpenAPI spec collection
    let (api_router, api) = OpenApiRouter::with_openapi(ApiDoc::openapi())
        .routes(routes!(
            handlers::repo::list_repos_handler,
            handlers::register_repo_handler
        ))
        .routes(routes!(
            handlers::get_repo_handler,
            handlers::delete_repo_handler
        ))
        .routes(routes!(handlers::sync_repo_handler))
        .routes(routes!(handlers::search_handler))
        .routes(routes!(handlers::callers_handler))
        .routes(routes!(handlers::explore_handler))
        .routes(routes!(handlers::deps_handler))
        .routes(routes!(handlers::graph_handler))
        .routes(routes!(handlers::graph_expand_handler))
        .routes(routes!(handlers::webhook_handler))
        .routes(routes!(handlers::health_handler))
        .split_for_parts();

    // Merge the generated API routes with non-API routes and Swagger UI
    let app = api_router
        .route("/favicon.ico", get(handlers::favicon_handler))
        .route("/graph", get(handlers::graph_viewer_handler))
        .merge(
            Router::from(SwaggerUi::new("/docs").url("/api-docs/openapi.json", api))
                .layer(middleware::from_fn(docs_html_override)),
        )
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(format!("{}:{}", cfg.bind_addr, cfg.port)).await?;
    tracing::info!("knot-server listening on {}:{}", cfg.bind_addr, cfg.port);

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    tracing::info!("knot-server shut down gracefully");
    Ok(())
}

/// Intercepts the HTML and redirect routes served by `utoipa-swagger-ui` for
/// `/docs` and `/docs/`, returning a custom Swagger UI page that injects a
/// badge with the `knot` library version between the `knot-server` version
/// stamp and the `OAS 3.1` stamp. All other requests (CSS, JS, favicon, the
/// OpenAPI JSON, …) pass through unchanged.
async fn docs_html_override(req: Request, next: Next) -> Response {
    match req.uri().path() {
        "/docs" => Redirect::to("/docs/").into_response(),
        "/docs/" => handlers::docs_handler().await,
        _ => next.run(req).await,
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request as HttpRequest, StatusCode};
    use tower::util::ServiceExt;

    async fn not_found_handler() -> Response {
        (StatusCode::NOT_FOUND, "not-found").into_response()
    }

    fn build_test_app() -> Router {
        Router::new()
            .fallback(not_found_handler)
            .layer(middleware::from_fn(docs_html_override))
    }

    #[tokio::test]
    async fn docs_root_redirects_to_slash() {
        let app = build_test_app();
        let req = HttpRequest::builder()
            .uri("/docs")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            response
                .headers()
                .get("location")
                .unwrap()
                .to_str()
                .unwrap(),
            "/docs/"
        );
    }

    #[tokio::test]
    async fn docs_slash_returns_custom_html() {
        let app = build_test_app();
        let req = HttpRequest::builder()
            .uri("/docs/")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("content-type")
                .unwrap()
                .to_str()
                .unwrap(),
            "text/html; charset=utf-8"
        );

        let body_bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let body = String::from_utf8(body_bytes.to_vec()).unwrap();
        assert!(body.contains(env!("KNOT_VERSION")));
        assert!(!body.contains("{{KNOT_VERSION}}"));
    }

    #[tokio::test]
    async fn docs_assets_pass_through() {
        let app = build_test_app();
        let req = HttpRequest::builder()
            .uri("/docs/swagger-ui.css")
            .body(Body::empty())
            .unwrap();
        // The middleware should not intercept asset paths; they reach the
        // fallback that responds 404 (proving the override did not fire).
        let response = app.oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}


fn setup_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
}

fn setup_rayon(threads: Option<usize>) {
    if let Some(t) = threads {
        rayon::ThreadPoolBuilder::new()
            .num_threads(t)
            .build_global()
            .expect("Failed to configure Rayon thread pool");
        tracing::info!("Rayon thread pool configured: {} threads", t);
    }
}

async fn setup_neo4j(uri: &str, user: &str, pass: &str) -> GraphDb {
    tracing::info!("Connecting to Neo4j at {}...", uri);
    let graph_db = loop {
        match GraphDb::connect(uri, user, pass).await {
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
    graph_db
}

fn setup_fastembed_cache(workspace_dir: &str) -> anyhow::Result<std::path::PathBuf> {
    let fastembed_cache_dir = std::path::Path::new(workspace_dir).join("fastembed_cache");
    let cache_str = fastembed_cache_dir
        .to_str()
        .expect("workspace_dir contains invalid UTF-8");
    std::fs::create_dir_all(cache_str)?;
    unsafe {
        std::env::set_var("KNOT_FASTEMBED_CACHE_DIR", cache_str);
    }
    tracing::info!("Fastembed cache dir: {cache_str}");
    Ok(fastembed_cache_dir)
}

async fn setup_qdrant(url: &str, collection: &str, dim: u64) -> anyhow::Result<VectorDb> {
    tracing::info!("Connecting to Qdrant at {}...", url);
    let vector_db = VectorDb::connect(url, collection, dim).await?;
    vector_db.ensure_collection().await?;
    tracing::info!("Qdrant connection established");
    Ok(vector_db)
}

fn setup_embedder(cache_dir: std::path::PathBuf) -> anyhow::Result<Embedder> {
    tracing::info!("Initializing embedding model...");
    let embedder = Embedder::init(cache_dir)?;
    tracing::info!("Embedding model ready");
    Ok(embedder)
}

fn recover_stuck_repos(
    registry: &mut Registry,
    job_tx: &tokio::sync::mpsc::Sender<models::IndexJob>,
) {
    let stuck: Vec<models::RepoEntry> = registry
        .list()
        .iter()
        .filter(|r| {
            matches!(
                r.status,
                models::RepoStatus::Pending
                    | models::RepoStatus::Cloning
                    | models::RepoStatus::Pulling
                    | models::RepoStatus::Indexing
            )
        })
        .cloned()
        .collect();

    for repo in stuck {
        let git_dir = std::path::Path::new(&repo.local_path).join(".git");
        if git_dir.exists() {
            tracing::warn!(
                "Recovering stuck repo '{}' (was {}): .git exists, enqueuing Pull",
                repo.id,
                repo.status
            );
            let _ = job_tx.try_send(models::IndexJob::Pull {
                repo_id: repo.id.clone(),
            });
        } else {
            tracing::warn!(
                "Recovering stuck repo '{}' (was {}): .git missing, removing partial path and enqueuing Clone",
                repo.id,
                repo.status
            );
            if std::path::Path::new(&repo.local_path).exists()
                && let Err(e) = std::fs::remove_dir_all(&repo.local_path)
            {
                tracing::warn!("Failed to remove partial repo dir {}: {e}", repo.local_path);
            }
            let _ = job_tx.try_send(models::IndexJob::Clone {
                repo_id: repo.id.clone(),
            });
        }
        if let Err(e) = registry.update_status(&repo.id, models::RepoStatus::Pending) {
            tracing::warn!("Failed to reset status for '{}': {e}", repo.id);
        }
    }
}
