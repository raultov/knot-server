# Changelog
 
All notable changes to `knot-server` will be documented in this file.
 
The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
 
---

## [0.5.0] - 2026-09-06

### Changed
- **knot 1.9.0:** bumped dependency from 1.8.1 to 1.9.0.
- **Modularized indexing worker:** split monolithic `src/worker.rs` into structured sub-modules `src/worker/mod.rs`, `src/worker/plan.rs`, and `src/worker/state.rs`.
- **Graph subgraph parameter handling:** replaced 8-positional-argument `fetch_subgraph` function with `SubgraphRequest` and `CommonGraphParams` trait, eliminating `#[expect(clippy::too_many_arguments)]`.

### Internal & Maintenance
- **Test helper consolidation:** centralized `make_test_repo` and `create_test_state_with_rx` in `src/handlers/tests_common.rs`.
- **Filesystem utilities:** introduced `src/fs_utils.rs` module for safe filesystem operations.

---

## [0.4.1] - 2026-09-03

### Fixed
- **`repo=all` reached unregistered repositories.** `resolve_scope` short-circuited
  `RepoScope::All` past the registry membership check, so `GET /api/search` and
  `GET /api/callers` with `repo=all` — or with `repo` omitted, the default — ran as an
  *unfiltered* database query and returned rows for repositories that had been deleted from
  the registry. The asymmetry was visible: `repo=<deleted>` was rejected with 400 while
  `repo=all` happily returned its rows. `All` now expands to the registry id list, so both
  spellings mean "all **registered** repositories". Named scopes, the 400 shape and the
  per-repo routes are unchanged. Note that `RepoScope::Many(vec![])` cannot express
  "nothing" — knot's DB layer reads an empty filter list as *unfiltered* — so an empty
  registry is handled by a distinct code path that returns an empty result without querying.
- **Graph viewer: details panel hidden behind the toolbar.** `#node-details` pinned
  `top: 50px` while the toolbar grows to ~92px when its controls wrap, hiding the entity
  name. Top and height are now derived from the live toolbar height.
- **Graph viewer: search results dropdown overflowed the viewport.** `#search-results` is
  now right-anchored.

### Added
- **Graph viewer: cross-repository search.** An "All repos" checkbox next to the search box
  switches it from `GET /api/repos/{id}/search` to the cross-repo `GET /api/search`. Checking
  "All repos" enables global searching even when no repository is selected. Results are grouped
  and badged by repository, and selecting a result from another repository automatically switches
  the active repository before focusing the entity. The 3D graph itself remains single-repo.

---

## [0.4.0] - 2026-09-02

### Added
- **Cross-repository search (`GET /api/search`):** new top-level endpoint exposing knot's
  repository scope selection. `repo` accepts a single id, a comma-separated list,
  or the sentinel `all` / `*`; omitting it searches every indexed repository. Each result
  entity carries `repo_name`, so multi-repo results are self-labeling. `max_results` is a
  global cap across the scope and is clamped to 1..=100. Unknown repository ids are
  rejected with 400 (the per-repo routes keep their existing silent-empty behavior).
- **Cross-repository caller analysis (`GET /api/callers`):** same scope syntax over
  `entity`, for impact analysis spanning several repositories. Every row identifies the
  repository of the caller (`repo_name`) and of the referenced entity (`target_repo_name`),
  so a genuine cross-repo reference is the row where the two differ; `resolution.targets[]`
  is labeled too. There is no `max_results`: the response is bounded by knot's 25-target
  resolution cap, surfaced as `resolution.truncated`.

### Changed
- **knot 1.8.1:** bumped from 1.8.0 (which itself replaced 1.7.2 in this cycle). Required by
  the new `/api/callers` endpoint — before it, multi-repo caller rows carried no repository
  and were unattributable, since file paths are repo-relative and collide across
  repositories. Additive only: no re-index, no signature change. It also makes the existing
  `GET /api/repos/{id}/callers` rows self-labeling (`repo_name` / `target_repo_name` on
  every reference row and `repo_name` on `resolution.targets[]`).
