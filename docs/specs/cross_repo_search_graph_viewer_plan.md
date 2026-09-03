# Cross-Repo Search in the Graph Viewer + `repo=all` Registry Semantics

**Status:** 📝 Planned (not implemented — this document is the implementation contract)
**Approach:** BDD (E2E + browser scenarios written first, must be RED) + TDD (unit tests per phase)
**Target version:** 0.4.1 — patch: one behavior fix, one viewer feature, one CSS bug
**Upstream dependency:** none. `knot 1.8.1` already pinned; no bump, no re-index.

---

## 1. Summary

0.4.0 exposed knot's `RepoScope` through `GET /api/search` and `GET /api/callers`. Two gaps
remain, and they are coupled:

1. **`repo=all` does not mean "all registered repositories".** `resolve_scope`
   (`src/handlers/scope.rs:30-32`) short-circuits `RepoScope::All` past the registry
   membership check, so `All` reaches knot as an *unfiltered* query and returns rows for
   repositories that are no longer in the registry. The asymmetry is stark: `repo=ghost` is
   rejected with `400`, but `repo=all` returns `ghost`'s rows.
2. **The `/graph` viewer's search box is single-repo.** It calls
   `/api/repos/{id}/search` (`assets/graph-viewer.html:528-530`) and cannot find an entity
   the user has not already selected the repository for.

They are coupled because (2) cannot ship on top of (1): a result labeled with an
unregistered `repo_name` is **unopenable** in the viewer — no dropdown option exists and
`/api/repos/{ghost}/graph` returns `404`. Fixing (1) is a precondition for (2).

This batch fixes the semantics for every consumer (agents included), then adds a single
"search all repos" checkbox to the viewer.

### Decisions already taken (do not re-litigate during implementation)

| # | Decision | Rationale |
|---|----------|-----------|
| D1 | `RepoScope::All` expands to the **registry id list** inside `resolve_scope` | Makes `repo=all` and omitted-`repo` mean "all *registered* repositories" for every consumer. Closes the audited asymmetry (§2). Recorded as a **fix**, not a feature: the old behavior leaked data from repositories the operator had deleted. |
| D2 | Expansion normalizes: 1 id ⇒ `One`, ≥2 ⇒ `Many`, ids **sorted** | Sorting makes handler tests deterministic. `repo_name IN [...]` is order-insensitive at the DB layer, so sorting is free. |
| D3 | Empty registry ⇒ a distinct `ResolvedScope::NoRepositories` variant; handlers return an empty body **without calling knot** | **`Many(vec![])` is indistinguishable from `All`.** `filter_names()` returns `vec![]` for both, and knot documents *"empty list indicates no DB filter"* (`knot-1.8.1/src/models/repo_scope.rs:97`). Expanding `All → Many([])` on an empty registry would therefore mean *everything* — the exact inverse of D1. A sentinel repo name was considered and rejected: it puts a magic string in queries and logs to dodge a case the type system can state outright. |
| D4 | The two hand-built empty bodies are pinned by an **E2E key-set drift guard** | The objection to hand-building `/api/callers`' six-bucket shape is drift. It is testable: knot's natural empty response for a nonexistent entity is fetched in the same suite and its key set compared. Drift fails the suite. |
| D5 | Per-repo routes stay **byte-for-byte unchanged** | `/api/repos/{id}/search` and `/api/repos/{id}/callers` build `RepoScope::One` directly (`src/handlers/search.rs:68`, `:123`) and never touch `resolve_scope`. Zero regression surface. |
| D6 | Viewer "all repos" mode **omits the `repo` parameter** | D1 makes omission exactly correct. No client-side id list, no drift between what the viewer thinks is registered and what the server knows. |
| D7 | Scope control is a **single checkbox**, not a multi-select popover | Two modes only: current repo / all registered. The toolbar has **16px of slack** and already wraps to 4 rows (§2); a checkbox fits inside the existing search wrapper. |
| D8 | Cross-repo **edges** in the 3D graph are out of reach | `src/handlers/graph_queries.rs:126-144` pins `{repo_name: $repo_name}` on **both** endpoints of every edge match. Cross-repo edges are filtered out by construction. Changing this is a separate batch (§11). |
| D9 | No `/api/callers` UI in the viewer | Caller rows carry **no `uuid`** — only `resolution.targets[]` do — so they cannot become graph nodes. It would be a new text panel, a distinct feature. |
| D10 | `#node-details` top/height computed from the live toolbar height | Pre-existing bug, in scope because this batch adds a control to the toolbar row and makes wrapping more likely. |

