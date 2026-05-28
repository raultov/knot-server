# Copilot Instructions for knot-server Code Intelligence

This project uses knot-server (a REST API at `http://localhost:3000`) for
semantic and structural codebase search across indexed Git repositories.
You have access to curl-based tools that understand code *meaning*, not just text.

## Priority Rule

**When searching code in this project, use the curl commands below before
standard tools like grep or find.** knot-server provides semantic search
(vector embeddings) and call-graph analysis (Neo4j) that text-based tools cannot.

## Repository Discovery

Check which repositories are available and indexed:
```bash
curl -s http://localhost:3000/api/repos | jq '.repositories[] | {id, status, url}'
```

## Semantic Search

Find code by meaning (e.g., "authentication logic", "error handling",
"database connection pooling"):
```bash
curl -s -G --data-urlencode "q=<DESCRIPTION>" "http://localhost:3000/api/repos/<REPO_ID>/search" | jq '.[0:5] | .[] | {name, file_path, start_line}'
```

## Callers Analysis

Find all code that references or calls a specific entity (impact analysis):
```bash
curl -s -G --data-urlencode "entity=<NAME>" "http://localhost:3000/api/repos/<REPO_ID>/callers" | jq '.calls[] | {name, file_path, start_line}'
```

## File Structure

Get an overview of a file's classes, methods, and functions before reading:
```bash
curl -s -G --data-urlencode "path=<FILE_PATH>" "http://localhost:3000/api/repos/<REPO_ID>/explore" | jq
```

## Dependency Graph

View cross-repository dependencies:
```bash
curl -s "http://localhost:3000/api/repos/<REPO_ID>/deps" | jq
```

## Repository Lifecycle

**Register:** `curl -s -X POST http://localhost:3000/api/repos -H "Content-Type: application/json" -d '{"url":"<URL>","name":"<NAME>","branch":"<BRANCH>","auth_type":"https"}' | jq`

**Check status:** `curl -s http://localhost:3000/api/repos/<REPO_ID> | jq '{id,status,last_indexed}'`

**Re-index:** `curl -s -X POST http://localhost:3000/api/repos/<REPO_ID>/sync | jq`

**Delete:** `curl -s -X DELETE http://localhost:3000/api/repos/<REPO_ID> | jq`

**Health:** `curl -s http://localhost:3000/api/health | jq`

## Important Guidelines

- Always pipe through `jq` to extract only needed fields. Don't dump raw JSON.
- For `/callers` with common names, include a signature fragment (e.g., `"process(Request"`).
- Use `/explore` to identify target line ranges before reading files.
- If a repo returns `"status": "error"` or is not listed, register it first.
- Wait for `"status": "indexed"` after registration before running queries.