- **knot 1.8.0 (mid-cycle bump):** the search/callers/explore handlers now build a
  `knot::models::RepoScope::One` from the registry id instead of passing `Option<&str>`,
  preserving the single-repo-per-request contract; `run_deps` and `run_get_subgraph`
  signatures are unchanged. No re-index or user action required — knot 1.8.0 keeps the
  on-disk index-state format and Neo4j/Qdrant schemas untouched. As an additive,
  non-breaking response shape enhancement, `GET /api/repos/{id}/search` entity results
  include a `repo_name` field per entity.

### Fixed
- **HTTP metrics attributed to `unmatched`:** `/api/repos/{id}/graph/repos` (live since
  0.3.4) was missing from `KNOWN_ROUTES`, so every request to it was counted under the
  `unmatched` route label. Added, together with `/api/search` and `/api/callers`, plus a
  drift-guard test that fails when a handler declares a route that the allowlist does not
  know.

---

## [0.3.6] - 2026-08-30
 
### Fixed
- **Worker status claim revert and atomic multi-instance registry updates:** (a) worker no longer leaves a claimed `cloning`/`pulling` status stuck when the repo lock is held by another node (the 202 sync could silently no-op), and (b) registry mutations now re-read repos.json under the workspace lock so multi-instance deployments can no longer lose status/last_indexed updates (last-writer-wins full-file overwrites).
- **Nested types invisible in the graph overview:** nested declarations (inner classes, C# nested records/enums) were unreachable in the graph overview because the traversal closure started from `CONTAINS`-free roots and followed outgoing relationships only. The overview now merges in a one-hop `CONTAINS` query so every visible-kind declaration is present; node counts grow (openlogi-net 335→400, csharp-code-map 687→710); `fetch_edges` and the response schema are untouched.
- **C# kinds categorised (graph overview was empty for C# repos):** the C# support recently added to the knot library emits `csharp_*` entity kinds, but none of them were present in `KIND_CATEGORY_CLASSES`, `KIND_CATEGORY_INTERFACES` or `KIND_CATEGORY_FUNCTIONS` in `src/handlers/models.rs`, so `parse_kinds("classes,interfaces")` matched nothing and `GET /api/repos/{id}/graph` returned an empty graph for every indexed C# repository. All C# declaration kinds are now categorised — types (`csharp_class`, `csharp_struct`, `csharp_record`, `csharp_enum`, `csharp_delegate`) in `classes`, `csharp_interface` in `interfaces`, and members (`csharp_method`, `csharp_constructor`, `csharp_local_function`, `csharp_operator`, `csharp_indexer`, `csharp_property`, `csharp_field`, `csharp_event`, `csharp_constant`) in `functions`. `assets/graph-viewer.html` gained matching `KIND_COLORS` entries (rebuild required, the file is embedded via `include_str!`). `csharp_namespace` intentionally stays uncategorized (only reachable via the `other` category): knot does not emit `namespace CONTAINS type` edges and namespace nodes carry almost no CALLS/EXTENDS/IMPLEMENTS edges, so they would otherwise flood the default overview with isolated containers. Four C# drift-guard unit tests added; the existing `kind_categories_are_disjoint_and_have_no_duplicates` test still passes (no kind belongs to two categories).

---

## [0.3.4] - 2026-08-28

### Added
- **Repository Dependency Graph endpoint**: Added `GET /api/repos/{id}/graph/repos` to query repository-level dependency graphs (`DEPENDS_ON` relations).
- **Graph Viewer Cross-Repo mode**: Added "Repo Deps" toggle and direction selector to the 3D graph viewer to visually explore codebase dependencies/dependents across the indexed ecosystem.
- **Details Panel Open Repository button**: Added an option to transition/explore another repository directly from the details panel when selecting dependency nodes.
- **Postman collection**: Added the new endpoint request to `knot-server.postman_collection.json`.
- **Spec**: Added the TDD/BDD implementation plan at `docs/specs/cross_repo_dependencies_graph_plan.md`.

### Fixed
- **Viewer `.hidden` CSS Rule**: Added `.hidden { display: none !important; }` to `assets/graph-viewer.html` stylesheet to fix existing buttons (`#clear-btn`, `#back-btn`, `#explore-btn`) not visually hiding on state changes.

---

## [0.3.3] - 2026-08-28

### Fixed
- **Fixed the `/index` skill:** the registration payload sent `"auth_type": {"type": "none"}`, an object form that the API never accepted (`AuthType` only deserializes the plain strings `"ssh"` and `"https"`), so every registration attempt failed with `422 Unprocessable Entity`. The skill now omits `auth_type` (optional, defaults to `"ssh"`) and documents the accepted values. Also corrected the same broken payload in the `repos` skill.

---

## [0.3.2] - 2026-08-15

### Changed
- **Bump `knot` to v1.6.2:** Upgraded the core library dependency to include the latest fixes and additions (including `total_entities` field for indexing progress).

---

## [0.3.1] - 2026-08-13

### Changed
- **Bump `knot` to v1.6.1:** Upgraded the core library dependency to include the latest fixes from knot.

---

## [0.3.0] - 2026-08-08

### Changed
- **Bump `knot` to v1.6.0:** Upgraded to the new knot 1.6.0 API.
- **Lint hardening:** Updated `clippy.toml` measured curves based on `knot-server` data, specifically raising `cognitive-complexity-threshold` to 40. Removed four stale `allow` suppressions across `src/git.rs`, `src/progress_store.rs`, and `src/telemetry.rs`.

### Added
- **Varnish support:** Added support for six new Varnish relationship types (`USES_BACKEND`, `USES_PROBE`, `USES_ACL`, `INCLUDES`, `IMPORTS_VMOD`, `DECLARED_UNUSED`) and properly categorized 18 Varnish entity kinds so they are visible by default in the graph view.

---

## [0.2.19] - 2026-07-31

### Changed
- **Bump `knot` to v1.5.6:** latest patch release of the indexing engine. Brings Groovy property accessor synthesis, bare property declarations, parser/Javadoc hardening, and reopens the `OVERRIDES` link between Groovy properties and interface getters (e.g. `nextflow.Session.getBaseDir` → `nextflow.ISession.getBaseDir`). **Existing Groovy repos must be re-indexed** (`POST /api/repos/{id}/sync`) for the new entities and override edges to materialize. Verified against the full unit suite (192 tests) and all E2E suites.

### Added
- **Graph viewer: synthetic accessor badge.** Nodes whose `signature` contains the `<synthetic Groovy property accessor>` marker (emitted by knot 1.5.6 for compiler-generated `getX`/`setX`/`isX`) now display a purple `synthetic` badge next to the entity kind in the detail panel. The raw marker string is stripped from the displayed signature so it does not masquerade as a parameter list.

### Fixed
- **`kotlin_enum` categorised as `classes`:** previously absent from `KIND_CATEGORY_CLASSES`, so Kotlin enums were invisible in the default graph overview (only reachable via the `other` category). They now appear alongside `kotlin_class`, `kotlin_object`, and other class-like Kotlin kinds. This affects the default graph contents for every indexed Kotlin repository.

---

## [0.2.18] - 2026-07-26

### Changed
- **Bump `knot` to v1.5.5:** latest patch release of the indexing engine. Brings the new `OVERRIDES` relationship type for JVM method-level overrides/implementations. Verified against the full unit suite (192 tests) and all E2E suites.

### Added
- `OVERRIDES` relationship support in the graph API: the `GET /api/repos/:id/graph?relationships=OVERRIDES` endpoint now accepts and traverses method-level override edges (JVM only, opt-in).
- `Overrides` relationship toggle in the `/graph` viewer, placed next to `Implements`. Works with the default `Classes`/`Interfaces` kinds, where method-level override edges are projected onto their enclosing classes.
- New test fixture `tests/fixtures/Greeter.java` with Java interface, implementation, and extended class for E2E override coverage.
- Unit tests in `src/handlers/graph_parse.rs` for OVERRIDES acceptance, rejection of wrong types, and defaults guard.
- Drift-guard unit test in `src/handlers/models.rs` asserting no duplicates and upper-case wire format in `VALID_RELATIONSHIPS`.
- E2E tests G14–G18 in `tests/run_e2e.sh`: method-level OVERRIDES via focused mode, class-level projection in overview mode (G14b), edge direction, error advertising, full allow-list round-trip, and default overview opt-in guard.
- Design spec `docs/specs/0001-overrides-relationship.md` documenting why `OVERRIDES` is only reachable via focused mode or class projection, never via an overview query scoped to `kinds=functions`.

---

## [0.2.17] - 2026-07-19

### Changed
- **Bump `knot` to v1.5.4:** latest patch release of the indexing engine. Verified against the full unit suite (185 tests) and all six E2E suites (lifecycle, reindex/recovery, cluster coordination, progress coherence, metrics, tracing).
- **Double default `KNOT_SERVER_BATCH_SIZE` from 64 to 128:** better fits typical Neo4j ingestion throughput and reduces per-batch commit overhead on large repositories.

### Added
- Test in `src/config.rs` asserting the default `batch_size` is 128.

---

## [0.2.16] - 2026-07-12

### Fixed
- **Release builds no longer download Swagger UI at compile time:** enabled the `vendored` feature of `utoipa-swagger-ui`, embedding the Swagger UI assets in the crate. This fixes the v0.2.15 release pipeline failure on the `aarch64-apple-darwin` runner, where the build script's `curl` download of Swagger UI failed with a DNS resolution error (`Could not resolve host: github.com`).

---

## [0.2.15] - 2026-07-12

### Changed
- **Bump `knot` to v1.5.3:** brings more complete Groovy language support to the indexing engine. Verified against the full unit suite (185 tests) and all six E2E suites (lifecycle, reindex/recovery, cluster coordination, progress coherence, metrics, tracing).

---

## [0.2.14] - 2026-07-11

### Changed
- **Bump `knot` to v1.5.2:** includes composite index `entity_repo_fqn ON (repo_name, fqn)` that fixes CONTAINS auto-link timeouts on large repositories (~50K entities). The index is created with `IF NOT EXISTS` so it migrates automatically into existing deployments without manual intervention.

### Added
- **E2E regression test B0:** verifies `entity_repo_fqn` index is created at server startup.
- **E2E regression test Qa:** verifies the index survives server restart (guards the `IF NOT EXISTS` migration path).
- **README roadmap:** updated language support status.

---

## [0.2.13] - 2026-07-09

### Fixed
- **CI/CD:** Fixed an issue where the `dist` binary was not found in the `$PATH` on `macos-14` (Apple Silicon) runners during the GitHub Actions release workflow.

---

## [0.2.12] - 2026-07-09

### Added
- **Distributed Tracing (OpenTelemetry):** Implemented W3C-compliant distributed tracing via `tracing-opentelemetry`. 
- Spans are exported asynchronously via OTLP gRPC. Disabled by default (`KNOT_SERVER_TRACING_ENABLED=false`).
- Instrumented all HTTP endpoints (with `http.route` and status codes), background indexing jobs (`Clone`, `Pull`, `Sync`), and the scheduler loop.
- Support for distributed context propagation: inbound W3C `traceparent` headers are correctly picked up and used as the root span's parent.
- E2E tests for tracing (`run_tracing_e2e.sh`) verifying OTLP export, Jaeger ingestion, and `traceparent` propagation.

---

## [0.2.11] - 2026-07-08

### Added
- **Prometheus Metrics:** Added `/metrics` endpoint (on the same port) to expose HTTP, indexing pipeline, registry queue, and process metrics.
- End-to-end metrics tests (`run_metrics_e2e.sh`).
- Documentation and scrape config for Grafana/Prometheus integration in `README.md`.

### Fixed
- Stabilized `Cluster Coordination: Stale Lock Recovery` E2E tests by adding wait loop for recovery job completion.

---

## [0.2.10] - 2026-07-07

### Fixed
- **Re-registering a repository no longer races with the indexing worker
  (issue #7, Bug A).** `POST /api/repos` used to spawn a background task that
  wiped the databases and `remove_dir_all`'d the local directory while the
  worker could already be pulling the *same* directory, causing
  `git fetch failed ... 255` (`Unable to read current working directory`) and
  leaving a previously-healthy repo in `error` with emptied databases. The
  destructive cleanup now happens exclusively inside the worker's `Clone` job,
  under the per-repo file lock, so it is serialized with all git/index work.

### Added — Recuperación de fallos de indexado (issue #7)
- The indexing jobs now have explicit semantics: `Clone` = *wipe databases +
  wipe local directory + fresh clone* ("start from scratch"); `Pull` =
  incremental, **falling back to a fresh clone when the local directory is
  missing** (so `POST /api/repos/{id}/sync` on an errored repo without a
  directory recovers instead of failing with "cannot pull").
- **On indexing failure**, the repo is always set to `error`, its progress
  snapshot and in-memory tracker are dropped, and the registry entry is kept
  (still visible via `GET /api/repos`; re-registering re-clones it clean).
  Additionally, **only** when the repo never indexed successfully
  (`last_indexed == None`) its Neo4j/Qdrant data and local directory are wiped
  ("remove from databases and erase metadata in disk"). A repo that was already
  indexed and fails a transient pull keeps its index and directory.

### Notes
- A registration with a malformed URL (e.g. `...repo.gitr`) derives a *different*
  id than the corrected URL, so the broken entry persists; remove it with
  `DELETE /api/repos/{id}`.
- Known limitation (multi-node): the file lock is per-fd, so on a shared
  workspace another node could recreate the lock file after the wipe's unlink.
  Pre-existing and not made worse here; documented in `worker.rs`/`cleanup.rs`.

---

## [0.2.9] - 2026-07-04

### Changed
- Upgrade `knot` dependency from 1.5.0 to 1.5.1.

### ⚠️ Breaking (indexer state & stored paths)
- **On-disk index state bumped v3 → v4.** knot 1.5.1 rejects the older v3
  `.knot/index_state.json` with *"Detected index_state v3; current version is
  v4. The on-disk index is incompatible."* The first sync after upgrading a
  repository automatically discards the incompatible state and performs a full
  re-index — no manual `knot-indexer --clean` is required for repos managed by
  knot-server (the worker's recovery path clears the stale file and rebuilds).
  Expect the first post-upgrade sync of each repository to take as long as an
  initial index.
- **Stored `file_path` is now repo-relative** instead of absolute (see the knot
  `relative_file_paths` spec). This too is materialized by the automatic
  re-index above; the Neo4j/Qdrant entries for a repo are rebuilt on its first
  post-upgrade sync.

### Fixed
- `GET /api/repos/{id}/explore?path=...` returned an empty `entities` array
  after the upgrade because the handler still prepended the repo's
  `local_path` to build an absolute path, which no longer matches knot 1.5.1's
  repo-relative `file_path` storage. The handler now passes the caller-supplied
  relative path straight through to `run_explore_file`, which normalizes it.
- Unit-test fixtures that seed a valid `index_state.json` now write `version: 4`
  so they load successfully under knot 1.5.1.

### Removed
- E2E: dropped the inherently racy *"Node B never saw live progress from A"*
  assertion from the cluster progress-coherence suite. Its `beta` observation
  could never win the timing race against `alpha`'s multi-second head start
  before the poll loop exited on `alpha` reaching `indexed`. Cross-node progress
  visibility remains covered by the reciprocal *"Node A reported live progress
  for a repo indexed by B"* assertion plus the terminal-coherence and
  snapshot-cleanup checks.

## [0.2.8] - 2026-07-04

### Added
- `GET /api/progress` batch endpoint: returns live indexing progress for every registered repository in a single call. Each entry resolves via the in-process `ProgressTracker`, then falls back to the on-disk snapshot at `<workspace>/progress/<id>.json`, so a request served by node B reports the real progress of a job running on node A.
- New `src/progress_store` module: atomic (temp + rename) write/read/remove of per-repo progress snapshots in the shared workspace. Worker persists the in-memory `ProgressTracker` snapshot at most every 1 s; the snapshot is removed on terminal success or failure.
- Graph viewer: the repository dropdown now refreshes its option labels every time the user opens it (mouse and keyboard), showing live percentages for repos that are actively indexing. Requests to `/api/progress` are throttled to 1.5 s so rapid re-opens do not spam the API; failures degrade silently.

### Fixed
- Registry: cross-instance coherence bugs (BUG-1, BUG-2 from the design plan).
  `Registry` is now read-through with an mtime fast-path, and all mutations
  (`add_or_replace`, `remove`, `update_status`, `update_last_indexed`) perform
  read-modify-write under the `repos.json.lock` so a node's stale in-memory
  copy can no longer clobber a peer's recent status change. `get`/`list` take
  `&mut self` (callers already hold the `Mutex<Registry>` in `AppState`,
  so only borrow sites changed). `repos.json` is now written atomically
  (temp + `fsync` + rename) so readers never observe a partially written
  file.
- Cross-instance progress visibility (BUG-3): a node with no in-process
  `ProgressTracker` for a repo now serves the peer's live progress from the
  shared snapshot file, so `GET /api/repos/{id}/progress` and
  `GET /api/progress` stay coherent across nodes.

## [0.2.7] - 2026-07-03

### Added
- `GET /api/repos/{id}/progress` endpoint exposing live indexing progress (stage, files parsed/total, percent, entities/batches ingested, error) powered by knot 1.5.0's `ProgressTracker`.

### Changed
- Upgrade `knot` dependency from 1.4.13 to 1.5.0.

## [0.2.6] - 2026-06-28

### Changed
- Upgrade `knot` dependency from 1.4.12 to 1.4.13

## [0.2.5] - 2026-06-28

### Fixed
- Graph viewer: relationship toggles (e.g. `CONTAINS`, `REFERENCES`) now
  correctly restore the kind filters to their state before that relationship
  was turned on, instead of leaving stray kinds active. Previously, toggling
  `CONTAINS` on auto-enabled `functions`/`other`, and toggling it back off
  either kept them pinned (because other active rels declared the same
  kinds in `REL_KINDS_MAP`) or removed them unconditionally — breaking
  the user's expected restore-to-default behavior and any subsequent chain
  of relationship activations.
- Chain activations: turning on a second relationship that overlaps the
  kinds of a previously activated one now joins the claim, so deactivating
  the first keeps the kinds alive as long as the second is still on.
- Manual kind toggles are now "sticky": if the user activates a kind by
  hand, no later relationship deactivation will remove it.

## [0.2.4] - 2026-06-21

### Changed
- Upgrade `knot` dependency from 1.4.10 to 1.4.12

### Docs
- Showcase the graph viewer and Swagger UI in the README intro
  with side-by-side animated WebP previews (`docs/demo-graph.webp`
  and `docs/demo-swagger.webp`)

### Chore
- Ignore `opencode.json` in `.gitignore`

## [0.2.3] - 2026-06-20

### Fixed
- Local repos no longer trigger a full re-embed on every scheduler Pull. The
  previous behaviour was caused by `copy_tree` copying the source repo's
  `.knot/` directory over the mirror's incremental state. `.knot/` and
  `.knot.lock` are now excluded from the local sync, symmetrically with `.git`.

### Added
- `docker-compose.yml` now exposes `KNOT_SERVER_POLL_INTERVAL_SECS`,
  `KNOT_SERVER_MAX_INDEX_AGE_SECS` and `KNOT_SERVER_STALE_LOCK_TIMEOUT_SECS`
  so scheduler timing can be tuned without rebuilding the image.
- More verbose worker logs for index state loading: the worker now reports
  whether the state was loaded successfully (with entry count and file size),
  was absent, was cleared as legacy, or fell back after a parse error.

## [0.2.2] - 2026-06-14

### Fixed
- Docker: include `build.rs` in the build context. The script emits the
  `KNOT_VERSION` env var that `/docs` substitutes into the Swagger UI,
  and the Dockerfile only copied `Cargo.toml`, `Cargo.lock`, `assets/`
  and `src/`. The release build failed with "environment variable
  `KNOT_VERSION` not defined at compile time". Add an explicit
  `COPY build.rs build.rs` step right after the manifest copy so Cargo
  auto-discovers the script.

## [0.2.1] - 2026-06-14

### Added
- Local working-tree sync: registering a local filesystem path mirrors the
  working tree into the workspace and indexes it like a normal clone.
  Universal build/IDE/dependency outputs are skipped (e.g. `target/`,
  `node_modules/`, `.gradle/`, `build/`, `dist/`, `__pycache__/`, `.idea/`,
  `.vscode/`). Self-overwrite (source equal to mirror destination) is
  refused with a clear error.
- `POST /api/repos` is now idempotent. Re-registering an existing
  repository atomically replaces the registry entry, cleans its graph and
  vector entries and removes the old local path in the background, then
  enqueues a fresh clone job. The response distinguishes "registered"
  from "re-registered".
- `Registry::add_or_replace` for atomic in-place updates, with unit
  tests covering both insert and overwrite paths.
- Knot library version badge rendered on `/docs` next to the SwaggerUI
  version stamp, mirroring the graph viewer.
- `build.rs` that resolves the linked `knot` version by reading
  `Cargo.lock` (falling back to `Cargo.toml`) and exports it as
  `KNOT_VERSION`.
- `/index` OpenCode slash command (`commands/index.toml`) that registers
  the current repository (or re-syncs an existing one) end-to-end:
  health check, derive id, register or sync, poll until `indexed`,
  verify with a quick search.
- Self-extracting agent-skills installer bundle
  (`.knot-server-agent-skills.sh`) plus three companion scripts:
  - `scripts/generate_skills_script.py` rebuilds the bundle from
    `skills/*.md`
  - `scripts/install-agent-skills.sh` runs the bundle locally
  - `scripts/download-agent-skills.sh` fetches the `.md` files
    individually when the tarball is blocked by a firewall
- Nine new per-topic skills (each documents a single endpoint or
  workflow, replacing the per-IDE monolithic files): `preflight`,
  `search`, `callers`, `explore`, `deps`, `graph`, `repos`, `index`,
  `workflows`.

### Changed
- README: replaced the per-IDE `curl` one-liners with a single
  installer (`curl | bash`) and added manual-registration fallbacks for
  Cursor and GitHub Copilot when the installer is skipped.
- `src/handlers/system.rs::docs_handler` substitutes the
  `{{KNOT_VERSION}}` placeholder in the embedded `swagger-ui.html` at
  startup.
- The three monolithic skill files (`copilot-instructions.md`,
  `cursor-rules.md`, `system-prompt.md`) were restructured to delegate
  to the new topic skills; they now share a single Connection & Port
  Handling section instead of duplicating the full REST reference.

### Fixed
- E2E: local live-repo sources are now placed under `/tmp` outside the
  workspace. The registry derives `local_path = workspace/<id>` from the
  URL basename, so a source inside the workspace used to collide with
  its own mirror and `fs::copy(file, file)` truncated every file to
  zero bytes.
- E2E: `/explore` check reads from the new `.entities[]` response
  wrapper instead of the previous flat array.
- E2E: `RECOVERY_LOG_R` is preserved through the local-sync phase so
  the S5 stale-state-removal assertion can grep it.

## [0.2.0] - 2026-06-07

### Changed
- Upgrade `knot` dependency from 1.3.13 to 1.4.0 — significant code cleanup and refactoring

## [0.1.17] - 2026-06-06

### Added
- `up.sh` and `down.sh` convenience scripts for Docker Compose lifecycle management

### Changed
- Upgrade `knot` dependency from 1.3.11 to 1.3.13

### Fixed
- E2E tests: cache fastembed model to avoid Hugging Face 429 rate limits in CI

## [0.1.16] - 2026-06-01

### Changed
- Upgrade `knot` dependency from 1.3.10 to 1.3.11

### Fixed
- Graph viewer: reset overview defaults on repo switch, isolate clear button, focus depth=1

### Docs
- Update roadmap language support status, remove duplicate line

## [0.1.15] - 2026-05-31

### Added
- Graph viewer: improve color coding, auto-enable kind filters, save depth on focus
- Postman collection for API testing
- Beta disclaimer and roadmap to README

### Changed
- Upgrade `knot` dependency from 1.3.9 to 1.3.10

### Fixed
- Filter self-referencing edges from graph responses

## [0.1.14] - 2026-05-29

### Added
- utoipa Swagger UI and OpenAPI spec generation (`/docs` endpoint)
- Expose Neo4j (7474, 7687) and Qdrant (6333, 6334) ports in docker-compose

### Docs
- Document Swagger UI, update installer URL, add API workflow step

## [0.1.13] - 2026-05-28

### Fixed
- CI: downgrade actions/download-artifact from v7 to v4 and actions/checkout from v6 to v4

## [0.1.12] - 2026-05-28

### Changed
- Rename `RepoStatus::Idle` to `Indexed` and introduce `Pending` initial state

## [0.1.11] - 2026-05-28

### Added
- Display knot-server version in `/graph` UI via compile-time placeholder substitution

## [0.1.10] - 2026-05-28

### Added
- Make `KNOT_SERVER_PORT` configurable in docker-compose.yml

## [0.1.9] - 2026-05-27

### Changed
- Version bump (no functional changes)

## [0.1.8] - 2026-05-26

### Added
- Dev compose overlay (`docker-compose.dev.yml`) and entrypoint script
- OpenAPI docs endpoint

### Fixed
- CI: use correct variant for allow-dirty
- CI: trigger docker publish only on tags to avoid duplicates
- CI: allow dirty release workflow for manual edits
- CI: handle existing release in gh release step
- CI: prevent duplicate docker push on tag+master events

## [0.1.7] - 2026-05-24

### Added
- Enhanced graph viewer UX and connectivity
- Enable docker publish on master push

## [0.1.6] - 2026-05-23

### Added
- Enhanced graph overview with relationship toggles and FQN support

## [0.1.5] - 2026-05-15

### Added
- Make `batch_size` and `ingest_concurrency` configurable for performance tuning
- Cluster coordination E2E test

### Docs
- Add performance tuning section and network host guide

## [0.1.4] - 2026-05-10

### Fixed
- Fix glibc compatibility with ubuntu-24.04 custom runner

## [0.1.3] - 2026-05-10

### Added
- cargo-dist installer with auto OS/arch detection
- E2E test workflow

### Docs
- Single curl install command with auto OS/arch detection

## [0.1.2] - 2026-05-10

### Added
- crates.io metadata
- AI assistant skills (Cursor, Copilot, Claude, Gemini)
- Retry logic for transient failures
- Docker Compose hardening

## [0.1.0] - 2026-05-10

### Added
- Initial release: clustered indexing engine with distributed locking and webhooks
- Docker support with debian:trixie-slim builder for glibc 2.40+ compatibility
- CI/CD pipeline with Docker build/push and GitHub releases

---

[Unreleased]: https://github.com/raultov/knot-server/compare/v0.4.1...HEAD
[0.4.1]: https://github.com/raultov/knot-server/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/raultov/knot-server/compare/v0.3.2...v0.4.0
[0.3.2]: https://github.com/raultov/knot-server/compare/v0.3.1...v0.3.2
[0.3.1]: https://github.com/raultov/knot-server/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/raultov/knot-server/compare/v0.2.19...v0.3.0
[0.2.9]: https://github.com/raultov/knot-server/compare/v0.2.8...v0.2.9
[0.2.8]: https://github.com/raultov/knot-server/compare/v0.2.7...v0.2.8
[0.2.7]: https://github.com/raultov/knot-server/compare/v0.2.6...v0.2.7
[0.2.6]: https://github.com/raultov/knot-server/compare/v0.2.5...v0.2.6
[0.2.5]: https://github.com/raultov/knot-server/compare/v0.2.4...v0.2.5
[0.2.4]: https://github.com/raultov/knot-server/compare/v0.2.3...v0.2.4
[0.2.2]: https://github.com/raultov/knot-server/compare/v0.2.1...v0.2.2
[0.2.1]: https://github.com/raultov/knot-server/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/raultov/knot-server/compare/v0.1.17...v0.2.0
[0.1.17]: https://github.com/raultov/knot-server/compare/v0.1.16...v0.1.17
[0.1.16]: https://github.com/raultov/knot-server/compare/v0.1.15...v0.1.16
[0.1.15]: https://github.com/raultov/knot-server/compare/v0.1.14...v0.1.15
[0.1.14]: https://github.com/raultov/knot-server/compare/v0.1.13...v0.1.14
[0.1.13]: https://github.com/raultov/knot-server/compare/v0.1.12...v0.1.13
[0.1.12]: https://github.com/raultov/knot-server/compare/v0.1.11...v0.1.12
[0.1.11]: https://github.com/raultov/knot-server/compare/v0.1.10...v0.1.11
[0.1.10]: https://github.com/raultov/knot-server/compare/v0.1.9...v0.1.10
[0.1.9]: https://github.com/raultov/knot-server/compare/v0.1.8...v0.1.9
[0.1.8]: https://github.com/raultov/knot-server/compare/v0.1.7...v0.1.8
[0.1.7]: https://github.com/raultov/knot-server/compare/v0.1.6...v0.1.7
[0.1.6]: https://github.com/raultov/knot-server/compare/v0.1.5...v0.1.6
[0.1.5]: https://github.com/raultov/knot-server/compare/v0.1.4...v0.1.5
[0.1.4]: https://github.com/raultov/knot-server/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/raultov/knot-server/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/raultov/knot-server/compare/v0.1.0...v0.1.2
[0.1.0]: https://github.com/raultov/knot-server/releases/tag/v0.1.0
