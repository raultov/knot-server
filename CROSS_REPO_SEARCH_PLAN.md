# Cross-Repo Search & Callers Plan — `GET /api/search`, `GET /api/callers` (knot `RepoScope`)

**Status:** 📝 Planned (not implemented — this document is the implementation contract)
**Approach:** BDD (E2E suite written first, must be RED) + TDD (unit tests per phase, red → green)
**Target version:** 0.4.0 — minor bump, two new endpoints
**Upstream dependency:** `knot 1.8.0` provides `RepoScope` and the `repo_name` projection on
search rows (currently pinned in `Cargo.toml`). **`knot 1.8.1` is published** and implements
reference repo attribution — the planning spec that drove it was consumed and deleted from
the knot repo, so the authoritative record is the **v1.8.1 entry in knot's `CHANGELOG.md`**.
This batch bumps to 1.8.1 (D8); the former callers blocker is **resolved** (D5).

---

## 1. Summary

knot 1.8.0 introduced `RepoScope` (`All` / `One` / `Many`) and threads it down to the DB
layer (`repo_name IN $repo_names` in Neo4j, `MatchValue::Keywords` in Qdrant). knot-server
consumes that API but hard-codes `RepoScope::One(id)` in every handler
(`src/handlers/search.rs:34`, `:102`, `:148`), so the capability is unreachable through the
REST API.

This plan exposes it for **search and callers**, through two **new top-level routes** —
`GET /api/search` and `GET /api/callers` — leaving every existing route byte-for-byte
unchanged. Callers is included because knot 1.8.1 made multi-repo reference rows
attributable (D5); the two endpoints share one scope-resolution core, one E2E suite and one
pair of fixture repositories, so folding them into one batch is cheaper than two.

### Decisions already taken (do not re-litigate during implementation)

| # | Decision | Rationale |
|---|----------|-----------|
| D1 | New top-level routes `/api/search` and `/api/callers`, **not** a `?scope=` parameter on the `/api/repos/{id}/...` routes | With `scope=all` the `{id}` path segment is semantically dead and contradicts the route. Separate routes keep both contracts honest and the existing routes risk-free. |
| D2 | `repo` omitted ⇒ `RepoScope::All` on both routes | They are the cross-repo routes by definition; requiring the parameter would make the common case verbose. |
| D3 | No config flag gating cross-repo access | knot-server has **no API authentication** (only webhook signature validation, `src/webhook.rs:17`), and `GET /api/repos` already lists every repo. `all` grants no new access; a flag would be security theatre. Cost is bounded by D4 instead. |
| D4 | `max_results` clamped to `[1, 100]` on `/api/search` only | The route is unauthenticated and unfiltered by default; an unbounded `max_results` over the whole corpus is a cheap way to exhaust the server. The per-repo route keeps its current unclamped behavior (no regression). `/api/callers` has no such knob — its bound is knot's own `MAX_TARGETS = 25` target truncation (§3.2). |
| D5 | `/api/callers` **is in this batch** | knot 1.8.1 ships `repo_name` / `target_repo_name` on every reference row and `resolution.targets[].repo_name`, so multi-repo caller rows are attributable. It reuses `resolve_scope` verbatim and needs no embedder, making it the cheapest possible second endpoint. |
| D6 | `/explore`, `/graph`, `/graph/expand`, `/deps` stay single-repo | Still technically blocked for explore: `get_file_entities_query` (`knot/src/db/graph/query.rs:282`) **still** projects no `repo_name` in 1.8.1 (deliberately deferred upstream), so a multi-repo scope silently merges N repos into one flat list. Subgraph traversal and deps remain single-repo at the DB layer. **Explicitly out of scope for this batch.** |
| D7 | The `KNOWN_ROUTES` metrics gap is fixed in this batch | Includes the pre-existing `/api/repos/{id}/graph/repos` omission (§6). |
| D8 | Bump `knot` 1.8.0 → **1.8.1** in this batch | **Required** by D5: without it, multi-repo caller rows carry no repository. The release is additive-only: `run_search_hybrid_context` / `run_find_callers` / `run_explore_file` signatures are unchanged (verified against the knot tree), so the bump itself needs no code change. It also gives the existing `/api/repos/{id}/callers` self-labeled rows for free. |

---

## 2. Current state audit

