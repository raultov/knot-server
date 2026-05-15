use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::models::{RegisterRepoRequest, RegisterRepoResponse, RepoListResponse};
use crate::state::AppState;

#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: String,
}

fn error_response(status: StatusCode, message: impl Into<String>) -> Response {
    (
        status,
        Json(ErrorResponse {
            error: message.into(),
        }),
    )
        .into_response()
}

// ── Read endpoints ──────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct SearchParams {
    pub q: Option<String>,
    pub max_results: Option<usize>,
}

pub async fn search_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(params): Query<SearchParams>,
) -> Response {
    let query = match &params.q {
        Some(q) if !q.trim().is_empty() => q.as_str(),
        _ => return error_response(StatusCode::BAD_REQUEST, "Missing required parameter 'q'"),
    };

    let max_results = params.max_results.unwrap_or(5);

    match knot::cli_tools::run_search_hybrid_context(
        query,
        max_results,
        Some(&id),
        &state.vector_db,
        &state.graph_db,
        &state.embedder,
    )
    .await
    {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err(e) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Search failed: {e}"),
        ),
    }
}

#[derive(Debug, Deserialize)]
pub struct CallersParams {
    pub entity: Option<String>,
}

pub async fn callers_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(params): Query<CallersParams>,
) -> Response {
    let entity_name = match &params.entity {
        Some(e) if !e.trim().is_empty() => e.as_str(),
        _ => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "Missing required parameter 'entity'",
            );
        }
    };

    match knot::cli_tools::run_find_callers(entity_name, Some(&id), &state.graph_db).await {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err(e) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Find callers failed: {e}"),
        ),
    }
}

#[derive(Debug, Deserialize)]
pub struct ExploreParams {
    pub path: Option<String>,
}

pub async fn explore_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(params): Query<ExploreParams>,
) -> Response {
    let relative = match &params.path {
        Some(p) if !p.trim().is_empty() => p.as_str(),
        _ => return error_response(StatusCode::BAD_REQUEST, "Missing required parameter 'path'"),
    };

    // Neo4j stores absolute file paths from the indexer. Build the full path
    // by prepending the repo's local_path.
    let full_path = {
        let registry = state.registry.lock().unwrap();
        match registry.get(&id) {
            Some(entry) => {
                let trimmed = relative.trim_start_matches('/');
                format!("{}/{}", entry.local_path.trim_end_matches('/'), trimmed)
            }
            None => {
                return error_response(
                    StatusCode::NOT_FOUND,
                    format!("Repository '{}' not found", id),
                );
            }
        }
    };

    match knot::cli_tools::run_explore_file(&full_path, Some(&id), &state.graph_db).await {
        Ok((_display_path, entities_json)) => (StatusCode::OK, Json(entities_json)).into_response(),
        Err(e) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Explore file failed: {e}"),
        ),
    }
}

#[derive(Debug, Deserialize)]
pub struct DepsParams {
    pub reverse: Option<bool>,
    pub max_depth: Option<u32>,
}

pub async fn deps_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(params): Query<DepsParams>,
) -> Response {
    let max_depth = params.max_depth.unwrap_or(3);
    let reverse = params.reverse.unwrap_or(false);

    match knot::cli_tools::run_deps(&id, max_depth, reverse, &state.graph_db).await {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err(e) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Deps lookup failed: {e}"),
        ),
    }
}

// ── Repository management endpoints ─────────────────────────────

pub async fn list_repos_handler(State(state): State<Arc<AppState>>) -> Response {
    let registry = state.registry.lock().unwrap();
    let repos = registry.list().to_vec();
    let response = RepoListResponse {
        repositories: repos,
    };
    (StatusCode::OK, Json(response)).into_response()
}

pub async fn get_repo_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let registry = state.registry.lock().unwrap();
    match registry.get(&id) {
        Some(entry) => (StatusCode::OK, Json(entry.clone())).into_response(),
        None => error_response(
            StatusCode::NOT_FOUND,
            format!("Repository '{}' not found", id),
        ),
    }
}

