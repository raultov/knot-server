#[cfg(test)]
mod tests {
    use crate::handlers::graph_parse::*;
    use crate::handlers::models::*;
    use crate::handlers::*;
    use crate::models::AppState;
    use crate::models::RegisterRepoResponse;
    use axum::Router;
    use axum::body::Body;
    use axum::http::Request;
    use axum::http::StatusCode;
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
            .route("/api/repos/{id}/graph", get(graph_handler))
            .route("/api/repos/{id}/graph/expand", get(graph_expand_handler))
            .route("/api/repos/{id}/sync", post(sync_repo_handler))
            .route("/api/webhook/{id}", post(webhook_handler))
            .route("/api/health", get(health_handler))
            .route("/graph", get(graph_viewer_handler))
            .with_state(state)
    }

    async fn post_repo(app: Router, body: &serde_json::Value) -> axum::response::Response {
        app.oneshot(
            Request::post("/api/repos")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap()
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

        let (job_tx, job_rx) = tokio::sync::mpsc::channel::<crate::models::IndexJob>(16);

        (
            Arc::new(AppState {
                vector_db: Arc::new(vector_db),
                graph_db: Arc::new(graph_db),
                embedder: None,
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
    async fn test_register_duplicate_is_idempotent() {
        let dir = TempDir::new().unwrap();
        let (state, _job_rx) = create_test_state_with_tempdir(&dir).await;
        let app = build_test_app(state);

        let body = serde_json::json!({
            "url": "git@github.com:org/repo.git",
            "auth_type": "ssh"
        });

        // First registration: 202 Accepted with "registered" message.
        let resp1 = post_repo(app.clone(), &body).await;
        assert_eq!(resp1.status(), StatusCode::ACCEPTED);
        let body1 = axum::body::to_bytes(resp1.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let json1: serde_json::Value = serde_json::from_slice(&body1).unwrap();
        assert_eq!(json1["message"], "Repository registered successfully");

        // Second registration: 202 Accepted with "re-registered" message.
        // The endpoint is idempotent: the existing entry is replaced
        // (not rejected with 409) so the new Clone job starts from
        // scratch.
        let resp2 = post_repo(app, &body).await;
        assert_eq!(resp2.status(), StatusCode::ACCEPTED);
        let body2 = axum::body::to_bytes(resp2.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let json2: serde_json::Value = serde_json::from_slice(&body2).unwrap();
        assert_eq!(json2["message"], "Repository re-registered successfully");
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
        let reg_response = post_repo(app.clone(), &body).await;
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
    async fn test_webhook_nonexistent_repo_no_auth_returns_404() {
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
    async fn test_graph_missing_entity_returns_overview_error() {
        let dir = TempDir::new().unwrap();
        let (state, _job_rx) = create_test_state_with_tempdir(&dir).await;
        let app = build_test_app(state);

        let body = serde_json::json!({
            "url": "git@github.com:org/repo.git",
            "auth_type": "ssh"
        });
        let res = post_repo(app.clone(), &body).await;

        let body_bytes = axum::body::to_bytes(res.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        let repo_id = json["id"].as_str().unwrap();

        let response = app
            .oneshot(
                Request::get(format!("/api/repos/{repo_id}/graph"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn test_graph_expand_missing_entity_returns_400() {
        let dir = TempDir::new().unwrap();
        let (state, _job_rx) = create_test_state_with_tempdir(&dir).await;
        let app = build_test_app(state);

        let body = serde_json::json!({
            "url": "git@github.com:org/repo.git",
            "auth_type": "ssh"
        });
        let res = post_repo(app.clone(), &body).await;

        let body_bytes = axum::body::to_bytes(res.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        let repo_id = json["id"].as_str().unwrap();

        let response = app
            .oneshot(
                Request::get(format!("/api/repos/{repo_id}/graph/expand"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_graph_viewer_returns_200() {
        let dir = TempDir::new().unwrap();
        let (state, _job_rx) = create_test_state_with_tempdir(&dir).await;
        let app = build_test_app(state);

        let response = app
            .oneshot(Request::get("/graph").body(Body::empty()).unwrap())
            .await
            .unwrap();

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
        assert!(body.contains("<!DOCTYPE html>"));
        assert!(body.contains("ForceGraph3D"));
        assert!(
            !body.contains("{{KNOT_SERVER_VERSION}}"),
            "version placeholder was not substituted"
        );
        assert!(
            body.contains(env!("CARGO_PKG_VERSION")),
            "package version absent from HTML"
        );
        assert!(body.contains("knot-server v"), "version badge malformed");
    }

    #[tokio::test]
    async fn test_graph_nonexistent_repo_returns_404() {
        let dir = TempDir::new().unwrap();
        let (state, _job_rx) = create_test_state_with_tempdir(&dir).await;
        let app = build_test_app(state);

        let response = app
            .oneshot(
                Request::get("/api/repos/nonexistent/graph?entity=some_function")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_graph_expand_nonexistent_repo_returns_404() {
        let dir = TempDir::new().unwrap();
        let (state, _job_rx) = create_test_state_with_tempdir(&dir).await;
        let app = build_test_app(state);

        let response = app
            .oneshot(
                Request::get("/api/repos/nonexistent/graph/expand?entity=some_function")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_register_returns_429_when_queue_full() {
        let dir = TempDir::new().unwrap();

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

        let (small_tx, mut small_rx) = tokio::sync::mpsc::channel::<crate::models::IndexJob>(1);
        let state2 = Arc::new(AppState {
            vector_db: Arc::new(vector_db2),
            graph_db: Arc::new(graph_db2),
            embedder: None,
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
        let resp1 = post_repo(app.clone(), &body).await;
        assert_eq!(resp1.status(), StatusCode::ACCEPTED);

        let body2 = serde_json::json!({
            "url": "git@github.com:org/bar.git",
            "auth_type": "ssh"
        });

        // Second registration: queue is full → 429
        let resp2 = post_repo(app, &body2).await;
        assert_eq!(resp2.status(), StatusCode::TOO_MANY_REQUESTS);

        // Drain the queue to avoid channel drop warnings
        let _ = small_rx.try_recv();
    }

    #[tokio::test]
    async fn test_search_missing_query_returns_400() {
        let dir = TempDir::new().unwrap();
        let (state, _job_rx) = create_test_state_with_tempdir(&dir).await;
        let app = build_test_app(state);

        let response = app
            .oneshot(
                Request::get("/api/repos/any-repo/search")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_callers_missing_entity_returns_400() {
        let dir = TempDir::new().unwrap();
        let (state, _job_rx) = create_test_state_with_tempdir(&dir).await;
        let app = build_test_app(state);

        let response = app
            .oneshot(
                Request::get("/api/repos/any-repo/callers")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_explore_missing_path_returns_400() {
        let dir = TempDir::new().unwrap();
        let (state, _job_rx) = create_test_state_with_tempdir(&dir).await;
        let app = build_test_app(state);

        let response = app
            .oneshot(
                Request::get("/api/repos/any-repo/explore")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_list_repos_returns_empty() {
        let dir = TempDir::new().unwrap();
        let (state, _job_rx) = create_test_state_with_tempdir(&dir).await;
        let app = build_test_app(state);

        let response = app
            .oneshot(Request::get("/api/repos").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body_bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        assert!(json["repositories"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_graph_invalid_relationship_returns_400() {
        let dir = TempDir::new().unwrap();
        let (state, _job_rx) = create_test_state_with_tempdir(&dir).await;
        let app = build_test_app(state);

        let body = serde_json::json!({
            "url": "git@github.com:org/repo.git",
            "auth_type": "ssh"
        });
        let res = post_repo(app.clone(), &body).await;

        let body_bytes = axum::body::to_bytes(res.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        let repo_id = json["id"].as_str().unwrap();

        let response = app
            .oneshot(
                Request::get(format!("/api/repos/{repo_id}/graph?relationships=INVALID"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_graph_invalid_kind_returns_400() {
        let dir = TempDir::new().unwrap();
        let (state, _job_rx) = create_test_state_with_tempdir(&dir).await;
        let app = build_test_app(state);

        let body = serde_json::json!({
            "url": "git@github.com:org/repo.git",
            "auth_type": "ssh"
        });
        let res = post_repo(app.clone(), &body).await;

        let body_bytes = axum::body::to_bytes(res.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        let repo_id = json["id"].as_str().unwrap();

        let response = app
            .oneshot(
                Request::get(format!("/api/repos/{repo_id}/graph?kinds=INVALID"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_graph_expand_invalid_kind_returns_400() {
        let dir = TempDir::new().unwrap();
        let (state, _job_rx) = create_test_state_with_tempdir(&dir).await;
        let app = build_test_app(state);

        let body = serde_json::json!({
            "url": "git@github.com:org/repo.git",
            "auth_type": "ssh"
        });
        let res = post_repo(app.clone(), &body).await;

        let body_bytes = axum::body::to_bytes(res.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        let repo_id = json["id"].as_str().unwrap();

        let response = app
            .oneshot(
                Request::get(format!(
                    "/api/repos/{repo_id}/graph/expand?entity=test&kinds=INVALID"
                ))
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_graph_subgraph_invalid_kind_returns_400() {
        let dir = TempDir::new().unwrap();
        let (state, _job_rx) = create_test_state_with_tempdir(&dir).await;
        let app = build_test_app(state);

        let body = serde_json::json!({
            "url": "git@github.com:org/repo.git",
            "auth_type": "ssh"
        });
        let res = post_repo(app.clone(), &body).await;

        let body_bytes = axum::body::to_bytes(res.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        let repo_id = json["id"].as_str().unwrap();

        let response = app
            .oneshot(
                Request::get(format!(
                    "/api/repos/{repo_id}/graph?entity=some_func&kinds=INVALID"
                ))
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_register_repo_empty_url() {
        let dir = TempDir::new().unwrap();
        let (state, _job_rx) = create_test_state_with_tempdir(&dir).await;
        let app = build_test_app(state);

        let body = serde_json::json!({
            "url": "   ",
            "auth_type": "ssh"
        });
        let res = post_repo(app.clone(), &body).await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_register_repo_invalid_url_empty_id() {
        let dir = TempDir::new().unwrap();
        let (state, _job_rx) = create_test_state_with_tempdir(&dir).await;
        let app = build_test_app(state);

        // A URL like "/" generates an empty ID
        let body = serde_json::json!({
            "url": "/",
            "auth_type": "ssh"
        });
        let res = post_repo(app.clone(), &body).await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn test_parse_kinds_empty_uses_default() {
        let result = parse_kinds("").unwrap();
        assert!(!result.is_empty());
        assert!(result.contains(&"class"));
        assert!(result.contains(&"interface"));
        assert!(result.contains(&"rust_struct"));
        assert!(result.contains(&"rust_trait"));
        assert!(!result.contains(&"function"));
    }

    #[test]
    fn test_parse_kinds_invalid_returns_error() {
        let result = parse_kinds("INVALID");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Invalid kind category"));
    }

    #[test]
    fn test_parse_kinds_classes_expands() {
        let result = parse_kinds("classes").unwrap();
        assert!(!result.is_empty());
        for k in KIND_CATEGORY_CLASSES {
            assert!(result.contains(k), "missing kind: {}", k);
        }
    }

    #[test]
    fn test_parse_kinds_interfaces_expands() {
        let result = parse_kinds("interfaces").unwrap();
        assert!(!result.is_empty());
        for k in KIND_CATEGORY_INTERFACES {
            assert!(result.contains(k), "missing kind: {}", k);
        }
    }

    #[test]
    fn test_parse_kinds_functions_expands() {
        let result = parse_kinds("functions").unwrap();
        assert!(!result.is_empty());
        for k in KIND_CATEGORY_FUNCTIONS {
            assert!(result.contains(k), "missing kind: {}", k);
        }
    }

    #[test]
    fn test_parse_kinds_other_returns_empty_visible() {
        let result = parse_kinds("other").unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_parse_kinds_multiple_categories() {
        let result = parse_kinds("classes,interfaces,other").unwrap();
        assert!(result.contains(&"class"));
        assert!(result.contains(&"interface"));
        assert!(!result.contains(&"function"));
    }

    #[test]
    fn test_includes_other_empty_default() {
        assert!(!includes_other(""));
    }

    #[test]
    fn test_includes_other_explicit() {
        assert!(includes_other("other"));
    }

    #[test]
    fn test_includes_other_not_present() {
        assert!(!includes_other("classes"));
        assert!(!includes_other("classes,interfaces"));
        assert!(!includes_other("functions"));
    }

    #[test]
    fn test_includes_other_mixed() {
        assert!(includes_other("classes,other"));
        assert!(includes_other("classes,interfaces,other"));
        assert!(includes_other("other,functions"));
    }
}
