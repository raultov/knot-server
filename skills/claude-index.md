---
name: index
description: Register or re-index the current repository in knot-server
disable-model-invocation: true
---

# /index — Register or Re-Index Current Repository in knot-server

Register or re-index the current repository in knot-server.

## Step 0: Verify knot-server is reachable

!`curl -fsS "${KNOT_SERVER_URL:-http://localhost:3000}/api/health" 2>&1 | head -5`

If the health check fails with connection refused or timeout, stop and ask the user:
"knot-server no responde. ¿En qué puerto está corriendo? (default 3000)"

## Step 1: Identify the current repository

- REPO_PATH = current working directory
- REPO_ID = basename of REPO_PATH

## Step 2: Check if already registered

!`curl -fsS "${KNOT_SERVER_URL:-http://localhost:3000}/api/repos" 2>/dev/null | jq -r --arg id "$(basename $(pwd))" '.repositories[] | select(.id == $id) | .status'`

Decision:
- No output (empty) → Not registered. Go to Step 3.
- "indexed" → Already indexed. Go to Step 5 (re-index via sync).
- "pending", "cloning", "pulling", "indexing" → In progress. Tell user and stop.
- "error" → Previous failure. Go to Step 5 (re-index via sync).

## Step 3: Register the repository (new repo)

Run:
```
curl -fsS -X POST "${KNOT_SERVER_URL:-http://localhost:3000}/api/repos" \
  -H "Content-Type: application/json" \
  -d "{\"url\": \"$(pwd)\", \"auth_type\": {\"type\": \"none\"}}"
```

The server detects a local path and mirrors the working tree. Expect HTTP 202.

## Step 4: Wait for indexing to complete

Poll GET /api/repos/REPO_ID every 5 seconds until status is "indexed" or "error".

## Step 5: Trigger sync (existing repo)

If already registered, run:
```
curl -fsS -X POST "${KNOT_SERVER_URL:-http://localhost:3000}/api/repos/REPO_ID/sync"
```

Expect HTTP 202 with {"message": "Sync job enqueued"}.

## Step 6: Verify

Run a quick search to confirm the repo is queryable:
```
curl -fsS -G --data-urlencode "q=main entry point" --data-urlencode "max_results=3" \
  "${KNOT_SERVER_URL:-http://localhost:3000}/api/repos/REPO_ID/search" | jq
```

Tell the user the repo is ready and show the first results.
