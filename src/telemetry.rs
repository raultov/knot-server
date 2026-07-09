//! OpenTelemetry distributed tracing.
//!
//! This module owns everything OTel-related, mirroring how [`crate::metrics`]
//! owns Prometheus metrics. The codebase already uses `tracing` for structured
//! logging; here we bridge those spans into OpenTelemetry with
//! `tracing-opentelemetry` rather than using the OTel API directly, so log
//! output stays untouched and every `tracing::info!/warn!` inside a span
//! becomes a span event for free.
//!
//! Behavior contract: with `KNOT_SERVER_TRACING_ENABLED=false` (the default),
//! [`init_tracer_provider`] is never called, [`init_subscriber`] installs no
//! OTel layer, and [`trace_http`] is not wired in — the server behaves exactly
//! as it did before tracing existed (no exporter, no background tasks, no
//! connection attempts).

use axum::extract::MatchedPath;
use axum::http::HeaderMap;
use axum::middleware::Next;
use axum::response::Response;

use opentelemetry::KeyValue;
use opentelemetry::propagation::Extractor;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::{SpanExporter, WithExportConfig};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::{Sampler, SdkTracerProvider};

use tracing::Instrument;
use tracing_opentelemetry::OpenTelemetrySpanExt;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use crate::config::ServerConfig;

/// Build the OTLP tracer provider. Only called when tracing is enabled.
///
/// - OTLP span exporter over gRPC (tonic), pointed at `cfg.otlp_endpoint`.
/// - Batch span processor (in opentelemetry 0.32 the batch processor runs on
///   its own dedicated background thread — no runtime handle is threaded in).
/// - Resource: `service.name = "knot-server"`, `service.version`, and a custom
///   `knot.version` attribute.
/// - Sampler: `ParentBased(TraceIdRatioBased(ratio))`, ratio clamped to
///   `[0.0, 1.0]`.
/// - W3C `TraceContext` installed as the global propagator.
pub fn init_tracer_provider(cfg: &ServerConfig) -> anyhow::Result<SdkTracerProvider> {
    let exporter = SpanExporter::builder()
        .with_tonic()
        .with_endpoint(cfg.otlp_endpoint.clone())
        .build()?;

    let resource = Resource::builder()
        .with_service_name("knot-server")
        .with_attributes([
            KeyValue::new("service.version", env!("CARGO_PKG_VERSION")),
            KeyValue::new("knot.version", env!("KNOT_VERSION")),
        ])
        .build();

    // Clamp instead of failing. The subscriber is not installed yet at this
    // point (see main()'s ordering), so a `tracing::warn!` here would be lost —
    // use eprintln! for the one-off startup warning.
    let ratio = cfg.trace_sample_ratio;
    let ratio = if (0.0..=1.0).contains(&ratio) {
        ratio
    } else {
        let clamped = ratio.clamp(0.0, 1.0);
        eprintln!(
            "KNOT_SERVER_TRACE_SAMPLE_RATIO={ratio} is out of range [0.0, 1.0], clamping to {clamped}"
        );
        clamped
    };
    let sampler = Sampler::ParentBased(Box::new(Sampler::TraceIdRatioBased(ratio)));

    let provider = SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(resource)
        .with_sampler(sampler)
        .build();

    // Let clients (e.g. knot-mcp) propagate their trace context to us.
    opentelemetry::global::set_text_map_propagator(TraceContextPropagator::new());

    Ok(provider)
}

/// Install the global tracing subscriber as a layered registry: env-filter +
/// fmt (unchanged log output) + an optional OpenTelemetry layer. `Option<L>`
/// implements `Layer`, so the OTel layer composes away cleanly when tracing is
/// disabled.
///
/// Replaces the old `setup_tracing()` in `main.rs`. Must run *after*
/// [`init_tracer_provider`] so the provider's tracer can back the OTel layer.
pub fn init_subscriber(provider: Option<&SdkTracerProvider>) {
    let otel_layer =
        provider.map(|p| tracing_opentelemetry::layer().with_tracer(p.tracer("knot-server")));

    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(tracing_subscriber::fmt::layer())
        .with(otel_layer)
        .init();
}