| Concern | Location | Today |
|---------|----------|-------|
| knot pin | `Cargo.toml:43` | `knot = "1.8.0"` — 1.8.1 is published; bumped by D8 |
| Search handler | `src/handlers/search.rs:34-80` | `RepoScope::One(id.clone())`; no registry validation (unknown id ⇒ empty result, HTTP 200) |
| Callers handler | `src/handlers/search.rs:102-125` | idem, no validation. After D8 its rows gain `repo_name` / `target_repo_name` with no code change (passthrough handler) |
| Explore handler | `src/handlers/search.rs:148-182` | validates the id against the registry ⇒ 404 (the only one that does) |
| Params | `src/handlers/models.rs:12-44` | `SearchParams { q, max_results }`, `CallersParams`, `ExploreParams`, `DepsParams` |
| Router / OpenAPI | `src/main.rs:152-173` | `OpenApiRouter` + `routes!` per handler |
| Metrics route allowlist | `src/metrics.rs:14-28` | 13 entries; `/api/repos/{id}/graph/repos` **missing** ⇒ its metrics land under `unmatched` (`intern_route`, `:30`) |
| Registry | `src/registry.rs:148` `get`, `:153` `list` | both take `&mut self` |
| Repo id ⇒ index key | `src/worker.rs:390` | `repo_name: repo.id.clone()` — the registry id **is** the `repo_name` stored in Neo4j/Qdrant, so scope names map 1:1 |
| Handler tests | `src/handlers/tests_common.rs:22` `build_test_app`, `:57` `create_test_state_with_tempdir` | Router built by hand; DBs point at `localhost:9999` (unreachable ⇒ deterministic 500s); `embedder: None` |
| E2E | `tests/run_e2e.sh:121` `create_fixture_repo`; `tests/run_all_e2e.sh:35-40` | one indexed fixture repo; 6 registered suites |
| Agent skills | `skills/*.md` (source) → `scripts/generate_skills_script.py` → `.knot-server-agent-skills.sh` → `.knot-server-agent-skills/` + `.opencode/skills/` | generated artifacts are committed |

---

## 3. API contract (normative)

Both routes share the `repo` parameter, its parsing authority, the registry-membership rule
and the `400` error shape. Only the query subject and the post-validation call differ.

### 3.1 `GET /api/search`

| Parameter | Type | Required | Default | Semantics |
|-----------|------|----------|---------|-----------|
| `q` | string | **yes** | — | Search query. Empty/whitespace ⇒ `400` |
| `repo` | string | no | *(absent ⇒ all repos)* | `RepoScope` syntax: one name, comma-separated list, or the sentinel `all` (case-insensitive) / `*` |
| `max_results` | integer | no | `5` | Clamped to `[1, 100]` (D4). **Global** cap across the whole scope, not per repo |

**Scope parsing** is delegated verbatim to `knot::models::RepoScope::parse` — knot-server
must not reimplement trimming, deduping or sentinel precedence.

**Validation order (fixed, so tests are deterministic):**

1. `q` present and non-blank → else `400 {"error":"Missing required parameter 'q'"}`
2. Scope resolves and every named repo exists in the registry → else
   `400 {"error":"Unknown repository ids: ghost, typo"}` (names sorted, comma-separated).
   `RepoScope::All` skips this check.
3. Embedder initialised → else `500 {"error":"Embedding model not initialized"}`

**Responses**

| Status | Body |
|--------|------|
| `200` | The knot result value, passed through unchanged. Each entity carries `repo_name` (knot 1.8.0). **`null` when there are no hits** — `run_search_hybrid_context` returns `Value::Null`; this mirrors the existing per-repo route and is deliberately not normalised to `[]` |
| `400` | `ErrorResponse` — missing `q`, or unknown repo names |
| `500` | `ErrorResponse` — embedder missing, or knot/DB failure |

**Documented caveats (must appear in README and the search skill):**

- `max_results` is global: with `repo=all` one dominant repository can crowd out the others.
- A repository literally named `all` (or `*`) is **not addressable** through this route — the
  token is the sentinel. Use `/api/repos/all/search`, which builds `RepoScope::One` directly.
- Existence is checked, **status is not**: a registered but not-yet-indexed repo is a valid
  scope member that simply contributes no rows (consistent with the per-repo route).

### 3.2 `GET /api/callers`

| Parameter | Type | Required | Default | Semantics |
|-----------|------|----------|---------|-----------|
| `entity` | string | **yes** | — | Entity name, FQN, or signature fragment. Empty/whitespace ⇒ `400` |
| `repo` | string | no | *(absent ⇒ all repos)* | Identical `RepoScope` syntax and identical membership validation as §3.1 |

There is **no `max_results`**: `run_find_callers` exposes no such knob. The response size is
bounded upstream by knot's `MAX_TARGETS = 25` target cap, surfaced as
`resolution.truncated == true`.

**Validation order (fixed):**

