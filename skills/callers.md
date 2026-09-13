# Knot-Server Callers: Reverse Dependency Lookup

**Endpoint:** `GET /api/repos/{id}/callers?entity=...`
**Cross-repo endpoint:** `GET /api/callers?entity=...&repo=...`

> **MCP equivalent:** if this agent is connected to a knot-server `/mcp`
> endpoint, prefer the `find_callers` tool — same engine, structured result, no
> `curl`. The command below is the fallback and the only option when the agent
> has no MCP support.

## Step 0: Preflight

Before running this, you **must** run the `[[preflight]]` check to ensure the
server is running and the target repository's status is `indexed`. If the repo
is not indexed, stop and inform the user.

## Purpose

Find all places where a specific entity is used, referenced, extended, or
implemented. This answers critical questions like:
- "Who uses this class/method?"
- "What will break if I change this?"
- "Is this code dead?"
- "How many places depend on this?"

`knot-server` answers this instantly by traversing the Neo4j graph database.

## Request

```bash
# Retrieve the repository ID from [[preflight]] (e.g. "billing-service")
REPO_ID="billing-service"

# Name of the entity (URL-encoded). See CRITICAL rule below for common names.
ENTITY="PaymentProcessor"

curl -fsS -G \
  --data-urlencode "entity=${ENTITY}" \
  "${KNOT_SERVER_URL:-http://localhost:3000}/api/repos/${REPO_ID}/callers" \
  | jq '.calls[] | {name, kind, file_path, start_line, target_name}'
```

### Parameters

- **`id`** (path): The repository ID.
- **`entity`** (query, required): Name of the entity to find callers for.
  - Can be a class name, interface, function, or method name.
  - Supports partial names and signature fragments.
  - Examples: "AuthService", "handleRequest", "processPayment".
- **`max_targets`** (query, optional): Maximum number of resolved targets to
  include (default 25, max 500). Raise it when `resolution.truncated` is true
  and you need the complete impact set.

## Output Format

The endpoint returns a JSON object grouped by relationship type:

```json
{
  "calls": [
    {
      "uuid": "...",
      "name": "processCheckout",
      "kind": "function",
      "file_path": "src/controllers/checkout.ts",
      "start_line": 42,
      "signature": "async processCheckout(req: Request)",
      "target_name": "PaymentProcessor.charge",
      "target_file_path": "src/services/payment.ts"
    }
  ],
  "extends": [],
  "implements": [
    {
      "uuid": "...",
      "name": "StripeProvider",
      "kind": "class",
      "file_path": "src/providers/stripe.ts",
      "start_line": 15,
      "target_name": "PaymentProcessor"
    }
  ],
  "references": [],
  "resolution": {
    "query": "PaymentProcessor",
    "tier": "exact_name",
    "fuzzy": false,
    "truncated": false,
    "total_targets": 1,
    "targets": [
      {
        "name": "PaymentProcessor",
        "fqn": "services.PaymentProcessor",
        "kind": "class",
        "file_path": "src/services/payment.ts",
        "start_line": 1
      }
    ]
  }
}
```

### Groups Explained

- **`calls`**: Function/method invocations. Where this entity is directly called.
- **`extends`**: Class inheritance. Classes that inherit from this class.
- **`implements`**: Interface implementation. Classes that implement this interface.
- **`references`**: Type usage. Where this entity is used in annotations, signatures, or type declarations.

### Resolution & truncation

`resolution` describes how the queried name was resolved and whether the answer
is complete:

- **`total_targets`** — the TRUE pre-truncation number of entities the name
  resolved to. It is *not* the number of returned rows nor of shown targets.
- **`truncated`** — when `true`, `resolution.targets[]` (and therefore every
  relationship bucket above) covers only a sample of the full impact set.
- **`tier`** / **`fuzzy`** — the match tier that won and whether it was a fuzzy
  substring match.
- **`targets[]`** — the resolved entities actually shown.

When `truncated` is `true`, re-run with a larger `max_targets` (up to 500) or a
more qualified name to move toward the complete set.

## ⚠️ CRITICAL: Avoiding Noisy Results with Common Method Names

This is the **most important rule** for using the callers endpoint effectively.

### The Problem

Methods like `accept`, `process`, `handle`, `get`, `run`, `execute`, `apply`,
`find`, `create`, `set`, `parse`, and `transform` exist in nearly every
codebase with different purposes. Searching by the bare name returns **thousands
of irrelevant results**.

### The Solution: Use Signature Fragments

**Always include the opening parenthesis `(` and at least part of the first
parameter type** when searching for common method names.