---

## 2. Current state audit

Measured live against a 7-repository local instance running 0.4.0.

| Concern | Location | Today |
|---|---|---|
| `All` bypasses membership | `src/handlers/scope.rs:30-32` | `if matches!(scope, RepoScope::All) { return Ok(scope); }` |
| `All` ⇒ unfiltered at DB | `knot-1.8.1/src/models/repo_scope.rs:97`, `:105` | `filter_names()` ⇒ `vec![]`; `is_unfiltered()` ⇒ `true` |
| Ghost rows are real | live `GET /api/search?q=class&max_results=100` | `core-js:40, openclaw:21, angular.js:16, koodo-reader:12, puppeteer:5, esphome:2, ComfyUI:1, ngx-admin:1, knot:1, okhttp:1` — only `puppeteer` and `knot` are registered |
| Asymmetry | live | `repo=openclaw` ⇒ `400 Unknown repository ids: openclaw`; `repo=all` ⇒ returns openclaw rows |
| Ghost rows are unopenable | live | `GET /api/repos/openclaw` ⇒ 404; `GET /api/repos/openclaw/graph` ⇒ 404 |
| Scope core | `src/handlers/scope.rs:28` `resolve_scope`, `:66` `scope_or_error`, `:79` `unknown_repos_error`, `:90` `scope_fields` | shared verbatim by both cross-repo handlers |
| Cross-repo handlers | `src/handlers/search.rs:161` `search_all_handler`, `:241` `callers_all_handler` | resolve at `:178-181` / `:258-261` |
| Params | `src/handlers/models.rs:30-41`, `:44-52` | `GlobalSearchParams`, `GlobalCallersParams` |
| Graph is single-repo by construction | `src/handlers/graph_queries.rs:126,132,138,143-144` | `{repo_name: $repo_name}` on both edge ends |
| Viewer search | `assets/graph-viewer.html:528-530` `searchEntity`, `:1070-1107` `doSearch` | `/api/repos/{id}/search?q=&max_results=10`; response consumed as a bare array |
| Viewer result click | `:1216-1265` `selectEntity` | focus mode, `state.selectedRepo`-scoped |
| Repo-switch precedent | `:1126-1145` `#open-repo-btn` | sets `select.value`, dispatches synthetic `change` — reused by this batch |
| Toolbar capacity | measured @1600px viewport | **1600px wide, 92px tall, 4 visual rows**, last child ends at 1584 ⇒ **16px slack** |
| Dropdown overflows | `:53-66` `#search-results` | `left: 0; min-width: 400px` ⇒ spans 1213→1613 in a 1600px viewport (**13px past the edge**) |
| Details panel overlap | `:91-102` `#node-details` | `top: 50px; height: calc(100vh - 50px)` vs a **92px** toolbar ⇒ title renders behind the toolbar |
| Result markup | `:1084-1101` | built by string concat into `innerHTML`, **unescaped** |
| E2E | `tests/run_cross_repo_search_e2e.sh`, `tests/run_all_e2e.sh:41` | 7 suites; groups S and C |

**Performance baseline** (same instance): global search `0.55s` @ `max_results=10`,
`1.05s` @ `50`; single-repo `0.17s` @ `10`.