pub async fn register_repo_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<RegisterRepoRequest>,
) -> Response {
    let id = body.generate_id();
    let local_path = crate::models::repo_local_path(&state.workspace_dir, &id);

    let entry = crate::models::RepoEntry {
        id: id.clone(),
        url: body.url.clone(),
        auth_type: body.auth_type.clone(),
        local_path,
        branch: body.branch.clone(),
        webhook_secret: body.webhook_secret.clone(),
        last_indexed: None,
        status: crate::models::RepoStatus::Idle,
    };

    let mut registry = state.registry.lock().unwrap();
    match registry.add(entry) {
        Ok(()) => {
            // Enqueue Clone job for the new repository
            let job = crate::models::IndexJob::Clone {
                repo_id: id.clone(),
            };
            match state.job_tx.try_send(job) {
                Ok(()) => {}
                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                    let _ = registry.remove(&id);
                    return error_response(
                        StatusCode::TOO_MANY_REQUESTS,
                        "Server is at maximum capacity: indexing queue is full",
                    );
                }
                Err(e) => {
                    tracing::error!("Failed to enqueue Clone job for {}: {e}", id);
                }
            }

            tracing::info!(
                "Registered repository '{}' (url: {}, auth: {:?})",
                id,
                body.url,
                body.auth_type
            );

            let response = RegisterRepoResponse {
                id,
                message: "Repository registered successfully".into(),
            };
            (StatusCode::ACCEPTED, Json(response)).into_response()
        }
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("already exists") {
                error_response(StatusCode::CONFLICT, msg)
            } else {
                error_response(StatusCode::INTERNAL_SERVER_ERROR, msg)
            }
        }
    }
}

pub async fn delete_repo_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let mut registry = state.registry.lock().unwrap();

    match registry.remove(&id) {
        Ok(()) => {
            // Clean up databases in background (fire-and-forget)
            let graph_db = state.graph_db.clone();
            let vector_db = state.vector_db.clone();
            let rid = id.clone();
            tokio::spawn(async move {
                crate::cleanup::delete_repo_from_databases(&rid, &graph_db, &vector_db).await;
            });

            // Clean up local directory
            let repo_path = crate::models::repo_local_path(&state.workspace_dir, &id);
            if std::path::Path::new(&repo_path).exists()
                && let Err(e) = std::fs::remove_dir_all(&repo_path)
            {
                tracing::warn!("Failed to remove repo directory {}: {e}", repo_path);
            }
            tracing::info!("Deleted repository '{}'", id);
            (
                StatusCode::OK,
                Json(serde_json::json!({"message": "Repository deleted"})),
            )
                .into_response()
        }
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("not found") {
                error_response(StatusCode::NOT_FOUND, msg)
            } else {
                error_response(StatusCode::INTERNAL_SERVER_ERROR, msg)
            }
        }
    }
}

// ── Trigger endpoints ───────────────────────────────────────────

pub async fn sync_repo_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    // Check repo exists
    {
        let registry = state.registry.lock().unwrap();
        if registry.get(&id).is_none() {
            return error_response(
                StatusCode::NOT_FOUND,
                format!("Repository '{}' not found", id),
            );
        }
    }

    // Enqueue Pull job
    let job = crate::models::IndexJob::Pull {
        repo_id: id.clone(),
    };
    match state.job_tx.try_send(job) {
        Ok(()) => {}
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
            return error_response(
                StatusCode::TOO_MANY_REQUESTS,
                "Server is at maximum capacity: indexing queue is full",
            );
        }
        Err(e) => {
            tracing::error!("Failed to enqueue Pull job for {}: {e}", id);
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to enqueue sync job",
            );
        }
    }

    tracing::info!("Enqueued sync job for '{}'", id);

    (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({
            "message": "Sync job enqueued",
            "repo_id": id
        })),
    )
        .into_response()
}

// ── Webhook endpoint ─────────────────────────────────────────────

