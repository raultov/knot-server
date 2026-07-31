# Spec — Upgrade knot-server to knot 1.5.6

Status: **Planned** (no implementation)
Scope: `knot-server` (this repo). Upstream library `knot` 1.5.5 → 1.5.6.
Related: `knot` repo `docs/specs/groovy_property_accessors_and_parser_hardening.md` (the spec 1.5.6 implements).

---

## 0. Verdict up front

**knot-server requires exactly one mandatory change: `Cargo.toml:25`, `knot = "1.5.5"` → `knot = "1.5.6"`.**

**The `/graph` interface does NOT need to be added to or modified.** Section 3
proves this. Everything else in this document is optional, decision-gated
enhancement work (Section 5) plus release hygiene (Phase 4).

---

## 1. What changed in knot 1.5.6

Three commits, `e0d34a1..741d43c`:

```
741d43c release: bump version to 1.5.6
65b5361 test(e2e): verify property accessors and optimize script speed
117856f feat(groovy): support property accessor synthesis and harden parser
```

```
 CHANGELOG.md                             |   13 +
 Cargo.lock                               |    2 +-
 Cargo.toml                               |    2 +-
 README.md                                |    2 +-
 queries/groovy.scm                       |    9 +-
 src/pipeline/ingest/resolve/overrides.rs |  195 +++++
 src/pipeline/parser/languages/groovy.rs  | 1163 ++++++++++++++++++++++++++++--
 tests/run_groovy_e2e.sh                  |  143 +++-
 8 files changed, 1480 insertions(+), 49 deletions(-)
```

Changelog (verbatim):

> ## v1.5.6 — Groovy Property Accessors & Parser Hardening
>
> - **Fix(groovy)**: Javadoc block-comment continuation lines no longer produce phantom method entities or corrupt scope tracking. New `strip_comments_line` helper tracks multi-line `/* */` state across lines, and brace counting operates on the code-bearing remainder only.
> - **Feat(groovy)**: Bare property declarations (`Path baseDir`, `boolean cacheable`, `private final Path ROOT`) are now indexed as `GroovyProperty` entities. Previously only initialized properties (`String name = 'test'`) were detected.
> - **Feat(groovy)**: Compiler-generated property accessors (`getX`/`setX`/`isX`) are synthesised as first-class `GroovyMethod` entities, enabling `OVERRIDES` linking between Groovy properties and interface getter declarations. Explicit getters/setters suppress synthetic ones, and `final` properties emit getters only.
> - **Fix(groovy-scm)**: Fixed `queries/groovy.scm` to compile against tree-sitter-groovy v0.1.2 by replacing `variable_declaration` with `local_variable_declaration`. Added `function_definition` capture patterns for `def`-style methods.
> - **Test(unit)**: 30+ new unit tests …
> - **Test(e2e)**: Added Group G in `tests/run_groovy_e2e.sh` …
> - **Docs**: Updated README Groovy section with property accessor synthesis details.

Note: the `.scm` fix makes the query *compile* (asserted by a test), but
tree-sitter parsing for Groovy is **still not re-enabled** —
`groovy.rs:32` remains `let mut entities: Vec<ParsedEntity> = vec![];`.
Parsing stays lexical. This matters only because it means the release carries
no risk of a wholesale change in Groovy entity extraction strategy.

---

## 2. Public API surface delta: none

```
git -C ../knot diff e0d34a1..741d43c -- src/models/ src/db/graph/ src/cli_tools/ src/lib.rs
```

returns **empty**. Not one byte changed in the modules knot-server consumes.

| Item knot-server depends on | Location in knot | Changed in 1.5.6? |
| --- | --- | --- |
| `run_get_subgraph` | `src/cli_tools/subgraph.rs` | No — file absent from diff |
| `SubgraphOptions`, `SubgraphDirection` | `src/db/graph/query_subgraph.rs` | No |
| `SubgraphNode`, `SubgraphEdge`, `SubgraphResult` | `src/models/subgraph.rs` | No |
| `EntityKind` (76 variants) | `src/models/entity.rs` | No — zero variants added |
| `RelationshipType` (13 variants) | `src/models/relationship.rs` | No — zero variants added |
| `run_search_hybrid_context`, `run_find_callers`, `run_explore_file`, `run_deps` | `src/cli_tools/` | No |
| `GraphDb`, `VectorDb`, `Embedder`, `ProgressTracker`, `IndexState`, `IndexingProgress`, `IndexingStage`, `Config` | `src/db/`, `src/pipeline/`, `src/config.rs` | No |