**Crowding baseline:** `repo=<all 7 registered>&max_results=30` ⇒ `puppeteer:19,
spring-ai:10, knot-server:1` — **four repositories returned zero rows**. `max_results` is a
global cap, not per-repo; this is inherent to the endpoint and is documented, not fixed.

---

## 3. API contract (normative)

Only the meaning of `All` changes. Parameter syntax, the `400` shape and the per-repo routes
are untouched.

| `repo` value | 0.4.0 (today) | 0.4.1 (this batch) |
|---|---|---|
| *(omitted)* | every repo **in the databases** | every repo **in the registry** |
| `all` / `*` (case-insensitive) | every repo in the databases | every repo in the registry |
| `repo-a` | that repo; `400` if unregistered | unchanged |
| `repo-a,repo-b` | union; `400` if any unregistered | unchanged |
| *(any, empty registry)* | every repo in the databases | **empty result, `200`** |

Parsing authority remains `knot::models::RepoScope::parse` — knot-server still never
reimplements trimming, deduping or sentinel precedence. Expansion happens strictly *after*
parsing.

**Empty-result bodies** (empty registry only):

- `GET /api/search` ⇒ `[]`
- `GET /api/callers` ⇒
  ```json
  {"calls":[],"extends":[],"implements":[],"overridden_by":[],"overrides":[],
   "references":[],
   "resolution":{"fuzzy":false,"query":"<entity>","targets":[],"tier":"none","truncated":false}}
  ```
  Key set verified against knot's natural empty response (§5, G6).

---

## 4. Design

### 4.1 `ResolvedScope` — `src/handlers/scope.rs`

```rust
/// The outcome of resolving the `repo` parameter against the registry.
///
/// `NoRepositories` exists because `RepoScope::Many(vec![])` is **not** a
/// representable "nothing": `filter_names()` returns an empty vec for it, and
/// knot's DB layer treats an empty filter list as *unfiltered*
/// (`knot::models::RepoScope::filter_names`). Expanding `All` over an empty
/// registry therefore cannot be expressed as a `RepoScope` at all — the caller
/// must skip the query entirely.
#[derive(Debug, PartialEq, Eq)]
pub enum ResolvedScope {
    Scope(RepoScope),
    NoRepositories,
}
```

### 4.2 `resolve_scope` — the pure, unit-testable core

Signature changes to `Result<ResolvedScope, Vec<String>>`. Behavior:

1. Parse via `RepoScope::parse_optional(raw)` (unchanged authority).
2. If `All`: expand against `known` — `[]` ⇒ `NoRepositories`; `[one]` ⇒ `Scope(One)`;
   `[a, b, ..]` ⇒ `Scope(Many(sorted))` (D2).
3. Otherwise: membership-check `filter_names()` against `known`; unknown names sorted +
   deduped ⇒ `Err`. Unchanged.

The `known` slice is assumed pre-sorted by the caller; `resolve_scope` sorts defensively so
the function is total and order-independent in tests.

### 4.3 `scope_or_error` and the handlers — `src/handlers/search.rs`

`scope_or_error` keeps its registry-snapshot-in-a-scoped-block shape (the `MutexGuard` is
not `Send` across an `.await`) and forwards the new return type. Both handlers gain one arm:

```rust
let scope = match scope_or_error(&state, params.repo.as_deref()) {
    Ok(ResolvedScope::Scope(scope)) => scope,
    Ok(ResolvedScope::NoRepositories) => return empty_search_response(),   // or empty_callers_response(entity_name)
    Err(unknown) => return unknown_repos_error(&unknown),
};
```

Validation order is preserved exactly: `q`/`entity` ⇒ scope ⇒ embedder/DB. The
`NoRepositories` arm sits **after** the parameter check, so `GET /api/search` with no `q`
still returns `400` on an empty registry.

`scope_fields` gains a `("none", 0)` case so tracing spans stay complete.

