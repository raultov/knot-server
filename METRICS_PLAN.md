# Plan de integración de métricas Prometheus en knot-server

> **Estado:** plan aprobado, pendiente de implementación.
> **Decisiones cerradas:**
> 1. El endpoint `/metrics` se sirve en el **mismo puerto de la app (3000)**, no en un listener dedicado.
> 2. `/metrics` **sin autenticación** (despliegue en red interna).
> 3. Las trazas OpenTelemetry quedan **diferidas a la Fase 4** — este plan cubre solo métricas.
> 4. Stack elegido: crates **`metrics` + `metrics-exporter-prometheus`** (agnóstico del framework), descartando `axum-prometheus` para poder emitir métricas por igual desde los handlers HTTP y desde el worker/scheduler (tareas Tokio fuera del ciclo request/response).

---

## 1. Contexto actual

- `Cargo.toml` no tiene ninguna dependencia de métricas; la observabilidad se limita a `tracing` + `tracing-subscriber` como logs planos.
- `axum 0.8.9`, `tokio` full, `utoipa`/`utoipa-axum` para OpenAPI. No hay `tower-http`.
- `AppState` (`src/models.rs:155-173`) ya centraliza todo lo que las métricas necesitan leer: `registry` (repos y sus estados), `job_tx` (capacidad de cola), `start_time` (uptime) y `progress_trackers`.
- `handlers/health.rs` ya calcula conteos por estado (cloning/pulling/indexing) para el JSON de `/api/health`: las métricas deben reutilizar la misma fuente (el registry) sin duplicar lógica ni introducir nuevos locks.
- El router se construye en `src/main.rs:133-163`: las rutas API van dentro de `OpenApiRouter` (aparecen en Swagger); las no-API (`/favicon.ico`, `/graph`) se añaden después con `.route(...)`. **`/metrics` debe añadirse en este segundo grupo** para no contaminar la spec OpenAPI.

## 2. Dependencias nuevas

```toml
[dependencies]
metrics = "0.24"
metrics-exporter-prometheus = { version = "0.16", default-features = false }
```

- `metrics`: fachada ligera estándar del ecosistema Rust. Expone las macros `counter!`, `gauge!`, `histogram!` y `describe_*!`, utilizables desde cualquier punto del código sin acoplarse a axum.
- `metrics-exporter-prometheus`: recorder global que agrega las series y las renderiza en formato de texto Prometheus (`PrometheusHandle::render()`).
- `default-features = false` desactiva el `http-listener` propio del crate (y el push-gateway): no lo necesitamos porque servimos `/metrics` desde el router axum existente en el puerto 3000.

> **Verificar al implementar:** la pareja de versiones compatible vigente en crates.io (`metrics` 0.24 ↔ `metrics-exporter-prometheus` 0.16/0.17). Ambos crates deben resolverse contra la **misma** versión de `metrics` o el recorder global no capturará las macros.

## 3. Nuevo módulo `src/metrics.rs`

Único módulo nuevo (~150 líneas + tests). Responsabilidades:

### 3.1. Inicialización

```rust
pub fn init() -> anyhow::Result<PrometheusHandle> {
    let handle = PrometheusBuilder::new()
        .set_buckets_for_metric(
            Matcher::Full("knot_http_request_duration_seconds".into()),
            &[0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0],
        )?
        .set_buckets_for_metric(
            Matcher::Full("knot_indexing_duration_seconds".into()),
            &[1.0, 5.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 1800.0, 3600.0],
        )?
        .install_recorder()?;
    describe_metrics();
    Ok(handle)
}
```

- `install_recorder()` registra el recorder **global del proceso** — solo puede llamarse una vez. Se invoca desde `main()` justo después de `setup_tracing()`.
- `describe_metrics()` privada: una llamada `describe_counter!`/`describe_gauge!`/`describe_histogram!` por métrica con su texto HELP (sección 4).
- Sin buckets explícitos, el exporter usa summaries/quantiles por defecto; los definimos nosotros para poder usar `histogram_quantile()` en Grafana.

### 3.2. Handler de `/metrics` (render + gauges bajo demanda)

Los gauges derivados de estado (repos por estado, capacidad de cola, uptime) **se recalculan en el momento del scrape**, no con tareas de fondo. El handler:

```rust
pub async fn metrics_handler(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    refresh_runtime_gauges(&state);       // lee registry + job_tx + start_time
    let handle = /* PrometheusHandle accesible (ver 3.4) */;
    (
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        handle.render(),
    )
}
```