The only `src/` files touched are `src/pipeline/parser/languages/groovy.rs` and
`src/pipeline/ingest/resolve/overrides.rs` — internal pipeline code reached by
knot-server only through `run_indexing_pipeline_with_progress`
(`src/worker.rs:454`), whose signature is unchanged.

**Consequence:** the upgrade cannot break compilation. `cargo build` is the
proof obligation, not a refactor.

---

## 3. `/graph` analysis — no interface change required

Four independent checks, each of which would have forced a change had it failed:

| Check | Constant / code | Result |
| --- | --- | --- |
| New relationship type to accept? | `src/handlers/models.rs:129` `VALID_RELATIONSHIPS` (13 entries) | Matches all 13 `RelationshipType` Display strings exactly. `OVERRIDES` was already added in knot-server 0.2.18 (`71bee63`, `acc1e24`). **No change.** |
| New entity kind to categorise? | `models.rs:149/168/176` `KIND_CATEGORY_{CLASSES,INTERFACES,FUNCTIONS}` | 1.5.6 added zero `EntityKind` variants. **No change.** |
| Are the new synthetic accessors visible? | They are emitted as `EntityKind::GroovyMethod` (`groovy.rs:463`) → serialises to `groovy_method`, already in `KIND_CATEGORY_FUNCTIONS` (`models.rs:199`). | **Already visible** under the `functions` kind category. |
| Are the new `OVERRIDES` edges renderable? | `graph_queries.rs` overview roll-up projects method-level edges onto `enclosing_class` nodes; focus mode passes `OVERRIDES` straight through; the viewer has an `Overrides` toggle (`assets/graph-viewer.html:256`) that Focus mode force-activates. | **Already renderable.** |

`GroovyProperty` (`groovy_property`) likewise already exists in
`KIND_CATEGORY_FUNCTIONS` (`models.rs:201`) since before this release.

**Verdict: `/graph` is forward-compatible with 1.5.6 as shipped in 0.2.18.**

---

## 4. Runtime behaviour knot-server *will* observe

These are data-shape changes visible after a Groovy repo is **re-indexed** with
1.5.6. None require code, but they should be understood before validation:

| Δ | Effect on knot-server |
| --- | --- |
| More `groovy_property` nodes (bare declarations now indexed) | Larger graphs and Qdrant collections for Groovy repos. Nodes appear only under the `functions` kind category, which is **off by default** in overview (`DEFAULT_VISIBLE_KINDS = "classes,interfaces"`). |
| New synthetic `groovy_method` nodes with `signature = "<synthetic Groovy property accessor>"` | Appear in `/graph` focus mode, `explore_file`, and hybrid search results. |
| Property and its synthetic accessor share the same `file_path` **and** `start_line` | Two distinct nodes point at the same source line. UUIDs differ (derived from `repo:file:fqn:start_line`, and the fqns differ), so there is no collision — but the viewer will show two nodes whose "Line:" field is identical. |
| More `OVERRIDES` edges | The reported nextflow case starts working: focusing `nextflow.ISession.getBaseDir` will surface `nextflow.Session.getBaseDir`. |
| Fewer phantom entities (Javadoc fix) | Entities such as `nextflow.ISession.name` and `nextflow.Session.name` disappear, along with the bogus `OVERRIDES` edge between them. Total entity count may go **down** for comment-heavy Groovy repos even as property count goes up. |

Two pre-existing limits worth re-reading in this light, both unchanged by the
upgrade but more likely to be hit as Groovy graphs grow:

- Focus/expand mode truncates at **500 nodes** (`max_nodes` default in knot's
  `subgraph.rs`; knot-server passes `None` at `graph.rs:84` and `:221`).
- Overview mode has **no node cap** and hardcodes `truncated: false`
  (`graph_queries.rs:184`).

---

## 5. Optional enhancements — decisions required

None of these are needed to adopt 1.5.6. Each is listed with a recommendation.
**Phases 2 and 3 below are executed only if the corresponding decision is "yes".**

### E1 — Mark synthetic accessors in the viewer

*Recommendation: yes.* Small, self-contained, and it prevents the "why are there
two nodes on line 173?" confusion described in Section 4.

knot emits the marker string `<synthetic Groovy property accessor>` in the
entity's `signature`, which knot-server already surfaces end to end
(`graph_map.rs` maps `knot::models::SubgraphNode` verbatim; the viewer already
renders `signature` in the detail panel — that is where `(Path path, File assetRoot)`
appears today). So this is a **frontend-only** change in
`assets/graph-viewer.html`; no Rust change, no API change.

### E2 — `kotlin_enum` is missing from `KIND_CATEGORY_CLASSES`

