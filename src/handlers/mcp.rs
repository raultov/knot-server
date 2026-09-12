//! `/mcp` — knot-server as a stateless MCP server (MCP_ENDPOINT_PLAN.md).
//!
//! Serves the exact knot MCP tool surface over HTTP so that MCP clients can
//! point at a load balancer and reach any knot-server node without session
//! affinity. The endpoint is stateless JSON-RPC over HTTP POST, hand-rolled on
//! axum (D1): the SDK's HTTP layer stores live `Arc<ServerRuntime>` objects in
//! a node-local session store, which would force sticky sessions.
//!
//! Key invariants (pinned by tests below):
//! - The server **never** emits `Mcp-Session-Id` (D2).
//! - The tool surface comes from `KnotMcpHandler::tools()` — a copy of knot's
//!   by construction, not by maintenance (D3).
//! - `repo_name` scope is a pure passthrough to knot; it deliberately does
//!   not go through knot-server's registry expansion (D4).
//! - Tool failures are `CallToolResult { isError: true }` inside a `200`,
//!   never JSON-RPC errors — mirroring the SDK runtime's stdio behavior.

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use rust_mcp_sdk::schema::{
    CallToolRequest, CallToolResult, ClientJsonrpcRequest, ClientMessage, ClientMessages,
    InitializeRequest, JsonrpcErrorResponse, ListToolsRequest, ListToolsResult, ProtocolVersion,
    RequestId, ResultFromServer, RpcError, ServerJsonrpcResponse,
};
use serde_json::json;
use std::sync::Arc;

use crate::handlers::models::ErrorResponse;
use crate::models::AppState;

/// HTTP method routing for `/mcp`: POST is the protocol, GET is refused
/// (no server-initiated stream in stateless mode), DELETE is a polite no-op.
#[utoipa::path(
    get,
    path = "/mcp",
    tag = "MCP",
    responses(
        (status = 405, description = "GET is not supported: a stateless MCP server opens no server-initiated stream. The response carries 'Allow: POST, DELETE'."),
    ),
    description = "Method probe for the MCP endpoint. A 405 with 'Allow: POST, DELETE' here is CORRECT and expected: a stateless server has no SSE stream to attach to. Use the POST operation to speak JSON-RPC.",
)]
pub async fn mcp_get_handler() -> Response {
    let mut resp = (
        StatusCode::METHOD_NOT_ALLOWED,
        Json(json!({"error": "GET is not supported: this MCP server is stateless and opens no server-initiated stream"})),
    )
        .into_response();
    resp.headers_mut()
        .insert(header::ALLOW, HeaderValue::from_static("POST, DELETE"));
    resp
}

#[utoipa::path(
    delete,
    path = "/mcp",
    tag = "MCP",
    responses(
        (status = 200, description = "No-op: there is no session to terminate."),
    ),
    description = "Session termination is a no-op. A stateless MCP server keeps no session state, so this always answers 200 to keep well-behaved clients quiet on shutdown. It never emits an Mcp-Session-Id.",
)]
pub async fn mcp_delete_handler() -> Response {
    // No session to terminate; answering 200 keeps well-behaved clients quiet
    // on shutdown.
    StatusCode::OK.into_response()
}

fn error_json_response(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({"error": message}))).into_response()
}

