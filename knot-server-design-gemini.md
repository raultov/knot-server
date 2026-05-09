# knot-server — Architectural Design

A distributed REST API server acting as a controller for `knot-indexer`, managing a cluster of cloned repositories with secure authentication and disk-based shared-state persistence.

---

## 1. Cluster Architecture (Shared Storage & Locking)

All server nodes operate on a shared filesystem volume (NFS / EFS / GlusterFS) mounted at
`/var/lib/knot/repos` (configurable via `KNOT_WORKSPACE_DIR` env var).

### Global State (`repos.json`)

A single JSON file at the root of the workspace serves as the master registry:

```json
{
  "repositories": [
    {
      "id": "my-backend-repo",
      "url": "git@github.com:company/backend.git",
      "auth_type": "ssh",
      "local_path": "/var/lib/knot/repos/my-backend-repo",
      "last_indexed": "2026-05-08T10:00:00Z"
    }
  ]
}
```

### Job Coordination (File Locks)

To enforce the rule **"one repository at a time per server"** and avoid collisions across the cluster:

- Each server maintains a **local Job Queue** (e.g., a Tokio `mpsc` channel) with concurrency capped at `1` (sequential execution).
- Before processing a repository (triggered by Webhook or Polling), a server attempts to atomically create a `.knot.lock` file inside that repository's directory (using `fs2` or OS-level exclusive file-lock syscalls).
- If the lock cannot be acquired, another cluster node is already indexing it — the job is silently discarded on this node.
- If the lock is acquired, the server runs `git pull`, then invokes the `knot` core library, and releases the lock on completion.

---

## 2. Enterprise Git Authentication

The server supports both required methods via secure credential injection:

### SSH (preferred for server-to-server secure connections)

- The process assumes the host system has SSH keys configured (`~/.ssh/id_rsa` or `~/.ssh/config`).
- The server spawns `git` as a child process and lets the system's Git client handle SSH negotiation natively.
- No credential management needed inside knot-server.

### HTTPS + Personal Access Token (PAT)

- The REST API receives the PAT at repository registration time.
- The server injects the token temporarily and securely using Git's credential helper mechanism (`GIT_ASKPASS` or a temporary credential store), avoiding writing tokens as plaintext in the cloned repository's `.git/config`.

---

## 3. REST API Endpoints (Axum)

A load balancer distributes traffic across cluster nodes. All endpoints return JSON.

### Repository Management (Write)

| Method   | Path                     | Description |
|----------|--------------------------|-------------|
| `POST`   | `/api/repos`             | Register a new repository. Writes to `repos.json` (with a write lock), then immediately enqueues `git clone` + first indexing. Returns `202 Accepted`. |
| `DELETE` | `/api/repos/:id`         | Remove a repository from knot management. Deletes the local directory (requires lock). |

### Triggers (Write)

| Method   | Path                     | Description |
|----------|--------------------------|-------------|
| `POST`   | `/api/webhook/:id`       | Receives GitLab/Bitbucket webhook payload. Attempts to acquire `.knot.lock`; if successful, enqueues `git pull` + incremental `knot index`. |
| `POST`   | `/api/repos/:id/sync`    | Manual endpoint to force a polling sync. Enqueues a `git pull` + incremental index job. |

### Search & Exploration (Read — Stateless)

| Method | Path                            | Description |
|--------|----------------------------------|-------------|
| `GET`  | `/api/search?q=query&repo=id`   | Calls `knot::mcp_tools::search_hybrid_context`. Uses the workspace registry to resolve the repo path. |
| `GET`  | `/api/callers?entity=name&repo=id` | Calls `knot::mcp_tools::find_callers`. |
| `GET`  | `/api/explore?path=file_path&repo=id` | Calls `knot::mcp_tools::explore_file`. |

---

## 4. Hybrid Worker Implementation (Rust)

Each node runs an internal worker that processes jobs sequentially.

```rust
async fn process_repository(repo: Repository, cfg: AppConfig) {
    // 1. Attempt to acquire file lock
    let lock_path = PathBuf::from(&repo.local_path).join(".knot.lock");
    let lock = match FileLock::try_acquire(&lock_path) {
        Ok(l) => l,
        Err(_) => return, // Another node in the cluster is processing it. Skip.
    };

    // 2. Git operation
    if path_exists(&repo.local_path) {
        run_git_command("pull", &repo).await;
    } else {
        run_git_command("clone", &repo).await;
    }

    // 3. Invoke knot core incrementally (clean = false)
    let mut knot_cfg = knot::config::Config::load_indexer();
    knot_cfg.repo_path = repo.local_path.clone();
    knot_cfg.clean = false; // Always incremental after initial clone

    // run_indexing_pipeline is CPU-intensive; wrap in spawn_blocking
    tokio::task::spawn_blocking(move || {
        knot::pipeline::runner::run_indexing_pipeline(
            &knot_cfg, &vector_db, &graph_db, &mut index_state,
        )
    })
    .await;

    // 4. Release lock
    drop(lock);
}
```

### Scheduling Strategy

The job queue can be fed by **three sources**:

1. **Initial Registration** (`POST /api/repos`) — clones the repo for the first time and runs a clean index.
2. **Webhook** (`POST /api/webhook/:id`) — GitLab/Bitbucket notifies of a push; job is enqueued for near-real-time update.
3. **Internal Timer** (optional) — a `tokio::time::interval` that once per day enqueues all repos as a safety net in case a webhook was missed.

---

## 5. Technology Stack

| Component     | Choice |
|---------------|--------|
| Language      | Rust (Edition 2024) |
| Web Framework | `axum` |
| Async Runtime | `tokio` |
| Core Library  | `knot` (published on crates.io) |
| File Locking  | `fs2` or `fd-lock` |
| JSON Handling | `serde` + `serde_json` |
| Git Operations| `std::process::Command` spawning `git` CLI |
| Configuration | `clap` + env vars + `.env` (knot convention) |

---

## 6. Startup Lifecycle

1. Load configuration via `knot::config::Config::load_indexer()`.
2. Initialize Qdrant (`VectorDb`) and Neo4j (`GraphDb`) clients.
3. Configure the `rayon` thread pool for parallel parsing.
4. Load `repos.json` from `KNOT_WORKSPACE_DIR`.
5. Mount Axum routes and inject shared state (`Arc<AppState>`).
6. Start the background timer (optional) and begin listening on the configured port.

---

## 7. Design Principles

- **Shared-Nothing Cluster** — Nodes share no in-memory state. The filesystem (`KNOT_WORKSPACE_DIR`) is the single source of truth.
- **Crash-Only** — If a node dies mid-index, the `.knot.lock` file may be stale. A lock staleness timeout (e.g., check file mtime) can be added in a future iteration; initially, manual cleanup is acceptable.
- **Incremental by Default** — After the initial clone, all subsequent indexing runs use `clean = false` to leverage knot's hash-based change detection.
- **No Unsafe Code** — All code must use safe Rust (consistent with knot's conventions).