### 4.4 Viewer — scope checkbox

Into the existing `position: relative` search wrapper (`:291`), before `#search-input`:

```html
<label id="scope-all-label" title="Search across every registered repository">
  <input type="checkbox" id="scope-all" /> All repos
</label>
```

Styled with the existing toolbar control conventions; **no new toolbar group** (D7). The
checkbox is enabled/disabled alongside `#search-input` in the repo `change` handler
(`:1052`), and its state is **not** reset on repo switch — a user who opted into global
search stays in it.

### 4.5 Viewer — search routing

`searchEntity(repoId, query, maxResults)` gains a scope argument:

- unchecked ⇒ `/api/repos/{id}/search?q=&max_results=10` — **unchanged**, the fast path
  (`0.17s`).
- checked ⇒ `/api/search?q=&max_results=40`, **`repo` omitted** (D6).

`max_results` rises to 40 in global mode to blunt crowding (§2); it stays under the server's
`100` clamp. `doSearch` gains a guard for a non-array response (`null` / object) before
reading `.length`.

### 4.6 Viewer — result rendering

Results are grouped under a per-repository header in global mode, and each row carries a
repo badge (new `.repo-badge` class, modeled on `.synthetic-badge` at `:143-153`). All
interpolated values — now including `repo_name` — go through an `escapeHtml` helper, closing
the pre-existing unescaped-`innerHTML` hole at `:1084-1101`.

`#search-results` switches from `left: 0` to `right: 0` so the 400px dropdown stops
overflowing the viewport (§2).

### 4.7 Viewer — cross-repo navigation

`selectEntity(entity)` gains a leading branch:

```
if entity.repo_name && entity.repo_name !== state.selectedRepo:
    if no <option> with that value exists:   # defensive; D1 should make this unreachable
        status "Repository <x> is not registered"; abort
    set #repo-select.value = entity.repo_name
    dispatch synthetic 'change'              # reuses the #open-repo-btn idiom (:1126-1145)
    await the overview settling, then run the existing focus path
```

The `change` handler (`:1021-1064`) already resets depth, toggles, focus state and reloads
the overview, so the repo transition needs no new teardown logic. Focus is deferred until
that path completes; the entity `uuid` is carried across, and the existing
`fetchSubgraph(state.selectedRepo, …)` call at `:1240` then resolves it against the correct
repository.

### 4.8 Viewer — `#node-details` overlap (D10)

Replace the hardcoded `top: 50px` / `height: calc(100vh - 50px)` with values derived from
`document.getElementById('toolbar').offsetHeight`, applied on `DOMContentLoaded` and on
`resize`. `#status-bar` keeps its own fixed placement.

### 4.9 What deliberately does **not** change

- `/api/repos/{id}/search`, `/callers`, `/explore`, `/deps`, `/graph`, `/graph/expand`,
  `/graph/repos` — all single-repo, all untouched (D5, D8).
- `RepoScope` parsing authority; the `400` error shape and its byte-identical text.
- `max_results` clamp bounds (`1..=100`), the default of `5`, and the per-repo route's
  unclamped behavior.
- `KNOWN_ROUTES` — no new routes, so the metrics allowlist and its drift guard are unaffected.
- Repo Deps mode, its data source and its colors.

---

## 5. BDD

### Group G — backend, `tests/run_cross_repo_search_e2e.sh` (extends the existing suite)

Setup: index fixture repo A, register and index a third throwaway fixture `ghost-repo`,
then `DELETE /api/repos/ghost-repo`. **Phase 0 must first verify that deletion leaves the
Neo4j/Qdrant rows behind** — the live audit says it does. If deletion turns out to clean the
databases, seed the ghost rows directly instead and record the finding here.