#[utoipa::path(
    post,
    path = "/mcp",
    tag = "MCP",
    request_body(
        content_type = "application/json",
        description = "JSON-RPC 2.0 request envelope. Supported methods: initialize, ping, tools/list, tools/call and notifications/*. Batching is rejected.",
        example = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}})
    ),
    responses(
        (status = 200, description = "JSON-RPC response: a result, or a protocol-level error with code -32601 for an unknown method (a tool failure is a 200 with result.isError=true, never a JSON-RPC error).", body = serde_json::Value),
        (status = 202, description = "Notification or client reply acknowledged; empty body."),
        (status = 400, description = "Body is not valid JSON (-32700) or is a JSON-RPC batch (-32600, removed from the protocol in 2025-06-18).", body = serde_json::Value),
        (status = 406, description = "Accept header does not include application/json.", body = ErrorResponse),
        (status = 415, description = "Content-Type is not application/json.", body = ErrorResponse),
    ),
    description = "Stateless MCP (Model Context Protocol) endpoint over HTTP. Serves the exact knot-mcp tool surface (search_hybrid_context, find_callers, explore_file, list_repo_dependencies, list_repositories) backed by the server's Neo4j and Qdrant connections. It never emits Mcp-Session-Id, so any node behind a load balancer can answer any request without session affinity. Troubleshooting: GET /mcp -> 405 is expected (no SSE stream); a 415 means Content-Type is not application/json; a 406 means the Accept header excludes application/json (or */*); a 404 means KNOT_SERVER_MCP_ENABLED=false."
)]
#[tracing::instrument(
    name = "mcp",
    skip_all,
    fields(mcp.method = tracing::field::Empty, mcp.tool = tracing::field::Empty)
)]
pub async fn mcp_post_handler(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    // Transport-level negotiation: a request that does not speak JSON at all
    // never reaches the JSON-RPC layer, so HTTP error codes are appropriate.
    let is_json = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .eq_ignore_ascii_case("application/json")
        });
    if !is_json {
        return error_json_response(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Content-Type must be application/json",
        );
    }

    let accepts_json = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(',').any(|part| {
                let mime = part.split(';').next().unwrap_or_default().trim();
                mime.eq_ignore_ascii_case("application/json")
                    || mime.eq_ignore_ascii_case("*/*")
                    || mime.ends_with("/*")
            })
        });
    if !accepts_json {
        return error_json_response(
            StatusCode::NOT_ACCEPTABLE,
            "Accept header must include application/json",
        );
    }

    // Parse errors have no id to correlate, so they are HTTP 400 with id null.
    let messages: ClientMessages = match serde_json::from_slice(&body) {
        Ok(m) => m,
        Err(_) => {
            let resp = JsonrpcErrorResponse::new(RpcError::parse_error(), None);
            return (StatusCode::BAD_REQUEST, Json(json_rpc_error_body(&resp))).into_response();
        }
    };

    // Batching was removed from the MCP protocol as of 2025-06-18.
    if messages.is_batch() {
        let resp = JsonrpcErrorResponse::new(
            RpcError::invalid_request()
                .with_message("Batch requests are no longer supported by the MCP protocol"),
            None,
        );
        return (StatusCode::BAD_REQUEST, Json(json_rpc_error_body(&resp))).into_response();
    }

    let Ok(message) = messages.as_single() else {
        let resp = JsonrpcErrorResponse::new(RpcError::invalid_request(), None);
        return (StatusCode::BAD_REQUEST, Json(json_rpc_error_body(&resp))).into_response();
    };

    match message {
        // Notifications and client→server replies carry nothing in flight in a
        // stateless server: acknowledge without a body and without a reply.
        ClientMessage::Notification(_) | ClientMessage::Response(_) | ClientMessage::Error(_) => {
            StatusCode::ACCEPTED.into_response()
        }
        ClientMessage::Request(request) => {
            handle_request(&state, request).await.unwrap_or_else(|| {
                // Defensive: handle_request only returns None for
                // notifications/responses, never for a Request.
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            })
        }
    }
}

/// Serialize a `JsonrpcErrorResponse` into a JSON value, preserving the
/// optional (possibly null) request id.
fn json_rpc_error_body(err: &JsonrpcErrorResponse) -> serde_json::Value {
    let mut body = serde_json::to_value(err).unwrap_or_else(|_| json!({}));
    body["jsonrpc"] = json!("2.0");
    body
}

/// Serialize a successful JSON-RPC response (id + jsonrpc + result).
fn json_rpc_result_body(id: RequestId, result: &ResultFromServer) -> serde_json::Value {
    let response = ServerJsonrpcResponse::new(id, result.clone());
    serde_json::to_value(&response).unwrap_or_else(|_| json!({}))
}

fn record_method(method: &str) {
    tracing::Span::current().record("mcp.method", method);
}

async fn handle_request(state: &AppState, request: ClientJsonrpcRequest) -> Option<Response> {
    match request {
        ClientJsonrpcRequest::InitializeRequest(req) => Some(handle_initialize(req).await),
        ClientJsonrpcRequest::PingRequest(req) => {
            record_method("ping");
            Some(ok_response(
                req.id,
                ResultFromServer::Result(Default::default()),
            ))
        }
        ClientJsonrpcRequest::ListToolsRequest(req) => Some(handle_list_tools(req).await),
        ClientJsonrpcRequest::CallToolRequest(req) => Some(handle_call_tool(state, req).await),
        other => {
            // Unknown / unsupported method lands here (CustomRequest catch-all
            // plus resources/*, prompts/*, tasks/*, logging, completion…).
            let method = other.method().to_string();
            record_method(&method);
            tracing::debug!("MCP method not supported: {method}");
            Some(method_not_found_response(other.request_id().clone()))
        }
    }
}

