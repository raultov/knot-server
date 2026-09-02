use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::MatchedPath;
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use metrics::{counter, describe_counter, describe_gauge, describe_histogram, gauge, histogram};
use metrics_exporter_prometheus::{Matcher, PrometheusBuilder, PrometheusHandle};

use crate::models::{AppState, RepoStatus};

pub(crate) const KNOWN_ROUTES: &[&str] = &[
    "/api/repos",
    "/api/repos/{id}",
    "/api/repos/{id}/sync",
    "/api/repos/{id}/progress",
    "/api/progress",
    "/api/repos/{id}/search",
    "/api/repos/{id}/callers",
    "/api/search",
    "/api/callers",
    "/api/repos/{id}/explore",
    "/api/repos/{id}/deps",
    "/api/repos/{id}/graph",
    "/api/repos/{id}/graph/expand",
    "/api/repos/{id}/graph/repos",
    "/api/webhook/{id}",
    "/api/health",
];

pub(crate) fn intern_route(path: &str) -> &str {
    KNOWN_ROUTES
        .iter()
        .find(|&&r| r == path)
        .copied()
        .unwrap_or("unmatched")
}

#[derive(Debug, Clone, Copy)]
pub enum JobKind {
    Clone,
    Pull,
}

impl JobKind {
    fn as_str(self) -> &'static str {
        match self {
            JobKind::Clone => "clone",
            JobKind::Pull => "pull",
        }
    }
}

struct InFlightGuard;

impl InFlightGuard {
    fn new() -> Self {
        gauge!("knot_http_requests_in_flight").increment(1.0);
        InFlightGuard
    }
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        gauge!("knot_http_requests_in_flight").decrement(1.0);
    }
}

pub fn init() -> anyhow::Result<PrometheusHandle> {
    let handle = PrometheusBuilder::new()
        .set_buckets_for_metric(
            Matcher::Full("knot_http_request_duration_seconds".into()),
            &[
                0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
            ],
        )?
        .set_buckets_for_metric(
            Matcher::Full("knot_indexing_duration_seconds".into()),
            &[
                1.0, 5.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 1800.0, 3600.0,
            ],
        )?
        .install_recorder()?;
    describe_metrics();
    Ok(handle)
}

fn describe_metrics() {
    describe_counter!(
        "knot_http_requests_total",
        "Total number of HTTP requests processed"
    );
    describe_histogram!(
        "knot_http_request_duration_seconds",
        "HTTP request latency in seconds"
    );
    describe_gauge!(
        "knot_http_requests_in_flight",
        "Number of HTTP requests currently being processed"
    );

    describe_counter!(
        "knot_indexing_jobs_total",
        "Total number of indexing jobs processed"
    );
    describe_histogram!(
        "knot_indexing_duration_seconds",
        "Indexing job duration in seconds"
    );
    describe_gauge!(
        "knot_indexing_percent_complete",
        "Indexing progress as a percentage (0-100)"
    );
    describe_gauge!(
        "knot_indexing_parsed_files",
        "Number of files parsed during current indexing run"
    );
    describe_gauge!(
        "knot_indexing_total_files",
        "Total number of files to parse during current indexing run"
    );
    describe_gauge!(
        "knot_indexing_entities_ingested",
        "Number of entities ingested during current indexing run"
    );
    describe_gauge!(
        "knot_indexing_last_success_timestamp_seconds",
        "Unix timestamp of the last successful indexing completion"
    );

    describe_gauge!(
        "knot_repositories_total",
        "Total number of registered repositories"
    );
    describe_gauge!(
        "knot_repositories_by_status",
        "Number of repositories grouped by current status"
    );
    describe_gauge!(
        "knot_queue_available_capacity",
        "Remaining capacity in the job queue"
    );
    describe_gauge!(
        "knot_process_uptime_seconds",
        "Server process uptime in seconds"
    );
    describe_gauge!(
        "knot_build_info",
        "Build metadata: server and knot library versions"
    );
}

fn refresh_runtime_gauges(state: &AppState) {
    let mut registry = state.registry.lock().unwrap();
    let repos = registry.list();

    let total = repos.len();
    gauge!("knot_repositories_total").set(total as f64);

    let all_statuses: [(RepoStatus, &str); 7] = [
        (RepoStatus::Pending, "pending"),
        (RepoStatus::Queued, "queued"),
        (RepoStatus::Indexed, "indexed"),
        (RepoStatus::Cloning, "cloning"),
        (RepoStatus::Pulling, "pulling"),
        (RepoStatus::Indexing, "indexing"),
        (RepoStatus::Error, "error"),
    ];

    for (status, label) in &all_statuses {
        let count = repos.iter().filter(|r| r.status == *status).count();
        gauge!("knot_repositories_by_status", "status" => *label).set(count as f64);
    }

    drop(registry);

    gauge!("knot_queue_available_capacity").set(state.job_tx.capacity() as f64);
    gauge!("knot_process_uptime_seconds").set(state.start_time.elapsed().as_secs_f64());
}

