# Knot-Server Preflight: Mandatory Step-0 Check

**Endpoints:** `GET /api/health` and `GET /api/repos`

## Purpose

**This is the first thing every LLM agent must do before invoking any other
knot-server endpoint.** It answers two questions:

1. **Is the server reachable on this port?** If not, ask the user which port
   `knot-server` is running on (default `3000`, env `KNOT_SERVER_PORT`,
   CLI flag `--port`).
2. **Is the repository I am about to query already indexed?** If not, stop and
   tell the user. Do **not** attempt to search/explore/etc. a repo whose
   status is `pending`, `cloning`, `pulling`, `indexing`, or `error` — the
   results will be empty or wrong.

If either check fails, **abort fast and explain the failure to the user**.
Do not silently fall back to grep/find/rg.

## Base URL Convention

Every curl example in this skill set uses:

```bash
"${KNOT_SERVER_URL:-http://localhost:3000}"
```

The user can override the base URL by exporting `KNOT_SERVER_URL` (e.g.
`export KNOT_SERVER_URL=http://localhost:4000`). If your call fails with
`Connection refused` or `timeout`, **ask the user which port knot-server is
on** and re-export the variable before retrying.

## Step 1 — Health Check

```bash
curl -fsS "${KNOT_SERVER_URL:-http://localhost:3000}/api/health" | jq
```

### Expected response

```json
{
  "status": "ok",
  "uptime_seconds": 12345,
  "queue_capacity": 100,
  "repositories_total": 4,
  "repositories_cloning": 0,
  "repositories_pulling": 0,
  "repositories_indexing": 1,
  "workspace_dir": "/var/lib/knot-server"
}
```

### Failure modes

| Symptom | Likely cause | Action |
|---|---|---|
| `curl: (7) Failed to connect` | Server not running, or wrong port | Ask: "knot-server is not responding at `${KNOT_SERVER_URL:-http://localhost:3000}`. What port is it running on? (default 3000, env `KNOT_SERVER_PORT`)". Re-export `KNOT_SERVER_URL` and retry. |
| `curl: (28) Connection timed out` | Wrong host / firewall | Ask the user the correct host and port. |
| HTTP 5xx | Server is up but in a bad state | Report the error verbatim to the user and stop. |
| `repositories_indexing > 0` | An index is in flight | Either wait and retry, or warn that results for that repo may be partial. |

**Do not proceed past Step 1 if the server is unreachable.**

## Step 2 — List Repositories and Verify Indexed Status

```bash
curl -fsS "${KNOT_SERVER_URL:-http://localhost:3000}/api/repos" \
  | jq '.repositories[] | {id, url, branch, status, last_indexed}'
```

### Expected response

```json
{ "id": "knot-server",  "url": "https://github.com/raultov/knot-server.git", "branch": "master", "status": "indexed", "last_indexed": "2026-06-14T08:31:22Z" }
{ "id": "knot",         "url": "https://github.com/raultov/knot.git",        "branch": "master", "status": "indexed", "last_indexed": "2026-06-13T19:02:11Z" }
```

### Status values (from `src/models.rs`)

| Status | Meaning | Safe to query? |
|---|---|---|
| `indexed` | Last indexing run finished successfully | ✅ Yes |
| `pending` | Just registered, clone not started | ❌ Stop, tell the user |
| `cloning` | `git clone` in progress | ❌ Stop, tell the user |
| `pulling` | `git fetch` in progress | ❌ Stop, tell the user |
| `indexing` | Indexer is writing to Qdrant + Neo4j | ❌ Stop, partial results |
| `error` | Last run failed | ❌ Stop, report the error |

### Decision tree

```
└── Is there a repo whose id matches the project I'm in?
    ├── No  → Ask the user: "The repo is not registered. Do you want to register it
    │         with POST /api/repos? (see skill knot-server-list-repos)". Then STOP.
    └── Yes → Is its status == "indexed"?
              ├── No  → Stop. Tell the user the current status and
              │         (if "error") fetch GET /api/repos/{id} for details.
              └── Yes → Proceed to the action skill (search, callers, explore,
                        deps, graph, …) using this id.
```

## One-liner: Quick Preflight

The full preflight in a single pipeline, returning the indexed repo IDs only:

```bash
curl -fsS "${KNOT_SERVER_URL:-http://localhost:3000}/api/health" >/dev/null \
  && curl -fsS "${KNOT_SERVER_URL:-http://localhost:3000}/api/repos" \
     | jq -r '.repositories[] | select(.status == "indexed") | .id'
```

If this prints nothing, no repository is queryable yet — register one with
the [[repos]] skill before continuing.

## Resolving the Repo ID

`knot-server` derives a repo's `id` from the last segment of its Git URL
(see `RegisterRepoRequest::generate_id` in `src/models.rs`). Examples:

| Git URL | Derived `id` |
|---|---|
| `https://github.com/raultov/knot-server.git` | `knot-server` |
| `git@github.com:raultov/knot.git` | `knot` |
| `/home/raul/workspace/rust/my-app` | `my-app` |

If you are unsure, list the repos and match against the `url` field:

```bash
curl -fsS "${KNOT_SERVER_URL:-http://localhost:3000}/api/repos" \
  | jq '.repositories[] | select(.url | contains("knot-server")) | .id'
```

## When to Re-run Preflight

- **Once per conversation** — at the very start, before any action skill.
- **After registering** a new repo, to confirm it moved to `status: indexed`.
- **After a long pause** in a conversation, since indexing state can change.
- **When a call fails** with a 404 or empty result — the repo may have been
  deleted or its id may be wrong.

## Connection-Error Footnote

⚠️ **If any call here returns connection refused / timeout / network
error, stop and ask the user:**

> *"knot-server is not responding at `${KNOT_SERVER_URL:-http://localhost:3000}`.
> What port is it running on? (default 3000, env `KNOT_SERVER_PORT`,
> CLI flag `--port`)."*

Then re-export `KNOT_SERVER_URL` and retry. Never silently fall back to
grep/find/rg without telling the user that knot-server is unreachable.

## See Also

- [[search]] — semantic + structural search (requires indexed repo)
- [[callers]] — reverse dependency lookup (requires indexed repo)
- [[explore]] — file anatomy inspection (requires indexed repo)
- [[deps]] — cross-repository dependency graph (requires indexed repo)
- [[graph]] — entity relationship subgraph (requires indexed repo)
- [[repos]] — repository lifecycle: register, sync, delete
- [[index]] — register and index the current repository (OpenCode `/index` command)
- [[workflows]] — multi-step patterns, all of which start with this preflight