async fn handle_initialize(req: InitializeRequest) -> Response {
    record_method("initialize");
    let mut details = knot::mcp_handler::build_server_details();
    details.server_info.name = "knot-server".to_string();
    details.server_info.version = env!("CARGO_PKG_VERSION").to_string();
    details.protocol_version = negotiate_protocol_version(&req.params.protocol_version);
    if let Some(instructions) = details.instructions.as_mut() {
        instructions.push_str(
            "\nTransport: this server speaks MCP over stateless HTTP (POST /mcp); \
             any node behind the load balancer can answer any request, no session affinity \
             is required. Note that repo_name=\"all\" covers everything indexed in Neo4j, \
             not only the repositories registered with this knot-server instance.\n\
             Not available over MCP (REST-only on this server): registering, syncing and \
             deleting repositories (POST /api/repos, POST /api/repos/{id}/sync, \
             DELETE /api/repos/{id}), server health (GET /api/health), indexing progress \
             (GET /api/repos/{id}/progress) and raw subgraph queries \
             (GET /api/repos/{id}/graph). The MCP tools are read-only: if a repository is \
             not indexed yet, ask the operator to register it via the REST API.",
        );
    }
    ok_response(req.id, ResultFromServer::InitializeResult(details))
}

async fn handle_list_tools(req: ListToolsRequest) -> Response {
    record_method("tools/list");
    ok_response(
        req.id,
        ResultFromServer::ListToolsResult(ListToolsResult {
            tools: knot::mcp_handler::KnotMcpHandler::tools(),
            meta: None,
            next_cursor: None,
        }),
    )
}

async fn handle_call_tool(state: &AppState, req: CallToolRequest) -> Response {
    record_method("tools/call");
    let handler = handler_from_state(state);
    tracing::Span::current().record("mcp.tool", req.params.name.as_str());
    let result: CallToolResult = handler
        .dispatch(req.params)
        .await
        .unwrap_or_else(|err| err.into());
    ok_response(req.id, ResultFromServer::CallToolResult(result))
}

fn ok_response(id: RequestId, result: ResultFromServer) -> Response {
    (StatusCode::OK, Json(json_rpc_result_body(id, &result))).into_response()
}

fn method_not_found_response(id: RequestId) -> Response {
    let err = JsonrpcErrorResponse::new(RpcError::method_not_found(), Some(id));
    // Protocol-level failures ride in a 200: a well-formed request deserves a
    // correlated reply.
    (StatusCode::OK, Json(json_rpc_error_body(&err))).into_response()
}

/// Build a `KnotMcpHandler` over the existing `AppState` connections. Three
/// `Arc` clones per request; no new connections, no session state.
fn handler_from_state(state: &AppState) -> knot::mcp_handler::KnotMcpHandler {
    knot::mcp_handler::KnotMcpHandler {
        vector_db: Some(state.vector_db.clone()),
        graph_db: Some(state.graph_db.clone()),
        embedder: state.embedder.clone(),
        dry_run: false,
    }
}

