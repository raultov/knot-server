# Copilot Instructions for knot-server Code Intelligence

This project uses knot-server (a REST API typically at `http://localhost:3000`) for
semantic and structural codebase search across indexed Git repositories.
You have access to curl-based tools that understand code *meaning*, not just text.

## Priority Rule

**When searching code in this project, use the knot-server curl commands before
standard tools like grep or find.** knot-server provides semantic search
(vector embeddings) and call-graph analysis (Neo4j) that text-based tools cannot.

## Available Skills

The specific API endpoints and workflow rules are documented in 8 topic skills.
Read these files to learn the exact `curl` syntax and JSON parsing rules:

1. **`[[preflight]]`** — **CRITICAL**: Server health and index status check. Run this first!
2. **`[[search]]`** — Semantic code discovery.
3. **`[[callers]]`** — Reverse dependency lookup and impact analysis.
4. **`[[explore]]`** — File anatomy and structure discovery.
5. **`[[deps]]`** — Cross-repository dependency graph traversal.
6. **`[[graph]]`** — Raw entity relationship subgraphs.
7. **`[[repos]]`** — Repository lifecycle: list, register, sync, delete.
8. **`[[index]]`** — Register and index the current repository in knot-server.
9. **`[[workflows]]`** — Common multi-step patterns and best practices.

## Connection & Port Handling

If your `curl` commands fail with `Connection refused` or `timeout`:
**STOP and ask the user** which port knot-server is running on (default 3000,
env `KNOT_SERVER_PORT`). Do not silently fall back to regex searches.