`refresh_runtime_gauges`:
- Toma `state.registry.lock()` una sola vez (misma sección crítica corta que ya usa `health_handler`), cuenta repos por estado y actualiza `knot_repositories_by_status{status=...}` para **los 7 estados siempre** (pending, queued, indexed, cloning, pulling, indexing, error), incluyendo ceros — evita series ausentes que rompen dashboards.
- `knot_repositories_total` = `registry.list().len()`.
- `knot_queue_available_capacity` = `state.job_tx.capacity()` (misma fuente que `health.rs:39`).
- `knot_process_uptime_seconds` = `state.start_time.elapsed().as_secs_f64()`.

### 3.3. Helpers tipados (API interna)

Para que el resto del código no manipule nombres/labels a mano (evita typos que crean series huérfanas):

```rust
pub fn record_http_request(route: &'static str, method: &Method, status: StatusCode, dur: Duration);
pub fn record_indexing_job(repo_id: &str, kind: JobKind, ok: bool, dur: Duration);
pub fn set_indexing_progress(repo_id: &str, stage: &str, snap: &ProgressSnapshot);
pub fn set_last_success(repo_id: &str);   // gauge con unix timestamp
pub fn set_build_info();                  // una vez en init()
```

`JobKind` deriva de `IndexJob` (`Clone` → "clone", `Pull` → "pull").

### 3.4. Acceso al `PrometheusHandle`

Dos opciones; **recomendada la (a)** por no tocar `AppState` ni sus dos constructores de test en `worker.rs`:

- **(a) Closure en la ruta:** `main()` guarda el handle y lo mueve a la ruta:
  `.route("/metrics", get(move |State(s)| metrics::metrics_handler(s, handle.clone())))` — `PrometheusHandle` es `Clone` y barato.
- (b) `static OnceLock<PrometheusHandle>` dentro de `metrics.rs`, poblado por `init()`. Más simple de rutear pero introduce estado global adicional.

## 4. Catálogo de métricas

Prefijo común `knot_`. Convención de labels: **nunca** poner la URI real (contiene `{id}`) — siempre la plantilla de ruta.

### 4.1. HTTP (middleware — Fase 1)

| Métrica | Tipo | Labels | Notas |
|---|---|---|---|
| `knot_http_requests_total` | counter | `route`, `method`, `status` | `status` como clase o código ("200", "404"...) |
| `knot_http_request_duration_seconds` | histogram | `route`, `method` | buckets 5ms–10s (ver 3.1) |
| `knot_http_requests_in_flight` | gauge | — | inc al entrar, dec al salir (guard RAII en el middleware) |

`route` = plantilla matcheada vía `axum::extract::MatchedPath` (p. ej. `/api/repos/{id}/search`), disponible en axum 0.8 como extensión de la request dentro de un `middleware::from_fn`. Si no hay `MatchedPath` (404, assets de Swagger), etiquetar `route="unmatched"` para acotar cardinalidad.

Rutas cubiertas automáticamente (todas las de `main.rs:134-153`): list/register/get/delete repo, sync, progress, batch_progress, search, callers, explore, deps, graph, graph_expand, webhook, health. Excluir del middleware (o aceptar y filtrar en Grafana): `/metrics` en sí, `/docs/*`, `/favicon.ico`, `/graph` (viewer).

### 4.2. Pipeline de indexación (worker — Fase 2)

| Métrica | Tipo | Labels | Punto de emisión |
|---|---|---|---|
| `knot_indexing_jobs_total` | counter | `repo_id`, `kind` (clone\|pull), `result` (ok\|err) | `worker_loop`, al terminar cada job |
| `knot_indexing_duration_seconds` | histogram | `kind`, `result` | ídem (sin `repo_id` para acotar cardinalidad de buckets) |
| `knot_indexing_percent_complete` | gauge | `repo_id`, `stage` | `spawn_progress_persister` |
| `knot_indexing_parsed_files` | gauge | `repo_id` | ídem (valor absoluto del snapshot, no delta) |
| `knot_indexing_total_files` | gauge | `repo_id` | ídem |
| `knot_indexing_entities_ingested` | gauge | `repo_id` | ídem |
| `knot_indexing_last_success_timestamp_seconds` | gauge | `repo_id` | `process_repository`, tras `update_last_indexed` |

Notas de diseño:
- Los valores de progreso se publican como **gauges absolutos** leídos del `ProgressSnapshot` (campos `stage`, `percent_complete`, `parsed_files`, `total_files`, `entities_ingested`, `batches_ingested` — los mismos que ya usa la firma del persister en `worker.rs:462-470`). Evita la contabilidad de deltas y es coherente con el polling de 100 ms que ya existe.
- Cardinalidad de `repo_id`: aceptable a escala actual (decenas de repos). Documentar que si el número creciera a miles habría que retirar el label de los gauges de progreso.

### 4.3. Cola y registro (scrape-time — Fase 1)

| Métrica | Tipo | Labels | Fuente |
|---|---|---|---|
| `knot_repositories_total` | gauge | — | `registry.list().len()` |
| `knot_repositories_by_status` | gauge | `status` (7 valores fijos) | conteo sobre `registry.list()` |
| `knot_queue_available_capacity` | gauge | — | `state.job_tx.capacity()` |