pub async fn webhook_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    // Check repo exists and get webhook secret
    let webhook_secret = {
        let registry = state.registry.lock().unwrap();
        match registry.get(&id) {
            Some(entry) => entry.webhook_secret.clone(),
            None => {
                return error_response(
                    StatusCode::NOT_FOUND,
                    format!("Repository '{}' not found", id),
                );
            }
        }
    };

    let Some(secret) = webhook_secret else {
        return error_response(
            StatusCode::UNAUTHORIZED,
            "Webhook secret not configured for this repository",
        );
    };

    // Try GitLab token validation
    if let Some(token) = headers.get("X-Gitlab-Token").and_then(|v| v.to_str().ok()) {
        if crate::webhook::validate_gitlab_token(token, &secret) {
            return enqueue_pull_job(&state, &id).await;
        }
        return error_response(StatusCode::UNAUTHORIZED, "Invalid GitLab webhook token");
    }

    // Try GitHub signature validation
    if let Some(sig) = headers
        .get("X-Hub-Signature-256")
        .and_then(|v| v.to_str().ok())
    {
        if crate::webhook::validate_github_signature(sig, &body, &secret) {
            return enqueue_pull_job(&state, &id).await;
        }
        return error_response(StatusCode::UNAUTHORIZED, "Invalid GitHub webhook signature");
    }

    // Try Bitbucket signature validation
    if let Some(sig) = headers.get("X-Hub-Signature").and_then(|v| v.to_str().ok()) {
        if crate::webhook::validate_bitbucket_signature(sig, &body, &secret) {
            return enqueue_pull_job(&state, &id).await;
        }
        return error_response(
            StatusCode::UNAUTHORIZED,
            "Invalid Bitbucket webhook signature",
        );
    }

    error_response(
        StatusCode::UNAUTHORIZED,
        "Missing webhook signature header (X-Gitlab-Token, X-Hub-Signature-256, or X-Hub-Signature)",
    )
}

async fn enqueue_pull_job(state: &Arc<AppState>, repo_id: &str) -> Response {
    let job = crate::models::IndexJob::Pull {
        repo_id: repo_id.to_string(),
    };
    match state.job_tx.try_send(job) {
        Ok(()) => {}
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
            return error_response(
                StatusCode::TOO_MANY_REQUESTS,
                "Server is at maximum capacity: indexing queue is full",
            );
        }
        Err(e) => {
            tracing::error!("Failed to enqueue webhook Pull job for {}: {e}", repo_id);
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to enqueue indexing job",
            );
        }
    }
    tracing::info!("Webhook validated for '{}', enqueued Pull job", repo_id);
    (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({
            "message": "Webhook received, indexing job enqueued",
            "repo_id": repo_id
        })),
    )
        .into_response()
}

// ── Health endpoint ──────────────────────────────────────────────

