use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

pub async fn favicon_handler() -> Response {
    const FAVICON_BYTES: &[u8] = include_bytes!("../../assets/favicon.png");

    Response::builder()
        .header("content-type", "image/png")
        .body(axum::body::Body::from(FAVICON_BYTES))
        .unwrap()
}

pub async fn graph_viewer_handler() -> Response {
    (
        StatusCode::OK,
        [("content-type", "text/html; charset=utf-8")],
        GRAPH_VIEWER_HTML.as_str(),
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