### 4.4. Proceso (Fase 1)

| Métrica | Tipo | Labels |
|---|---|---|
| `knot_process_uptime_seconds` | gauge | — |
| `knot_build_info` | gauge (=1) | `version` = `CARGO_PKG_VERSION`, `knot_version` = `env!("KNOT_VERSION")` |

## 5. Cambios concretos por archivo

| Archivo | Cambio |
|---|---|
| `Cargo.toml` | Añadir `metrics` y `metrics-exporter-prometheus` (sección 2) |
| `src/metrics.rs` | **Nuevo** — init, describe, handler, helpers, middleware HTTP, tests |
| `src/main.rs` | (1) declarar `mod metrics;` (2) en `main()`, tras `setup_tracing()`: `let metrics_handle = if cfg.metrics_enabled { Some(metrics::init()?) } else { None };` + `metrics::set_build_info()` (3) añadir `.route("/metrics", ...)` junto a `/favicon.ico` y `/graph` (línea ~157), **fuera** del `OpenApiRouter` (4) aplicar `.layer(middleware::from_fn(metrics::track_http))` al router completo |
| `src/config.rs` | Añadir `#[arg(long, env = "KNOT_SERVER_METRICS_ENABLED", default_value_t = true)] pub metrics_enabled: bool` + test de default |
| `src/worker.rs` | (1) en `worker_loop` (línea ~200): envolver `process_repository` con `Instant::now()` y llamar a `record_indexing_job(...)` tanto en éxito como en el branch de `handle_job_failure` (2) en `spawn_progress_persister` (~460): tras `tracker.snapshot()`, llamar a `set_indexing_progress(...)` (3) en `process_repository` tras `update_last_indexed` (~441): `set_last_success(&repo.id)` |
| `src/handlers/health.rs` | Sin cambios funcionales. Opcional: añadir `"metrics_endpoint": "/metrics"` al JSON |
| `docker-compose.yml` | Variable `KNOT_SERVER_METRICS_ENABLED` (default true). No hay puerto nuevo que exponer (3000 ya está publicado) |
| `README.md` | Sección "Metrics": endpoint, flag de configuración, scrape config de ejemplo (sección 7), tabla resumida de métricas (cumple AGENTS.md: actualizar README) |
| `docs/grafana/knot-server-dashboard.json` | **Nuevo (Fase 3)** — dashboard exportado |

### 5.1. Middleware HTTP (detalle)

Implementado en `metrics.rs` como `middleware::from_fn` puro de axum (~30 líneas), sin `tower-http`:

```rust
pub async fn track_http(req: Request, next: Next) -> Response {
    let method = req.method().clone();
    let route: &'static str = /* MatchedPath → tabla estática, o "unmatched" */;
    let start = Instant::now();
    let _guard = InFlightGuard::new();          // inc/dec del gauge
    let resp = next.run(req).await;
    record_http_request(route, &method, resp.status(), start.elapsed());
    resp
}
```

Detalle importante: las macros de `metrics` con labels dinámicos aceptan `String`, pero conviene internar las rutas conocidas a `&'static str` (match sobre `MatchedPath::as_str()`) para no alocar por request.

Si `cfg.metrics_enabled == false`, no se instala el recorder: las macros de `metrics` se convierten en no-ops (comportamiento del crate sin recorder global), y la ruta `/metrics` devuelve `404` o directamente no se registra — preferible **no registrarla**.

## 6. Comportamiento de apagado y mantenimiento

- **Shutdown:** no requiere flush — el modelo es pull (Prometheus scrapea); no hay buffer que drenar como en OTLP. Sin cambios en `shutdown_signal()`.
- **Upkeep:** `metrics-exporter-prometheus` acumula histogramas hasta que se drenan en `render()`. Con scrapes cada 15 s no hay problema; como red de seguridad, configurar `idle_timeout` en el builder (p. ej. `Some(Duration::from_secs(600))` para `MetricKind::HISTOGRAM`) de forma que series inactivas se purguen.

## 7. Configuración Prometheus y Grafana

### 7.1. Scrape config

```yaml
scrape_configs:
  - job_name: 'knot-server'
    scrape_interval: 15s
    metrics_path: '/metrics'
    static_configs:
      - targets: ['knot-server:3000']   # o localhost:3000 fuera de compose
```

### 7.2. Panels del dashboard (Fase 3)

