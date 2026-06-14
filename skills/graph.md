# Knot-Server Graph: Entity Relationship Subgraphs

**Endpoints:** `GET /api/repos/{id}/graph` and `GET /api/repos/{id}/graph/expand`

## Step 0: Preflight

Before running this, you **must** run the `[[preflight]]` check to ensure the
server is running and the target repository's status is `indexed`. If the repo
is not indexed, stop and inform the user.

## Purpose

Query the raw entity relationship graph. The graph endpoints allow you to
extract a subgraph centered on a specific entity, or get an overview of the
entire repository architecture. This provides raw nodes and edges that you can
analyze or render.

Users can also explore these graphs visually by opening the interactive viewer
at `http://localhost:3000/graph` in their browser.

## Request 1: Get Subgraph

```bash
# Retrieve the repository ID from [[preflight]] (e.g. "my-app")
REPO_ID="my-app"

# Center the graph on this entity
ENTITY="AuthService"

curl -fsS -G \
  --data-urlencode "entity=${ENTITY}" \
  --data-urlencode "depth=2" \
  --data-urlencode "direction=both" \
  --data-urlencode "relationships=CALLS,EXTENDS,IMPLEMENTS,REFERENCES" \
  --data-urlencode "kinds=classes,interfaces,functions" \
  "${KNOT_SERVER_URL:-http://localhost:3000}/api/repos/${REPO_ID}/graph" \
  | jq
```

### Parameters

- **`id`** (path): The repository ID.
- **`entity`** (query, optional): Entity name to center the graph on. Supports
  signature fragments. If omitted, returns a repository-wide overview.
- **`entity_id`** (query, optional): Entity UUID (alternative to `entity`).
- **`depth`** (query, optional, default: 2): Traversal depth (1-5).
- **`direction`** (query, optional, default: "both"): Traversal direction
  (`incoming`, `outgoing`, `both`).
- **`relationships`** (query, optional): Comma-separated list of edges to include.
  - Options: `CALLS`, `EXTENDS`, `IMPLEMENTS`, `REFERENCES`, `REFERENCES_DOM`,
    `USES_CSS_CLASS`, `IMPORTS_SCRIPT`, `IMPORTS_STYLESHEET`, `MACRO_CALLS`,
    `CONTAINS`, `GENERIC_BOUND`, `DEPENDS_ON`.
- **`kinds`** (query, optional): Comma-separated node kinds to include
  (`classes`, `interfaces`, `functions`, `other`). Default: `classes,interfaces`.

## Request 2: Expand Node

Once you have a subgraph, you can "expand" a specific leaf node to see its
neighborhood, while ignoring nodes you already know about.

```bash
# Expand from this UUID
NODE_UUID="1234-5678-..."

# Comma-separated UUIDs to exclude (e.g. nodes you already fetched)
KNOWN_UUIDS="abcd-efgh-...,9876-5432-..."

curl -fsS -G \
  --data-urlencode "entity_id=${NODE_UUID}" \
  --data-urlencode "exclude=${KNOWN_UUIDS}" \
  --data-urlencode "depth=1" \
  "${KNOT_SERVER_URL:-http://localhost:3000}/api/repos/${REPO_ID}/graph/expand" \
  | jq
```

## Output Format

Both endpoints return a `GraphResponse` object:

```json
{
  "root_id": "88cd6ec3-441f-...",
  "nodes": [
    {
      "id": "88cd6ec3-441f-...",
      "name": "AuthService",
      "kind": "class",
      "language": "typescript",
      "file_path": "src/auth.ts",
      "start_line": 10
    },
    {
      "id": "abcd-efgh-...",
      "name": "validateToken",
      "kind": "method",
      "file_path": "src/auth.ts",
      "start_line": 15
    }
  ],
  "edges": [
    {
      "source": "88cd6ec3-441f-...",
      "target": "abcd-efgh-...",
      "type": "CONTAINS"
    }
  ],
  "truncated": false,
  "total_nodes_found": 2
}
```

## When to Use Graph

- **Custom Analysis:** When [[search]], [[callers]], or [[explore]] don't provide
  the exact view you need (e.g. you need to see `CONTAINS` edges or HTML DOM
  references).
- **Deep Traversal:** When you need to trace an execution path 4 or 5 levels deep
  and a flat list of callers isn't enough.
- **Visualizing:** If the user asks you to "draw a diagram" or "map out the
  architecture", you can fetch the raw graph and format it as Mermaid.js or
  Graphviz.

## Troubleshooting

### "Entity not found" (HTTP 404)

**Cause:** The entity name is wrong, or too generic and was filtered out.
**Solutions:**
- Use signature fragments to be precise (e.g. `entity=handle(Request`).
- Ensure you are querying the correct repository ID.

### Empty Nodes/Edges Array

**Cause:** The entity exists but has no relationships of the requested types,
or they were filtered out by `kinds`.
**Solutions:**
- Widen the `kinds` filter (e.g. `kinds=classes,interfaces,functions,other`).
- Check if you restricted `relationships` too much.

## Connection-Error Footnote

⚠️ **If the call returns connection refused / timeout / network error, stop
and ask the user:**
> *"knot-server no responde en `${KNOT_SERVER_URL:-http://localhost:3000}`.
> ¿En qué puerto está corriendo? (default 3000, env `KNOT_SERVER_PORT`,
> CLI flag `--port`)."*

Then re-export `KNOT_SERVER_URL` and retry.