/// Echo the client's requested protocol version when it is one we support;
/// otherwise answer with the latest stable version and let the client decide
/// whether to proceed.
fn negotiate_protocol_version(requested: &str) -> String {
    if let Ok(v) = ProtocolVersion::try_from(requested)
        && ProtocolVersion::supported_versions(false).contains(&v)
    {
        return v.to_string();
    }
    ProtocolVersion::latest().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::tests_common::create_test_state_with_rx;
    use axum::Router;
    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use axum::http::StatusCode;
    use axum::routing::post;
    use tempfile::TempDir;
    use tower::ServiceExt;

    /// Build a router over a test state with lazy, never-connected DB clients.
    /// Every protocol-level test runs without Neo4j or Qdrant.
    async fn mcp_app() -> Router {
        let dir = TempDir::new().unwrap();
        let (state, _job_rx) = create_test_state_with_rx(dir.path()).await;
        Router::new()
            .route(
                "/mcp",
                post(mcp_post_handler)
                    .get(mcp_get_handler)
                    .delete(mcp_delete_handler),
            )
            .with_state(state)
    }

    fn post_request(body: &serde_json::Value) -> HttpRequest<Body> {
        HttpRequest::post("/mcp")
            .header("content-type", "application/json")
            .header("accept", "application/json")
            .body(Body::from(serde_json::to_string(body).unwrap()))
            .unwrap()
    }

    fn post_raw(body: &str) -> HttpRequest<Body> {
        HttpRequest::post("/mcp")
            .header("content-type", "application/json")
            .header("accept", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    fn request_with_headers(
        method: &str,
        content_type: Option<&str>,
        accept: Option<&str>,
        body: &str,
    ) -> HttpRequest<Body> {
        let mut builder = HttpRequest::builder().method(method).uri("/mcp");
        if let Some(ct) = content_type {
            builder = builder.header("content-type", ct);
        }
        if let Some(acc) = accept {
            builder = builder.header("accept", acc);
        }
        builder.body(Body::from(body.to_string())).unwrap()
    }

    async fn body_json(resp: Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            panic!("body must be valid JSON: {e}");
        })
    }

    async fn post_json(app: Router, body: &serde_json::Value) -> (StatusCode, serde_json::Value) {
        let resp = app.oneshot(post_request(body)).await.unwrap();
        let status = resp.status();
        (status, body_json(resp).await)
    }

    fn initialize_body(version: &str) -> serde_json::Value {
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": version,
                "capabilities": {},
                "clientInfo": {"name": "test-client", "version": "0.0.1"}
            }
        })
    }

    // ── Cycle 0: scaffolding ─────────────────────────────────────────

    #[tokio::test]
    async fn post_mcp_returns_json_content_type() {
        let resp = mcp_app()
            .await
            .oneshot(post_request(&initialize_body("2025-06-18")))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get("content-type").unwrap(),
            "application/json"
        );
    }

    // ── Cycle 1: the statelessness invariant (D2) ────────────────────

    #[tokio::test]
    async fn initialize_never_returns_a_session_id() {
        let resp = mcp_app()
            .await
            .oneshot(post_request(&initialize_body("2025-06-18")))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(
            resp.headers().get("mcp-session-id").is_none(),
            "knot-server must stay stateless: emitting Mcp-Session-Id would force \
             the load balancer into session affinity"
        );
    }

    // ── Cycle 2: initialize ──────────────────────────────────────────

    #[tokio::test]
    async fn initialize_returns_server_info() {
        let (_, body) = post_json(mcp_app().await, &initialize_body("2025-06-18")).await;
        assert_eq!(body["result"]["serverInfo"]["name"], "knot-server");
        assert_eq!(
            body["result"]["serverInfo"]["version"],
            env!("CARGO_PKG_VERSION")
        );
    }

    #[tokio::test]
    async fn initialize_advertises_tools_capability() {
        let (_, body) = post_json(mcp_app().await, &initialize_body("2025-06-18")).await;
        assert!(
            body["result"]["capabilities"]["tools"].is_object(),
            "tools capability must be advertised: {body}"
        );
    }

    #[tokio::test]
    async fn initialize_echoes_supported_protocol_versions() {
        for version in ["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"] {
            let (_, body) = post_json(mcp_app().await, &initialize_body(version)).await;
            assert_eq!(
                body["result"]["protocolVersion"], version,
                "supported version must be echoed back"
            );
        }
    }

    #[tokio::test]
    async fn initialize_falls_back_to_latest_protocol_version() {
        let (_, body) = post_json(mcp_app().await, &initialize_body("1999-01-01")).await;
        assert_eq!(
            body["result"]["protocolVersion"],
            ProtocolVersion::latest().to_string()
        );
    }

    #[tokio::test]
    async fn initialize_result_id_matches_request_id() {
        let (_, body) = post_json(mcp_app().await, &initialize_body("2025-06-18")).await;
        assert_eq!(body["id"], 1);
    }

    #[tokio::test]
    async fn initialize_mentions_statelessness_and_scope_semantics() {
        let (_, body) = post_json(mcp_app().await, &initialize_body("2025-06-18")).await;
        let instructions = body["result"]["instructions"].as_str().unwrap_or_default();
        assert!(
            instructions.contains("stateless"),
            "instructions must mention the stateless transport"
        );
        assert!(
            instructions.contains("repo_name"),
            "instructions must document the D4 scope semantics"
        );
        assert!(
            instructions.contains("/api/repos"),
            "instructions must point MCP clients at the REST-only repository lifecycle"
        );
    }

    // ── Cycle 3: tools/list is knot's surface (D3) ───────────────────

    /// The literal is the contract: knot's five tools, spelled out
    /// independently rather than compared against `tools()` (comparing a
    /// value to itself proves nothing). If knot adds a sixth tool this test
    /// fails and we *choose* to update it.
    const EXPECTED_TOOLS: [&str; 5] = [
        "search_hybrid_context",
        "find_callers",
        "explore_file",
        "list_repo_dependencies",
        "list_repositories",
    ];

    fn tools_list_body() -> serde_json::Value {
        json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "tools/list",
            "params": {}
        })
    }

    #[tokio::test]
    async fn tools_list_returns_the_knot_surface() {
        let (_, body) = post_json(mcp_app().await, &tools_list_body()).await;
        let tools = body["result"]["tools"].as_array().expect("tools array");
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, EXPECTED_TOOLS);
    }

    #[tokio::test]
    async fn tools_list_entries_have_description_and_object_schema() {
        let (_, body) = post_json(mcp_app().await, &tools_list_body()).await;
        let tools = body["result"]["tools"].as_array().expect("tools array");
        for tool in tools {
            let name = tool["name"].as_str().unwrap();
            assert!(
                !tool["description"].as_str().unwrap_or_default().is_empty(),
                "{name} has no description"
            );
            assert_eq!(
                tool["inputSchema"]["type"], "object",
                "{name} inputSchema must be an object schema"
            );
        }
    }

    #[tokio::test]
    async fn tools_list_works_without_initialize() {
        // A stateless server has no handshake state to enforce; this is what
        // lets a client's second request land on a different node.
        let (_, body) = post_json(mcp_app().await, &tools_list_body()).await;
        assert_eq!(body["result"]["tools"].as_array().unwrap().len(), 5);
    }

    // ── Cycle 4: notifications ───────────────────────────────────────

    #[tokio::test]
    async fn initialized_notification_returns_202_empty() {
        let body = json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        });
        let resp = mcp_app().await.oneshot(post_request(&body)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
        let bytes = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
        assert!(bytes.is_empty(), "notification ack must have an empty body");
    }

    #[tokio::test]
    async fn cancelled_notification_returns_202_empty() {
        let body = json!({
            "jsonrpc": "2.0",
            "method": "notifications/cancelled",
            "params": {"requestId": 1}
        });
        let resp = mcp_app().await.oneshot(post_request(&body)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
    }

    #[tokio::test]
    async fn client_response_message_returns_202_empty() {
        let body = json!({
            "jsonrpc": "2.0",
            "id": 3,
            "result": {}
        });
        let resp = mcp_app().await.oneshot(post_request(&body)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
    }

    // ── Cycle 5: ping ────────────────────────────────────────────────

    #[tokio::test]
    async fn ping_returns_empty_result() {
        let body = json!({
            "jsonrpc": "2.0",
            "id": 9,
            "method": "ping"
        });
        let (status, body) = post_json(mcp_app().await, &body).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["result"], json!({}));
    }

    // ── Cycle 6: protocol errors ─────────────────────────────────────

    #[tokio::test]
    async fn malformed_json_is_parse_error() {
        let resp = mcp_app()
            .await
            .oneshot(post_raw("{not valid json"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = body_json(resp).await;
        assert_eq!(body["error"]["code"], -32700);
        assert!(body["id"].is_null(), "parse error must carry id null");
    }

    #[tokio::test]
    async fn batch_request_is_invalid_request() {
        let body = json!([
            {"jsonrpc": "2.0", "id": 1, "method": "ping"},
            {"jsonrpc": "2.0", "id": 2, "method": "ping"}
        ]);
        let resp = mcp_app().await.oneshot(post_request(&body)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = body_json(resp).await;
        assert_eq!(body["error"]["code"], -32600);
    }

    #[tokio::test]
    async fn unknown_method_is_method_not_found() {
        let body = json!({
            "jsonrpc": "2.0",
            "id": 42,
            "method": "resources/list"
        });
        let (status, body) = post_json(mcp_app().await, &body).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["error"]["code"], -32601);
        assert_eq!(body["id"], 42, "error id must match the request id");
    }

    #[tokio::test]
    async fn non_json_content_type_is_415() {
        let req = request_with_headers(
            "POST",
            Some("text/plain"),
            Some("application/json"),
            r#"{"jsonrpc": "2.0", "id": 1, "method": "ping"}"#,
        );
        let resp = mcp_app().await.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }

    #[tokio::test]
    async fn accept_without_json_is_406() {
        let req = request_with_headers(
            "POST",
            Some("application/json"),
            Some("text/plain"),
            r#"{"jsonrpc": "2.0", "id": 1, "method": "ping"}"#,
        );
        let resp = mcp_app().await.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_ACCEPTABLE);
    }

    // ── Cycle 7: tools/call error semantics ──────────────────────────

    #[tokio::test]
    async fn unknown_tool_returns_is_error_result_not_jsonrpc_error() {
        // The highest-value test in the file: pins the one place where the
        // intuitive implementation (CallToolError → JSON-RPC error) silently
        // diverges from knot-mcp over stdio.
        let body = json!({
            "jsonrpc": "2.0",
            "id": 11,
            "method": "tools/call",
            "params": {"name": "no_such_tool", "arguments": {}}
        });
        let (status, body) = post_json(mcp_app().await, &body).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body["result"]["isError"], true,
            "tool failure must surface as isError, not a protocol error"
        );
        assert!(
            body.get("error").is_none(),
            "a tool error must never be a JSON-RPC error: {body}"
        );
        let text = body["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default();
        assert!(
            text.contains("no_such_tool"),
            "result text must mention the unknown tool name: {text}"
        );
    }

    // ── Cycle 8: method routing ──────────────────────────────────────

    #[tokio::test]
    async fn get_mcp_is_405_with_allow_header() {
        let resp = mcp_app()
            .await
            .oneshot(request_with_headers("GET", None, None, ""))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(
            resp.headers().get("allow").unwrap(),
            "POST, DELETE",
            "Allow header must list POST and DELETE"
        );
    }

    #[tokio::test]
    async fn delete_mcp_is_200() {
        let resp = mcp_app()
            .await
            .oneshot(request_with_headers("DELETE", None, None, ""))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[test]
    fn openapi_registers_the_three_mcp_operations() {
        // The E2E proves /mcp routes at runtime; this pins that the utoipa
        // wiring in main.rs advertises all three operations in /docs (a
        // dropped `routes!` entry would silently remove one from Swagger).
        use utoipa_axum::router::OpenApiRouter;
        use utoipa_axum::routes;

        let (_router, api) = OpenApiRouter::new()
            .routes(routes!(
                mcp_post_handler,
                mcp_get_handler,
                mcp_delete_handler
            ))
            .split_for_parts();
        let spec = serde_json::to_value(&api).unwrap();
        let mcp = &spec["paths"]["/mcp"];
        assert!(mcp["post"].is_object(), "POST /mcp missing from OpenAPI");
        assert!(mcp["get"].is_object(), "GET /mcp missing from OpenAPI");
        assert!(
            mcp["delete"].is_object(),
            "DELETE /mcp missing from OpenAPI"
        );
    }

    // ── Cycle 9: configuration ───────────────────────────────────────

    #[tokio::test]
    async fn disabled_server_does_not_route_mcp() {
        // Mirrors main.rs: with mcp_enabled = false the route simply is not
        // mounted, so axum's fallback answers 404.
        let dir = TempDir::new().unwrap();
        let (state, _job_rx) = create_test_state_with_rx(dir.path()).await;
        let app = Router::new().with_state(state); // no /mcp route mounted
        let resp = app
            .oneshot(post_request(&initialize_body("2025-06-18")))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    // ── Cycle 11: unit tests for the pure helpers ────────────────────

    #[test]
    fn negotiate_protocol_version_echoes_supported() {
        for v in ["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"] {
            assert_eq!(negotiate_protocol_version(v), v);
        }
    }

    #[test]
    fn negotiate_protocol_version_falls_back_to_latest() {
        assert_eq!(
            negotiate_protocol_version("1999-01-01"),
            ProtocolVersion::latest().to_string()
        );
        assert_eq!(
            negotiate_protocol_version(""),
            ProtocolVersion::latest().to_string()
        );
        assert_eq!(
            negotiate_protocol_version("DRAFT-2026-v1"),
            ProtocolVersion::latest().to_string()
        );
    }
}