| Panel | PromQL |
|---|---|
| Request rate por ruta | `sum by (route) (rate(knot_http_requests_total[1m]))` |
| Tasa de errores 5xx | `sum(rate(knot_http_requests_total{status=~"5.."}[5m]))` |
| Latencia P95 / P99 | `histogram_quantile(0.95, sum by (le, route) (rate(knot_http_request_duration_seconds_bucket[5m])))` |
| Requests in flight | `knot_http_requests_in_flight` |
| Repos por estado | `knot_repositories_by_status` (bar gauge / state timeline) |
| Capacidad libre de cola | `knot_queue_available_capacity` |
| Duración de indexación P95 | `histogram_quantile(0.95, sum by (le, kind) (rate(knot_indexing_duration_seconds_bucket[15m])))` |
| Jobs fallidos / hora | `sum(increase(knot_indexing_jobs_total{result="err"}[1h]))` |
| Progreso por repo | `knot_indexing_percent_complete` |
| Antigüedad del último índice | `time() - knot_indexing_last_success_timestamp_seconds` |
| Uptime | `knot_process_uptime_seconds` |

## 8. Tests (patrón `mod tests` por archivo, según AGENTS.md)

Restricción clave: `install_recorder()` es **global e irrepetible por proceso**, y los tests de cargo corren en paralelo en el mismo proceso. Estrategia:

1. **No llamar a `init()` en tests.** Los helpers (`record_http_request`, `record_indexing_job`, ...) se testean con `metrics_util::debugging::DebuggingRecorder` + `metrics::with_local_recorder(...)`, que instala un recorder por-closure sin tocar el global. Añadir `metrics-util = "0.x"` (la versión hermana de `metrics 0.24`) a `[dev-dependencies]`.
2. `metrics.rs::tests`:
   - `test_record_http_request_emits_counter_and_histogram` — con recorder local, verificar nombre, labels y valor.
   - `test_refresh_runtime_gauges_covers_all_seven_statuses` — construir un `AppState` de test (reutilizar el patrón `create_test_state` de `worker.rs`) y verificar que se emiten 7 series de `knot_repositories_by_status`, incluyendo ceros.
   - `test_matched_path_interning_falls_back_to_unmatched`.
3. `main.rs::tests` (o `metrics.rs`): test del middleware con `Router` mínimo + `tower::ServiceExt::oneshot` (patrón ya usado en `main.rs:351-374`), con recorder local, asterando que un GET registra counter con `route`/`status` correctos.
4. `config.rs::tests`: `test_metrics_enabled_default_true`.
5. **E2E (opcional, Fase 1):** siguiendo el patrón del suite e2e existente, un smoke test que haga `GET /metrics` contra el server real y compruebe `content-type` y presencia de `knot_build_info`.

## 9. Riesgos y decisiones documentadas

1. **Recorder global vs. tests:** mitigado con `with_local_recorder` (sección 8). Nunca invocar `install_recorder()` fuera de `main()`.
2. **Cardinalidad:** `route` siempre plantilla; `repo_id` aceptado (decenas de series); histogramas sin `repo_id`.
3. **Contención de locks:** `refresh_runtime_gauges` usa la misma sección crítica corta que `health_handler`; con scrape cada 15 s el impacto es despreciable. No introducir nuevos `Mutex`.
4. **Sin auth en `/metrics`:** decisión explícita (red interna). Anotar en README que si el puerto 3000 se expone públicamente, `/metrics` queda visible — la mitigación futura sería un reverse proxy, no cambios en el server.
5. **`/metrics` fuera de Swagger:** al registrarse con `.route()` plano no aparece en `/docs` — verificar en el e2e que `/api-docs/openapi.json` no lo contiene.
6. **Multi-instancia:** cada nodo expone su `/metrics`; Prometheus distingue por label `instance` automáticamente. Sin coordinación necesaria.

## 10. Roadmap por fases (PRs incrementales)

| Fase | Contenido | Criterio de verificación |
|---|---|---|
| **1** | Deps + `src/metrics.rs` (init, handler, middleware HTTP) + flag `metrics_enabled` + gauges de repos/cola/proceso + README | `curl localhost:3000/metrics` devuelve `knot_http_requests_total`, `knot_repositories_by_status` (7 series), `knot_build_info`; Swagger no lista `/metrics`; `cargo test` verde |
| **2** | Instrumentación del worker: jobs, duración, progreso, last_success | Tras indexar un repo local de prueba, `/metrics` muestra `knot_indexing_jobs_total{result="ok"}` ≥ 1 y el histograma con muestras |
| **3** | Dashboard Grafana (`docs/grafana/*.json`) + scrape config documentado + capturas en README | Dashboard importable renderiza todos los panels de la sección 7.2 contra un Prometheus local |
| **4** | *(Diferida)* Trazas OpenTelemetry: capa `tracing-opentelemetry` + exportador OTLP + `#[instrument]` en worker/handlers, correlacionadas con estas métricas vía exemplars/labels | Ver plan de trazas discutido previamente; se abordará cuando las métricas estén en producción |