```gherkin
Scenario G1: repo=all is confined to registered repositories
  Given "ghost-repo" has index rows but is absent from the registry
  When  GET /api/search?q=GhostEntity&max_results=100
  Then  status is 200
  And   no entity has repo_name == "ghost-repo"

Scenario G2: omitting repo behaves identically to repo=all
  When  GET /api/search?q=GhostEntity&max_results=100
  And   GET /api/search?q=GhostEntity&repo=all&max_results=100
  Then  both repo_name sets are equal
  And   neither contains "ghost-repo"

Scenario G3: callers under repo=all is confined too
  When  GET /api/callers?entity=GhostUtil.work
  Then  no row in any bucket has repo_name == "ghost-repo"
  And   no row has target_repo_name == "ghost-repo"
  And   no resolution.targets[] entry has repo_name == "ghost-repo"

Scenario G4: registered repositories are still fully reachable under all
  When  GET /api/search?q=SharedUtil&repo=all
  Then  entities include repo_name == REPO_A_ID and repo_name == REPO_B_ID

Scenario G5 (regression): named scopes and per-repo routes are unchanged
  When  GET /api/search?q=SharedUtil&repo=<REPO_A_ID>
  Then  every entity has repo_name == REPO_A_ID
  When  GET /api/search?q=SharedUtil&repo=ghost-repo
  Then  status is 400 and the message contains "ghost-repo"
  When  GET /api/repos/<REPO_A_ID>/search?q=SharedUtil
  Then  results are confined to REPO_A_ID

Scenario G6: empty-body drift guard for /api/callers
  When  GET /api/callers?entity=NoSuchEntityXyz123
  Then  the top-level key set is exactly
        {calls, extends, implements, overridden_by, overrides, references, resolution}
  And   resolution's key set is exactly {fuzzy, query, targets, tier, truncated}
  # Pins the hand-built NoRepositories body of §3 against upstream shape drift (D4).
```

**Red criterion:** G1–G3 fail before implementation (ghost rows present); G4–G6 pass from
the start and are pure regression guards.

### Group V — viewer, browser scenarios

Executed against a live stack via CDP; each is also a manual checklist item. No unit-test
harness exists for the embedded HTML asset, so these are the viewer's test contract.

```gherkin
V1  unchecked ⇒ requests /api/repos/{id}/search, behavior byte-identical to 0.4.0
V2  checked   ⇒ requests /api/search with q and max_results=40 and NO repo parameter
V3  every result row in global mode shows a repository badge
V4  no result is labeled with an unregistered repository
V5  a null / non-array response renders "no results" instead of throwing
V6  the results dropdown stays inside the viewport at 1280px and 1600px
V7  clicking a same-repo result focuses it exactly as in 0.4.0
V8  clicking a cross-repo result switches the dropdown, reloads, then focuses the entity
V9  a result whose repo has no dropdown option reports it and aborts cleanly
V10 the details panel title is fully visible with the toolbar wrapped to 4 rows
V11 the checkbox survives a repository switch
```

---

## 6. Implementation phases (TDD)

`cargo fmt` + `cargo clippy --all-targets --all-features -- -D warnings` + `cargo test`
green at every phase boundary.

| Phase | Content | Exit criterion |
|---|---|---|
| **0 — BDD red** | Verify the ghost-row precondition; add Group G to `run_cross_repo_search_e2e.sh` | G1–G3 red, G4–G6 green; red count recorded in the commit message |
| **1 — Pure core** | §7.1 unit tests (red) → `ResolvedScope` + new `resolve_scope` | §7.1 green; no axum, no registry types |
| **2 — Handlers** | §7.2 unit tests (red) → `scope_or_error` forwarding, the two empty-body helpers, `scope_fields` `"none"` arm | §7.2 green; **Group G green** |
| **3 — Viewer: scope + search** | Checkbox, routing, badge, grouping, `escapeHtml`, `null` guard, `right: 0` | V1–V6 |
| **4 — Viewer: navigation** | Cross-repo click ⇒ repo switch ⇒ focus | V7–V9, V11 |
| **5 — `#node-details` fix** | Toolbar-derived top/height | V10 |
| **6 — Docs** | §8 in full | Every surface updated and regenerated |
| **7 — Gates** | `fmt`, `clippy -D warnings`, `test`, `./tests/run_all_e2e.sh` | **7/7** suites green |