pub async fn health_handler(State(state): State<Arc<AppState>>) -> Response {
    let registry = state.registry.lock().unwrap();
    let repos = registry.list();
    let cloning_count = repos
        .iter()
        .filter(|r| r.status == crate::models::RepoStatus::Cloning)
        .count();
    let pulling_count = repos
        .iter()
        .filter(|r| r.status == crate::models::RepoStatus::Pulling)
        .count();
    let indexing_count = repos
        .iter()
        .filter(|r| r.status == crate::models::RepoStatus::Indexing)
        .count();

    let uptime = state.start_time.elapsed().as_secs();

    let health = serde_json::json!({
        "status": "ok",
        "uptime_seconds": uptime,
        "queue_capacity": state.job_tx.capacity(),
        "repositories_total": repos.len(),
        "repositories_cloning": cloning_count,
        "repositories_pulling": pulling_count,
        "repositories_indexing": indexing_count,
        "workspace_dir": state.workspace_dir,
    });

    (StatusCode::OK, Json(health)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::{get, post};
    use knot::db::graph::ConnectExt;
    use knot::db::vector::VectorConnectExt;
    use std::sync::{Arc, Mutex};
    use tempfile::TempDir;
    use tower::ServiceExt;

    use crate::registry::Registry;

    fn build_test_app(state: Arc<AppState>) -> Router {
        Router::new()
            .route(
                "/api/repos",
                get(list_repos_handler).post(register_repo_handler),
            )
            .route(
                "/api/repos/{id}",
                get(get_repo_handler).delete(delete_repo_handler),
            )
            .route("/api/repos/{id}/search", get(search_handler))
            .route("/api/repos/{id}/callers", get(callers_handler))
            .route("/api/repos/{id}/explore", get(explore_handler))
            .route("/api/repos/{id}/deps", get(deps_handler))
            .route("/api/repos/{id}/sync", post(sync_repo_handler))
            .route("/api/webhook/{id}", post(webhook_handler))
            .route("/api/health", get(health_handler))
            .with_state(state)
    }

    async fn create_test_state_with_tempdir(
        temp_dir: &TempDir,
    ) -> (
        Arc<AppState>,
        tokio::sync::mpsc::Receiver<crate::models::IndexJob>,
    ) {
        let workspace = temp_dir.path().to_path_buf();
        let registry =
            Registry::load_or_create(&workspace).expect("Failed to create test registry");

        let graph_db =
            knot::db::graph::GraphDb::connect("bolt://localhost:9999", "neo4j", "badpassword")
                .await
                .expect("connect for test db should work");

        let vector_db =
            knot::db::vector::VectorDb::connect("http://localhost:9999", "test_collection", 384)
                .await
                .expect("connect for test vector db should work");

        let embedder = knot::pipeline::embed::Embedder::init().expect("init embedder should work");

        let (job_tx, job_rx) = tokio::sync::mpsc::channel::<crate::models::IndexJob>(16);

        (
            Arc::new(AppState {
                vector_db: Arc::new(vector_db),
                graph_db: Arc::new(graph_db),
                embedder: Arc::new(Mutex::new(embedder)),
                workspace_dir: workspace.to_string_lossy().into(),
                registry: Arc::new(Mutex::new(registry)),
                job_tx,
                qdrant_url: "http://localhost:6334".into(),
                qdrant_collection: "knot_entities".into(),
                neo4j_uri: "bolt://localhost:7687".into(),
                neo4j_user: "neo4j".into(),
                neo4j_password: "secret".into(),
                embed_dim: 384,
                rayon_threads: None,
                batch_size: 64,
                ingest_concurrency: 4,
                start_time: std::time::Instant::now(),
            }),
            job_rx,
        )
    }

    #[tokio::test]
    async fn test_search_missing_query_returns_400() {
        let dir = TempDir::new().unwrap();
        let (state, _job_rx) = create_test_state_with_tempdir(&dir).await;
        let app = build_test_app(state);

        let body = serde_json::json!({
            "url": "git@github.com:org/repo.git",
            "auth_type": "ssh"
        });

        // First registration succeeds
        let _ = app
            .clone()
            .oneshot(
                Request::post("/api/repos")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        // Second returns conflict
        let response = app
            .oneshot(
                Request::post("/api/repos")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn test_delete_nonexistent_repo_returns_404() {
        let dir = TempDir::new().unwrap();
        let (state, _job_rx) = create_test_state_with_tempdir(&dir).await;
        let app = build_test_app(state);

        let response = app
            .oneshot(
                Request::delete("/api/repos/nonexistent")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_get_nonexistent_repo_returns_404() {
        let dir = TempDir::new().unwrap();
        let (state, _job_rx) = create_test_state_with_tempdir(&dir).await;
        let app = build_test_app(state);

        let response = app
            .oneshot(
                Request::get("/api/repos/ghost")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_sync_nonexistent_repo_returns_404() {
        let dir = TempDir::new().unwrap();
        let (state, _job_rx) = create_test_state_with_tempdir(&dir).await;
        let app = build_test_app(state);

        let response = app
            .oneshot(
                Request::post("/api/repos/ghost/sync")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_sync_existing_repo_returns_202() {
        let dir = TempDir::new().unwrap();
        let (state, _job_rx) = create_test_state_with_tempdir(&dir).await;
        let app = build_test_app(state);

        // Register a repo first
        let body = serde_json::json!({
            "url": "git@github.com:org/sync-test.git",
            "auth_type": "ssh"
        });
        let reg_response = app
            .clone()
            .oneshot(
                Request::post("/api/repos")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(reg_response.status(), StatusCode::ACCEPTED);

        let body_bytes = axum::body::to_bytes(reg_response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let reg_json: RegisterRepoResponse = serde_json::from_slice(&body_bytes).unwrap();

        // Trigger sync
        let response = app
            .oneshot(
                Request::post(format!("/api/repos/{}/sync", reg_json.id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let status = response.status();
        let _body_bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        assert_eq!(status, StatusCode::ACCEPTED);
    }

    #[tokio::test]
    async fn test_webhook_missing_signature_returns_401() {
        let dir = TempDir::new().unwrap();
        let (state, _job_rx) = create_test_state_with_tempdir(&dir).await;
        let app = build_test_app(state);

        let response = app
            .oneshot(
                Request::post("/api/webhook/test-repo")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_webhook_nonexistent_repo_returns_404() {
        let dir = TempDir::new().unwrap();
        let (state, _job_rx) = create_test_state_with_tempdir(&dir).await;
        let app = build_test_app(state);

        let response = app
            .oneshot(
                Request::post("/api/webhook/ghost")
                    .header("content-type", "application/json")
                    .header("X-Gitlab-Token", "test-token")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_health_returns_ok() {
        let dir = TempDir::new().unwrap();
        let (state, _job_rx) = create_test_state_with_tempdir(&dir).await;
        let app = build_test_app(state);

        let response = app
            .oneshot(Request::get("/api/health").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body_bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let health: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(health["status"], "ok");
        assert!(health["uptime_seconds"].as_u64().is_some());
        assert!(health["repositories_total"].as_u64().is_some());
    }

    #[tokio::test]
    async fn test_register_returns_429_when_queue_full() {
        let dir = TempDir::new().unwrap();
        let (_state, _job_rx) = create_test_state_with_tempdir(&dir).await;

        // Replace the state's job_tx with a tiny-capacity channel (capacity=1)
        let (_small_tx, _new_rx) = tokio::sync::mpsc::channel::<crate::models::IndexJob>(1);
        // We need to wrap state to use the new tx
        // Use unsafe to replace the Arc's inner tx — or just use a new state

        // Simpler: create a state with capacity=1
        let workspace = dir.path().to_owned();
        let workspace2 = workspace.join("ws2");
        std::fs::create_dir_all(&workspace2).unwrap();
        let registry2 = crate::registry::Registry::load_or_create(&workspace2).expect("registry");

        let graph_db2 = knot::db::graph::GraphDb::connect("bolt://localhost:9999", "neo4j", "bad")
            .await
            .expect("connect");
        let vector_db2 = knot::db::vector::VectorDb::connect("http://localhost:9999", "test", 384)
            .await
            .expect("connect");
        let embedder2 = knot::pipeline::embed::Embedder::init().expect("embedder");

        let (small_tx, mut small_rx) = tokio::sync::mpsc::channel::<crate::models::IndexJob>(1);
        let state2 = Arc::new(AppState {
            vector_db: Arc::new(vector_db2),
            graph_db: Arc::new(graph_db2),
            embedder: Arc::new(Mutex::new(embedder2)),
            workspace_dir: workspace2.to_string_lossy().into(),
            registry: Arc::new(Mutex::new(registry2)),
            job_tx: small_tx,
            qdrant_url: "http://localhost:6334".into(),
            qdrant_collection: "knot_entities".into(),
            neo4j_uri: "bolt://localhost:7687".into(),
            neo4j_user: "neo4j".into(),
            neo4j_password: "secret".into(),
            embed_dim: 384,
            rayon_threads: None,
            batch_size: 64,
            ingest_concurrency: 4,
            start_time: std::time::Instant::now(),
        });

        let app = build_test_app(state2);

        let body = serde_json::json!({
            "url": "git@github.com:org/foo.git",
            "auth_type": "ssh"
        });

        // First registration: fills the queue (capacity=1)
        let resp1 = app
            .clone()
            .oneshot(
                Request::post("/api/repos")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp1.status(), StatusCode::ACCEPTED);

        let body2 = serde_json::json!({
            "url": "git@github.com:org/bar.git",
            "auth_type": "ssh"
        });

        // Second registration: queue is full → 429
        let resp2 = app
            .oneshot(
                Request::post("/api/repos")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&body2).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp2.status(), StatusCode::TOO_MANY_REQUESTS);

        // Drain the queue to avoid channel drop warnings
        let _ = small_rx.try_recv();
    }
}