/// Flush and shut down the tracer provider so the last batch of spans is not
/// dropped. Called from `main()` after graceful shutdown. `shutdown()` blocks
/// while it joins the exporter thread; in opentelemetry 0.32 that thread owns
/// its own runtime, so calling it from the async context is safe, but we still
/// hand it to `spawn_blocking` to keep the async worker unblocked.
pub async fn shutdown(provider: SdkTracerProvider) {
    let result = tokio::task::spawn_blocking(move || provider.shutdown()).await;
    match result {
        Ok(Ok(())) => {}
        Ok(Err(e)) => eprintln!("OTel tracer shutdown error: {e}"),
        Err(e) => eprintln!("OTel tracer shutdown task panicked: {e}"),
    }
}

/// Paths that should not produce a root span: infra/monitoring endpoints and
/// static UI assets that would only add noise to the trace backend.
fn should_skip(path: &str) -> bool {
    path == "/metrics"
        || path == "/favicon.ico"
        || path == "/graph"
        || path.starts_with("/docs")
        || path.starts_with("/api-docs")
        || path.starts_with("/assets")
}

/// Adapts an `axum` [`HeaderMap`] to the OTel [`Extractor`] trait so the W3C
/// propagator can read `traceparent`/`tracestate` from the incoming request.
/// A manual impl avoids pulling in the extra `opentelemetry-http` dependency.
struct HeaderExtractor<'a>(&'a HeaderMap);

impl Extractor for HeaderExtractor<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|v| v.to_str().ok())
    }

    fn keys(&self) -> Vec<&str> {
        self.0.keys().map(|k| k.as_str()).collect()
    }
}

