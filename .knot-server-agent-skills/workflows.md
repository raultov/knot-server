# Knot-Server Workflows: Patterns and Best Practices

This guide shows common multi-step workflows for using the knot-server API effectively.

## Step 0: Preflight

Every workflow here **must** start with the `[[preflight]]` check to ensure
the server is running and the target repository is `indexed`. If it is not,
stop and inform the user.

## Pattern 1: Feature Discovery & Onboarding

**Goal:** Understand how a feature is implemented in a new or complex codebase.

1. **Semantic Search ([[search]]):**
   `GET /api/repos/{id}/search?q=user+login+flow&max_results=10`
2. **Review:** Look at the `file_path` and `name` of the top results.
3. **Explore Files ([[explore]]):**
   `GET /api/repos/{id}/explore?path=src/auth/login.ts`
   Get the big-picture structure of the most relevant files.
4. **Find Dependencies ([[callers]]):**
   `GET /api/repos/{id}/callers?entity=loginUser`
   See who calls the key method to trace the execution path backwards.
5. **Read Code (`read` tool):**
   Use the `start_line` values from Explore and Callers to read only the relevant
   chunks of the source files.

## Pattern 2: Impact Analysis Before Refactoring

**Goal:** Understand what will break if you change an entity, plan refactoring safely.

1. **Find the Entity ([[search]]):**
   `GET /api/repos/{id}/search?q=payment+processor`
2. **Find All Dependents ([[callers]]):**
   `GET /api/repos/{id}/callers?entity=PaymentProcessor`
3. **Understand Context ([[explore]]):**
   For each caller file returned, explore its structure.
4. **Cross-Repo Impact ([[deps]]):**
   If this is a shared library, check if other repos depend on it:
   `GET /api/repos/{id}/deps?reverse=true`
   If so, run the `callers` query again against those consumer repo IDs.
5. **Refactor & Verify:**
   After refactoring, re-run the `callers` query to ensure no missed updates.

## Pattern 3: Dead Code Detection

**Goal:** Identify and safely remove unused code.

1. **Find Callers ([[callers]]):**
   `GET /api/repos/{id}/callers?entity=legacyFunction`
2. **Interpret Results:**
   - If the JSON arrays (`calls`, `extends`, `implements`, `references`) are all
     empty `[]` → likely dead code.
3. **Verify Existence ([[explore]]):**
   Confirm the file exists and the function is where you think it is.
4. **Remove & Test:**
   Remove the code and run the project's test suite to be completely sure.

## Pattern 4: Cross-Language / Microservice Tracing

**Goal:** Track how code flows across multiple services.

1. **Find Backend Service ([[search]]):**
   `GET /api/repos/backend/search?q=user+repository`
2. **Find API Endpoints ([[callers]]):**
   `GET /api/repos/backend/callers?entity=UserRepository`
3. **Trace to Frontend ([[deps]] & [[callers]]):**
   Check if the `frontend` repo depends on the API schema. Query callers in the
   frontend repo to see where the API endpoints are consumed.

## Pattern 5: Cross-Repo Discovery

**Goal:** Locate a concept anywhere in the indexed ecosystem, then assess its
impact across repositories.

1. **Locate across repos ([[search]]):**
   `GET /api/search?q=user+authentication&max_results=10` — every hit carries
   `repo_name`, so you learn *which* repositories own the concept. Omitting
   `repo` (or `all` / `*`) means every **registered** repository: deleted repos
   no longer leak rows, and an empty registry returns an empty result.
2. **Assess cross-repo impact ([[callers]]):**
   `GET /api/callers?entity=AuthService&repo=all` — read `repo_name` vs
   `target_repo_name` per row to spot genuine cross-repo references, and check
   `resolution.truncated` before trusting the answer. The scope is confined to
   registered repositories, so rows from deleted repos cannot appear.
3. **Narrow down ([[search]] per-repo):** Once the owning repos are known,
   re-run per-repo queries for token-efficient detail.
4. **Check repo links ([[deps]]):** Confirm whether the callers' repos depend
   on the callee's repo or resolve against homonyms.

## Workflow Rules for LLM Agents

1. **Never guess file structures.** If you need to know what's in a file, use `[[explore]]`.
2. **Never guess dependents.** Before you delete or rename a function, use `[[callers]]`.
3. **Never guess where a concept is.** If you need to find "error handling", use `[[search]]` instead of regex grep.
4. **Be token-efficient.** Pipe knot-server JSON through `jq` to extract only
   the fields you need (`name`, `file_path`, `start_line`, `signature`). Don't
   dump massive AST payloads into the context window.

## Connection-Error Footnote

⚠️ **If any call returns connection refused / timeout / network error, stop
and ask the user:**
> *"knot-server is not responding at `${KNOT_SERVER_URL:-http://localhost:3000}`.
> What port is it running on? (default 3000, env `KNOT_SERVER_PORT`,
> CLI flag `--port`)."*

Then re-export `KNOT_SERVER_URL` and retry.
