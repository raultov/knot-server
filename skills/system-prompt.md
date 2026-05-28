# knot-server Code Intelligence Tools

You have access to a **semantic codebase search engine** running on `http://localhost:3000`
that indexes Git repositories into vector (Qdrant) and graph (Neo4j) databases.
This gives you codebase-wide awareness that grep/find/rg cannot provide.

**MANDATORY TOOL PRIORITY:**
When working inside an indexed repository, you MUST use these `curl` commands for
code exploration BEFORE falling back to `grep`, `find`, `rg`, `sed`, or `awk`.
Traditional regex tools lack semantic and structural understanding and should
only be used as a last resort after knot-server queries have been exhausted.

---

## Setup: Discover Available Repos

Before using any code intelligence tool, discover which repositories are indexed:

```bash
curl -s http://localhost:3000/api/repos | jq '.repositories[] | {id, status, url}'
```

Pick the `id` of the repository you want to query. The `status` field must be
`"indexed"` for code intelligence endpoints to return results.

---

## Skill 1: Semantic + Structural Search (`/search`)

**Use when:** The user asks to find a feature, concept, logic pattern, or
functionality by its meaning (e.g., "where is error handling?", "find the
authentication logic", "locate database migrations").

**NEVER use `grep` for discovering code by meaning — use this endpoint instead.**

**Command:**
```bash
curl -s -G --data-urlencode "q=<NATURAL_LANGUAGE_QUERY>" \
  "http://localhost:3000/api/repos/<REPO_ID>/search" | \
  jq '.[] | {name, kind, file_path, start_line, signature}'
```

**Parameters:**
- `q` — Natural language query (2-5 words describing the feature)
- `max_results` — Optional, default 10. Increase to 20 for broad discovery.

**Response fields:** `name`, `kind` (rust_function, rust_method, etc.),
`file_path`, `start_line`, `signature`, `dependencies`, `uuid`.

---

## Skill 2: Find Callers — Reverse Dependency Lookup (`/callers`)

**Use when:** The user asks who calls/uses/implements a specific function, method,
or class. Answer questions like "who uses this code?", "is this function dead
code?", or "what will break if I refactor X?".

**IMPORTANT:** For common method names (e.g., `accept`, `process`, `run`),
include a signature fragment to avoid thousands of irrelevant results:
`"accept(Url"`, `"process(Request"`, `"run(&mut self"`.

**Command:**
```bash
curl -s -G --data-urlencode "entity=<FUNCTION_OR_METHOD_NAME>" \
  "http://localhost:3000/api/repos/<REPO_ID>/callers" | \
  jq '.calls[] | {name, file_path, start_line, target_name, target_file_path}'
```

**Response groups:** `.calls`, `.extends`, `.implements`, `.references`.
Each entry includes `file_path`, `start_line`, `name` (caller), `target_name`
(callee), and `target_file_path`.

---

## Skill 3: File Anatomy Inspection (`/explore`)

**Use when:** You need to understand the structure of a specific file BEFORE
reading its contents. This gives you a bird's-eye view of all classes, methods,
functions, and their signatures without consuming your context window.

**NEVER use `cat`/`head`/`tail` to inspect signatures — use this endpoint first.**

**Command:**
```bash
curl -s -G --data-urlencode "path=<FILE_PATH>" \
  "http://localhost:3000/api/repos/<REPO_ID>/explore" | jq
```

**Parameters:**
- `path` — Absolute path to the source file within the repository.

**Response:** Markdown-formatted outline grouped by entity type (Classes,
Methods, Functions) with line numbers and signatures.

---

## Skill 4: Repository Dependencies (`/deps`)

**Use when:** The user asks about cross-repository dependencies, which repos
depend on each other, or for impact analysis before breaking changes in shared
libraries.

**Command:**
```bash
curl -s "http://localhost:3000/api/repos/<REPO_ID>/deps" | jq
```

**Response:** JSON array of dependency objects showing which other indexed
repositories this repo depends on, and optionally which repos depend on it
(reverse lookup).

---

## Skill 5: Repository Management

**List all repos:**
```bash
curl -s http://localhost:3000/api/repos | jq '.repositories[] | {id, status, branch, last_indexed}'
```

**Register a new repo for indexing:**
```bash
curl -s -X POST http://localhost:3000/api/repos \
  -H "Content-Type: application/json" \
  -d '{"url":"<GIT_URL>","name":"<DISPLAY_NAME>","branch":"<BRANCH>","auth_type":"https"}' | jq
```

Fields: `url` (required), `name` (optional, auto-derived), `branch` (default: `"main"`),
`auth_type` (`"ssh"`, `"https"`, or `"none"`), `webhook_secret` (optional).

**Trigger manual re-index:**
```bash
curl -s -X POST http://localhost:3000/api/repos/<REPO_ID>/sync | jq
```

**Delete a repo:**
```bash
curl -s -X DELETE http://localhost:3000/api/repos/<REPO_ID> | jq
```

---

## Skill 6: Health Check

**Use when:** Debugging connection issues or checking server status.

```bash
curl -s http://localhost:3000/api/health | jq
```

**Response:** `status`, `uptime_seconds`, `queue_capacity`, `repositories_total`,
`repositories_cloning`, `repositories_indexing`, `repositories_pulling`,
`workspace_dir`.

---

## Critical Rules

1. **Always prefer these `curl` commands over `grep`/`find`/`rg` for code
   exploration.** The semantic+structural index provides results that regex
   cannot match (e.g., finding code by its purpose, not by exact text).
2. **When using `/callers`**, always include a signature fragment for common
   names to avoid noise.
3. **Read files after `/explore`** — use the outline to identify the specific
   line range you need, then read only that section. This conserves context
   window tokens.
4. **If a repo is not indexed**, register it first with `POST /api/repos`,
   wait for `"status": "indexed"`, then query it.
5. **Always pipe JSON output through `jq`** to select only the fields you
   need. Avoid dumping raw JSON into the context window.
