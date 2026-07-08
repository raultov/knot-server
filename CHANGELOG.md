# Changelog

All notable changes to `knot-server` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [0.2.11] - 2026-07-08

### Added
- **Prometheus Metrics:** Added `/metrics` endpoint (on the same port) to expose HTTP, indexing pipeline, registry queue, and process metrics.
- End-to-end metrics tests (`run_metrics_e2e.sh`).
- Documentation and scrape config for Grafana/Prometheus integration in `README.md`.

### Fixed
- Stabilized `Cluster Coordination: Stale Lock Recovery` E2E tests by adding wait loop for recovery job completion.

---

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

[Unreleased]: https://github.com/raultov/knot-server/compare/v0.2.9...HEAD
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