*Recommendation: fix, but as a clearly-labelled separate commit.* This is a
**pre-existing bug unrelated to 1.5.6**, surfaced while auditing the kind
constants. `enum` (`models.rs:165`) and `groovy_enum` (`models.rs:164`) are both
in `KIND_CATEGORY_CLASSES`, and every other Kotlin kind is categorised — but
`kotlin_enum` appears in none of the three constants, so Kotlin enums are
invisible in the default overview.

Caveat for the decision: fixing it **changes the default graph contents for
every indexed Kotlin repo**. That is the intent, but it is a visible behaviour
change and should not be smuggled into a dependency bump.

### E3 — The other 28 uncategorised entity kinds

*Recommendation: out of scope; separate spec.* 47 of 76 `EntityKind` strings are
covered by the three constants. Besides `kotlin_enum`, these 28 are absent:

`html_element`, `html_id`, `html_class`, `css_class`, `css_id`, `css_variable`,
`build_dependency`, `build_plugin`, `build_task`, `pipeline_stage`,
`pipeline_step`, `cargo_package`, `cargo_feature`, `workspace_member`,
`config_property`, `k8s_deployment`, `k8s_service`, `k8s_configmap`,
`k8s_secret`, `k8s_ingress`, `k8s_namespace`, `k8s_resource`, `helm_chart`,
`helm_value`, `helm_template_var`, `project_identity`, `markdown_document`,
`markdown_section`.

They are reachable today only via the `other` category, which sets
`kind_filter = None` and drops the Cypher predicate entirely
(`graph_parse.rs:30-33`). Deciding a taxonomy for K8s/Helm/build/markdown kinds
is a design task, not an upgrade task.

---

## 6. Plan

Quality gate after **every** phase: invoke the `validator` subagent
(`cargo fmt`, `cargo clippy -- -D warnings`, `cargo test`, plus the E2E scripts
under `tests/`). Enumerate `tests/` before running rather than assuming script
names.

### Phase 1 — Dependency bump (mandatory)

No TDD cycle: there is no behaviour to specify, and no test can meaningfully go
red first for a version-number change. The proof obligation is that the existing
suite stays green.

1. `Cargo.toml:25` — `knot = "1.5.5"` → `knot = "1.5.6"`.
2. `cargo update -p knot --precise 1.5.6` to refresh `Cargo.lock` (do not run a
   blanket `cargo update`; keep the diff to the single crate).
3. Verify `Cargo.lock` shows `version = "1.5.6"` and a new `checksum` for the
   `knot` package (currently `b1e8d265…` for 1.5.5 at `Cargo.lock:2031-2034`).
4. `cargo build --all-targets` — must succeed with no new warnings.
5. Run the validator subagent.
6. Rebuild the Docker image (`Dockerfile`) and confirm `up.sh` brings the stack
   up healthy.

**Exit criteria:** clean build, full suite green, `git diff` limited to
`Cargo.toml` + `Cargo.lock`.

### Phase 2 — E1: synthetic accessor badge *(only if E1 = yes)*

BDD, driven from the viewer. Frontend-only, in `assets/graph-viewer.html`.

| Scenario | Given | When | Then |
| --- | --- | --- | --- |
| S1 | A focused node whose `signature` contains `<synthetic` | the detail panel renders | a `synthetic` badge is shown next to the kind, and the raw marker string is **not** printed as if it were a parameter list |
| S2 | A node with an ordinary signature `(Path path, File assetRoot)` | the detail panel renders | no badge; signature rendered as today |
| S3 | A node with no `signature` (null) | the detail panel renders | no badge, no crash |
| S4 | Graph contains a property and its synthetic accessor on the same `file:line` | the graph renders | both nodes are present and visually distinguishable |

Implementation note: the check must be on the marker substring, not on
`kind === 'groovy_method'` — real Groovy methods share that kind.

Test approach: knot-server has no JS test harness. Either (a) drive S1–S4
manually against a re-indexed Groovy repo and record the evidence in the PR, or
(b) extract the predicate into a tiny pure function and assert it in a Rust unit
test if the badge decision is moved server-side. **Recommend (a)** — moving the
decision server-side would mean adding a field to the `/graph` response, i.e.
an API change, which contradicts Section 3's finding that none is needed.

### Phase 3 — E2: categorise `kotlin_enum` *(only if E2 = yes)*

Genuine TDD, in `src/handlers/models.rs`'s existing `#[cfg(test)] mod tests`
(starts at `models.rs:209`, alongside
`valid_relationships_has_no_duplicates_and_is_upper_case`).

**RED**

```
#[test]
fn every_enum_kind_is_categorised_as_a_class() {
    // GIVEN the three kind-category constants
    // WHEN collecting all entries
    // THEN every "*enum*" serialized kind knot can emit is present
    for k in ["enum", "groovy_enum", "kotlin_enum"] { … }
}
```

