# knot-server

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-2024-brightgreen.svg)](https://www.rust-lang.org)

**knot-server** is a distributed REST API and background task scheduler for managing and indexing Git repositories across a cluster. It sits on top of the core [knot](https://github.com/raultov/knot) indexing engine, transforming it from a single-machine CLI tool into a highly available, cluster-aware enterprise service.

See [CHANGELOG.md](CHANGELOG.md) for the full version history.

**knot-server** and the [knot](https://github.com/raultov/knot) indexing engine are in **beta**. Expect rough edges, occasional breaking changes, and indexing quirks as we approach a stable 1.0 release.

With `knot-server`, you can register Git repositories via a REST API, trigger automatic codebase indexing through webhooks (GitHub, GitLab, Bitbucket), and query the vector (Qdrant) and graph (Neo4j) databases—all while coordinating work safely across multiple server instances via NFS/EFS workspace locks.

---

## ✨ Key Features & API Endpoints

**knot-server** provides a comprehensive REST API to manage the lifecycle of your codebases.

### 📦 Repository Management
- **`POST /api/repos`**: Register a new Git repository. Accepts a JSON body with a URL, name, and optional authentication.
  ```json
  {
    "url": "https://github.com/raultov/knot.git",
    "name": "knot-core",
    "branch": "master",
    "webhook_secret": "your-secret-token",
    "auth": { "type": "none" }
  }
  ```
  | Field | Required | Description |
  |-------|----------|-------------|
  | `url` | Yes | Git repository URL (HTTPS, SSH, or local path) |
  | `name` | No | Display name (auto-derived from URL if omitted) |
  | `branch` | No | Branch to clone (defaults to `"main"`) |
  | `webhook_secret` | No | Shared secret for validating webhook signatures (HMAC-SHA256 or token). **Required to use the `/api/webhook` endpoint.** |
  | `auth` | No | Authentication method: `{"type": "ssh"}`, `{"type": "https", "token": "..."}`, or `{"type": "none"}` (default: `{"type": "ssh"}`) |
- **`GET /api/repos`**: List all registered repositories, along with their current status (`pending`, `cloning`, `pulling`, `indexing`, `indexed`, `error`) and last indexed timestamp.
- **`GET /api/repos/:id`**: Retrieve detailed information about a specific repository.
- **`DELETE /api/repos/:id`**: Remove a repository from the registry and delete its local workspace. (No request body required).

### 🔄 Indexing & Webhooks
- **`POST /api/repos/:id/sync`**: Manually trigger an asynchronous sync and re-indexing job for a repository. (No request body required).
- **`POST /api/webhook/:id`**: Endpoint for Git provider webhooks (GitHub, GitLab, Bitbucket). Securely validates payload signatures (HMAC-SHA256) or tokens, triggering a fast, incremental background re-index on push events. The request body should be the standard JSON webhook payload sent by the Git provider.

### 🔍 Code Intelligence Search
- **`GET /api/repos/:id/search?q=...`**: Semantic + structural search. Find code by meaning, class name, method signature, or docstrings.
- **`GET /api/repos/:id/callers?entity=...`**: Reverse dependency lookup. Identify callers, dead code, and perform impact analysis.
- **`GET /api/repos/:id/explore?path=...`**: File anatomy inspection. Quickly see all classes, interfaces, methods, and functions in a specific file.
- **`GET /api/repos/:id/deps`**: View repository dependencies (transitive and reverse) across the indexed ecosystem.

### 🧬 Graph Visualization (Web UI)
- **`GET /graph`**: Interactive 3D codebase graph viewer. Open in your browser to visually explore entity relationships.
  - **Dynamic Filtering**: Real-time toggles for relationship types (`Calls`, `Extends`, `Implements`, `Contains`, etc.) and entity kinds (`Classes`, `Interfaces`, `Functions`).
  - **Node Interaction**: 
    - **Click**: Automatically discover and expand neighbors.
    - **Focus on Entity**: Isolate a specific entity and its deep relationship subgraph.
    - **Back to Overview**: Return to the global entry-points view.
  - **High-Contrast Selection**: The currently selected node is highlighted in white for maximum visibility.
  - **Performance Optimized**: Default overview mode excludes noisy child relationships (`CONTAINS`), while focused mode uses physical hierarchy edges to maintain connectivity.
  - **Smart Tooltips**: Hover over nodes to see Fully Qualified Names (FQN), kind, file path, and line numbers.
  - **Contextual Search**: Find entities by FQN or name; results include package/module context.
- **`GET /api/repos/:id/graph?entity=...`**: Query the entity subgraph for a given repository root entity. Returns nodes and edges in JSON format for programmatic consumption.

  | Parameter | Type | Default | Description |
  |-----------|------|---------|-------------|
  | `entity` | String | *optional* | Name or FQN of the root entity. If omitted, returns a repository overview. |
  | `depth` | u32 | `2` | Traversal depth (1–5) |
  | `relationships` | CSV | `CALLS,EXTENDS,IMPLEMENTS` | Edge types to follow. |
  | `direction` | String | `both` | `outgoing`, `incoming`, or `both` |
  | `kinds` | CSV | `classes,interfaces` | Entity types to include. |

  **Note on Overview Mode:** When no `entity` is provided, the server identifies "entry points" (entities not contained by others) and traverses from them using the selected relationship types. Disconnected nodes are automatically pruned in focused views.

  **Response** (`200 OK`):
  ```json
  {
    "root_id": "abc123...",
    "nodes": [
      { "id": "...", "name": "handleRequest", "kind": "rust_function", "language": "rust", "file_path": "src/handler.rs", "start_line": 42, "signature": "fn handleRequest(req: Request) -> Response" }
    ],
    "edges": [
      { "source": "...", "target": "...", "type": "CALLS" }
    ],
    "truncated": false,
    "total_nodes_found": 15
  }
  ```

- **`GET /api/repos/:id/graph/expand?entity=...&exclude=...`**: Same as `/graph` but with `depth=1` fixed, plus an `exclude` parameter (CSV of UUIDs) to skip nodes the frontend already has. Used by the graph viewer when clicking on unexpanded nodes.

### 📖 Interactive API Documentation (Swagger UI)

- **`GET /docs`**: Interactive Swagger UI page where you can browse every endpoint, inspect request/response schemas, and execute live "Try it out" requests — no external tools needed.
- **`GET /api-docs/openapi.json`**: Raw OpenAPI 3.1 JSON spec for importing into Postman, Insomnia, or generating client SDKs.
- **Postman Collection**: We also include a `knot-server.postman_collection.json` file in the repository root to help you quickly test the API with Postman.

The spec is auto-generated at compile time via [`utoipa`](https://crates.io/crates/utoipa) and embedded directly in the binary — no external CDN or internet access required.

### ⚙️ Cluster & Health
- **`GET /api/health`**: Check the health of the server, including connections to Qdrant and Neo4j, and view repository statistics.
- **Distributed Locking**: File-based locking (`.knot.lock`) allows multiple `knot-server` instances to share a single NFS/EFS workspace, ensuring only one instance indexes a given repository at a time.
- **Background Scheduler**: Automatically detects and cleans up stale locks, and periodically re-indexes repositories that haven't been synced recently.

---

## 🤖 Using with AI Assistants (Cursor, Copilot, Claude, Gemini)

`knot-server` transforms any LLM with terminal access (Cursor, GitHub Copilot,
Claude Code, Gemini CLI, opencode, Cline, Aider) into a codebase-aware engineer.
By teaching the LLM to call the REST API via `curl`, you give it **semantic
understanding** of your entire codebase — far beyond what `grep` or file embeddings
can provide.

The AI learns **four code intelligence skills** that replace traditional text search:

| # | Skill | Endpoint | Use Case |
|---|-------|----------|----------|
| 1 | **Semantic Search** | `/search?q=` | Find code by *meaning*, not exact text |
| 2 | **Callers Analysis** | `/callers?entity=` | Impact analysis — who uses this function? |
| 3 | **File Exploration** | `/explore?path=` | Get a file's structure without reading it |
| 4 | **Dependency Graph** | `/deps` | Cross-repo dependencies |
| 5 | **Graph Visualization** | `/graph` | Interactive 3D entity relationship explorer |

These skills teach the LLM to **always prefer knot-server `curl` calls over
`grep`/`find`/`rg`** for code exploration, dramatically improving accuracy
and reducing hallucinations.

### Quick Install — One Command Per IDE

Download the pre-built skill instructions directly into your project:

**Cursor** (writes to `.cursorrules`):
```bash
curl -sL https://raw.githubusercontent.com/raultov/knot-server/master/skills/cursor-rules.md >> .cursorrules
```

**GitHub Copilot** (writes to `.github/copilot-instructions.md`):
```bash
mkdir -p .github && curl -sL https://raw.githubusercontent.com/raultov/knot-server/master/skills/copilot-instructions.md >> .github/copilot-instructions.md
```

**Claude Code / Gemini CLI / opencode / Cline / Aider** (generic system prompt):
```bash
curl -sL https://raw.githubusercontent.com/raultov/knot-server/master/skills/system-prompt.md > knot-skills.md
# Then instruct your agent: "Read knot-skills.md and use those tools"
```

### How It Works

Each skill file injects a **system prompt** into the LLM that defines:

- **When** to use each endpoint (trigger phrases)
- **How** to construct the `curl` command (parameters, `jq` filters)
- **How** to interpret the JSON response (field meanings)

The LLM learns to:
1. Instead of `grep "authenticate"`, call `GET /api/repos/{id}/search?q=authentication+logic`
2. Instead of searching for callers manually, call `GET /api/repos/{id}/callers?entity=handleRequest`
3. Instead of `cat src/file.rs`, call `GET /api/repos/{id}/explore?path=src/file.rs` to get the outline first
4. Before breaking a shared library, call `GET /api/repos/{id}/deps` to see the impact

### Example: AI-Assisted Code Exploration

```
User: "Where is the password hashing logic?"
AI (via knot-server):
  curl "/api/repos/myproject/search?q=password+hashing" | jq
  → Found `hash_password` in `src/auth/crypto.rs:142`
  → Reads only lines 142-180 instead of entire file
```

---

## 🛠️ Installation

### Prerequisites

| Component    | Version | Notes                              |
|--------------|---------|-----------------------------------|
| Docker       | 20.10+  | For running Qdrant and Neo4j      |
| qdrant       | 1.x     | Vector database (docker)          |
| neo4j        | 5.x     | Graph database (docker)           |

### 🐳 Official Docker Image

The official Docker image is available on Docker Hub:
**[`raultov/knot-server:latest`](https://hub.docker.com/r/raultov/knot-server)**

This image is lightweight (`debian:trixie-slim` based) and comes pre-packaged
with the `knot-server` binary, `git`, and SSH clients — everything needed to
clone and index repositories. It is the recommended way to deploy `knot-server`
in containerized environments (Docker, Docker Compose, or Kubernetes).

### Option A: Quick Install (curl)

A single command that auto-detects your OS and architecture — no `sudo` or
manual platform selection needed:

```bash
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/raultov/knot-server/releases/latest/download/knot-server-installer.sh | sh
```

For a specific version, replace `latest` with the version tag:
```bash
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/raultov/knot-server/releases/latest/download/knot-server-installer.sh | sh
```

### Option B: Docker Compose (Pre-built Image)

The easiest way to run `knot-server` with its dependencies. Just download the
`docker-compose.yml` file and run:

```bash
curl -O https://raw.githubusercontent.com/raultov/knot-server/master/docker-compose.yml
curl -O https://raw.githubusercontent.com/raultov/knot-server/master/.env.example
curl -O https://raw.githubusercontent.com/raultov/knot-server/master/up.sh
curl -O https://raw.githubusercontent.com/raultov/knot-server/master/down.sh
chmod +x up.sh down.sh
cp .env.example .env          # edit .env to set KNOT_LOCAL_REPOS_DIR etc.
# Create the required empty placeholder directory (only needed once)
mkdir -p ~/.knot/empty
./up.sh -d
```

This pulls the pre-built [`raultov/knot-server`](https://hub.docker.com/r/raultov/knot-server)
image from Docker Hub along with Qdrant and Neo4j — no compilation needed.

The `up.sh` and `down.sh` scripts are convenience wrappers around `docker compose`.
See the [Convenience Scripts](#-convenience-scripts) section below for usage details.

#### SSH credentials for private repositories

The container copies your SSH keys at startup and fixes permissions automatically
(avoiding the `Bad owner or permissions` error that occurs with a direct bind-mount
into `/root/.ssh`).

By default it uses `~/.ssh`. Override with `KNOT_SSH_KEYS_DIR`:

```bash
# Use a specific key directory (e.g. corporate Bitbucket keys)
KNOT_SSH_KEYS_DIR=/path/to/your/ssh/keys docker compose up
```

##### Passphrase-protected keys (corporate / enterprise environments)

Copying SSH key files alone is not enough when keys are protected by a
passphrase — the `ssh-agent` running on the host must be forwarded into the
container. The `docker-compose.yml` does this automatically by mounting the
host socket:

```yaml
environment:
  - SSH_AUTH_SOCK=/ssh-agent
volumes:
  - ${SSH_AUTH_SOCK}:/ssh-agent:ro
```

Make sure your host `ssh-agent` is running and the key is loaded before
starting the stack (`ssh-add ~/.ssh/id_rsa`).

#### Indexing local repositories

To index a repository that lives on your host machine instead of a remote URL,
mount the parent directory into the container **at the same absolute path** so
that paths you pass to the API resolve transparently.

The easiest way is to set `KNOT_LOCAL_REPOS_DIR` in the `.env` file (copy
`.env.example` as a starting point):

```bash
# .env
KNOT_LOCAL_REPOS_DIR=/home/raultov/workspace
```

Then just run `docker compose up`. Alternatively, prefix the variable on the
command line:

```bash
KNOT_LOCAL_REPOS_DIR=/home/raultov/workspace docker compose up
```

Then register the repo with its local path:

```bash
curl -X POST http://localhost:3000/api/repos \
  -H "Content-Type: application/json" \
  -d '{
    "url": "/home/raultov/workspace/github/ui",
    "name": "ui",
    "branch": "master",
    "auth": { "type": "none" }
  }'
```

> **Note:** `KNOT_LOCAL_REPOS_DIR` is mounted **read-only**. The server will
> read the existing repo from that path — it skips `git clone` because `.git`
> already exists — and index it in place.

If you already have Neo4j and Qdrant running on your host machine (not in containers),
use `--network host` so the container can reach them via `localhost`:

```bash
docker run --network host \
  -v ${HOME}/.ssh:/tmp/ssh_keys:ro \
  raultov/knot-server:latest
```

> **Note:** The `raultov/knot-server` image does **not** include Neo4j or Qdrant.
> Running `docker run` without `--network host` and without pointing to external
> databases will fail — the container defaults to `localhost` which refers to
> itself, not your host.

### Option C: Build from Source

Clone the repository and build the binary:

```bash
git clone https://github.com/raultov/knot-server
cd knot-server
cargo build --release
```

#### Running with Docker Compose from source

The repo includes `docker-compose.dev.yml`, a development overlay that adds
`build: .` on top of `docker-compose.yml`. Use it when you want docker compose
to build the image locally instead of pulling from DockerHub:

```bash
# Build the image from source
docker compose -f docker-compose.yml -f docker-compose.dev.yml build

# Start the full stack using the locally built image
docker compose -f docker-compose.yml -f docker-compose.dev.yml up

# Rebuild and start in one step
docker compose -f docker-compose.yml -f docker-compose.dev.yml up --build

# With local repos exposed (see "Indexing local repositories" above)
KNOT_LOCAL_REPOS_DIR=/home/user/workspace \
  docker compose -f docker-compose.yml -f docker-compose.dev.yml up
```

> **Tip:** You can add a shell alias to avoid repeating the `-f` flags:
> ```bash
> alias dc-dev='docker compose -f docker-compose.yml -f docker-compose.dev.yml'
> dc-dev up --build
> ```

###  Convenience Scripts

The repo includes `up.sh` and `down.sh` wrappers around `docker compose`.

**Start the stack:**
```bash
# Default (port 3000)
./up.sh

# Detached mode
./up.sh -d

# With custom port
KNOT_SERVER_PORT=6060 ./up.sh -d

# Rebuild from source (dev overlay)
KNOT_SERVER_PORT=6060 docker compose -f docker-compose.yml -f docker-compose.dev.yml up --build -d
```

**Stop the stack:**
```bash
# Stop containers, keep all data
./down.sh

# Stop and delete DB volumes (Qdrant, Neo4j, workspace)
./down.sh --clean

# Stop and delete only repos.json (keep DB volumes)
./down.sh --json

# Stop and delete EVERYTHING (volumes + repos.json)
./down.sh --all
```

| Flag | Effect |
|------|--------|
| *(none)* | Stop containers, preserve all data |
| `--clean` / `-c` | Also delete Docker volumes (Qdrant, Neo4j, workspace, fastembed cache) |
| `--json` / `-j` | Also delete `~/.knot/repos/repos.json` (repository registry) |
| `--all` / `-a` | Delete volumes AND repos.json |

---

## ⚙️ Configuration

`knot-server` is configured entirely via environment variables or CLI flags.

| Environment Variable | Default Value | Description |
|----------------------|---------------|-------------|
| `KNOT_SERVER_PORT` | `3000` | Port the REST API binds to |
| `KNOT_SERVER_BIND_ADDR` | `0.0.0.0` | Address the server binds to |
| `KNOT_WORKSPACE_DIR` | `/var/lib/knot/repos` | Directory where Git repos are cloned & locks are managed. Ensure the user running the server has write access (e.g., `export KNOT_WORKSPACE_DIR=$HOME/.knot/repos`). |
| `KNOT_SERVER_QDRANT_URL` | `http://localhost:6334` | URL to the Qdrant instance |
| `KNOT_SERVER_QDRANT_COLLECTION`| `knot_entities` | Qdrant collection name |
| `KNOT_SERVER_NEO4J_URI` | `bolt://localhost:7687` | URI to the Neo4j instance |
| `KNOT_SERVER_NEO4J_USER` | `neo4j` | Neo4j username |
| `KNOT_NEO4J_PASSWORD` | *(required)* | Neo4j password |
| `KNOT_SERVER_EMBED_DIM` | `384` | Embedding dimension (must match the model) |
| `KNOT_SERVER_RAYON_THREADS`| *(all cores)* | Number of threads for parallel source code parsing. Reduces CPU usage when set to a low value (e.g. `2`). |
| `KNOT_SERVER_BATCH_SIZE` | `64` | Number of code entities buffered in memory per indexing batch. Lower values reduce RAM usage. |
| `KNOT_SERVER_INGEST_CONCURRENCY` | `4` | Number of concurrent async tasks for embedding computation and database ingestion. Lower values reduce RAM and CPU usage. |
| `KNOT_SERVER_POLL_INTERVAL_SECS` | `86400` (24h) | How often the background scheduler runs |
| `KNOT_SERVER_MAX_INDEX_AGE_SECS` | `86400` (24h) | Age before a repository is automatically re-indexed |
| `KNOT_SERVER_QUEUE_CAPACITY` | `16` | Maximum number of jobs in the background indexing queue. Returns `429 Too Many Requests` when full. |
| `RUST_LOG` | `info` | Log level (`debug`, `info`, `warn`, `error`) |

> **Note:** When using Docker Compose, export `KNOT_SERVER_PORT` _before_ `docker compose up`
> so the port mapping in `docker-compose.yml` also changes (defaults to `3000:3000`).
> Example: `KNOT_SERVER_PORT=8080 docker compose up`

### Docker Compose Host Variables

These variables are consumed by `docker compose` itself (not by the server binary) to configure volume mounts.
Copy `.env.example` to `.env` and set the values there so you do not have to prefix them on every `docker compose up`.

| Variable | Default | Description |
|----------|---------|-------------|
| `KNOT_SSH_KEYS_DIR` | `~/.ssh` | Directory of SSH key *files* to make available inside the container. Keys are copied to `/root/.ssh` with correct ownership and permissions at startup. |
| `SSH_AUTH_SOCK` | *(host socket)* | Path to the host SSH agent socket. Forwarded into the container at `/ssh-agent` so passphrase-protected keys work without re-entering the passphrase. Requires the host `ssh-agent` to be running with the key already loaded (`ssh-add`). |
| `KNOT_LOCAL_REPOS_DIR` | `~/.knot/empty` | Host directory to mount at the same absolute path inside the container (read-only). Set this to the parent directory of any local repos you want to index by path. |

---

## 🎛️ Performance Tuning

`knot-server` is highly parallel by default, which can cause high CPU and memory
usage during indexing. Three environment variables control resource consumption:

| Variable | Controls | Default | Effect of lowering |
|----------|----------|---------|-------------------|
| `KNOT_SERVER_RAYON_THREADS` | CPU | all cores | Fewer parallel parsers → lower CPU, slightly slower |
| `KNOT_SERVER_BATCH_SIZE` | RAM | `64` | Fewer entities buffered in memory → lower RAM |
| `KNOT_SERVER_INGEST_CONCURRENCY` | RAM + CPU | `4` | Fewer concurrent embedding + DB writes → lower RAM and CPU |

### Preconfigured Profiles

| Profile | RAYON_THREADS | BATCH_SIZE | INGEST_CONCURRENCY | Expected RAM | Expected CPU |
|---------|---------------|------------|--------------------|--------------|--------------|
| **Kubernetes / Low memory** | `2` | `16` | `1` | < 1 GiB | ~200% |
| **Balanced** | `4` | `32` | `2` | ~2 GiB | ~400% |
| **Maximum throughput** (default) | all cores | `64` | `4` | ~5 GiB | all cores |

### Docker run with tuning

```bash
docker run --network host \
  -v ${HOME}/.ssh:/root/.ssh:ro \
  -e KNOT_SERVER_RAYON_THREADS=2 \
  -e KNOT_SERVER_BATCH_SIZE=16 \
  -e KNOT_SERVER_INGEST_CONCURRENCY=1 \
  raultov/knot-server:latest \
  --neo4j-password <your-password> \
  --workspace-dir /var/lib/knot/repos
```

### Docker Compose with tuning

```yaml
services:
  knot-server:
    image: raultov/knot-server:latest
    ports:
      - "3000:3000"
    environment:
      - KNOT_WORKSPACE_DIR=/var/lib/knot/repos
      - KNOT_SERVER_QDRANT_URL=http://qdrant:6334
      - KNOT_SERVER_NEO4J_URI=bolt://neo4j:7687
      - KNOT_SERVER_NEO4J_USER=neo4j
      - KNOT_NEO4J_PASSWORD=knotsecret
      - KNOT_SERVER_RAYON_THREADS=2
      - KNOT_SERVER_BATCH_SIZE=16
      - KNOT_SERVER_INGEST_CONCURRENCY=1
    volumes:
      - knot_workspace:/var/lib/knot/repos
    depends_on:
      qdrant:
        condition: service_started
      neo4j:
        condition: service_started
```

---

## 🔄 Example Workflow

Here is an end-to-end example of managing a repository with `knot-server` using `curl`:

**1. Start the server**
```bash
export KNOT_WORKSPACE_DIR=$HOME/.knot/repos
export KNOT_NEO4J_PASSWORD=mysecret
export KNOT_SERVER_QDRANT_URL=http://localhost:6334
export KNOT_SERVER_NEO4J_URI=bolt://localhost:7687
knot-server
```

**2. Register a repository**
```bash
curl -X POST http://localhost:3000/api/repos \
  -H "Content-Type: application/json" \
  -d '{
    "url": "https://github.com/raultov/knot.git",
    "name": "knot-core",
    "branch": "master",
    "webhook_secret": "my-webhook-secret"
  }'
```
*The server will instantly clone the repository and queue it for indexing.*

**3. Check indexing status**
```bash
curl http://localhost:3000/api/repos/knot-core
```
*Wait until `"status": "indexed"`.*

**4. Perform a semantic search**
```bash
curl "http://localhost:3000/api/repos/knot-core/search?q=webhook+validation"
```

**5. Trigger manual re-index (Sync)**
```bash
curl -X POST http://localhost:3000/api/repos/knot-core/sync
```

**6. Setup Git Webhooks**
In your GitHub/GitLab repository settings, add a webhook pointing to:
`http://your-server.com/api/webhook/knot-core`

Set the **secret/token** to the same value as `webhook_secret` you used when registering
the repository. Whenever a push occurs, `knot-server` will validate the signature and
automatically perform a fast incremental update.

**7. Browse the interactive API documentation**
Open `http://localhost:3000/docs` in your browser to explore all endpoints with Swagger UI. Use "Try it out" to test requests directly, or import `http://localhost:3000/api-docs/openapi.json` into Postman.

**8. Explore the codebase visually**
Open `http://localhost:3000/graph` in your browser. Select a repository from the
dropdown, search for an entity, and click nodes to expand their call/relationship graph
in 3D.

---

## 🚀 Cluster & High-Availability Deployment

`knot-server` is designed to run in horizontal scale-out clusters. Multiple instances
share a common workspace directory (NFS, EFS, or Kubernetes RWX PVC) and coordinate
via file-based locks — no distributed consensus protocol required.

### Docker Compose (Multi-Instance)

```yaml
services:
  knot-server:
    image: raultov/knot-server:latest
    environment:
      - KNOT_WORKSPACE_DIR=/var/lib/knot/repos
      - KNOT_SERVER_QDRANT_URL=http://qdrant:6334
      - KNOT_SERVER_NEO4J_URI=bolt://neo4j:7687
      - KNOT_SERVER_NEO4J_USER=neo4j
      - KNOT_NEO4J_PASSWORD=your-secure-password
      # Performance tuning (see Performance Tuning section)
      # - KNOT_SERVER_RAYON_THREADS=2
      # - KNOT_SERVER_BATCH_SIZE=16
      # - KNOT_SERVER_INGEST_CONCURRENCY=1
    volumes:
      - knot_shared_workspace:/var/lib/knot/repos
      - ~/.ssh:/root/.ssh:ro
    deploy:
      replicas: 3
    depends_on:
      - qdrant
      - neo4j

  qdrant:
    image: qdrant/qdrant:latest
    volumes:
      - qdrant_data:/qdrant/storage

  neo4j:
    image: neo4j:5
    environment:
      - NEO4J_AUTH=neo4j/your-secure-password
    volumes:
      - neo4j_data:/data

volumes:
  knot_shared_workspace:
    driver: local
  qdrant_data:
  neo4j_data:
```

### Kubernetes

You can deploy the official `raultov/knot-server:latest` image to Kubernetes
with a standard `Deployment`.

> **Reference:** The included [`docker-compose.yml`](docker-compose.yml) file is
> the canonical reference for configuring `knot-server`. It documents the exact
> environment variables, service dependencies (Qdrant + Neo4j), and volume
> mounts you need to translate into Kubernetes Deployments, Services, and
> ConfigMaps.

In Kubernetes, the key requirement for horizontal scaling is a
`PersistentVolumeClaim` with **`accessModes: [ReadWriteMany]`** (RWX). This
allows all knot-server Pods to share the workspace and coordinate safely.

```yaml
apiVersion: v1
kind: PersistentVolumeClaim
metadata:
  name: knot-shared-workspace
spec:
  accessModes:
    - ReadWriteMany
  resources:
    requests:
      storage: 50Gi
  # storageClassName: nfs-client  # or efs-sc, cephfs, etc.
---
apiVersion: apps/v1
kind: Deployment
metadata:
  name: knot-server
spec:
  replicas: 3
  selector:
    matchLabels:
      app: knot-server
  template:
    metadata:
      labels:
        app: knot-server
    spec:
      containers:
        - name: knot-server
          image: raultov/knot-server:latest
          ports:
            - containerPort: 3000
          env:
            - name: KNOT_WORKSPACE_DIR
              value: /var/lib/knot/repos
            - name: KNOT_SERVER_QDRANT_URL
              value: http://qdrant.default.svc.cluster.local:6334
            - name: KNOT_SERVER_NEO4J_URI
              value: bolt://neo4j.default.svc.cluster.local:7687
            - name: KNOT_SERVER_NEO4J_USER
              value: neo4j
            - name: KNOT_NEO4J_PASSWORD
              valueFrom:
                secretKeyRef:
                  name: knot-secrets
                  key: neo4j-password
            - name: KNOT_SERVER_RAYON_THREADS
              value: "2"
            - name: KNOT_SERVER_BATCH_SIZE
              value: "16"
            - name: KNOT_SERVER_INGEST_CONCURRENCY
              value: "1"
          resources:
            requests:
              memory: "512Mi"
              cpu: "500m"
            limits:
              memory: "1Gi"
              cpu: "2000m"
          volumeMounts:
            - name: shared-workspace
              mountPath: /var/lib/knot/repos
      volumes:
        - name: shared-workspace
          persistentVolumeClaim:
            claimName: knot-shared-workspace
```

Any Pod can receive webhook events or sync requests; the shared workspace
(`repos.json`, `.knot.lock` files) ensures exactly-once processing per repository.

---

## 🗺️ Roadmap

- Language support in the knot library: Java, Kotlin, JavaScript, and TypeScript have been refined and are polished for production use. The next refinement effort will target the remaining languages, with Rust being the next one.
- Implement language-based color coding in the `/graph` view to distinguish nodes by programming language.
- Resolve cross-file aliases for JavaScript and TypeScript (`require`, `import`): when a local alias shadows an imported entity, graph relationships should resolve to the original definition rather than the alias constant. Python alias resolution to follow. 
  See PR2 plan in the knot repository.
- After alias resolution: add a dedicated `TypeScriptModule` entity kind for synthetic `<module>` entities generated by re-export-only files, with its own color and filter toggle in `/graph`.
- Add a HELP section in the `/graph` viewer to assist users in understanding the graph visualization.

---

## 📜 License

This project is licensed under the **MIT License**. See [LICENSE](LICENSE) for details.
