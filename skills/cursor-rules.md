# Cursor Rules for knot-server Code Intelligence

You have access to a **semantic codebase search engine** running on
`http://localhost:3000` that indexes Git repositories into vector (Qdrant)
and graph (Neo4j) databases. This gives Cursor codebase-wide awareness
that the built-in grep/symbol search cannot provide.

<critical-enforcement>
**MANDATORY TOOL PRIORITY:**
When exploring or searching code inside a repository indexed by knot-server,
you MUST use the `curl` commands below BEFORE falling back to Cursor's built-in
grep or codebase search. Traditional text search lacks semantic and structural
understanding and should only be used as a LAST RESORT.
</critical-enforcement>

## Discover Indexed Repositories

Before using any tool, run this to see which repos are available:
```bash
curl -s http://localhost:3000/api/repos | jq '.repositories[] | {id, status, branch}'
```
The `id` field is the `<REPO_ID>` used in all subsequent calls.

---

## Tool 1: Semantic Code Search

**Use for:** "Find the authentication logic", "Where are database migrations?",
"Locate error handling code". Any query about code *meaning*, not exact text.

```bash
curl -s -G --data-urlencode "q=<QUERY>" \
  "http://localhost:3000/api/repos/<REPO_ID>/search" | \
  jq '.[] | "[\(.kind)] \(.name) — \(.file_path):\(.start_line)"'
```

Increase `max_results` to 20 for broad discovery: append `&max_results=20` to the URL.

## Tool 2: Find Callers (Reverse Dependencies)

**Use for:** "Who calls `process_request`?", "Is `oldParser` dead code?",
"Impact analysis before refactoring".

```bash
curl -s -G --data-urlencode "entity=<FUNCTION_NAME>" \
  "http://localhost:3000/api/repos/<REPO_ID>/callers" | \
  jq '.calls[] | "\(.name) in \(.file_path):\(.start_line) calls \(.target_name)"'
```

**CRITICAL:** For common names like `run`, `accept`, `process`, include a
signature fragment: `"process(Request"`, `"accept(List"`, `"run(&mut self"`.

## Tool 3: File Structure Overview

**Use for:** Getting a bird's-eye view of a file BEFORE reading it. Saves
context window and helps you target the right line range.

```bash
curl -s -G --data-urlencode "path=<ABSOLUTE_FILE_PATH>" \
  "http://localhost:3000/api/repos/<REPO_ID>/explore" | jq
```

## Tool 4: Repository Dependency Graph

**Use for:** "What does this repo depend on?", "Which repos depend on this one?",
Cross-repo impact analysis.

```bash
curl -s "http://localhost:3000/api/repos/<REPO_ID>/deps" | jq
```

## Tool 5: Repo Management

**Register a new repo for indexing:**
```bash
curl -s -X POST http://localhost:3000/api/repos \
  -H "Content-Type: application/json" \
  -d '{"url":"<GIT_URL>","name":"<DISPLAY_NAME>","branch":"<BRANCH>","auth_type":"https"}' | jq
```

**Check repo status:**
```bash
curl -s http://localhost:3000/api/repos/<REPO_ID> | jq '{id, status, last_indexed}'
```

**Trigger re-index:**
```bash
curl -s -X POST http://localhost:3000/api/repos/<REPO_ID>/sync | jq
```

**Delete a repo:**
```bash
curl -s -X DELETE http://localhost:3000/api/repos/<REPO_ID> | jq
```

**Health check:**
```bash
curl -s http://localhost:3000/api/health | jq
```

---

## Workflow Protocol

1. **Start with `/search`** when asked to find a feature or concept.
2. **Use `/callers`** before modifying any function to understand its impact.
3. **Use `/explore`** to get file structure, then read only the specific lines
   you need instead of whole files.
4. **Use `/deps`** to understand cross-repo relationships before breaking changes.
5. **Always pipe through `jq`** to extract only relevant fields — never dump raw
   JSON into the context.
6. **If a repo is not indexed**, register it, wait for `"status": "indexed"`,
   then query it.
