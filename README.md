# knot-server

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-2024-brightgreen.svg)](https://www.rust-lang.org)

**knot-server** (v0.1.0) is a distributed REST API and background task scheduler for managing and indexing Git repositories across a cluster. It sits on top of the core [knot](https://github.com/raultov/knot) indexing engine, transforming it from a single-machine CLI tool into a highly available, cluster-aware enterprise service.

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
    "branch": "master", // Optional, defaults to "main"
    "auth": { "type": "none" } // Or {"type":"ssh", "key":"..."}, {"type":"https", "token":"..."}
  }
  ```
- **`GET /api/repos`**: List all registered repositories, along with their current status (`pending`, `indexing`, `idle`, `error`) and last indexed timestamp.
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

### ⚙️ Cluster & Health
- **`GET /api/health`**: Check the health of the server, including connections to Qdrant and Neo4j, and view repository statistics.
- **Distributed Locking**: File-based locking (`.knot.lock`) allows multiple `knot-server` instances to share a single NFS/EFS workspace, ensuring only one instance indexes a given repository at a time.
- **Background Scheduler**: Automatically detects and cleans up stale locks, and periodically re-indexes repositories that haven't been synced recently.

---

## 🛠️ Installation

### Prerequisites

| Component    | Version | Notes                              |
|--------------|---------|-----------------------------------|
| Docker       | 20.10+  | For running Qdrant and Neo4j      |
| qdrant       | 1.x     | Vector database (docker)          |
| neo4j        | 5.x     | Graph database (docker)           |

### Option A: Docker (Recommended)

You can run `knot-server` alongside its dependencies using Docker Compose.

```yaml
version: '3.8'

services:
  knot-server:
    image: raultov/knot-server:0.1.0
    ports:
      - "3000:3000"
    environment:
      - KNOT_WORKSPACE_DIR=/var/lib/knot/repos
      - KNOT_QDRANT_URL=http://qdrant:6334
      - KNOT_NEO4J_URI=bolt://neo4j:7687
      - KNOT_NEO4J_USER=neo4j
      - KNOT_NEO4J_PASSWORD=your-secure-password
    volumes:
      - knot_workspace:/var/lib/knot/repos
      - ~/.ssh:/root/.ssh:ro # Optional: for SSH git clone
    depends_on:
      - qdrant
      - neo4j

  # Add qdrant and neo4j services here...
```

### Option B: Build from Source

```bash
git clone https://github.com/raultov/knot-server
cd knot-server
cargo build --release
```

---

## ⚙️ Configuration

`knot-server` is configured entirely via environment variables or CLI flags.

| Environment Variable | Default Value | Description |
|----------------------|---------------|-------------|
| `KNOT_SERVER_PORT` | `3000` | Port the REST API binds to |
| `KNOT_SERVER_BIND_ADDR` | `0.0.0.0` | Address the server binds to |
| `KNOT_WORKSPACE_DIR` | `/var/lib/knot/repos` | Directory where Git repos are cloned & locks are managed. Ensure the user running the server has write access (e.g., `export KNOT_WORKSPACE_DIR=$HOME/.knot/repos`). |
| `KNOT_QDRANT_URL` | `http://localhost:6334` | URL to the Qdrant instance |
| `KNOT_QDRANT_COLLECTION`| `knot_entities` | Qdrant collection name |
| `KNOT_NEO4J_URI` | `bolt://localhost:7687` | URI to the Neo4j instance |
| `KNOT_NEO4J_USER` | `neo4j` | Neo4j username |
| `KNOT_NEO4J_PASSWORD` | *(required)* | Neo4j password |
| `KNOT_EMBED_DIM` | `384` | Embedding dimension (must match the model) |
| `KNOT_SERVER_RAYON_THREADS`| *(logical cores - 1)* | Number of threads for parallel parsing |
| `KNOT_SERVER_POLL_INTERVAL_SECS` | `86400` (24h) | How often the background scheduler runs |
| `KNOT_SERVER_MAX_INDEX_AGE_SECS` | `86400` (24h) | Age before a repository is automatically re-indexed |
| `RUST_LOG` | `info` | Log level (`debug`, `info`, `warn`, `error`) |

---

## 🔄 Example Workflow

Here is an end-to-end example of managing a repository with `knot-server` using `curl`:

**1. Start the server**
```bash
export KNOT_WORKSPACE_DIR=$HOME/.knot/repos
export KNOT_NEO4J_PASSWORD=mysecret
knot-server
```

**2. Register a repository**
```bash
curl -X POST http://localhost:3000/api/repos \
  -H "Content-Type: application/json" \
  -d '{
    "url": "https://github.com/raultov/knot.git",
    "name": "knot-core",
    "branch": "master"
  }'
```
*The server will instantly clone the repository and queue it for indexing.*

**3. Check indexing status**
```bash
curl http://localhost:3000/api/repos/knot-core
```
*Wait until `"status": "idle"`.*

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
Whenever a push occurs, `knot-server` will automatically perform a fast incremental update.

---

## 📜 License

This project is licensed under the **MIT License**. See [LICENSE](LICENSE) for details.