### ❌ Bad Examples (Bare Names - Produces Noise)

```bash
ENTITY="accept"  # Returns EVERY accept() method
ENTITY="process" # Returns EVERY process() method in the codebase
ENTITY="handle"  # Returns EVERY handle() method
ENTITY="get"     # Returns EVERY get() method (thousands of results)
```

### ✅ Good Examples (With Signature Fragments - Targeted Results)

```bash
# By parameter type
ENTITY="accept(List<Document"  # Only the specific accept() you care about
ENTITY="findById(String"       # Specific findById variant
ENTITY="process(Event"         # Process that takes an Event
ENTITY="handle(Request"        # Handle that takes a Request

# With multiple parameter hints
ENTITY="transform(List,String" # Even more specific
ENTITY="create(User,boolean"   # Clear which overload you want

# By return type (if known)
ENTITY="get()LookupService"    # Get that returns LookupService
```

### Why This Works

The graph query looks for entities where the `signature` field contains your
fragment string. Even a partial match is far more specific than just the method
name. A method `accept(List<Document>)` is very different from `accept(Socket)`.

## When to Use Callers

### 1. Impact Analysis Before Refactoring
Before changing `PaymentProcessor`, find all dependents. Before modifying a
`validate()` method, find all callers.

### 2. Dead Code Detection
If `GET /api/repos/{id}/callers?entity=legacyFunction` returns empty arrays
for all groups, it is likely dead code. Confirm by exploring the file where
it's defined.

### 3. Cross-Repository Tracing
If you query `callers` on a shared library entity (e.g. `TokenVerifier`) inside
a repository that depends on it (e.g. `my-app` depends on `auth-lib`), the
graph will correctly return calls made from `my-app` into `auth-lib`. Use the
[[deps]] skill to discover cross-repo links.

## Cross-repo callers

Use `GET /api/callers` for impact analysis that **spans repositories** — e.g.
"who calls `SharedUtil.work` anywhere in the indexed ecosystem?". Same scope
syntax as cross-repo search: omit `repo` (or `all` / `*`) for every registered
repository, one id, or a comma-separated list.

```bash
curl -fsS -G \
  --data-urlencode "entity=SharedUtil.work" \
  --data-urlencode "repo=all" \
  --data-urlencode "max_targets=25" \
  "${KNOT_SERVER_URL:-http://localhost:3000}/api/callers" \
  | jq '.calls[] | {name, repo_name, target_repo_name, file_path}'
```

### Reading repo attribution

- Every row carries `repo_name` (the **referencing** entity's repository) and
  `target_repo_name` (the **referenced** entity's repository). A genuine
  cross-repo reference is exactly the row where the two differ.
- `resolution.targets[]` carries `repo_name` too, so you can see which
  repositories the query name resolved against.

### Caveats

- **Read `resolution.truncated` and `resolution.total_targets`.** The response
  always reports the true pre-truncation target count in
  `resolution.total_targets`; when `resolution.truncated` is `true` the
  relationship buckets cover only `resolution.targets[]` — a *sample*, not the
  full impact set. Under `repo=all` a common name resolves against every
  registered repository, so the default 25-target cap fills faster.
- **Raise `max_targets` (up to 500)** when truncated and you need the complete
  set, or pass a qualified name (`Namespace.Type.Member`) / narrow the scope.
- `repo=all` — and an omitted `repo` — are confined to the registry: rows from
  unregistered (deleted) repositories are never returned, and an empty registry
  yields empty buckets with `200` without querying.
- There is no `max_results` on this route; `max_targets` is the cap, and REST
  and the MCP `find_callers` tool honour it identically (they share knot's
  engine, so totals agree).
- The match ladder is unchanged by scope (exact FQN → FQN suffix → exact name →
  signature prefix → fuzzy): a wide scope does not loosen matching, it only
  widens the candidate set.

## Interpreting Results

- **High Reference Count (20+):** Critical, widely-used entity. Change carefully; test extensively.
- **Medium Reference Count (5-20):** Important entity. Review all callers before making breaking changes.
- **Low Reference Count (1-4):** Specialized or newly added. Easier to modify safely.
- **Zero References:** Dead code, newly created API, or statically referenced via reflection/strings.

## Connection-Error Footnote

⚠️ **If the call returns connection refused / timeout / network error, stop
and ask the user:**
> *"knot-server is not responding at `${KNOT_SERVER_URL:-http://localhost:3000}`.
> What port is it running on? (default 3000, env `KNOT_SERVER_PORT`,
> CLI flag `--port`)."*

Then re-export `KNOT_SERVER_URL` and retry.