1. `entity` present and non-blank → else `400 {"error":"Missing required parameter 'entity'"}`
2. Scope resolves and every named repo exists → else the same
   `400 {"error":"Unknown repository ids: ..."}` as §3.1
3. **No embedder step** — `run_find_callers` is a pure graph query (`graph_db` only), so the
   handler has one fewer failure mode than search. This asymmetry is deliberate; do not add
   a symmetric embedder check "for consistency".

**Responses**

| Status | Body |
|--------|------|
| `200` | The `find_references` object, passed through unchanged: buckets `calls`, `extends`, `implements`, `references`, `overridden_by`, `overrides`, plus `resolution`. **Always an object** — an entity with no references yields empty buckets, never `null` (unlike search, §3.1) |
| `400` | `ErrorResponse` — missing `entity`, or unknown repo names |
| `500` | `ErrorResponse` — knot/DB failure |

Row shape (knot 1.8.1, additive over 1.8.0): every bucket row carries `repo_name` (the
**referencing** entity's repo) and `target_repo_name` (the **referenced** entity's repo);
`resolution.targets[]` carries `repo_name`. A genuine cross-repo reference is exactly the row
where the two differ.

**Documented caveats (must appear in README and the callers skill):**

- Under `repo=all` a common name resolves against every indexed repository, so
  `resolution.targets` fills up faster and `resolution.truncated` flips to `true` sooner.
  Pass a qualified name (`Namespace.Type.Member`) or narrow the scope to avoid it.
- knot's match ladder is unchanged by scope: exact FQN → FQN suffix → exact name → signature
  prefix → fuzzy. A wide scope does **not** loosen matching, it only widens the candidate set.
- The `all` / `*` sentinel and the not-indexed-repo notes of §3.1 apply verbatim.

---

## 4. Design

### 4.1 New params structs — `src/handlers/models.rs`

```rust
#[derive(Debug, Deserialize, IntoParams)]
pub struct GlobalSearchParams {
    /// The search query string
    #[param(example = "authentication logic")]
    pub q: Option<String>,
    /// Repository scope: one name, a comma-separated list, or `all` / `*`.
    /// Omit to search every indexed repository.
    #[param(example = "repo-a,repo-b")]
    pub repo: Option<String>,
    /// Maximum number of results (global across the scope), clamped to 1..=100
    #[param(example = 5)]
    pub max_results: Option<usize>,
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct GlobalCallersParams {
    /// Name of the entity to find callers for
    #[param(example = "handleRequest")]
    pub entity: Option<String>,
    /// Repository scope: one name, a comma-separated list, or `all` / `*`.
    /// Omit to search every indexed repository.
    #[param(example = "repo-a,repo-b")]
    pub repo: Option<String>,
}
```

The two structs stay separate rather than sharing a `repo`-carrying base: `IntoParams`
derives per struct, and a flattened base would make the generated OpenAPI parameter list
harder to read for no gain.

### 4.2 Pure scope resolution (the unit-testable core)

New module `src/handlers/scope.rs`, kept free of axum, DB and registry types:

```rust
/// Resolve the `repo` query parameter against the set of known repository ids.
///
/// `Ok(scope)` when every named repository exists (or the scope is `All`);
/// `Err(unknown)` with the unknown names **sorted and deduped** otherwise.
pub fn resolve_scope(raw: Option<&str>, known: &[String]) -> Result<RepoScope, Vec<String>>;

/// Clamp a caller-supplied `max_results` into the accepted range.
pub fn clamp_max_results(requested: Option<usize>) -> usize;   // default 5, clamp 1..=100
```

`resolve_scope` = `RepoScope::parse_optional(raw)` + membership check over
`scope.filter_names()`. `All` short-circuits to `Ok`.

### 4.3 Search handler — `src/handlers/search.rs`

```rust
#[utoipa::path(get, path = "/api/search", tag = "Search", params(GlobalSearchParams), ...)]
#[tracing::instrument(
    name = "search_all",
    skip_all,
    fields(query_len = Empty, max_results = Empty, repo_scope = Empty, repo_count = Empty)
)]
pub async fn search_all_handler(
    State(state): State<Arc<AppState>>,
    Query(params): Query<GlobalSearchParams>,
) -> Response
```

Body outline:

1. Validate `q`; record `query_len` (never the query text — same rule as `search_handler`).
2. Snapshot the registry ids **inside a block that ends before the first `.await`** — a
   `std::sync::MutexGuard` is not `Send` and would not compile across an await point
   (`explore_handler:164-173` uses the same pattern):
   ```rust
   let known: Vec<String> = {
       let mut registry = state.registry.lock().unwrap();
       registry.list().iter().map(|r| r.id.clone()).collect()
   };
   ```
