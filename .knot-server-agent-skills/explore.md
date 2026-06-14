# Knot-Server Explore: File Anatomy Discovery

**Endpoint:** `GET /api/repos/{id}/explore?path=...`

## Step 0: Preflight

Before running this, you **must** run the `[[preflight]]` check to ensure the
server is running and the target repository's status is `indexed`. If the repo
is not indexed, stop and inform the user.

## Purpose

List all classes, methods, functions, and properties in a source file. Quickly
understand a file's structure without reading the entire source code and burning
through your context window.

## Request

```bash
# Retrieve the repository ID from [[preflight]] (e.g. "api")
REPO_ID="api"

# Relative file path within the repository
FILE_PATH="src/controllers/user.controller.ts"

curl -fsS -G \
  --data-urlencode "path=${FILE_PATH}" \
  "${KNOT_SERVER_URL:-http://localhost:3000}/api/repos/${REPO_ID}/explore" \
  | jq
```

### Parameters

- **`id`** (path): The repository ID.
- **`path`** (query, required): Relative file path within the repository.
  - Examples:
    - `src/main/java/com/app/AuthHandler.java` (Java)
    - `src/services/user.ts` (TypeScript)
    - `src/controllers/payment.kt` (Kotlin)

## Output Format

The endpoint returns a JSON object where keys are category names (like
"Classes", "Interfaces", "Methods", "Properties") and values are arrays of
entities.

```json
{
  "Classes": [
    {
      "name": "LoginHandler",
      "start_line": 10,
      "signature": "export class LoginHandler",
      "decorators": ["@Controller", "@UseGuards"],
      "docstring": "Handles authentication endpoints"
    }
  ],
  "Methods": [
    {
      "name": "login",
      "start_line": 15,
      "signature": "async login(email: string, password: string): Promise<Token>",
      "decorators": ["@Post('/login')"],
      "docstring": "Authenticates user with credentials"
    }
  ],
  "Interfaces": [],
  "Properties": []
}
```

### Entity Fields Explained

- **`name`**: The entity identifier.
- **`start_line`**: Where it's defined (use this to jump directly to the code with `read`).
- **`signature`**: Method/function parameters and return type. Reveals the contract.
- **`decorators`**: Important metadata like `@Component`, `@Override`, `@Post`.
- **`docstring`**: The first line of the comment block above the entity.

## When to Use Explore

### 1. Navigating New Codebases
When joining a project, use explore to quickly map out key files without
reading them line-by-line.

### 2. Before Deep Code Reading
Before diving into a complex file (e.g. `src/utils/complex-algorithm.ts`),
get the structure first, then use your `read` tool to pull only the specific
line ranges (`offset` and `limit`) that matter to your task.

### 3. Understanding Class Hierarchies
For classes with inheritance, the `signature` field often reveals the
`extends` or `implements` clauses.

## Output Interpretation

### Signatures Reveal Contracts

A signature tells you:
- What parameters the entity expects
- What it returns
- Whether it's async (e.g. returns a `Promise` or `Future`)
- Whether it's generic

### Line Numbers for Targeted Reading

Every entity includes a `start_line`. Use it to fetch exactly what you need.
If the explore output shows:
```json
{ "name": "validateToken", "start_line": 150 }
```
You can use your standard file `read` tool starting at line 145 to see the
implementation, bypassing the 149 lines of imports and unrelated code above it.

## Troubleshooting

### "Repository 'X' not found" (HTTP 404)

**Cause:** The repo ID is wrong.
**Solution:** Verify the repository ID via [[repos]] or the [[preflight]] list.

### Empty JSON `{}`

**Cause:** File path is incorrect, file doesn't exist, or file doesn't contain
code entities (e.g. a JSON config file).
**Solutions:**
- Verify the file path is correct relative to the repository root.
- Ensure it's a supported source file (Java, TS/JS, Kotlin, Rust, Python, C/C++, etc).
- Double check capitalization.

## Connection-Error Footnote

⚠️ **If the call returns connection refused / timeout / network error, stop
and ask the user:**
> *"knot-server no responde en `${KNOT_SERVER_URL:-http://localhost:3000}`.
> ¿En qué puerto está corriendo? (default 3000, env `KNOT_SERVER_PORT`,
> CLI flag `--port`)."*

Then re-export `KNOT_SERVER_URL` and retry.
