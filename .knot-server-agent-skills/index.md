---
name: index
description: Register and index the current repository in knot-server
---

# knot-server /index

Register or re-index the current repository in knot-server.

## Step 0: Preflight

Before running this, you **must** run the `[[preflight]]` check to ensure the
server is running. If the server is unreachable, stop and ask the user which
port it is on.

## Step 1: Get the Current Repository Path

```bash
REPO_PATH="$(pwd)"
```

## Step 2: Check if Already Registered

```bash
REPO_ID=$(basename "$REPO_PATH")
curl -fsS "${KNOT_SERVER_URL:-http://localhost:3000}/api/repos" \
  | jq -r --arg id "$REPO_ID" '.repositories[] | select(.id == $id) | .status'
```

### Decision tree

- **No output (empty)** → Repo not registered. Proceed to Step 3.
- **`indexed`** → Repo already indexed. Proceed to Step 5 (re-index via sync).
- **`pending`, `cloning`, `pulling`, `indexing`** → Currently being processed.
  Tell the user and stop.
- **`error`** → Previous run failed. Proceed to Step 5 (re-index via sync).

## Step 3: Register the Repository

```bash
curl -fsS -X POST "${KNOT_SERVER_URL:-http://localhost:3000}/api/repos" \
  -H "Content-Type: application/json" \
  -d "{
    \"url\": \"$REPO_PATH\",
    \"auth_type\": \"ssh\"
  }" | jq
```

The server will attempt to read this as a local path. **Note:** if running in Docker, the path must be mounted inside the container (e.g., via `KNOT_LOCAL_REPOS_DIR` or inside `/var/lib/knot/repos`). Always provide a valid, non-empty URL or path to avoid creating an undeletable empty repository entry. The response should be HTTP 202 Accepted.

## Step 4: Wait for Indexing to Complete

Poll `GET /api/repos/{id}` until `status` becomes `indexed`. *(Note: Initial indexing of large repositories can take 5-10 minutes. Status updates happen roughly every 5 seconds.)*

```bash
while true; do
  STATUS=$(curl -fsS "${KNOT_SERVER_URL:-http://localhost:3000}/api/repos/$REPO_ID" \
    | jq -r '.status')
  case "$STATUS" in
    indexed)
      printf '✅ Repository indexed successfully.\n'
      break
      ;;
    error)
      printf '❌ Indexing failed.\n'
      curl -fsS "${KNOT_SERVER_URL:-http://localhost:3000}/api/repos/$REPO_ID" | jq
      break
      ;;
    *)
      printf '⏳ Status: %s — waiting 5s...\n' "$STATUS"
      sleep 5
      ;;
  esac
done
```

## Step 5: Re-Index an Existing Repository

If the repo was already registered (status `indexed` or `error`), trigger
a sync to pull latest changes and re-index:

```bash
curl -fsS -X POST "${KNOT_SERVER_URL:-http://localhost:3000}/api/repos/$REPO_ID/sync" | jq
```

This returns HTTP 202 Accepted with `{"message": "Sync job enqueued"}`.
Then wait for completion using the same polling loop from Step 4.

## Step 6: Verify

Once indexed, run a quick search to confirm the repo is queryable:

```bash
curl -fsS -G \
  --data-urlencode "q=main entry point" \
  --data-urlencode "max_results=3" \
  "${KNOT_SERVER_URL:-http://localhost:3000}/api/repos/$REPO_ID/search" \
  | jq '.[0:3] | .[] | {name, file_path, start_line}'
```

Tell the user the repo is ready and show the first few search results as proof.

## Connection-Error Footnote

⚠️ **If any call returns connection refused / timeout / network error, stop
and ask the user:**
> *"knot-server no responde en `${KNOT_SERVER_URL:-http://localhost:3000}`.
> ¿En qué puerto está corriendo? (default 3000, env `KNOT_SERVER_PORT`,
> CLI flag `--port`)."*

Then re-export `KNOT_SERVER_URL` and retry.
