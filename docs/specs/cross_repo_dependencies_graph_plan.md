# Cross-Repository Dependencies in the Graph Viewer — Implementation Plan

**Status:** Planned — not yet implemented
**Issue:** [#8 — Show cross dependencies](https://github.com/raultov/knot-server/issues/8)
**Target version:** knot-server `0.3.4` (no change required in `knot`; stays on `1.6.2`)
**Methodology:** TDD/BDD — every behaviour starts as a failing test

---

## 1. Problem Statement

> *"Show in /graph what other indexed repos the current one depends on (dependant libraries)."*

The `/graph` viewer renders **entity-level** graphs only (classes, interfaces,
functions and their `CALLS`/`EXTENDS`/`IMPLEMENTS` edges). It has no concept of a
repository as a graph node. A user looking at `chrome-control-mcp` cannot see
that it consumes `cdp-browser-lite` and `cdp-lite`, even though the server
already knows this.

---

## 2. Current State

### 2.1 The data already exists — no `knot` change is needed

During indexing, `link_cross_repo_dependencies`
(`knot-1.6.2/src/pipeline/ingest/resolve/cross_repo.rs:8`) upserts a
`:Repository` node for the repo being indexed and then, for every
`BuildDependency` entity, resolves it against already-known repositories via
`find_repository_by_artifact` and creates a `DEPENDS_ON` edge.

The edge is created by `upsert_repo_dependency`
(`knot-1.6.2/src/db/graph/upsert.rs:379`):

```cypher
MATCH (from:Repository {name: $from_repo})
MATCH (to:Repository {name: $to_repo})
MERGE (from)-[:DEPENDS_ON]->(to)
```

Because **both** sides must already be `MATCH`-able, an edge can only ever exist
between two locally indexed repositories. This is exactly the semantics the
issue asks for: *"what other **indexed** repos"*. Nothing has to change in the
`knot` crate.

Verified against the live development Neo4j (8 `:Repository` nodes):

```
cdp-browser-lite   -> cdp-lite
chrome-control-mcp -> cdp-browser-lite
chrome-control-mcp -> cdp-lite
job-watch          -> cdp-browser-lite
```

Note the transitive chain `chrome-control-mcp -> cdp-browser-lite -> cdp-lite`.
Any design that renders a flat star from the root would misrepresent it.

`:Repository` nodes carry `name`, `build_system`, `group_id`, `artifact_id`,
`version` and `indexed_at` (`upsert.rs:360-366`). Crucially, `Repository.name`
is `cfg.repo_name`, which downstream equals the **knot-server registry id** —
so repo names cross-reference directly against `state.registry`.

### 2.2 The existing `/deps` endpoint cannot drive a graph

`deps_handler` (`src/handlers/search.rs:204`) delegates to
`knot::cli_tools::run_deps`, which returns a flat list of names:

```json
[{"repo_name": "cdp-browser-lite"}, {"repo_name": "cdp-lite"}]
```

`find_repo_dependencies` (`knot-1.6.2/src/db/graph/query_repo.rs:38`) does
`MATCH (from)-[:DEPENDS_ON*1..N]->(to) RETURN DISTINCT to.name` — the traversal
is transitive but the **path is discarded**. There is no way to know that
`cdp-lite` is reached *through* `cdp-browser-lite`. Rendering this as a graph
would produce three edges radiating from the root, which is wrong.

`/deps` is also consumed by the published agent skills, so its response shape is
a compatibility surface.

### 2.3 The viewer has no repository concept

`assets/graph-viewer.html` (1107 lines, single file, 3d-force-graph from CDN at
line 308). `repo` appears only as `state.selectedRepo`, the `#repo-select`
dropdown, and a path segment in API URLs. Relevant facts:

| Fact | Location |
|---|---|
| `state` object | 312-330 |
| `KIND_COLORS` map — the only colour source; no CSS variables | 338-358 |
| `apiGet` — the single `fetch` call site | 365-372 |
| `mergeSubgraph` — **whitelists** node fields; unknown fields are dropped | 500-545 |
| `initGraph` — `nodeColor`/`nodeVal`/`linkColor` callbacks | 555-588 |
| `showNodeDetails` / `hideNodeDetails` | 619-664 |
| `refreshGraphWithFilters` — shared re-fetch entry point | 670-688 |
| `loadOverview` | 950-982 |
| `clearFocusedEntity` | 1063-1104 |

Two pre-existing quirks that this work must account for:

- **`.hidden` is not styled.** The only rule is `#node-details.hidden`
  (line 104). Every `classList.add('hidden')` on `#clear-btn`, `#explore-btn`
  and `#back-btn` is a **visual no-op** today. A new button cannot rely on it.
- **`fetchExpandNode` (479-493) is dead code** — `/graph/expand` is never called
  from the viewer. Not our problem, but do not mistake it for a live path.

---

## 3. Design

### 3.1 Decisions taken (confirmed with the maintainer)

| Question | Decision | Rationale |
|---|---|---|
| Viewer UX | **Dedicated "Repo Deps" mode toggle** | Keeps repo-level and entity-level granularity separated. Overlaying 3 repo nodes onto 3 000 entity nodes is unreadable. |
| Direction | **`both` by default**, with a selector | Same query cost, and *"who breaks if I change this library"* is the natural companion question. Arrows disambiguate. |
| Backend | **New endpoint**, not an extension of `/deps` | Needs edges + build metadata; `/deps` is a stable agent-facing contract. |

### 3.2 New endpoint

```
GET /api/repos/{id}/graph/repos?depth=<1..5>&direction=<outgoing|incoming|both>
```

- Tag: `Graph`
- `depth` default `3`, clamped to `1..=5` (matches `graph_handler`'s
  `.clamp(1, 5)` at `src/handlers/graph.rs:55`)
- `direction` default `both`

Response — `nodes` + real `edges`, so transitive chains keep their true shape:

```json
{
  "root_id": "chrome-control-mcp",
  "nodes": [
    { "id": "chrome-control-mcp", "name": "chrome-control-mcp",
      "build_system": "cargo", "group_id": "", "artifact_id": "chrome-debug-mcp",
      "version": "1.3.2", "is_root": true, "registered": true, "relation": "root" },
    { "id": "cdp-browser-lite", "name": "cdp-browser-lite", "build_system": "cargo",
      "group_id": "", "artifact_id": "cdp-browser-lite", "version": "0.3.4",
      "is_root": false, "registered": true, "relation": "dependency" }
  ],
  "edges": [
    { "source": "chrome-control-mcp", "target": "cdp-browser-lite", "type": "DEPENDS_ON" },
    { "source": "cdp-browser-lite",  "target": "cdp-lite",          "type": "DEPENDS_ON" }
  ],
  "total_nodes_found": 3
}
```

`registered` is resolved against `state.registry` so the viewer only offers
"open this repository" for repos that actually appear in the dropdown. A repo
can have a `:Repository` node while having been deleted from the registry.

### 3.3 Why two Cypher queries

`knot::db::graph::GraphDb.graph` is `pub(crate)`
(`knot-1.6.2/src/db/graph/mod.rs:22`), so knot-server cannot reach the inner
`neo4rs::Graph`. The established workaround is `fetch_all_entities`
(`src/handlers/graph_queries.rs:158`), which opens its own connection from
`state.neo4j_uri` / `state.neo4j_user` / `state.neo4j_password`. The new module
follows the same pattern.

**Query 1 — nodes.** Assembled per direction:

```cypher
-- outgoing (what this repo depends on)
MATCH (root:Repository {name: $repo_name})-[:DEPENDS_ON*1..D]->(d:Repository)
RETURN DISTINCT d.name AS name, d.build_system AS build_system,
       d.group_id AS group_id, d.artifact_id AS artifact_id, d.version AS version

-- incoming (what depends on this repo)
MATCH (d:Repository)-[:DEPENDS_ON*1..D]->(root:Repository {name: $repo_name})
RETURN DISTINCT ...
```

plus a root-properties lookup `MATCH (r:Repository {name: $repo_name})`.

`D` is interpolated into the query string because Cypher does **not** accept a
bound parameter inside a variable-length pattern. This is safe: `D` is a clamped
`u32` and can never carry user text. `$repo_name` stays a bound parameter — it
is the only user-controlled value and must never be interpolated.

**Query 2 — edges among the discovered set:**

```cypher
MATCH (a:Repository)-[:DEPENDS_ON]->(b:Repository)
WHERE a.name IN $names AND b.name IN $names
RETURN DISTINCT a.name AS source, b.name AS target
```

This is what preserves `chrome-control-mcp -> cdp-browser-lite -> cdp-lite`
instead of flattening it. It is skipped entirely when the node set is empty.

### 3.4 Empty results are a 200, not an error

A registered repo may legitimately have **no** `:Repository` node — it has not
finished indexing yet, or it has no build manifest (`Cargo.toml`, `pom.xml`,
`package.json`…), or it simply has no indexed neighbours. All of these return
`200` with `nodes: []` and `root_id: null`. The viewer renders an explanatory
empty state rather than an error. Only an unknown registry id is a `404`, and
only an unparseable `direction` is a `400`.

### 3.5 Viewer mode

A new toolbar `.filter-group` after `#kind-toggles` (line 280) holding
`#repo-deps-toggle` and `#repo-deps-direction`, both disabled until a repo is
selected. Entering the mode saves the entity view, calls `resetGraph()` and
`loadRepoGraph()`; leaving it restores the entity overview through the existing
`loadOverview()` path.

Repo nodes need their own mapper: `mergeSubgraph`'s field whitelist
(lines 504-514) silently drops anything it does not know, so `build_system` and
friends would vanish if it were reused.

---

## 4. Blast Radius

| File | Change |
|---|---|
| `src/handlers/repo_graph.rs` | **new** — handler + Cypher + mapping |
| `src/handlers/models.rs` | `RepoGraphParams`, `RepoGraphNode`, `RepoGraphResponse`, `RepoRelation` |
| `src/handlers/mod.rs` | `pub mod repo_graph;` + re-export |
| `src/main.rs` | `.routes(routes!(handlers::repo_graph_handler))` next to line 169 |
| `src/handlers/tests_common.rs` | register the route in `build_test_app` (line 22) |
| `assets/graph-viewer.html` | toolbar controls, mode state, mapper, colours, details panel, `.hidden` rule |
| `tests/run_e2e.sh` | new cross-repo fixture + assertions |
| `README.md`, `CHANGELOG.md`, `skills/graph.md` | documentation |
| `.knot-server-agent-skills.sh`, `.knot-server-agent-skills/graph.md` | regenerated (tracked files) |

**Nothing existing changes behaviour.** `/deps`, `/graph` and `/graph/expand`
are untouched, so the agent skills keep working. The only edit to shared code is
adding a `.hidden { display: none; }` CSS rule, which *fixes* three buttons that
are silently broken today (see §6, Step 7).

---

## 5. Naming and Route Collision

`/api/repos/{id}/graph/repos` sits under the existing `/graph` prefix alongside
`/graph/expand`. axum matches literal segments before wildcards, and `graph`
already has a `{id}` capture only at the repo level, so there is no ambiguity
with `graph_handler` (`/api/repos/{id}/graph`) — that route has no trailing
segment.

---

## 6. TDD/BDD Plan

Every step is **Red → Green → Refactor**. No production line is written before a
failing test justifies it. Unit test names follow Given/When/Then.

The Cypher-facing code is split so that **everything except the two `execute`
calls is a pure function**. That is what makes Steps 1-4 testable with no live
Neo4j, in line with the AGENTS.md preference for unit over integration tests.

---

### Step 1 — Direction parsing

**File:** `src/handlers/repo_graph.rs` (`mod tests`)

#### Red

```
given_no_direction_when_parsed_then_defaults_to_both
given_outgoing_when_parsed_then_direction_is_outgoing
given_incoming_when_parsed_then_direction_is_incoming
given_both_when_parsed_then_direction_is_both
given_mixed_case_outgoing_when_parsed_then_direction_is_outgoing
given_an_unknown_direction_when_parsed_then_error_lists_the_valid_values
```

Fails to compile — `RepoDirection` and `parse_repo_direction` do not exist.
Compile failure is a valid Red.

> **Deliberate divergence from the existing code.** `parse_direction`
> (`src/handlers/graph_parse.rs:50`) silently falls back to `Both` for garbage
> input. The new endpoint **rejects** an unknown direction with a `400` that
> lists the valid values, matching how `parse_relationships` and `parse_kinds`
> behave. Silent fallback hides typos; the last test above pins that contract.

#### Green

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepoDirection { Outgoing, Incoming, Both }
```

plus `parse_repo_direction(&str) -> Result<RepoDirection, String>`.

---

### Step 2 — Depth clamping

**File:** `src/handlers/repo_graph.rs`

#### Red

```
given_no_depth_when_clamped_then_it_is_three
given_a_depth_of_zero_when_clamped_then_it_is_one
given_a_depth_of_nine_hundred_when_clamped_then_it_is_five
given_a_depth_of_two_when_clamped_then_it_is_unchanged
```

#### Green

`fn clamp_repo_depth(depth: Option<u32>) -> u32 { depth.unwrap_or(3).clamp(1, 5) }`

---

### Step 3 — Cypher assembly

**File:** `src/handlers/repo_graph.rs`

This is the security-relevant step. The tests pin both the traversal shape and
the injection boundary.

#### Red

```
given_outgoing_when_building_the_node_query_then_it_traverses_depends_on_forwards
    → contains "(root:Repository {name: $repo_name})-[:DEPENDS_ON*1..3]->"

given_incoming_when_building_the_node_query_then_it_traverses_depends_on_backwards
    → contains "-[:DEPENDS_ON*1..3]->(root:Repository {name: $repo_name})"

given_both_when_building_the_node_query_then_it_contains_both_traversals

given_a_clamped_depth_when_building_the_node_query_then_the_depth_is_interpolated
    → depth 5 ⇒ "*1..5"

given_any_direction_when_building_the_node_query_then_repo_name_is_a_bound_parameter
    → the query string contains "$repo_name" and never the literal repo name
      (feed it a repo id like `x") MATCH (n) DETACH DELETE n //`)

given_a_node_set_when_building_the_edge_query_then_it_filters_both_endpoints
    → contains "a.name IN $names AND b.name IN $names"
```

#### Green

`fn build_repo_node_query(direction: RepoDirection, depth: u32) -> String` and
`fn build_repo_edge_query() -> &'static str`.

#### Refactor

If the direction cascade pushes `too_many_lines` (threshold 80, `clippy.toml:45`),
extract the two traversal fragments as `const &str` templates. Do **not** reach
for `#[expect]`.

---

### Step 4 — Response mapping

**File:** `src/handlers/repo_graph.rs`

The mapper takes already-fetched raw rows, so it needs no database.

#### Red

```
given_a_root_and_its_dependencies_when_mapped_then_the_root_is_flagged_and_relation_is_root
given_a_dependency_row_when_mapped_then_its_relation_is_dependency
given_a_dependent_row_when_mapped_then_its_relation_is_dependent
given_a_repo_reachable_both_ways_when_mapped_then_it_is_classified_as_a_dependency
    → a dependency cycle must not produce a duplicate node
given_a_repo_present_in_the_registry_when_mapped_then_registered_is_true
given_a_repo_absent_from_the_registry_when_mapped_then_registered_is_false
given_duplicate_rows_when_mapped_then_nodes_are_deduplicated_by_name
given_an_empty_result_when_mapped_then_nodes_is_empty_and_root_id_is_none
given_mapped_nodes_when_counted_then_total_nodes_found_matches_the_node_count
```

#### Green

`fn map_repo_graph(root, deps, dependents, registered_ids) -> RepoGraphResponse`,
deduplicating by `name` into an insertion-ordered map with `root` first.

---

### Step 5 — Response types serialise as documented

**File:** `src/handlers/models.rs` (`mod tests`, extends the block at line 229)

#### Red

```
given_a_repo_graph_node_when_serialised_then_the_field_names_match_the_documented_contract
given_a_repo_relation_when_serialised_then_it_is_lowercase
    → RepoRelation::Dependency ⇒ "dependency"
given_a_depends_on_edge_when_serialised_then_the_type_field_is_named_type
    → reuses GraphEdgeResponse (models.rs:102) whose #[serde(rename = "type")]
      must survive
```

#### Green

Add the structs with `Serialize` + `ToSchema`, `RepoRelation` with
`#[serde(rename_all = "lowercase")]` (mirrors `RepoStatus`, `src/models.rs:21`).

---

### Step 6 — Handler wiring

**File:** `src/handlers/tests_common.rs`

#### Red

```
given_an_unknown_repo_id_when_requesting_the_repo_graph_then_the_response_is_404
given_an_invalid_direction_when_requesting_the_repo_graph_then_the_response_is_400
```

Both fail with `404` from axum itself until the route is registered — an honest
Red, but confirm the *reason* for the failure before writing the fix, otherwise
the second test would pass for the wrong reason.

> These handler tests deliberately stop at the argument-validation boundary.
> `create_test_state_with_tempdir` (`tests_common.rs:56`) points Neo4j at
> `bolt://localhost:9999`, so any test reaching a query would fail on connection,
> not on logic. Query behaviour is covered by Step 3 (unit) and Step 8 (E2E).

#### Green

- `src/handlers/repo_graph.rs`: `repo_graph_handler` with the `#[utoipa::path]`
  annotation and a `#[tracing::instrument(name = "repo_graph", skip_all,
  fields(repo_id = %id, depth = …, direction = …))]` attribute, matching
  `graph_handler` (`src/handlers/graph.rs:30`).
- Reuse the `check_repo_exists` pattern (`src/handlers/graph.rs:260`).
  If it is lifted into `graph_utils.rs` for sharing, that is a pure move — do it
  as a separate refactor commit so the diff stays readable.
- `src/handlers/mod.rs`, `src/main.rs`, and `build_test_app`.

---

### Step 7 — Viewer: `.hidden` regression guard first

**File:** `assets/graph-viewer.html`, `tests/run_e2e.sh`

The new "open this repository" button depends on `.hidden` actually hiding
things, which it does not today.

#### Red

Extend the existing **Test G7** (`tests/run_e2e.sh:434`), which already greps the
served HTML:

```
HAS_HIDDEN_RULE=$(grep -cE '\.hidden[[:space:]]*\{[^}]*display:[[:space:]]*none' /tmp/g7.html)
HAS_REPO_DEPS=$(grep -c 'repo-deps-toggle' /tmp/g7.html)
```

Both are `0` before the change.

#### Green

Add `.hidden { display: none; }` to the stylesheet and the toolbar markup.

#### Refactor

`#node-details.hidden` (line 104) becomes redundant once the generic rule
exists — remove it. Confirm the details panel still hides on background click
before deleting.

---

### Step 8 — Viewer: repo-deps mode

**File:** `assets/graph-viewer.html`

Driven by manual acceptance (§7) rather than unit tests — the file has no JS test
harness, and introducing one is out of scope for this issue.

Changes, in order:

1. **State** — add `repoDepsMode: false` and `savedDepth: null` to `state`
   (line 312).
2. **Colours** — add to `KIND_COLORS` (line 338): `repository_root` (white
   `#ffffff`), `repository_dependency` (teal `#2EC4B6`),
   `repository_dependent` (amber `#F39C12`).
3. **API client** — `fetchRepoGraph(repoId, depth, direction)` next to
   `fetchSubgraph` (line 463), routed through `apiGet`.
4. **Mapper** — `mergeRepoGraph(graph)` beside `mergeSubgraph` (line 500).
   A separate function, because `mergeSubgraph`'s whitelist would drop
   `build_system`, `version` and `relation`.
5. **Link colour** — `.linkColor` (line 576) gains a `DEPENDS_ON` branch, amber,
   distinct from the existing blue (normal) and green (highlighted).
6. **Node size** — `.nodeVal` (line 571) returns `6` for the root repo and `4`
   for the rest, so the root reads clearly at a glance.
7. **Details panel** — `showRepoDetails(node)` renders build system, artifact
   coordinates and version instead of file/line; hides `#explore-btn` (focusing
   an entity is meaningless for a repo); shows `#open-repo-btn` **only when
   `node.registered`**, which sets `#repo-select.value` and dispatches a
   `change` event, reusing the existing repo-switch handler (line 845).
8. **Mode toggle** — entering saves the depth, disables the rel/kind filter
   groups (they are entity-level and meaningless here), calls `resetGraph()` +
   `loadRepoGraph()`. Leaving restores everything and calls `loadOverview()`.
9. **Repo switch** — the `change` handler (line 845) must exit repo-deps mode,
   or the viewer would show the old repo's dependency graph under a new label.
10. **Empty state** — `setStatus('No cross-repo dependencies found for ' + id +
    ' — only indexed repos with a build manifest are linked')`.

---

### Step 9 — E2E: a real cross-repo link

**File:** `tests/run_e2e.sh`

#### Red

New fixtures under `tests/fixtures/crossrepo/`:

```
lib/Cargo.toml   →  [package] name = "e2e-cross-lib"  version = "0.1.0"
lib/src/lib.rs   →  a trivial parseable function
app/Cargo.toml   →  [package] name = "e2e-cross-app"
                    [dependencies] e2e-cross-lib = "0.1.0"
app/src/main.rs  →  a trivial parseable function
```

Two bare repos built the same way as `create_fixture_repo`
(`tests/run_e2e.sh:121`).

> **Ordering is load-bearing.** `match_dependency_to_repository`
> (`cross_repo.rs:115`) resolves a build dependency against `:Repository` nodes
> that **already exist**. The library must be registered and reach `indexed`
> **before** the app is registered. Index them in the other order and the edge is
> silently never created — the test would fail for a reason that has nothing to
> do with this feature. Add a comment saying so; the next person will otherwise
> lose an hour to it.

Assertions, following the `Test G*` style of the graph block:

```
Test G13: repo-deps graph returns the dependency and a DEPENDS_ON edge
  → GET /api/repos/${APP_ID}/graph/repos?direction=outgoing
  → .root_id == APP_ID
  → .nodes[] contains LIB_ID with relation == "dependency"
  → .edges[] contains {source: APP_ID, target: LIB_ID, type: "DEPENDS_ON"}

Test G14: reverse direction reports the dependent
  → GET /api/repos/${LIB_ID}/graph/repos?direction=incoming
  → .nodes[] contains APP_ID with relation == "dependent"

Test G15: repo-deps graph rejects an invalid direction
  → ...?direction=sideways  ⇒ 400

Test G16: repo-deps graph 404s for an unknown repo
  → /api/repos/does-not-exist/graph/repos  ⇒ 404

Test G17: a repo with no build manifest returns 200 and an empty node set
  → reuse ${REPO_ID} (the Java fixture has no pom.xml)  ⇒ 200, nodes == []
```

G17 is the one that pins §3.4 — the "no data" path must not be an error.

#### Green

Implement the fixtures and register both repos. Delete both at the end of the
block, mirroring the cleanup at `run_e2e.sh:1355`.

Update the closing summary line (`run_e2e.sh:1373`) to mention cross-repo graph
coverage.

---

### Step 10 — Documentation and release metadata

- **`README.md`** — extend *"🧬 Graph Visualization (Web UI)"* (line 97) with the
  repo-deps mode and the new endpoint. Mention the constraint that only repos
  with a recognised build manifest, indexed in dependency order, are linked;
  this is the single most likely support question.
- **`skills/graph.md`** — document `GET /api/repos/{id}/graph/repos` for agents,
  then regenerate the tracked installer:
  ```bash
  python3 scripts/generate_skills_script.py
  ```
  which rewrites `.knot-server-agent-skills.sh` and
  `.knot-server-agent-skills/graph.md`. Both are tracked (`.opencode/` is
  gitignored, so its skill copies need no commit).
- **`CHANGELOG.md`** — new section at the top matching the existing prefix style
  (`Feat(graph)`, `Feat(viewer)`, `Test(e2e)`, `Docs`).
- **`Cargo.toml`** — bump to `0.3.4`. **Do not publish and do not push to
  `master`** without explicit maintainer approval (AGENTS.md).

---

### Step 11 — Verification gates

Run via the `validator` subagent, in this order:

1. `cargo fmt`
2. `cargo clippy --all-targets --all-features -- -D warnings`
3. `cargo test`
4. `./tests/run_all_e2e.sh`

**Policy reminder:** any `fmt`/`clippy` finding is resolved by refactoring.
`#[allow(...)]` is prohibited; `#[expect(..., reason = "...")]` only as a genuine
last resort and must be flagged to the maintainer. This work is not expected to
need either.

---

## 7. Manual Acceptance

Against the running development stack on `localhost:3000`, which already holds
the dependency chain from §2.1:

1. Open `/graph`, select **`chrome-control-mcp`**, enable **Repo Deps**.
2. Expect **3 nodes** and **3 edges** — including
   `cdp-browser-lite -> cdp-lite`. A star of edges all radiating from the root
   means the edge query (§3.3) regressed to `/deps` semantics.
3. Switch direction to **incoming** on `cdp-lite`: expect `cdp-browser-lite` and
   `chrome-control-mcp` as dependents.
4. Click a dependency node: the panel shows `cargo`, artifact and version, the
   *Focus on Entity* button is hidden, *Open this repository* is present.
5. Click *Open this repository*: the dropdown switches and the entity graph for
   that repo loads.
6. Select a repo with no `:Repository` node (e.g. a freshly registered one):
   expect the empty-state message, **not** an error toast.
7. Toggle Repo Deps off: the original entity overview returns at the original
   depth.

---

## 8. Acceptance Criteria

- [ ] `GET /api/repos/{id}/graph/repos` returns repository nodes **and**
      `DEPENDS_ON` edges.
- [ ] Transitive chains keep their intermediate edges
      (`chrome-control-mcp -> cdp-browser-lite -> cdp-lite`), not a flattened star.
- [ ] `direction` supports `outgoing`, `incoming`, `both`; default `both`;
      unknown values are a `400` that lists the valid ones.
- [ ] `depth` is clamped to `1..=5`, default `3`.
- [ ] A repo with no cross-repo data returns `200` with `nodes: []`, never a `5xx`.
- [ ] An unknown registry id returns `404`.
- [ ] `$repo_name` is a bound Cypher parameter in every query; only the clamped
      depth is interpolated, and a test proves it.
- [ ] `registered` is `false` for repos present in Neo4j but absent from the
      registry, and the viewer hides *Open this repository* for them.
- [ ] `/deps`, `/graph` and `/graph/expand` are byte-for-byte unchanged in
      behaviour.
- [ ] `.hidden { display: none; }` exists, fixing `#clear-btn`, `#explore-btn`
      and `#back-btn` as a side effect.
- [ ] `cargo clippy --all-targets --all-features -- -D warnings` is clean, with
      no new `#[allow]` or `#[expect]`.
- [ ] Full E2E suite passes, including the new `Test G13`-`G17`.
- [ ] `README.md`, `CHANGELOG.md`, `skills/graph.md` and the regenerated
      agent-skills installer are updated.

---

## 9. Rejected Alternatives

**Extend `/deps` with an opt-in graph response.** Fewer endpoints, but it
overloads a stable contract that the published agent skills already consume, and
the flat-list shape would have to be preserved alongside the new one anyway.

**Reuse `knot::cli_tools::run_deps`.** It discards path information
(`query_repo.rs:38` returns only `DISTINCT to.name`), so transitive
relationships would render as a star centred on the root. Fixing it upstream
would mean a `knot` release for a knot-server-only need.

**Overlay repo nodes onto the entity overview.** Mixes granularity: a repo with
3 000 indexed entities would show 3 repo nodes lost in the crowd, and the force
layout would treat them as peers of individual methods.

**A side-panel list instead of graph nodes.** Cheapest option, but the issue asks
to show it *in `/graph`*, and a list cannot convey a transitive chain.

**Reuse `GraphResponse` for repo nodes.** Would let `mergeSubgraph` work
unchanged by stuffing the repo name into `name`, `cargo:knot` into `fqn` and the
version into `signature`. Rejected: it makes the OpenAPI schema lie about what
the fields mean, and the viewer would still need repo-specific branches in the
details panel, so it saves no real work.

**Global repo graph when no repo is selected.** Rendering all `:Repository`
nodes and every `DEPENDS_ON` edge is a genuinely useful view, but it is a
different feature from the one filed. Worth a follow-up issue.

---

## 10. Known Adjacent Defect (out of scope)

`skills/deps.md` documents the query parameter as `depth=`, but `DepsParams`
declares **`max_depth`** (`src/handlers/models.rs:43`). Agents following that
skill send `depth=2`, which is ignored, and silently get the default of `3`.

A one-line documentation fix, unrelated to this feature. Either fold it into
this branch or file it separately — maintainer's call.