3. `resolve_scope(params.repo.as_deref(), &known)` → `400` on `Err`.
4. Record `repo_scope` (`"all" | "one" | "many"`) and `repo_count` (`0` for `All`).
   Names are not recorded: the kind + count is what makes a trace readable, and it keeps the
   span payload bounded for `all` over a large cluster.
5. Embedder check → `500`.
6. `knot::cli_tools::run_search_hybrid_context(q, max_results, &scope, &SearchContext { .. })`
   — identical call shape to `search_handler`, only the scope differs.

### 4.4 Callers handler — `src/handlers/search.rs`

```rust
#[utoipa::path(get, path = "/api/callers", tag = "Search", params(GlobalCallersParams), ...)]
#[tracing::instrument(
    name = "callers_all",
    skip_all,
    fields(entity = Empty, repo_scope = Empty, repo_count = Empty)
)]
pub async fn callers_all_handler(
    State(state): State<Arc<AppState>>,
    Query(params): Query<GlobalCallersParams>,
) -> Response
```

Same skeleton as §4.3 with three differences:

1. Subject is `entity`, not `q`. It **is** recorded in the span (`callers_handler:116`
   already records it): an entity name is a public identifier, not user prose, so the
   privacy argument that hides `q` does not apply here.
2. No embedder step (§3.2).
3. Final call: `knot::cli_tools::run_find_callers(entity, &scope, &state.graph_db)`.

Steps 2–4 of §4.3 (registry snapshot in a scoped block, `resolve_scope`, span scope fields)
are **identical and must be factored, not copy-pasted** — extract a small private helper in
`scope.rs`, e.g.:

```rust
/// Snapshot the registry ids and resolve the scope, or render the 400 response.
pub fn scope_or_error(state: &AppState, raw: Option<&str>) -> Result<RepoScope, Response>;
```

It touches `AppState` and axum, so it lives beside `resolve_scope` but is **not** the pure
function — §8.1 tests target `resolve_scope`; `scope_or_error` is covered through the two
handlers' tests (§8.2, §8.3).

### 4.5 Wiring

- `src/handlers/mod.rs` — `pub mod scope;`.
- `src/main.rs` — `.routes(routes!(handlers::search_all_handler))` and
  `.routes(routes!(handlers::callers_all_handler))` next to the existing search routes.
- `src/handlers/tests_common.rs:22` — add `/api/search` and `/api/callers` to `build_test_app`.
- No route collision: `/api/search` and `/api/callers` are literal segments, distinct from
  `/api/repos/{id}` and from each other.

### 4.6 What deliberately does **not** change

`search_handler`, `callers_handler`, `explore_handler`, `deps_handler`, the graph handlers
and all their routes keep their current behavior — including `search_handler`'s lack of
registry validation. Aligning that is a separate, breaking-ish decision (200 → 404) and is
out of this batch.

---

## 5. Metrics fix (D7)

`src/metrics.rs:14` — add all three missing entries:

```rust
    "/api/search",
    "/api/callers",
    "/api/repos/{id}/graph/repos",
```

`/api/repos/{id}/graph/repos` has been a live route since 0.3.4 but was never added to the
allowlist, so all of its requests are counted as `unmatched` — a real, pre-existing metrics
bug fixed here.

**Drift guard** (new unit test in `src/metrics.rs`): parse the `#[utoipa::path(... path = "…")]`
declarations out of the handler sources at compile time via `include_str!` and assert every
`/api/...` literal appears in `KNOWN_ROUTES`.

```rust
const HANDLER_SOURCES: &[&str] = &[
    include_str!("handlers/repo.rs"),      include_str!("handlers/indexing.rs"),
    include_str!("handlers/progress.rs"),  include_str!("handlers/search.rs"),
    include_str!("handlers/graph.rs"),     include_str!("handlers/repo_graph.rs"),
    include_str!("handlers/webhooks.rs"),  include_str!("handlers/health.rs"),
];
```

Extraction is plain string splitting on `path = "` → up to the next `"` (no regex
dependency). Known limitation, to be stated in a comment: a **new handler file** must be
added to `HANDLER_SOURCES`; the guard catches new *routes* in existing files, which is the
common case.

---

## 6. BDD — E2E suite (Phase 0b, must be RED)

New suite `tests/run_cross_repo_search_e2e.sh`, registered as the **7th** entry in
`tests/run_all_e2e.sh` (after `:40`):

```bash
run_test "Cross-Repo Scopes: /api/search + /api/callers" "run_cross_repo_search_e2e.sh"
```