Fails today on `kotlin_enum`.

Companion test pinning the invariant that motivated the fix:

```
#[test]
fn kind_categories_are_disjoint_and_have_no_duplicates()
```

**GREEN** — add `"kotlin_enum"` to `KIND_CATEGORY_CLASSES` (`models.rs:149`),
adjacent to the other Kotlin class-like kinds.

**Regression check** — `graph_parse.rs` tests that assert on expanded kind sets
must be re-read; if any asserts an exact set length for `classes`, update it
deliberately and note why.

Ship as its own commit with a message that makes the behaviour change explicit,
e.g. `fix(graph): categorise kotlin_enum under the classes kind category`.

### Phase 4 — Release hygiene

- `CHANGELOG.md` — new entry. State plainly that this is a dependency bump, that
  the Groovy fixes require a **re-index** to take effect, and (if Phase 3 ran)
  call out the Kotlin enum behaviour change separately.
- `README.md` — per project convention, reflect any behaviour change. If only
  Phase 1 ran, a README change is likely unnecessary; do not manufacture one.
- `skills/graph.md` and `.knot-server-agent-skills/graph.md` — these two files
  are duplicates. If either documents Groovy entity kinds or the `OVERRIDES`
  toggle, update **both** or they will drift.
- Version bump in `Cargo.toml:3` (`0.2.18` → next).
- **Do not push to `master` and do not publish to crates.io without explicit
  approval.**

### Phase 5 — Validation against real data (user-performed)

The user re-indexes `nextflow` (registered repo id: `nextflow`, not `nextdown`).

Acceptance checks:

1. `knot callers getBaseDir -r nextflow` → `Session` appears under
   *Overridden by*; `ISession` under *Overrides*.
2. Neo4j: `MATCH (e:Entity {fqn:'nextflow.Session.getBaseDir'}) RETURN e` → 1 row
   (was 0).
3. Neo4j: `MATCH (a)-[:OVERRIDES]->(b {fqn:'nextflow.ISession.getBaseDir'}) RETURN a.fqn`
   → returns `nextflow.Session.getBaseDir` (was 0 rows).
4. Neo4j regression guard for the Javadoc phantom:
   `MATCH (e:Entity {repo_name:'nextflow', name:'name'}) RETURN count(e)` → 0
   (was ≥ 2).
5. Viewer: search `getBaseDir`, focus `ISession.getBaseDir` — the subgraph now
   contains `Session.getBaseDir`, not just `ISession` and `HashBuilder.isAssetFile`.
6. Confirm the local `knot` CLI is upgraded too (it was at **1.5.0** during
   diagnosis, i.e. older than the 1.5.5 the server vendors) — otherwise CLI and
   server results will keep diverging.

---

## 7. Risks

| Risk | Severity | Mitigation |
| --- | --- | --- |
| Transitive dependency drift from `cargo update` | Medium | Use `cargo update -p knot --precise 1.5.6`; review the `Cargo.lock` diff and reject unrelated bumps |
| Users read the changelog and expect the Groovy fixes without re-indexing | Medium | State the re-index requirement prominently in `CHANGELOG.md` |
| Groovy graphs grow enough to hit the 500-node focus cap | Low | Pre-existing; measure entity-count delta on `nextflow` during Phase 5 and record it. Note that overview mode still reports `truncated: false` unconditionally |
| Phase 3 silently changes Kotlin graph defaults | Medium | Separate commit, explicit changelog entry |
| E1 badge keys off the wrong field and hides real methods | Low | Match the marker substring, never the kind; scenario S2 pins this |

## 8. Rollback

Revert `Cargo.toml` + `Cargo.lock` to `knot = "1.5.5"` (checksum
`b1e8d265f00745f27f76b14e9f2e933aff4b601bcfe2b0bd8a71181365a2d2ae`) and rebuild.
Because there is no schema or API change, no data migration is involved. Note
that already-re-indexed repositories keep their 1.5.6-shaped data until
re-indexed again — the rollback is code-only.

## 9. Files touched

| File | Phase | Mandatory? |
| --- | --- | --- |
| `Cargo.toml`, `Cargo.lock` | 1 | Yes |
| `assets/graph-viewer.html` | 2 | No — E1 |
| `src/handlers/models.rs` | 3 | No — E2 |
| `CHANGELOG.md`, `README.md`, `skills/graph.md`, `.knot-server-agent-skills/graph.md` | 4 | Partly |

No changes to `src/handlers/graph.rs`, `graph_queries.rs`, `graph_parse.rs`,
`graph_map.rs`, or `graph_utils.rs`.
