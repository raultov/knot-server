# Knot-Server Search: Semantic Code Discovery

**Endpoint:** `GET /api/repos/{id}/search?q=...&max_results=...`

## Step 0: Preflight

Before running this, you **must** run the `[[preflight]]` check to ensure the
server is running and the target repository's status is `indexed`. If the repo
is not indexed, stop and inform the user.

## Purpose

Find code entities by semantic meaning. This is your primary tool for
exploratory searches when you don't know exact names or locations.
`knot-server` uses vector embeddings (Qdrant) to match natural language
queries against code, docstrings, and signatures.

## Request

```bash
# Retrieve the repository ID from [[preflight]] (e.g. "my-app")
REPO_ID="my-app"

# Natural language query (URL-encoded)
QUERY="user authentication"

curl -fsS -G \
  --data-urlencode "q=${QUERY}" \
  --data-urlencode "max_results=5" \
  "${KNOT_SERVER_URL:-http://localhost:3000}/api/repos/${REPO_ID}/search" \
  | jq '.[0:5] | .[] | {name, kind, file_path, start_line, signature}'
```

### Parameters

- **`id`** (path): The repository ID.
- **`q`** (query, required): Natural language description of what you're looking for.
  - Examples: "user authentication", "error handling", "database connection"
  - Good queries describe *what the code does*, not specific names.
  - Works best with 2-5 word descriptions.
- **`max_results`** (query, optional, default: 5): Limit the number of results.
  - Use higher values (10-20) when exploring unfamiliar codebases.
  - Use lower values (3-5) for focused results.

## Output Format

The endpoint returns a JSON array of `Entity` objects.

```json
[
  {
    "uuid": "88cd6ec3-441f-4903-b09e-f00de21f57fc",
    "name": "authenticateUser",
    "kind": "function",
    "language": "typescript",
    "file_path": "src/auth/auth.ts",
    "start_line": 42,
    "end_line": 60,
    "signature": "async authenticateUser(email: string, password: string): Promise<User>",
    "docstring": "Authenticates a user with email and password using bcrypt",
    "dependencies": ["bcrypt", "User", "Database"],
    "score": 0.892
  }
]
```

**Tip:** Always pipe through `jq` to extract `name`, `kind`, `file_path`,
`start_line`, and `signature`. Do not dump the raw JSON array into the context
window — it includes the full `content` field which wastes tokens.

## When to Use Search

- **Feature Discovery:** Finding code that handles a specific feature.
- **Pattern Location:** Searching for architectural patterns (e.g., "caching strategy").
- **Code Exploration:** When you don't know exact class/function names.
- **Cross-Language Analysis:** Finding similar functionality across polyglot repos.
- **Refactoring Discovery:** Locating all implementations of a pattern before refactoring.

## Query Tips for Better Results

### ✅ Good Semantic Queries

```bash
# Specific and descriptive
QUERY="user login validation"
# Describes the pattern
QUERY="database connection pooling"
# Clear functionality
QUERY="JWT token refresh"
# Specific responsibility
QUERY="error logging middleware"
```

### ❌ Poor Queries (Too Vague)

```bash
QUERY="user"           # Too generic, will return everything user-related
QUERY="authentication" # Too broad
QUERY="get"            # Way too vague
```

### ❌ Poor Queries (Too Specific/Exact Names)

```bash
QUERY="UserAuthenticationService" # Use semantic search, not exact names
QUERY="authenticate"              # Single word too vague for semantic search
```

## Workflow: Feature Discovery Pattern

### Step 1: Initial Semantic Search
Query `q=user login flow&max_results=10`.

### Step 2: Review Results
Look for files and functions related to login. Note the file paths (`file_path`)
and entity names (`name`).

### Step 3: Explore Identified Files
Once you find promising results, explore their structure using the [[explore]] skill:
`GET /api/repos/{id}/explore?path=src/auth/login.ts`

### Step 4: Find Related Code (Optional)
If you identified a key entity, find who uses it using the [[callers]] skill:
`GET /api/repos/{id}/callers?entity=loginUser`

## Troubleshooting

### Empty Array `[]`

**Cause:** Query is too specific or doesn't match code terminology.
**Solutions:**
- Try broader semantic query: "authentication" instead of "OAuth2 JWT bearer token validation".
- Use simpler language: "error handling" instead of "exception management strategy".
- Try different keywords: "login" instead of "authentication".

### Results don't match the codebase

**Cause:** Index may be stale.
**Solutions:**
- Check when the repo was last indexed with `GET /api/repos/{id}` (via [[repos]]).
- Trigger a sync: `POST /api/repos/{id}/sync`.

## Connection-Error Footnote

⚠️ **If the call returns connection refused / timeout / network error, stop
and ask the user:**
> *"knot-server no responde en `${KNOT_SERVER_URL:-http://localhost:3000}`.
> ¿En qué puerto está corriendo? (default 3000, env `KNOT_SERVER_PORT`,
> CLI flag `--port`)."*

Then re-export `KNOT_SERVER_URL` and retry.