**Fixtures.** The suite needs two properties that pull in opposite directions — *disjoint*
names so search-scope assertions cannot be satisfied by the wrong repo, and a *homonym* so
callers has something genuinely ambiguous to attribute. Both are provided by a dedicated
fixture tree, **independent of `tests/fixtures/*.java`** so no other suite's repo content
changes:

```
tests/fixtures/cross_repo/
  repo_a/  AlphaService.java   (class AlphaService)          ← unique to A
           SharedUtil.java     (class SharedUtil { work() }) ← homonym
           AlphaCaller.java    (calls SharedUtil.work())     ← caller in A
  repo_b/  BillingService.java (class BillingService)        ← unique to B
           SharedUtil.java     (class SharedUtil { work() }) ← homonym, same FQN
           BetaCaller.java     (calls SharedUtil.work())     ← caller in B
```

This mirrors knot's own `tests/testing_files/repo_scope/{scope_alpha,scope_beta}` design,
which exists for exactly this reason. Two bare repos are built by cloning
`create_fixture_repo` (`tests/run_e2e.sh:121`) into a helper parameterised by source
directory. Both are registered via `POST /api/repos` with a filesystem URL and indexed
(reuse the `sync_and_wait_indexed` / status-polling pattern from `run_e2e.sh:75-115`);
capture `REPO_A_ID` and `REPO_B_ID` from the responses.

### Group S — `/api/search`

```gherkin
Scenario S1: default scope spans every indexed repository
  When  GET /api/search?q=AlphaService
  Then  status is 200
  And   at least one entity has repo_name == REPO_A_ID

Scenario S2: the union really is a union
  When  GET /api/search?q=SharedUtil
  Then  entities include repo_name == REPO_A_ID and repo_name == REPO_B_ID

Scenario S3: explicit sentinel behaves like the default
  When  GET /api/search?q=SharedUtil&repo=all
  Then  the repo_name set equals the one from S2

Scenario S4: comma list restricts to the listed repos
  When  GET /api/search?q=SharedUtil&repo=<REPO_B_ID>
  Then  status is 200
  And   every returned entity has repo_name == REPO_B_ID
  And   no entity has repo_name == REPO_A_ID   # the homonym must not leak

Scenario S5: two-element list returns both
  When  GET /api/search?q=SharedUtil&repo=<REPO_A_ID>,<REPO_B_ID>
  Then  both repo_name values appear

Scenario S6: whitespace and duplicates are tolerated
  When  GET /api/search?q=SharedUtil&repo=" <A> , <A> , <B> "
  Then  status is 200 and both repos appear (dedupe/trim handled by RepoScope::parse)

Scenario S7: unknown repository names are rejected loudly
  When  GET /api/search?q=SharedUtil&repo=<REPO_A_ID>,ghost
  Then  status is 400
  And   the error message contains "ghost"

Scenario S8: missing query
  When  GET /api/search
  Then  status is 400

Scenario S9: max_results is clamped, not honored blindly
  When  GET /api/search?q=SharedUtil&max_results=99999
  Then  status is 200 and at most 100 entities are returned

Scenario S10 (regression guard): the per-repo route is untouched
  When  GET /api/repos/<REPO_A_ID>/search?q=AlphaService
  Then  status is 200 and every entity has repo_name == REPO_A_ID

Scenario S11 (regression guard): a repo id is never parsed as a scope sentinel
  When  GET /api/repos/<REPO_A_ID>/search?q=SharedUtil
  Then  results are confined to REPO_A_ID
```

### Group C — `/api/callers`

The homonym `SharedUtil.work()` is called from `AlphaCaller` in repo A and `BetaCaller` in
repo B, with colliding repo-relative paths — the case that was unattributable before
knot 1.8.1.

