# Cursor Rules for knot-server Code Intelligence

You have access to a **semantic codebase search engine** powered by `knot-server`
(a REST API) that indexes Git repositories into vector (Qdrant) and graph (Neo4j)
databases. This gives Cursor codebase-wide awareness that the built-in grep/symbol
search cannot provide.

<critical-enforcement>
**MANDATORY TOOL PRIORITY:**
When exploring or searching code inside a repository indexed by knot-server,
you MUST use `curl` against the REST API BEFORE falling back to Cursor's built-in
grep or codebase search. Traditional text search lacks semantic and structural
understanding and should only be used as a LAST RESORT.
</critical-enforcement>

## Available Skills

The specific API endpoints and workflow rules are documented in 8 topic skills.
Read these files to learn the exact `curl` syntax and JSON parsing rules:

1. **`[[preflight]]`** — **MANDATORY**: Server health and index status check. Run this first!
2. **`[[search]]`** — Semantic code discovery.
3. **`[[callers]]`** — Reverse dependency lookup and impact analysis.
4. **`[[explore]]`** — File anatomy and structure discovery.
5. **`[[deps]]`** — Cross-repository dependency graph traversal.
6. **`[[graph]]`** — Raw entity relationship subgraphs.
7. **`[[repos]]`** — Repository lifecycle: list, register, sync, delete.
8. **`[[index]]`** — Register and index the current repository in knot-server.
9. **`[[workflows]]`** — Common multi-step patterns and best practices.

## Connection & Port Handling

All skills use `${KNOT_SERVER_URL:-http://localhost:3000}`.
If your `curl` commands fail with `Connection refused` or `timeout`, **STOP and
ask the user** which port the server is running on (default 3000, env
`KNOT_SERVER_PORT`). Do not silently fall back to standard text search.
