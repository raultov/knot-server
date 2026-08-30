# Knot-Server Repos: Repository Lifecycle Management

**Endpoints:** `GET/POST/DELETE /api/repos`, `GET /api/repos/{id}`, `POST /api/repos/{id}/sync`, `POST /api/webhook/{id}`

## Step 0: Preflight

You can run `GET /api/repos` as part of the `[[preflight]]` check to see what
is indexed. To use the other endpoints here, the server must be reachable.

## Purpose

List registered repositories, check their indexing status, register new ones,
trigger manual syncs, or remove them. 

## 1. List Repositories

```bash
curl -fsS "${KNOT_SERVER_URL:-http://localhost:3000}/api/repos" | jq
```

Returns a list of `RepoEntry` objects:
```json
{
  "repositories": [
    {
      "id": "knot-server",
      "url": "https://github.com/raultov/knot-server.git",
      "branch": "master",
      "status": "indexed",
      "last_indexed": "2026-06-14T08:31:22Z"
    }
  ]
}
```

### Status Values
- `indexed`: Ready for queries (search, callers, etc).
- `pending`: Registered, waiting to clone.
- `cloning`: Git clone in progress.
- `pulling`: Git fetch in progress.
- `indexing`: Qdrant/Neo4j ingestion in progress.
- `error`: Last run failed.

## 2. Get Single Repository

```bash
curl -fsS "${KNOT_SERVER_URL:-http://localhost:3000}/api/repos/my-app" | jq
```

Returns detailed information about the repo, including its local workspace path
and exact error message if `status == "error"`.

## 3. Register a New Repository

```bash
curl -fsS -X POST "${KNOT_SERVER_URL:-http://localhost:3000}/api/repos" \
  -H "Content-Type: application/json" \
  -d '{
    "url": "https://github.com/user/my-app.git",
    "branch": "main",
    "auth_type": "ssh"
  }' | jq
```

The server queues a clone + index job. **The endpoint is idempotent** — if
the repo already exists, it is re-registered (cleaned up and cloned from
scratch).

### Parameters
- `url` (required): Git URL or **local absolute path** (e.g. `/home/user/code`).
  Local paths are mirrored without git, picking up uncommitted changes.
- `name` (optional): Display name. Derived from URL if omitted.
- `branch` (optional, default "main"): Branch to clone.
- `auth_type` (optional, default `"ssh"`): accepts only `"ssh"` or `"https"` (plain strings — no nested object).
- `webhook_secret` (optional): Secret for the webhook endpoint.

## 4. Trigger Manual Sync

```bash
curl -fsS -X POST "${KNOT_SERVER_URL:-http://localhost:3000}/api/repos/my-app/sync" | jq
```

Queues a `git fetch` (or local mirror update) + incremental re-index. Returns
HTTP 202 Accepted.

## 5. Delete a Repository

```bash
curl -fsS -X DELETE "${KNOT_SERVER_URL:-http://localhost:3000}/api/repos/my-app" | jq
```

Removes the repo from the registry, deletes its local workspace, and fires a
background job to wipe its data from Qdrant and Neo4j.

## 6. Webhooks (Git Providers)

`POST /api/webhook/{id}`

Used by GitHub, GitLab, and Bitbucket to trigger syncs on `push` events.
Requires the `webhook_secret` to be set during registration. You generally
don't call this directly, but you can configure the Git provider to hit this URL.

## Connection-Error Footnote

⚠️ **If the call returns connection refused / timeout / network error, stop
and ask the user:**
> *"knot-server is not responding at `${KNOT_SERVER_URL:-http://localhost:3000}`.
> What port is it running on? (default 3000, env `KNOT_SERVER_PORT`,
> CLI flag `--port`)."*

Then re-export `KNOT_SERVER_URL` and retry.