```gherkin
Scenario C1: default scope finds callers in every repository
  When  GET /api/callers?entity=SharedUtil.work
  Then  status is 200
  And   the union of all buckets contains a row named AlphaCaller
  And   the union of all buckets contains a row named BetaCaller

Scenario C2: every returned row is attributable
  When  GET /api/callers?entity=SharedUtil.work
  Then  every row in every non-empty bucket has a non-null repo_name
  And   the AlphaCaller row has repo_name == REPO_A_ID
  And   the BetaCaller row has repo_name == REPO_B_ID

Scenario C3: resolution targets are attributable too
  When  GET /api/callers?entity=SharedUtil.work
  Then  resolution.targets has 2 entries
  And   their repo_name values are exactly {REPO_A_ID, REPO_B_ID}

Scenario C4: single-repo scope restricts and still labels
  When  GET /api/callers?entity=SharedUtil.work&repo=<REPO_A_ID>
  Then  status is 200
  And   a row named AlphaCaller is present with repo_name == REPO_A_ID
  And   no row named BetaCaller is present

Scenario C5: comma list unions the listed repos
  When  GET /api/callers?entity=SharedUtil.work&repo=<REPO_A_ID>,<REPO_B_ID>
  Then  both AlphaCaller and BetaCaller are present

Scenario C6: unknown repository names are rejected loudly
  When  GET /api/callers?entity=SharedUtil.work&repo=ghost
  Then  status is 400
  And   the error message contains "ghost"

Scenario C7: missing entity
  When  GET /api/callers
  Then  status is 400
  And   the error message mentions 'entity'

Scenario C8: unreferenced entity is an empty object, never null
  When  GET /api/callers?entity=BillingService&repo=<REPO_B_ID>
  Then  status is 200
  And   the body is a JSON object with the six buckets present
  And   every bucket is an empty array (or contains only rows in REPO_B_ID)

Scenario C9 (regression guard): the per-repo callers route is untouched
  When  GET /api/repos/<REPO_A_ID>/callers?entity=SharedUtil.work
  Then  status is 200
  And   AlphaCaller is present and BetaCaller is not
  And   the AlphaCaller row carries repo_name == REPO_A_ID   # gained via the 1.8.1 bump
```

**Red criterion:** S1–S9 and C1–C8 fail with `404` (routes absent) before implementation.
S10–S11 and C9 pass from the start — except C9's last assertion, which is **red until
Phase 0a** (the knot 1.8.1 bump) and is the E2E proof that the bump landed.

---

## 7. Implementation phases (TDD)

`cargo fmt` + `cargo clippy --all-targets --all-features -- -D warnings` + `cargo test`
green at every phase boundary.

| Phase | Content | Exit criterion |
|-------|---------|----------------|
| **0a — Upstream bump (D8)** | `knot = "1.8.1"` in `Cargo.toml` + `Cargo.lock` | `cargo check --all-targets` clean with **zero** source edits (the release is additive; any required edit means an unannounced break — stop and re-audit). Unit tests green |
| **0b — BDD red** | `tests/run_cross_repo_search_e2e.sh` + `fixtures/cross_repo/{repo_a,repo_b}` + registration in `run_all_e2e.sh` | Suite runs; S1–S9 and C1–C8 red, S10–S11 and C9 green (C9 fully green only after 0a); red count recorded in the commit message |
| **1 — Pure core** | Unit tests of §8.1 (red) → implement `src/handlers/scope.rs` (`resolve_scope`, `clamp_max_results`) | §8.1 green; no axum involved |
| **2 — Search handler + wiring** | Unit tests of §8.2 (red) → `GlobalSearchParams`, `search_all_handler`, `scope_or_error`, `main.rs` route, `build_test_app` route | §8.2 green; E2E group S green |
| **3 — Callers handler** | Unit tests of §8.3 (red) → `GlobalCallersParams`, `callers_all_handler`, route + `build_test_app` entry, reusing `scope_or_error` unchanged | §8.3 green; E2E group C green. **No new logic in `scope.rs`** — if this phase needs to touch `resolve_scope`, the §4.4 factoring was wrong |
| **4 — Metrics** | Drift-guard test (red — it now catches `graph/repos` **and** both new routes) → add the three `KNOWN_ROUTES` entries | §8.4 green |
| **5 — Docs** | §9 in full | Every surface updated and regenerated |
| **6 — Gates** | `cargo fmt`, `clippy -D warnings`, `cargo test`, `./tests/run_all_e2e.sh` | **7/7** suites green, all E2E scenarios green |

Phase 3 is deliberately sequenced **after** Phase 2 rather than merged with it: it is the
cheap proof that the scope core is reusable. If callers can be added without editing
`scope.rs`, the abstraction is right; if not, it is better to find out in a 30-line handler
than in the docs phase.

---

## 8. Test matrix (unit)

### 8.1 `src/handlers/scope.rs`