pub async fn metrics_handler(state: Arc<AppState>, handle: PrometheusHandle) -> impl IntoResponse {
    refresh_runtime_gauges(&state);
    (
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        handle.render(),
    )
}

pub async fn track_http(req: axum::extract::Request, next: Next) -> Response {
    let method = req.method().clone();
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map(|mp| intern_route(mp.as_str()))
        .unwrap_or("unmatched")
        .to_string();
    let method_str = method.as_str().to_string();

    let _guard = InFlightGuard::new();
    let start = Instant::now();
    let resp = next.run(req).await;

    record_http_request(route, method_str, resp.status(), start.elapsed());
    resp
}

pub fn record_http_request(route: String, method: String, status: StatusCode, dur: Duration) {
    let status_str = status.as_u16().to_string();

    let counter_route = route.clone();
    let counter_method = method.clone();
    counter!(
        "knot_http_requests_total",
        "route" => counter_route,
        "method" => counter_method,
        "status" => status_str,
    )
    .increment(1);

    histogram!(
        "knot_http_request_duration_seconds",
        "route" => route,
        "method" => method,
    )
    .record(dur.as_secs_f64());
}

pub fn record_indexing_job(repo_id: &str, kind: JobKind, ok: bool, dur: Duration) {
    let result = if ok { "ok" } else { "err" };
    let kind_str = kind.as_str();

    counter!(
        "knot_indexing_jobs_total",
        "repo_id" => repo_id.to_string(),
        "kind" => kind_str,
        "result" => result,
    )
    .increment(1);

    histogram!(
        "knot_indexing_duration_seconds",
        "kind" => kind_str,
        "result" => result,
    )
    .record(dur.as_secs_f64());
}

pub fn set_indexing_progress(
    repo_id: &str,
    stage: &str,
    snap: &knot::pipeline::progress::IndexingProgress,
) {
    let repo_str = repo_id.to_string();
    let stage_str = stage.to_string();
    gauge!("knot_indexing_percent_complete", "repo_id" => repo_str.clone(), "stage" => stage_str)
        .set(snap.percent_complete as f64);
    gauge!("knot_indexing_parsed_files", "repo_id" => repo_str.clone())
        .set(snap.parsed_files as f64);
    gauge!("knot_indexing_total_files", "repo_id" => repo_str.clone()).set(snap.total_files as f64);
    gauge!("knot_indexing_entities_ingested", "repo_id" => repo_str)
        .set(snap.entities_ingested as f64);
}

pub fn set_last_success(repo_id: &str) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    gauge!("knot_indexing_last_success_timestamp_seconds", "repo_id" => repo_id.to_string())
        .set(now);
}