Phase 2 is sequenced before any viewer work on purpose: D6 lets the viewer omit `repo`
entirely, which is only correct once Phase 2 has landed. Building the viewer first would
mean writing a client-side id list and then deleting it.

`assets/graph-viewer.html` is embedded via `include_str!` — **phases 3–5 require a rebuild**
to be observable.

---

## 7. Test matrix (unit)

### 7.1 `src/handlers/scope.rs`

Existing tests are updated for the new return type; the sentinel-precedence and
unknown-name tests keep their assertions.

| Test | Asserts |
|---|---|
| `all_expands_to_registered_repos` | `None` with `[b, a]` ⇒ `Scope(Many([a, b]))` — sorted (D2) |
| `sentinel_expands_to_registered_repos` | `"all"`, `"ALL"`, `"*"` ⇒ same as above |
| `all_with_single_registered_repo_is_one` | `None` with `[a]` ⇒ `Scope(One(a))` |
| `all_with_empty_registry_is_no_repositories` | `None` with `[]` ⇒ `NoRepositories` — **never** `Many([])` (D3) |
| `sentinel_with_empty_registry_is_no_repositories` | `"all"` / `"*"` with `[]` ⇒ `NoRepositories` |
| `expansion_never_yields_an_empty_filter_list` | for every `known`, the resulting `RepoScope` (if any) has a non-empty `filter_names()` — the invariant that makes D3 necessary |
| `sentinel_no_longer_skips_membership_check` | `"all,ghost"` ⇒ `Scope(...)` over the registry, and `ghost` is **not** in it |
| `named_scopes_are_unchanged` | `One` / `Many` / unknown-name `Err` behavior identical to 0.4.0 |
| `empty_registry_still_rejects_named_unknowns` | `"ghost"` with `[]` ⇒ `Err(["ghost"])`, not `NoRepositories` |
| `clamp_*` | unchanged |

### 7.2 `src/handlers/tests_common.rs`

Uses the existing embedder-less state (unreachable DBs ⇒ deterministic 500s), so a
`200` proves the handler returned **before** touching knot.

| Test | Asserts |
|---|---|
| `search_all_with_empty_registry_returns_empty_array` | `200`, body `[]` — not a 500, proving knot was never called |
| `callers_all_with_empty_registry_returns_empty_buckets` | `200`, six empty buckets + `resolution` present |
| `empty_registry_response_preserves_validation_order` | missing `q` / `entity` ⇒ `400` even with an empty registry |
| `unknown_repo_still_400_on_empty_registry` | `repo=ghost` ⇒ `400`, byte-identical across both routes |
| `populated_registry_reaches_the_db_layer` | valid scope ⇒ the deterministic `500` branch (regression guard for D1 not short-circuiting too eagerly) |

---

## 8. Documentation surface (all of it — none optional)

1. **`README.md`** — the `repo` scope table (line ~156): `*(omitted)*` and `all` become
   "All **registered** repositories". Add the empty-registry row. Update the caveat bullets:
   the "unknown ids rejected with 400" note gains its counterpart — `all` no longer reaches
   unregistered repositories. In the `🧬 Graph Visualization` feature list, document the
   "All repos" checkbox and the cross-repo result navigation.
