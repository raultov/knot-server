# OpenAPI / Swagger UI Integration via `utoipa`

## Goal

Add auto-generated, interactive API documentation to `knot-server` by integrating
the [`utoipa`](https://crates.io/crates/utoipa) ecosystem. After this change,
navigating to `/docs` in a browser will render a Swagger UI page where users can
browse every endpoint, inspect request/response schemas, and execute live "Try it
out" requests — the same experience as `springdoc-openapi` in Spring Boot.

Additionally, the raw OpenAPI 3.1 JSON spec will be available at
`/api-docs/openapi.json`, enabling Postman imports, client generation, and CI
contract testing.

---

## Dependencies

Add the following crates to `Cargo.toml`:

```toml
utoipa = { version = "5", features = ["axum_extras"] }
utoipa-axum = "0.2"
utoipa-swagger-ui = { version = "9", features = ["axum"] }
```

| Crate | Purpose |
|-------|---------|
| `utoipa` | Proc macros (`#[utoipa::path]`, `#[derive(ToSchema)]`) for generating the OpenAPI spec at compile time |
| `utoipa-axum` | `OpenApiRouter` — collects routes and builds the spec automatically |
| `utoipa-swagger-ui` | Embeds the Swagger UI static assets and serves them as an Axum router |

> **Note:** `utoipa-swagger-ui` embeds the Swagger UI assets at compile time via
> `rust-embed`. In debug builds, it reads them from disk (via `debug-embed`
> feature, which is enabled by default in v9). No external CDN or download needed.

---

## Schemas — `#[derive(ToSchema)]`

Every struct/enum that appears in a request body, response body, or query parameter
needs `#[derive(utoipa::ToSchema)]`.

### File: `src/models.rs`

| Struct/Enum | Role | Notes |
|-------------|------|-------|
| `AuthType` | Enum (`ssh`, `https`) | Add `#[derive(ToSchema)]` |
| `RepoStatus` | Enum (`pending`, `indexed`, `cloning`, `pulling`, `indexing`, `error`) | Add `#[derive(ToSchema)]` |
| `RepoEntry` | Response body for `GET /api/repos/:id` and items in list | Add `#[derive(ToSchema)]`. Mark `webhook_secret` with `#[schema(write_only)]` so it doesn't leak in docs. |
| `RegisterRepoRequest` | Request body for `POST /api/repos` | Add `#[derive(ToSchema)]`. Add `#[schema(example = ...)]` for `url` and `branch`. |
| `RegisterRepoResponse` | Response body for `POST /api/repos` | Add `#[derive(ToSchema)]` |
| `RepoListResponse` | Response body for `GET /api/repos` | Add `#[derive(ToSchema)]` |

#### Example diff for `AuthType`:

```diff
+use utoipa::ToSchema;
+
-#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
+#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, ToSchema)]
 #[serde(rename_all = "lowercase")]
 pub enum AuthType {
     Ssh,
     Https,
 }
```

#### Example diff for `RegisterRepoRequest`:

```diff
-#[derive(Debug, Deserialize)]
+#[derive(Debug, Deserialize, ToSchema)]
 pub struct RegisterRepoRequest {
+    /// Git repository URL (HTTPS, SSH, or local absolute path)
+    #[schema(example = "https://github.com/raultov/knot.git")]
     pub url: String,
+    /// Authentication method
     #[serde(default = "default_auth_type")]
     pub auth_type: AuthType,
+    /// Branch to clone
+    #[schema(example = "main")]
     #[serde(default = "default_branch")]
     pub branch: String,
+    /// Shared secret for webhook signature validation (HMAC-SHA256)
     #[serde(skip_serializing_if = "Option::is_none")]
     pub webhook_secret: Option<String>,
 }
```

### File: `src/handlers.rs`

| Struct | Role | Notes |
|--------|------|-------|
| `ErrorResponse` | Standard error body `{ "error": "..." }` | Add `#[derive(ToSchema)]` |
| `SearchParams` | Query params for `/search` | Add `#[derive(ToSchema, IntoParams)]` |
| `CallersParams` | Query params for `/callers` | Add `#[derive(ToSchema, IntoParams)]` |
| `ExploreParams` | Query params for `/explore` | Add `#[derive(ToSchema, IntoParams)]` |
| `DepsParams` | Query params for `/deps` | Add `#[derive(ToSchema, IntoParams)]` |
| `GraphParams` | Query params for `/graph` | Add `#[derive(ToSchema, IntoParams)]` |
| `GraphExpandParams` | Query params for `/graph/expand` | Add `#[derive(ToSchema, IntoParams)]` |
| `GraphNodeResponse` | Part of graph JSON response | Add `#[derive(ToSchema)]` |
| `GraphEdgeResponse` | Part of graph JSON response | Add `#[derive(ToSchema)]` |
| `GraphResponse` | Response body for `/graph` | Add `#[derive(ToSchema)]` |

#### Query parameter structs — `IntoParams`

For query-parameter structs, use `#[derive(IntoParams)]` in addition to
`ToSchema`. This generates the OpenAPI `parameters` array automatically:

```diff
+use utoipa::{ToSchema, IntoParams};
+
-#[derive(Debug, Deserialize)]
+#[derive(Debug, Deserialize, IntoParams)]
 pub struct SearchParams {
+    /// The search query string
+    #[param(example = "authentication logic")]
     pub q: Option<String>,
+    /// Maximum number of results to return
+    #[param(example = 5)]
     pub max_results: Option<usize>,
 }
```

---

## Handler Annotations — `#[utoipa::path]`

Every public handler function gets a `#[utoipa::path(...)]` attribute. This is
the core of the OpenAPI spec generation.

### Complete handler annotation table

| # | Handler | Method | Path | Tag | Request Body | Responses |
|---|---------|--------|------|-----|--------------|-----------|
| 1 | `list_repos_handler` | GET | `/api/repos` | Repositories | — | 200→`RepoListResponse` |
| 2 | `register_repo_handler` | POST | `/api/repos` | Repositories | `RegisterRepoRequest` | 202→`RegisterRepoResponse`, 409→`ErrorResponse`, 429→`ErrorResponse` |
| 3 | `get_repo_handler` | GET | `/api/repos/{id}` | Repositories | — | 200→`RepoEntry`, 404→`ErrorResponse` |
| 4 | `delete_repo_handler` | DELETE | `/api/repos/{id}` | Repositories | — | 200→`json`, 404→`ErrorResponse` |
| 5 | `sync_repo_handler` | POST | `/api/repos/{id}/sync` | Indexing | — | 202→`json`, 404→`ErrorResponse`, 429→`ErrorResponse` |
| 6 | `search_handler` | GET | `/api/repos/{id}/search` | Search | — (query: `SearchParams`) | 200→`json`, 400→`ErrorResponse` |
| 7 | `callers_handler` | GET | `/api/repos/{id}/callers` | Search | — (query: `CallersParams`) | 200→`json`, 400→`ErrorResponse` |
| 8 | `explore_handler` | GET | `/api/repos/{id}/explore` | Search | — (query: `ExploreParams`) | 200→`json`, 400→`ErrorResponse`, 404→`ErrorResponse` |
| 9 | `deps_handler` | GET | `/api/repos/{id}/deps` | Search | — (query: `DepsParams`) | 200→`json`, 400→`ErrorResponse` |
| 10 | `graph_handler` | GET | `/api/repos/{id}/graph` | Graph | — (query: `GraphParams`) | 200→`GraphResponse`, 400→`ErrorResponse`, 404→`ErrorResponse` |
| 11 | `graph_expand_handler` | GET | `/api/repos/{id}/graph/expand` | Graph | — (query: `GraphExpandParams`) | 200→`GraphResponse`, 400→`ErrorResponse`, 404→`ErrorResponse` |
| 12 | `webhook_handler` | POST | `/api/webhook/{id}` | Webhooks | raw bytes | 202→`json`, 401→`ErrorResponse`, 404→`ErrorResponse` |
| 13 | `health_handler` | GET | `/api/health` | Health | — | 200→`json` |

### Example annotation for `search_handler`:

```rust
#[utoipa::path(
    get,
    path = "/api/repos/{id}/search",
    tag = "Search",
    params(
        ("id" = String, Path, description = "Repository ID"),
        SearchParams,
    ),
    responses(
        (status = 200, description = "Search results", body = serde_json::Value),
        (status = 400, description = "Missing or invalid query parameter", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse),
    ),
    description = "Semantic + structural search. Find code by meaning, class name, method signature, or docstrings.",
)]
pub async fn search_handler(...) -> Response { ... }
```

### Example annotation for `register_repo_handler`:

```rust
#[utoipa::path(
    post,
    path = "/api/repos",
    tag = "Repositories",
    request_body = RegisterRepoRequest,
    responses(
        (status = 202, description = "Repository registered and clone job enqueued", body = RegisterRepoResponse),
        (status = 409, description = "Repository already exists", body = ErrorResponse),
        (status = 429, description = "Indexing queue is full", body = ErrorResponse),
    ),
    description = "Register a new Git repository. The server clones it and queues it for indexing.",
)]
pub async fn register_repo_handler(...) -> Response { ... }
```

### Example annotation for `webhook_handler`:

```rust
#[utoipa::path(
    post,
    path = "/api/webhook/{id}",
    tag = "Webhooks",
    params(
        ("id" = String, Path, description = "Repository ID"),
    ),
    responses(
        (status = 202, description = "Webhook accepted, indexing job enqueued"),
        (status = 401, description = "Invalid or missing webhook signature", body = ErrorResponse),
        (status = 404, description = "Repository not found", body = ErrorResponse),
    ),
    description = "Endpoint for Git provider webhooks (GitHub, GitLab, Bitbucket). Validates payload signatures and triggers incremental re-indexing on push events.",
)]
pub async fn webhook_handler(...) -> Response { ... }
```

### Example annotation for `health_handler`:

```rust
#[utoipa::path(
    get,
    path = "/api/health",
    tag = "Health",
    responses(
        (status = 200, description = "Server health status"),
    ),
    description = "Check server health, uptime, queue capacity, and repository statistics.",
)]
pub async fn health_handler(...) -> Response { ... }
```

---

## Router Integration — `OpenApiRouter`

### File: `src/main.rs`

Replace the current `Router::new()` chain with `OpenApiRouter` from `utoipa-axum`,
which automatically collects the `#[utoipa::path]` metadata from each handler.

#### Current code (lines 147–170):

```rust
let app = Router::new()
    .route("/api/repos", get(handlers::list_repos_handler).post(handlers::register_repo_handler))
    .route("/api/repos/{id}", get(handlers::get_repo_handler).delete(handlers::delete_repo_handler))
    // ... 10 more routes ...
    .with_state(state);
```

#### Proposed code:

```rust
use utoipa::OpenApi;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use utoipa_swagger_ui::SwaggerUi;

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
    .routes(routes!(handlers::list_repos_handler, handlers::register_repo_handler))
    .routes(routes!(handlers::get_repo_handler, handlers::delete_repo_handler))
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
    .merge(SwaggerUi::new("/docs").url("/api-docs/openapi.json", api))
    .with_state(state);
```

#### Key points:
- The `routes!()` macro reads the `#[utoipa::path]` annotations from each handler
  and registers both the Axum route and the OpenAPI path entry.
- Handlers that share the same path (e.g., `list_repos_handler` + `register_repo_handler`
  on `/api/repos`) must go in the **same** `routes!()` call.
- Non-API routes (`/favicon.ico`, `/graph`) stay as regular `.route()` calls —
  they don't need OpenAPI documentation.
- `SwaggerUi::new("/docs")` serves the interactive UI at `http://localhost:3000/docs`.
- The raw spec is served at `http://localhost:3000/api-docs/openapi.json`.

---

## Test Router

### File: `src/handlers.rs` — `build_test_app()` (line ~1182)

The test helper `build_test_app()` constructs a `Router` without Swagger UI. This
function does **not** need `OpenApiRouter` — it can stay as plain `Router::new()`.
The `#[utoipa::path]` attributes on the handlers are inert when used with a plain
`Router`, so no test changes are needed.

---

## Graph Viewer — Discreet Link to API Docs

### File: `assets/graph-viewer.html`

Add a subtle link to `/docs` in the **status bar** at the bottom of the graph
viewer. The status bar (line ~263) is the thin strip at the very bottom of the
screen — low-profile, not competing with the graph for attention.

#### Current HTML (line ~263):

```html
<div id="status-bar">Ready</div>
```

#### Proposed HTML:

```html
<div id="status-bar">
  <span id="status-text">Ready</span>
  <a href="/docs" target="_blank" style="float: right; color: #555; text-decoration: none; font-size: 10px;" title="Open API documentation (Swagger UI)">API Docs ↗</a>
</div>
```

The link floats to the far right of the status bar, uses a muted `#555` color
and `10px` font to stay out of the way. On hover, it should lighten slightly.
Add this CSS rule inside the existing `<style>` block:

```css
#status-bar a:hover { color: #aaa; }
```

The `setStatus()` function (line ~557) needs a one-line update to target the
inner `<span>` instead of the whole `<div>`:

```diff
 function setStatus(msg) {
-  document.getElementById('status-bar').textContent = msg;
+  document.getElementById('status-text').textContent = msg;
 }
```

---

## Files Changed — Summary

| File | Changes |
|------|---------|
| `Cargo.toml` | Add `utoipa`, `utoipa-axum`, `utoipa-swagger-ui` dependencies |
| `src/models.rs` | Add `#[derive(ToSchema)]` to `AuthType`, `RepoStatus`, `RepoEntry`, `RegisterRepoRequest`, `RegisterRepoResponse`, `RepoListResponse` (6 types) |
| `src/handlers.rs` | Add `#[derive(ToSchema)]` to `ErrorResponse`, `GraphNodeResponse`, `GraphEdgeResponse`, `GraphResponse` (4 types). Add `#[derive(IntoParams)]` to `SearchParams`, `CallersParams`, `ExploreParams`, `DepsParams`, `GraphParams`, `GraphExpandParams` (6 types). Add `#[utoipa::path(...)]` to 13 handler functions. |
| `src/main.rs` | Replace `Router::new()` with `OpenApiRouter`, add `SwaggerUi` merge, add `ApiDoc` struct |
| `assets/graph-viewer.html` | Add discreet "API Docs ↗" link in the status bar, update `setStatus()` to target inner `<span>` |
| `README.md` | Document `/docs` endpoint in the API section |

No new source files are created. No existing tests should break.

---

## Binary Size Impact

`utoipa-swagger-ui` embeds the Swagger UI static assets (~3.5 MB gzipped) into
the binary via `rust-embed`. This increases the release binary size by
approximately **3–4 MB**. This is acceptable for a server binary.

If binary size becomes a concern in the future, the Swagger UI can be served from
a CDN instead by using the `url` feature of `utoipa-swagger-ui` (not recommended
for self-hosted/offline deployments).

---

## Verification Plan

### Automated
1. `cargo build` — confirms all macros expand correctly.
2. `cargo test` — confirms existing tests still pass (no handler signatures changed).
3. `cargo clippy` — no new warnings.

### Manual
1. Start the server: `cargo run`
2. Navigate to `http://localhost:3000/docs` — Swagger UI should render with all 13 endpoints.
3. Verify tags: endpoints should be grouped under "Repositories", "Search", "Graph", "Indexing", "Webhooks", "Health".
4. Click "Try it out" on `GET /api/health` — should return 200 with JSON body.
5. Click "Try it out" on `POST /api/repos` — fill in request body, verify 202 response.
6. Fetch `http://localhost:3000/api-docs/openapi.json` — verify valid OpenAPI 3.1 JSON.
7. Import `openapi.json` into Postman — verify all endpoints appear.
8. Navigate to `http://localhost:3000/graph` — verify the "API Docs ↗" link appears at the bottom-right of the status bar and opens `/docs` in a new tab.

### Docker
1. `docker compose -f docker-compose.yml -f docker-compose.dev.yml up --build`
2. Navigate to `http://localhost:3000/docs` — verify Swagger UI works in the container.

---

## Migration Path for the Postman Collection

After this feature is implemented:

1. The Postman collection (`knot-server.postman_collection.json`) can still be kept
   in the repo as a convenience for users who prefer Postman.
2. The `openapi.json` served by the running server becomes the **canonical** API
   contract. Users can import it directly into Postman, Insomnia, or any OpenAPI
   tool.
3. Long-term, the Postman collection can be deprecated in favor of the OpenAPI spec.
