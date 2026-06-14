# Knot-Server Deps: Repository Dependency Graph

**Endpoint:** `GET /api/repos/{id}/deps?depth=...&reverse=...`

## Step 0: Preflight

Before running this, you **must** run the `[[preflight]]` check to ensure the
server is running and the target repository's status is `indexed`. If the repo
is not indexed, stop and inform the user.

## Purpose

Traverse the `DEPENDS_ON` graph between indexed repositories. Knot auto-discovers
cross-repository dependencies from build-system files (Maven `pom.xml`, Gradle
`build.gradle`, Cargo `Cargo.toml`, npm `package.json`, etc.) and stores them as
edges between repositories in Neo4j.

This answers:
- "Which repositories does this project depend on?"
- "Which projects depend on this library?" (with `reverse=true`)
- "How deep does the dependency chain go?"

## Request

```bash
# Retrieve the repository ID from [[preflight]] (e.g. "backend")
REPO_ID="backend"

curl -fsS -G \
  --data-urlencode "depth=2" \
  --data-urlencode "reverse=false" \
  "${KNOT_SERVER_URL:-http://localhost:3000}/api/repos/${REPO_ID}/deps" \
  | jq
```

### Parameters

- **`id`** (path): The repository ID. Must match an indexed repository.
- **`depth`** (query, optional, default: 3): Maximum depth for transitive traversal.
  - `1` = direct dependencies only
  - `2` = direct + one level deeper
  - Maximum: 10
- **`reverse`** (query, optional, default: false): Show **reverse** dependencies
  — repositories that depend ON this one. Useful for impact analysis.

## Output Format

The endpoint returns a JSON array of dependency objects. The exact shape
depends on the traversal, but typically includes the repository names.

```json
[
  { "repo_name": "auth-lib" },
  { "repo_name": "common-utils" },
  { "repo_name": "billing-api" }
]
```

## Cross-Repository Call Resolution

`DEPENDS_ON` edges are not just informational — they enable **cross-repository
call resolution**. When the [[callers]] endpoint follows a CALLS edge and the target
entity is not in the current repository, knot-server automatically looks up matching
entities in any of the directly-depended-on repositories.

This means that if you have:
- `auth-lib` indexed (defines `TokenVerifier.verify()`)
- `my-app` indexed, with a `DEPENDS_ON` edge to `auth-lib`

Then querying [[callers]] for `TokenVerifier` on `my-app` will show calls to
`TokenVerifier.verify()` that come from `my-app` *and* any repository `my-app`
depends on.

## When to Use Deps

### 1. Onboarding to a Multi-Repository Workspace
When joining a project with microservices or shared libraries, start with
[[repos]] to see what is indexed, then `deps` to understand the topology.

### 2. Breaking-Change Impact Analysis (Library Author)
Before making a breaking change to a shared library, find every consumer:
`GET /api/repos/my-lib/deps?reverse=true`
Then for each consumer, trace which specific functions they call using the
[[callers]] endpoint.

### 3. Verifying Auto-Discovered Dependencies
After registering and indexing a new repository, confirm that knot-server
picked up the build-system declarations.

## Limitations

- **Only indexed repositories appear:** A dependency declared in `package.json`
  but not yet registered and indexed in knot-server will not show up.
- **Build-system-driven:** Edge discovery comes from package manifests — it
  does not crawl source code for `import` statements.
- **Retroactive linking:** If you index a library *after* its consumer, the
  consumer will retroactively gain a `DEPENDS_ON` edge on the next index run.
  Trigger a sync on the consumer via [[repos]] to pick it up.

## Connection-Error Footnote

⚠️ **If the call returns connection refused / timeout / network error, stop
and ask the user:**
> *"knot-server no responde en `${KNOT_SERVER_URL:-http://localhost:3000}`.
> ¿En qué puerto está corriendo? (default 3000, env `KNOT_SERVER_PORT`,
> CLI flag `--port`)."*

Then re-export `KNOT_SERVER_URL` and retry.