2. **`CHANGELOG.md`** — new `## [0.4.1]` section (§10) and the compare link.
3. **Agent skills — respect the generation pipeline.** Source of truth is `skills/*.md`;
   `.knot-server-agent-skills/` and `.opencode/skills/` are generated **and committed**:
   1. `skills/search.md` and `skills/callers.md` — the scope table's `all` semantics.
   2. `skills/workflows.md` — the cross-repo discovery workflow inherits the narrower scope.
   3. `skills/graph.md` — mention the viewer's global search.
   4. Regenerate: `python3 scripts/generate_skills_script.py`.
   5. Refresh committed outputs via `./.knot-server-agent-skills.sh --no-register`, commit
      the diff in **both** generated trees.
4. **`knot-server.postman_collection.json`** — update the `repo` parameter descriptions on
   the Global Search / Global Callers requests.
5. **OpenAPI** — descriptions live in the `#[utoipa::path]` attributes on
   `search_all_handler` / `callers_all_handler`; update the `repo` wording there and confirm
   both render at `/docs`.

---

## 9. Risks & mitigations

| Risk | Severity | Mitigation |
|---|---|---|
| **`Many([])` silently means "everything"** | **High** | D3's `NoRepositories` variant; pinned by `expansion_never_yields_an_empty_filter_list` and `all_with_empty_registry_is_no_repositories` |
| Hand-built empty `/api/callers` body drifts from knot's shape | Medium | D4's E2E key-set guard (G6) against a live natural empty response |
| `repo=all` changes meaning for existing agents | Medium | Deliberate (D1). Filed under **Fixed** in the changelog: the old behavior exposed deleted repositories. Called out in README + skills |
| Crowding makes global search look broken | Medium | Measured and inherent (§2): `max_results` is a global cap. Viewer raises it to 40 and groups by repo; documented, not "fixed" |
| Global search latency (~0.55–1.05s vs 0.17s) | Low | Opt-in checkbox; the default path is untouched |
| Toolbar has 16px slack | Medium | D7 checkbox inside the search wrapper; D10 makes the details panel wrap-proof |
| Unescaped `innerHTML` now carries repo ids | Medium | `escapeHtml` helper applied to every interpolated field (§4.6) |
| Cross-repo focus races the repo-switch reload | Medium | Focus deferred until the `change` path settles (§4.7); V8 pins it |
| Ghost-row precondition unavailable in CI | Low | Phase 0 verifies it first and falls back to direct seeding (§5) |
| Viewer changes invisible without a rebuild | Low | `include_str!` noted in §6 |

---

## 10. Release

**0.4.1** (patch: behavior fix + additive viewer feature; no API signature change).

```markdown
## [0.4.1] - YYYY-MM-DD

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
  switches it from `GET /api/repos/{id}/search` to the cross-repo `GET /api/search`. Results
  are grouped and badged by repository, and selecting a result from another repository
  switches the active repository before focusing the entity. The 3D graph itself remains
  single-repo: knot-server's subgraph queries match `repo_name` on both ends of every edge,
  so cross-repo edges are not representable today.
```

---

## 11. Out of scope for this batch (recorded so it is not re-discovered)

- **Cross-repo edges in the 3D graph.** Requires relaxing the `{repo_name: $repo_name}`
  pinning on both edge endpoints in `src/handlers/graph_queries.rs:126-144`, plus a scoped
  overview query and node-level repository attribution in the viewer. A batch of its own.
- **A `/api/callers` panel in the viewer** (D9). Blocked on caller rows carrying no `uuid`;
  it would be a text panel, not a graph feature.
- **Cross-repo `/api/explore`** — still blocked upstream: `get_file_entities_query` projects
  no `repo_name`, so a multi-repo scope silently merges N repositories into one flat list.
- **Purging orphaned index rows.** This batch stops `repo=all` from *reading* them; it does
  not delete them. A `DELETE /api/repos/{id}` database-cleanup audit is a separate concern.
- **Per-repo result quotas for global search.** Would require N queries or an upstream
  per-repo cap in knot; the global cap is documented instead.
- **Persisting the "All repos" checkbox** across reloads (`localStorage`, alongside
  `knotGraphDepth`). Deferred until the feature has been used.