| Test | Asserts |
|------|---------|
| `absent_repo_param_is_all` | `resolve_scope(None, &known)` ⇒ `RepoScope::All` |
| `empty_repo_param_is_all` | `Some("")` and `Some("  ")` ⇒ `All` |
| `sentinel_all_is_case_insensitive` | `"all"`, `"ALL"`, `"All"`, `"*"` ⇒ `All` |
| `single_known_repo_is_one` | `Some("repo-a")` ⇒ `One("repo-a")` |
| `list_of_known_repos_is_many` | `"repo-a,repo-b"` ⇒ `Many([a, b])` |
| `whitespace_and_duplicates_are_normalised` | `" repo-a , repo-a , repo-b "` ⇒ `Many([a, b])` |
| `unknown_single_name_is_rejected` | `Err(["ghost"])` |
| `unknown_names_are_sorted_and_deduped` | `"z,ghost,ghost"` ⇒ `Err(["ghost", "z"])` |
| `partially_unknown_list_is_rejected_whole` | `"repo-a,ghost"` ⇒ `Err(["ghost"])` — no silent partial scope |
| `sentinel_skips_membership_check` | `"all"` with empty `known` ⇒ `Ok(All)` |
| `sentinel_wins_over_unknown_name_in_list` | `"all,ghost"` ⇒ `Ok(All)` (knot's precedence rule) |
| `repo_names_are_case_sensitive` | `"REPO-A"` with known `repo-a` ⇒ `Err(["REPO-A"])` |
| `clamp_defaults_to_five` / `clamp_floor_is_one` / `clamp_ceiling_is_hundred` | `None ⇒ 5`, `Some(0) ⇒ 1`, `Some(99999) ⇒ 100` |

### 8.2 Search handler — `src/handlers/tests_common.rs`

Add `/api/search` to `build_test_app` (`:22`). With `embedder: None` and unreachable DBs, a
*valid* request deterministically reaches the `500` branch — which is exactly what pins the
validation order.

| Test | Asserts |
|------|---------|
| `search_all_missing_q_returns_400` | body mentions `'q'` |
| `search_all_blank_q_returns_400` | `q=%20%20` |
| `search_all_unknown_repo_returns_400` | error lists the unknown id |
| `search_all_unknown_repo_checked_before_embedder` | unknown repo ⇒ `400`, **not** `500` (order pin) |
| `search_all_valid_scope_without_embedder_returns_500` | "Embedding model not initialized" |
| `search_all_sentinel_with_empty_registry_reaches_embedder_check` | `repo=all` ⇒ `500`, not `400` |
| `search_all_known_repo_passes_validation` | register a repo first, then `repo=<id>` ⇒ `500` |
| `per_repo_search_route_unchanged` | `/api/repos/ghost/search?q=x` still `500` (no new 404) — regression pin for §4.6 |

### 8.3 Callers handler — `src/handlers/tests_common.rs`

Add `/api/callers` to `build_test_app`. Here the unreachable graph DB is the *only* failure
mode after validation, so a valid request lands on `500` with a knot/DB error — never on the
embedder message, which is the assertion that pins the missing-embedder-step of §3.2.

| Test | Asserts |
|------|---------|
| `callers_all_missing_entity_returns_400` | body mentions `'entity'` |
| `callers_all_blank_entity_returns_400` | `entity=%20%20` ⇒ `400` |
| `callers_all_unknown_repo_returns_400` | error lists the unknown id |
| `callers_all_unknown_repo_checked_before_db_call` | unknown repo ⇒ `400`, not `500` (order pin) |
| `callers_all_valid_scope_reaches_graph_db` | `500` whose body does **not** mention the embedder (asymmetry pin vs §8.2) |
| `callers_all_sentinel_with_empty_registry_is_accepted` | `repo=all` ⇒ `500`, not `400` |
| `callers_all_known_repo_passes_validation` | register a repo, then `repo=<id>` ⇒ `500` |
| `callers_all_and_search_all_share_scope_errors` | the `400` body for the same bad `repo` value is byte-identical across both routes (proves the shared helper) |
| `per_repo_callers_route_unchanged` | `/api/repos/ghost/callers?entity=x` still `500` (no new 404) |

### 8.4 Metrics — `src/metrics.rs`

| Test | Asserts |
|------|---------|
| `known_routes_covers_every_declared_api_path` | drift guard of §5 (fails today for `graph/repos`, and for both new routes once their handlers exist) |
| `intern_route_maps_global_search` | `intern_route("/api/search") == "/api/search"` |
| `intern_route_maps_global_callers` | `intern_route("/api/callers") == "/api/callers"` |
| `known_routes_has_no_duplicates` | cheap invariant |

---

## 9. Documentation surface (all of it — none optional)

1. **`README.md`**
   - §`🔍 Code Intelligence Search` (line 135): new bullets for `GET /api/search?q=...&repo=...`
     and `GET /api/callers?entity=...&repo=...`, with the scope syntax table and the caveats
     of §3.1 / §3.2.
   - Note next to the per-repo bullets that they remain single-repo by design.
   - Mention that `/api/repos/{id}/callers` rows now carry `repo_name` (knot 1.8.1 bump).
2. **`CHANGELOG.md`** — `[Unreleased]` → `## [0.4.0]` (§11).
3. **`knot-server.postman_collection.json`** — two new requests, *Global Search* and
   *Global Callers*, in the folder style of the existing `Search` / `Callers` entries
   (`repo` present but disabled by default, to demonstrate D2).
4. **Agent skills — respect the generation pipeline.** Source of truth is `skills/*.md`;
   `.knot-server-agent-skills/` and `.opencode/skills/` are **generated and committed**:
   1. Edit `skills/search.md` (new "Cross-repo search" section: endpoint, syntax, when to
      prefer it over per-repo search, the `max_results` caveat).
   2. Edit `skills/callers.md` (cross-repo section: endpoint, syntax, how to read
      `repo_name` vs `target_repo_name`, and the `resolution.truncated` caveat of §3.2).
   3. Edit `skills/workflows.md` (a cross-repo discovery workflow: `/api/search` to locate,
      `/api/callers` to assess impact across repos) and `skills/index.md`
      (skill/endpoint table).
   4. Regenerate the installer: `python3 scripts/generate_skills_script.py`.
   5. Refresh the committed outputs by running the installer locally
      (`./.knot-server-agent-skills.sh --no-register`) and commit the diff in
      `.knot-server-agent-skills/` **and** `.opencode/skills/`.
   6. Verify `skills/copilot-instructions.md`, `skills/cursor-rules.md` and
      `skills/system-prompt.md` mention both endpoints where they enumerate the API.
5. **OpenAPI** — descriptions come from the `#[utoipa::path]` attribute; the Swagger page
   needs no separate edit. Confirm both new operations render under the `Search` tag at
   `/docs`.

---

## 10. Risks & mitigations

| Risk | Severity | Mitigation |
|------|----------|------------|
| `MutexGuard` held across `.await` ⇒ compile error | Low (caught by rustc) | Registry snapshot in a scoped block, §4.3 step 2, shared by both handlers via `scope_or_error` |
| `max_results` global cap makes `all` look "broken" (one repo dominates) | Medium | Documented in README + skill; D4 clamp bounds the blast radius |
| Unauthenticated corpus-wide scan as a cost amplifier | Medium | D4 clamp on search; callers is bounded by knot's `MAX_TARGETS = 25`. Per-request cost profile is otherwise unchanged (one Qdrant filter / one Cypher `IN`) |
| **Callers under `repo=all`: silent target truncation** | Medium | A common name resolves against every repo, so `resolution.truncated` flips to `true` and the answer is a *sample*, not the full set. Callers must read the flag — documented in §3.2, the README and `skills/callers.md`; E2E C3 pins the untruncated 2-target case |
| **Copy-paste divergence between the two handlers** | Medium | §4.4 mandates the shared `scope_or_error`; §8.3 pins it with a test asserting both routes emit a byte-identical `400` for the same bad `repo` |
| Repo literally named `all` becomes unreachable on the new routes | Low | Documented; `/api/repos/all/search` and `/api/repos/all/callers` still work (they build `One` directly) |
| E2E runtime grows (two index runs + a larger fixture set) | Medium | Both fixture repos are 3 small Java files each; the suite reuses the existing compose stack and covers **both** endpoints in one setup |
| Skills drift (editing generated files instead of `skills/*.md`) | Medium | §9.4 spells out the pipeline order |
| knot bumps its `RepoScope` parse rules later | Low | knot-server never reimplements parsing — single upstream authority |

---

## 11. Release

**0.4.0** (minor: two new endpoints, no breaking change).

The existing `[Unreleased]` entry (the knot 1.8.0 bump, already written) is folded into this
section rather than duplicated.

```markdown
## [0.4.0] - YYYY-MM-DD

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
  `GET /api/repos/{id}/callers` rows self-labeling.

### Fixed
- **HTTP metrics attributed to `unmatched`:** `/api/repos/{id}/graph/repos` (live since
  0.3.4) was missing from `KNOWN_ROUTES`, so every request to it was counted under the
  `unmatched` route label. Added, together with `/api/search` and `/api/callers`, plus a
  drift-guard test that fails when a handler declares a route that the allowlist does not
  know.
```

---

## 12. Out of scope for this batch (recorded so it is not re-discovered)

- `/api/repos/{id}/graph` and `/graph/expand` — **explicitly excluded by decision**.
- `/api/repos/{id}/explore` multi-repo — blocked by `get_file_entities` not projecting
  `repo_name` upstream (silent merge); no consumer demand.
- `/api/repos/{id}/deps` — single-repo subject, not a filter.
- Adding registry validation to the existing `/search` and `/callers` routes (200 → 404 would
  be a behavior change for current clients).
- A cross-repo `/api/explore` — genuinely blocked upstream (D6), not merely unscheduled. It
  becomes viable only if knot projects `repo_name` in `get_file_entities_query`; that is the
  single upstream change to request if the need appears.