pub fn set_build_info() {
    gauge!(
        "knot_build_info",
        "version" => env!("CARGO_PKG_VERSION"),
        "knot_version" => env!("KNOT_VERSION"),
    )
    .set(1.0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use knot::db::graph::ConnectExt;
    use knot::db::vector::VectorConnectExt;
    use metrics_util::debugging::{DebuggingRecorder, Snapshotter};
    use std::sync::OnceLock;

    /// `metrics::set_global_recorder` only succeeds once per process, so we
    /// share a single `DebuggingRecorder` across the parallel test threads via
    /// `OnceLock`. Tests grab the snapshotter at the start, install once on
    /// first access, and use the snapshotter afterwards.
    fn shared_snapshotter() -> &'static Snapshotter {
        static SNAPSHOTTER: OnceLock<Snapshotter> = OnceLock::new();
        SNAPSHOTTER.get_or_init(|| {
            let recorder = DebuggingRecorder::new();
            let snapshotter = recorder.snapshotter();
            // Ignore the result: another concurrent test may have raced us and
            // installed its own recorder first, which is fine — the snapshotter
            // we keep around still works against whichever recorder is live.
            let _ = recorder.install();
            snapshotter
        })
    }

    #[test]
    fn test_intern_route_matches_known_paths() {
        assert_eq!(intern_route("/api/repos"), "/api/repos");
        assert_eq!(intern_route("/api/repos/{id}"), "/api/repos/{id}");
        assert_eq!(intern_route("/api/health"), "/api/health");
    }

    #[test]
    fn test_intern_route_falls_back_to_unmatched() {
        assert_eq!(intern_route("/metrics"), "unmatched");
        assert_eq!(intern_route("/docs/swagger-ui.css"), "unmatched");
        assert_eq!(intern_route("/favicon.ico"), "unmatched");
    }

    #[test]
    fn intern_route_maps_global_search() {
        assert_eq!(intern_route("/api/search"), "/api/search");
    }

    #[test]
    fn intern_route_maps_global_callers() {
        assert_eq!(intern_route("/api/callers"), "/api/callers");
    }

    #[test]
    fn known_routes_has_no_duplicates() {
        let mut seen = std::collections::HashSet::new();
        for route in KNOWN_ROUTES {
            assert!(seen.insert(*route), "duplicate KNOWN_ROUTES entry: {route}");
        }
    }

    /// Compile-time-extracted handler sources. A **new handler file** must be
    /// added to this list; the guard catches new routes declared in existing
    /// files, which is the common case.
    const HANDLER_SOURCES: &[&str] = &[
        include_str!("handlers/repo.rs"),
        include_str!("handlers/indexing.rs"),
        include_str!("handlers/progress.rs"),
        include_str!("handlers/search.rs"),
        include_str!("handlers/graph.rs"),
        include_str!("handlers/repo_graph.rs"),
        include_str!("handlers/webhooks.rs"),
        include_str!("handlers/health.rs"),
    ];

    /// Drift guard: every `/api/...` path literal declared in a
    /// `#[utoipa::path(... path = "…")]` attribute must appear in
    /// KNOWN_ROUTES. Extraction is plain string splitting on `path = "` up
    /// to the next `"` — no regex dependency.
    #[test]
    fn known_routes_covers_every_declared_api_path() {
        let mut declared: Vec<String> = Vec::new();
        for source in HANDLER_SOURCES {
            let mut rest: &str = source;
            while let Some(pos) = rest.find("path = \"") {
                rest = &rest[pos + "path = \"".len()..];
                let end = rest
                    .find('"')
                    .expect("utoipa path attribute must close its path string");
                let path = &rest[..end];
                if path.starts_with("/api/") {
                    declared.push(path.to_string());
                }
            }
        }
        assert!(
            !declared.is_empty(),
            "extraction found no declared /api paths — the parse pattern drifted"
        );
        for path in &declared {
            assert!(
                KNOWN_ROUTES.contains(&path.as_str()),
                "route '{path}' is declared by a handler but missing from KNOWN_ROUTES; \
                 its metrics would be counted under 'unmatched'"
            );
        }
    }

    #[test]
    fn test_record_http_request_emits_counter_and_histogram() {
        let snapshotter = shared_snapshotter();
        let before = snapshotter.snapshot().into_hashmap();

        record_http_request(
            "/api/repos".to_string(),
            "GET".to_string(),
            StatusCode::OK,
            Duration::from_millis(150),
        );

        let after = snapshotter.snapshot().into_hashmap();
        assert!(
            after.len() > before.len(),
            "expected new metrics to be recorded: before={before:?} after={after:?}"
        );
    }

    #[test]
    fn test_refresh_runtime_gauges_does_not_panic() {
        use crate::models::{AuthType, RepoEntry};
        use crate::registry::Registry;
        use std::collections::HashMap;
        use std::sync::Mutex;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let mut registry = Registry::load_or_create(dir.path()).unwrap();

        for (i, status) in [
            RepoStatus::Pending,
            RepoStatus::Queued,
            RepoStatus::Indexed,
            RepoStatus::Cloning,
            RepoStatus::Pulling,
            RepoStatus::Indexing,
            RepoStatus::Error,
        ]
        .iter()
        .enumerate()
        {
            let repo = RepoEntry {
                id: format!("repo-{i}"),
                url: format!("https://example.com/repo-{i}.git"),
                local_path: format!("/tmp/repo-{i}"),
                auth_type: AuthType::Ssh,
                branch: "main".into(),
                webhook_secret: None,
                last_indexed: None,
                status: status.clone(),
            };
            registry.add_or_replace(repo).unwrap();
        }

        let (job_tx, _rx) = tokio::sync::mpsc::channel::<crate::models::IndexJob>(16);
        let state = AppState {
            vector_db: Arc::new(tokio::runtime::Runtime::new().unwrap().block_on(async {
                knot::db::vector::VectorDb::connect("http://localhost:9999", "test", 384)
                    .await
                    .unwrap()
            })),
            graph_db: Arc::new(tokio::runtime::Runtime::new().unwrap().block_on(async {
                knot::db::graph::GraphDb::connect("bolt://localhost:9999", "neo4j", "bad")
                    .await
                    .unwrap()
            })),
            embedder: None,
            workspace_dir: dir.path().to_string_lossy().into(),
            registry: Arc::new(Mutex::new(registry)),
            job_tx,
            qdrant_url: "http://localhost:6334".into(),
            qdrant_collection: "test".into(),
            neo4j_uri: "bolt://localhost:7687".into(),
            neo4j_user: "neo4j".into(),
            neo4j_password: "secret".into(),
            embed_dim: 384,
            rayon_threads: None,
            batch_size: 64,
            ingest_concurrency: 4,
            start_time: Instant::now(),
            progress_trackers: Arc::new(Mutex::new(HashMap::new())),
        };

        refresh_runtime_gauges(&state);
    }
}
