# knot-server Code Intelligence Tools

You have access to a **semantic codebase search engine** powered by `knot-server`,
a REST API that indexes Git repositories into vector (Qdrant) and graph (Neo4j)
databases. This gives you codebase-wide awareness that grep/find/rg cannot provide.

## MANDATORY TOOL PRIORITY

When working inside an indexed repository, you MUST use `curl` against the
knot-server REST API for code exploration BEFORE falling back to `grep`, `find`,
or `rg`. Traditional regex tools lack semantic and structural understanding and
should only be used as a last resort.

## Available Skills

You have been equipped with 8 specific skills covering the knot-server API.
**Load and read these skills when you need to perform the corresponding tasks.**

1. **`[[preflight]]`** — **MANDATORY STEP 0**. Server health and indexing status check.
   You must run this before any other skill.
2. **`[[search]]`** — Semantic code discovery (find features by meaning, not exact name).
3. **`[[callers]]`** — Reverse dependency lookup and impact analysis.
4. **`[[explore]]`** — File anatomy and structure discovery (classes, methods, signatures).
5. **`[[deps]]`** — Cross-repository dependency graph traversal.
6. **`[[graph]]`** — Raw entity relationship subgraphs.
7. **`[[repos]]`** — Repository lifecycle: register, sync, delete, list.
8. **`[[index]]`** — Register and index the current repository in knot-server.
9. **`[[workflows]]`** — Common multi-step patterns and best practices.

## Connection & Port Handling

All skills assume the server is at `${KNOT_SERVER_URL:-http://localhost:3000}`.
If you receive a connection error (e.g. `Connection refused`), **STOP and ask
the user** which port the server is running on. Do not silently fail over to
grep.
