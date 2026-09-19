use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::models::AppState;

pub async fn favicon_handler() -> Response {
    const FAVICON_BYTES: &[u8] = include_bytes!("../../assets/favicon.png");

    Response::builder()
        .header("content-type", "image/png")
        .body(axum::body::Body::from(FAVICON_BYTES))
        .unwrap()
}

pub async fn graph_viewer_handler(State(state): State<Arc<AppState>>) -> Response {
    // The version is compile-time (a `LazyLock`), but the embedding model and
    // its dimension are resolved at startup, so they are substituted per
    // request from the running state.
    let html = GRAPH_VIEWER_HTML
        .replace("{{KNOT_EMBED_MODEL}}", &state.embed_model)
        .replace("{{KNOT_EMBED_DIM}}", &state.embed_dim.to_string());
    (
        StatusCode::OK,
        [("content-type", "text/html; charset=utf-8")],
        html,
    )
        .into_response()
}

const GRAPH_VIEWER_HTML_TEMPLATE: &str = include_str!("../../assets/graph-viewer.html");

static GRAPH_VIEWER_HTML: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    GRAPH_VIEWER_HTML_TEMPLATE.replace("{{KNOT_SERVER_VERSION}}", env!("CARGO_PKG_VERSION"))
});

pub async fn docs_handler() -> Response {
    const DOCS_HTML_TEMPLATE: &str = include_str!("../../assets/swagger-ui.html");
    static DOCS_HTML: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
        DOCS_HTML_TEMPLATE.replace("{{KNOT_VERSION}}", env!("KNOT_VERSION"))
    });
    axum::response::Html(DOCS_HTML.as_str()).into_response()
}