/// HTTP root-span middleware, analogous to [`crate::metrics::track_http`].
///
/// Creates one span per request named `{METHOD} {route}` (OTel semconv), sets
/// its parent from any inbound W3C context, and records response status. It is
/// layered *outside* `metrics::track_http` so the metrics timing runs inside
/// the span (the ordering between the two is otherwise unimportant).
pub async fn trace_http(req: axum::extract::Request, next: Next) -> Response {
    if should_skip(req.uri().path()) {
        return next.run(req).await;
    }

    let method = req.method().clone();
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map(|mp| crate::metrics::intern_route(mp.as_str()))
        .unwrap_or("unmatched")
        .to_string();

    let parent_cx = opentelemetry::global::get_text_map_propagator(|prop| {
        prop.extract(&HeaderExtractor(req.headers()))
    });

    let span = tracing::info_span!(
        "http_request",
        otel.name = tracing::field::Empty,
        otel.kind = "server",
        otel.status_code = tracing::field::Empty,
        http.request.method = %method,
        http.route = %route,
        http.response.status_code = tracing::field::Empty,
    );
    // Parent the span on the remote context. Errors only when no OTel layer is
    // installed (e.g. under a test subscriber); trace_http is wired only when
    // tracing is enabled, so in production the layer is always present.
    let _ = span.set_parent(parent_cx);
    // OTel span name is dynamic; the tracing span name must be static, so we
    // override it via the special `otel.name` field.
    span.record("otel.name", format!("{} {route}", method.as_str()).as_str());

    async move {
        let response = next.run(req).await;
        let status = response.status();
        let span = tracing::Span::current();
        span.record("http.response.status_code", status.as_u16());
        if status.is_server_error() {
            span.record("otel.status_code", "ERROR");
        }
        response
    }
    .instrument(span)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::get;
    use opentelemetry::propagation::TextMapPropagator;
    use opentelemetry::trace::TraceContextExt;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use tower::util::ServiceExt;
    use tracing_subscriber::Layer;
    use tracing_subscriber::layer::Context;

    #[test]
    fn should_skip_excludes_infra_and_assets() {
        for p in [
            "/metrics",
            "/favicon.ico",
            "/graph",
            "/docs",
            "/docs/swagger-ui.css",
            "/api-docs/openapi.json",
            "/assets/app.js",
        ] {
            assert!(should_skip(p), "expected {p} to be skipped");
        }
    }

    #[test]
    fn should_skip_allows_api_routes() {
        for p in ["/api/repos", "/api/health", "/api/repos/x/search"] {
            assert!(!should_skip(p), "expected {p} to be traced");
        }
    }

    #[test]
    fn header_extractor_reads_and_lists_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "traceparent",
            "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01"
                .parse()
                .unwrap(),
        );
        let ex = HeaderExtractor(&headers);
        assert_eq!(
            ex.get("traceparent"),
            Some("00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01")
        );
        assert!(ex.keys().contains(&"traceparent"));
    }

    #[test]
    fn traceparent_extraction_yields_valid_remote_context() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "traceparent",
            "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01"
                .parse()
                .unwrap(),
        );
        let prop = TraceContextPropagator::new();
        let cx = prop.extract(&HeaderExtractor(&headers));
        let binding = cx.span();
        let sc = binding.span_context();
        assert!(sc.is_valid(), "expected a valid remote span context");
        assert_eq!(
            sc.trace_id().to_string(),
            "0af7651916cd43dd8448eb211c80319c"
        );
    }

    /// Captures `(span_name, fields)` for every span created under this
    /// subscriber, so tests can assert what `trace_http` produced without an
    /// OTel provider or any network.
    #[derive(Clone, Default)]
    #[allow(clippy::type_complexity)]
    struct Recorder(Arc<Mutex<Vec<(String, HashMap<String, String>)>>>);

    struct FieldVisitor(HashMap<String, String>);

    impl tracing::field::Visit for FieldVisitor {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            self.0
                .insert(field.name().to_string(), format!("{value:?}"));
        }
        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            self.0.insert(field.name().to_string(), value.to_string());
        }
        fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
            self.0.insert(field.name().to_string(), value.to_string());
        }
    }

    impl<S: tracing::Subscriber> Layer<S> for Recorder {
        fn on_new_span(
            &self,
            attrs: &tracing::span::Attributes<'_>,
            _id: &tracing::span::Id,
            _ctx: Context<'_, S>,
        ) {
            let mut v = FieldVisitor(HashMap::new());
            attrs.record(&mut v);
            self.0
                .lock()
                .unwrap()
                .push((attrs.metadata().name().to_string(), v.0));
        }
    }

    #[tokio::test]
    async fn trace_http_creates_span_with_method_and_route() {
        let recorder = Recorder::default();
        let subscriber = tracing_subscriber::registry().with(recorder.clone());
        let _guard = tracing::subscriber::set_default(subscriber);

        let app = Router::new()
            .route("/api/health", get(|| async { "ok" }))
            .layer(axum::middleware::from_fn(trace_http));
        let req = Request::builder()
            .uri("/api/health")
            .body(Body::empty())
            .unwrap();
        let _ = app.oneshot(req).await.unwrap();

        let spans = recorder.0.lock().unwrap();
        let (_, fields) = spans
            .iter()
            .find(|(name, _)| name == "http_request")
            .expect("trace_http should create an http_request span");
        assert_eq!(
            fields.get("http.request.method").map(String::as_str),
            Some("GET")
        );
        assert_eq!(
            fields.get("http.route").map(String::as_str),
            Some("/api/health")
        );
    }

    #[tokio::test]
    async fn trace_http_skips_excluded_paths() {
        let recorder = Recorder::default();
        let subscriber = tracing_subscriber::registry().with(recorder.clone());
        let _guard = tracing::subscriber::set_default(subscriber);

        let app = Router::new()
            .route("/metrics", get(|| async { "m" }))
            .layer(axum::middleware::from_fn(trace_http));
        let req = Request::builder()
            .uri("/metrics")
            .body(Body::empty())
            .unwrap();
        let _ = app.oneshot(req).await.unwrap();

        let spans = recorder.0.lock().unwrap();
        assert!(
            !spans.iter().any(|(name, _)| name == "http_request"),
            "skip-list paths must not produce a span"
        );
    }
}
