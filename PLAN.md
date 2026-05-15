# Plan: Configurable Performance Tuning for knot-server

## Motivation

During indexing, `knot-server` can consume >5 GiB of RAM and >2000% CPU because
`batch_size` (64) and `ingest_concurrency` (4) are hardcoded. This makes
deployment on resource-constrained Kubernetes clusters impractical.

---

## Implementation Steps

| Step | File | Change |
|------|------|--------|
| 1 | `src/config.rs` | Add `batch_size` and `ingest_concurrency` to `ServerConfig` with `#[arg]` + env vars (`KNOT_SERVER_BATCH_SIZE` default 64, `KNOT_SERVER_INGEST_CONCURRENCY` default 4) |
| 2 | `src/state.rs` | Add `batch_size: usize` and `ingest_concurrency: usize` to `AppState` |
| 3 | `src/main.rs` | Pass `cfg.batch_size` and `cfg.ingest_concurrency` when constructing `AppState` |
| 4 | `src/worker.rs` | Replace hardcoded `64` and `4` with `state.batch_size` and `state.ingest_concurrency` |
| 5 | Tests | Update all 3 test `AppState` constructions (`handlers.rs:528`, `handlers.rs:770`, `worker.rs:156`) |
| 6 | `README.md` | Full documentation update (see details below) |
| 7 | `src/config.rs` | Remove dead code (`build_knot_config` / `KnotConfigParams`) |

---

## Step 6 Detail: README.md Documentation

### 6a. Configuration Table (line 239)

Add the 2 new variables to the existing table:

- `KNOT_SERVER_BATCH_SIZE` | `64` | Number of code entities buffered in memory
  per indexing batch. Lower values reduce RAM usage.
- `KNOT_SERVER_INGEST_CONCURRENCY` | `4` | Number of concurrent async tasks for
  embedding computation and database ingestion. Lower values reduce RAM and CPU
  usage.

Also improve the description of `KNOT_SERVER_RAYON_THREADS` to clarify that the
default uses all available cores (not "logical cores - 1").

### 6b. New "Performance Tuning" Section (between Configuration and Example Workflow)

A dedicated section covering:

- Explanation of the 3 variables that control CPU and RAM (`RAYON_THREADS`,
  `BATCH_SIZE`, `INGEST_CONCURRENCY`)
- Table with preconfigured profiles:

  | Profile | RAYON_THREADS | BATCH_SIZE | INGEST_CONCURRENCY | Expected RAM | Expected CPU |
  |---------|---------------|------------|--------------------|--------------|--------------|
  | Low memory / Kubernetes | 2 | 16 | 1 | < 1 GiB | ~200% |
  | Balanced | 4 | 32 | 2 | ~2 GiB | ~400% |
  | Maximum throughput (default) | all cores | 64 | 4 | ~5 GiB | all cores |

- Concrete Docker Compose example with Kubernetes-friendly values
- `docker run` example with `--network host` and performance variables

### 6c. Update Kubernetes Section (line 356)

Add the performance variables to the existing Deployment YAML example, including
Kubernetes `resources.requests` and `resources.limits` to make it
production-ready:

```yaml
env:
  - name: KNOT_SERVER_RAYON_THREADS
    value: "2"
  - name: KNOT_SERVER_BATCH_SIZE
    value: "16"
  - name: KNOT_SERVER_INGEST_CONCURRENCY
    value: "1"
resources:
  requests:
    memory: "512Mi"
    cpu: "500m"
  limits:
    memory: "1Gi"
    cpu: "2000m"
```

### 6d. Update Cluster Docker Compose (line 300)

Add the performance variables as comments to the multi-instance example so that
anyone copy-pasting the example knows they exist.

---

## Summary of Documentation Impact

The README will go from having zero mention of performance tuning to:

1. Variables documented in the configuration table
2. A dedicated section with profiles and examples
3. Kubernetes and Docker Compose examples with realistic production values
