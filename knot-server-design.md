# knot-server — Architectural Design (v2)

A distributed REST API server acting as a controller for `knot-indexer`, managing
a cluster of cloned repositories with secure Git authentication and disk-based
shared-state persistence.

This document reflects the **actual public API** of the `knot` crate (v1.2.6) and
can be used directly as an implementation blueprint.

---

## 1. Cluster Architecture (Shared Storage & Locking)

All server nodes operate on a shared filesystem volume (NFS / EFS / GlusterFS)
mounted at a configurable root directory (env var `KNOT_WORKSPACE_DIR`, default
`/var/lib/knot/repos`).

### 1.1 Global State (`repos.json`)

A single JSON file at the root of the workspace serves as the master registry of
all managed repositories:

```json
{
  "repositories": [
    {
      "id": "my-backend-repo",
      "url": "git@github.com:company/backend.git",
      "auth_type": "ssh",
      "local_path": "/var/lib/knot/repos/my-backend-repo",
      "default_branch": "main",
      "webhook_secret": "hmac-sha256-secret-here",
      "last_indexed": "2026-05-08T10:00:00Z",
      "status": "idle"
    }
  ]
}
```

**Concurrency protection:** Since multiple cluster nodes may attempt to modify
`repos.json` simultaneously (e.g., two concurrent `POST /api/repos` requests
hitting different nodes), all reads and writes to this file MUST be serialized
using a dedicated file lock at `KNOT_WORKSPACE_DIR/repos.json.lock`. The lock is
acquired before reading and held until the write completes.

### 1.2 Per-Repository Job Coordination (File Locks)

To enforce the rule **"one repository at a time across the entire cluster"** and
avoid collisions between nodes:

- Each server maintains a **local Job Queue** (a Tokio `mpsc` channel) with
  concurrency capped at **1** (sequential execution within each node).
- Before a server begins processing a repository (triggered by Webhook, Timer,
  or Manual Sync), it attempts to atomically acquire an exclusive file lock on
  `<repo_local_path>/.knot.lock` (using `fs2::FileExt::try_lock_exclusive` or
  equivalent OS-level syscalls).
- If the lock **cannot** be acquired, another cluster node is already indexing
  that repo — the job is silently discarded on this node.
- If the lock **is** acquired, the server runs `git pull` + indexing pipeline,
  then releases the lock on completion (via `Drop`).

### 1.3 Stale Lock Recovery

If a node crashes mid-indexing, its `.knot.lock` file becomes stale and would
permanently block re-indexing of that repository. To prevent this:

- The background timer (Section 4.3) checks the `mtime` of every `.knot.lock`
  file it encounters.
- If a lock file is older than a configurable threshold (default: **60 minutes**),
  it is considered stale, forcibly removed, and the repository is re-enqueued for
  indexing.
- This provides automatic self-healing without manual operator intervention.

---

## 2. Enterprise Git Authentication

The server supports two authentication methods. The method is selected per
repository at registration time via the `auth_type` field.

### 2.1 SSH (preferred for server-to-server connections)

- The process assumes the host system has SSH keys configured
  (`~/.ssh/id_rsa` or `~/.ssh/config`).
- The server spawns `git` as a child process and lets the system's Git client
  handle SSH negotiation natively.
- No credential management needed inside knot-server.

### 2.2 HTTPS + Personal Access Token (PAT)

- The REST API receives the PAT at repository registration time
  (`POST /api/repos`).
- The PAT is stored encrypted or at minimum protected by filesystem permissions
  (not in `repos.json` — a separate credentials store, e.g.,
  `KNOT_WORKSPACE_DIR/.credentials/<repo_id>`).
- At clone/pull time, the server injects the token temporarily via the
  `GIT_ASKPASS` environment variable or a short-lived Git credential helper,
  avoiding plaintext tokens in the cloned repository's `.git/config`.

---

## 3. REST API Endpoints (Axum)

A load balancer distributes traffic across cluster nodes. All endpoints return
JSON with appropriate HTTP status codes.

### 3.1 Repository Management (Write)

| Method   | Path               | Description |
|----------|--------------------|-------------|
| `POST`   | `/api/repos`       | Register a new repository. Acquires the `repos.json.lock`, writes the entry, then enqueues `git clone` + first indexing. Returns `202 Accepted` with the new repo ID. |
| `GET`    | `/api/repos`       | List all managed repositories with their current status (`idle`, `indexing`, `error`), `last_indexed` timestamp, and metadata. |
| `GET`    | `/api/repos/:id`   | Get details for a single repository. |
| `DELETE` | `/api/repos/:id`   | Remove a repository from knot management. Acquires the repo `.knot.lock`, deletes the local directory, removes the entry from `repos.json`, and cleans up data from Qdrant and Neo4j. |

### 3.2 Triggers (Write)

| Method   | Path                     | Description |
|----------|--------------------------|-------------|
| `POST`   | `/api/webhook/:id`       | Receives GitLab/Bitbucket/GitHub webhook payload. **Validates the request signature** (see Section 3.5). If valid, enqueues `git pull` + incremental indexing. Returns `202 Accepted`. |
| `POST`   | `/api/repos/:id/sync`    | Manual endpoint to force a sync. Enqueues a `git pull` + incremental indexing job. Returns `202 Accepted`. |

### 3.3 Search & Exploration (Read — Stateless)

These endpoints call functions from `knot::cli_tools::*` which return
`serde_json::Value`, making them trivial to serialize as JSON responses.

| Method | Path             | Query Params                        | Calls                                          |
|--------|------------------|-------------------------------------|-------------------------------------------------|
| `GET`  | `/api/search`    | `q` (required), `repo`, `max_results` | `knot::cli_tools::run_search_hybrid_context()` |
| `GET`  | `/api/callers`   | `entity` (required), `repo`         | `knot::cli_tools::run_find_callers()`          |
| `GET`  | `/api/explore`   | `path` (required), `repo`           | `knot::cli_tools::run_explore_file()`          |
| `GET`  | `/api/deps`      | `repo` (required), `reverse`, `max_depth` | `knot::cli_tools::run_deps()`            |

### 3.4 Function Signatures (from `knot::cli_tools`)

These are the actual Rust function signatures that the endpoint handlers will
call. Note that **all return `anyhow::Result<serde_json::Value>`** except
`run_explore_file` which returns a tuple:

```rust
// Requires: VectorDb + GraphDb + Embedder
pub async fn run_search_hybrid_context(
    query: &str,
    max_results: usize,
    repo_name: Option<&str>,
    vector_db: &Arc<VectorDb>,
    graph_db: &Arc<GraphDb>,
    embedder: &Arc<Mutex<Embedder>>,
) -> anyhow::Result<serde_json::Value>

// Requires: GraphDb only
pub async fn run_find_callers(
    entity_name: &str,
    repo_name: Option<&str>,
    graph_db: &Arc<GraphDb>,
) -> anyhow::Result<serde_json::Value>

// Requires: GraphDb only — returns (display_path, entities_json)
pub async fn run_explore_file(
    file_path: &str,
    repo_name: Option<&str>,
    graph_db: &Arc<GraphDb>,
) -> anyhow::Result<(String, serde_json::Value)>

// Requires: GraphDb only
pub async fn run_deps(
    repo_name: &str,
    max_depth: u32,
    reverse: bool,
    graph_db: &Arc<GraphDb>,
) -> anyhow::Result<serde_json::Value>
```

### 3.5 Webhook Authentication

Every incoming webhook request MUST be validated before enqueuing work:

- **GitLab:** Verify the `X-Gitlab-Token` header matches the per-repo
  `webhook_secret` stored in the registry.
- **GitHub:** Compute `HMAC-SHA256` of the request body using the per-repo
  secret and compare against the `X-Hub-Signature-256` header.
- **Bitbucket:** Verify the `X-Hub-Signature` header (HMAC-SHA256).

Requests with invalid or missing signatures MUST return `401 Unauthorized`
without enqueuing any work.

---

## 4. Worker Implementation (Rust)

### 4.1 Shared Application State

The server initializes these components once at startup and shares them across
all Axum handlers via `Arc<AppState>`:

```rust
use std::sync::Arc;
use tokio::sync::Mutex;
use knot::db::graph::GraphDb;
use knot::db::vector::VectorDb;
use knot::pipeline::embed::Embedder;

pub struct AppState {
    pub vector_db: Arc<VectorDb>,
    pub graph_db: Arc<GraphDb>,
    pub embedder: Arc<Mutex<Embedder>>,  // Mutex: embed_query() takes &mut self
    pub workspace_dir: String,
    pub job_tx: tokio::sync::mpsc::Sender<IndexJob>,
}
```

**Why `Arc<Mutex<Embedder>>`?** The `Embedder` wraps the fastembed ONNX model.
Its `embed_query(&mut self, query)` method requires `&mut self` because the
underlying ONNX runtime session is not reentrant. A `tokio::sync::Mutex` allows
multiple async handlers to share the embedder without blocking the Tokio runtime.

**Why are `VectorDb` and `GraphDb` in `Arc` without `Mutex`?** Both types wrap
async connection pools (`qdrant_client::Qdrant` and `neo4rs::Graph`) that are
internally thread-safe (`Send + Sync`). All their query methods take `&self`.

### 4.2 Job Processing Loop

Each node runs a single worker task that reads from the job queue channel:

```rust
pub enum IndexJob {
    Clone { repo_id: String },
    Pull { repo_id: String },
}

async fn worker_loop(
    mut rx: tokio::sync::mpsc::Receiver<IndexJob>,
    state: Arc<AppState>,
) {
    while let Some(job) = rx.recv().await {
        let repo = load_repo_from_registry(&state.workspace_dir, &job.repo_id());
        if let Err(e) = process_repository(&repo, &state).await {
            tracing::error!("Indexing failed for {}: {e:#}", repo.id);
            update_repo_status(&state.workspace_dir, &repo.id, "error");
        }
    }
}
```

### 4.3 Repository Processing (Corrected)

```rust
use knot::config::Config;
use knot::pipeline::runner::run_indexing_pipeline;
use knot::pipeline::state::IndexState;

async fn process_repository(repo: &Repository, state: &AppState) -> anyhow::Result<()> {
    // 1. Attempt to acquire exclusive file lock
    let lock_path = PathBuf::from(&repo.local_path).join(".knot.lock");
    let lock_file = std::fs::File::create(&lock_path)?;
    if fs2::FileExt::try_lock_exclusive(&lock_file).is_err() {
        tracing::info!("{}: another node is indexing, skipping", repo.id);
        return Ok(());
    }
    // Lock is held until `lock_file` is dropped

    // 2. Git operation
    update_repo_status(&state.workspace_dir, &repo.id, "indexing");
    if Path::new(&repo.local_path).join(".git").exists() {
        run_git_pull(&repo).await?;
    } else {
        run_git_clone(&repo).await?;
    }

    // 3. Build Config programmatically (NOT Config::load_indexer())
    //
    // Config::load_indexer() reads CLI args via clap which would conflict
    // with knot-server's own CLI. Since all Config fields are `pub`, we
    // construct it directly with struct literal syntax.
    let knot_cfg = Config {
        repo_path: repo.local_path.clone(),
        repo_name: repo.id.clone(),
        qdrant_url: state.qdrant_url.clone(),
        qdrant_collection: state.qdrant_collection.clone(),
        neo4j_uri: state.neo4j_uri.clone(),
        neo4j_user: state.neo4j_user.clone(),
        neo4j_password: state.neo4j_password.clone(),
        custom_queries_path: None,
        embed_dim: 384,
        batch_size: 64,
        clean: false,  // Always incremental after initial clone
        dependency_repos: Vec::new(),
        watch: false,   // knot-server manages scheduling, not knot's watcher
        dry_run: false,
        custom_ca_certs: None,
        output_format: knot::config::OutputFormat::Markdown,
        ingest_concurrency: 4,
        rayon_threads: None,
        include_config_files: false,
    };

    // 4. Load IndexState (hash-based change detection for incremental indexing)
    //
    // IndexState lives at <repo_path>/.knot/index_state.json and tracks
    // SHA-256 hashes of every previously indexed file. On the shared volume,
    // this file is protected by the .knot.lock we already hold.
    let mut index_state = IndexState::load(&repo.local_path)?;

    // 5. Run the indexing pipeline
    //
    // IMPORTANT: run_indexing_pipeline is `pub async fn`. It internally
    // manages its own concurrency (spawn_blocking for Rayon parsing,
    // tokio::spawn for ingestion workers, Semaphore for backpressure).
    // Do NOT wrap this in spawn_blocking — call it directly from async.
    run_indexing_pipeline(
        &knot_cfg,
        &state.vector_db,
        &state.graph_db,
        &mut index_state,
    )
    .await?;

    // 6. Update registry
    update_repo_status(&state.workspace_dir, &repo.id, "idle");
    update_repo_last_indexed(&state.workspace_dir, &repo.id);

    // 7. Lock released automatically when lock_file is dropped
    Ok(())
}
```

### 4.4 Scheduling Strategy

The job queue is fed by **three sources**:

1. **Initial Registration** (`POST /api/repos`) — enqueues a `Clone` job.
   The first indexing run uses `clean: false` on an empty `IndexState`, which
   is equivalent to a full index.
2. **Webhook** (`POST /api/webhook/:id`) — after signature validation, enqueues
   a `Pull` job for near-real-time update.
3. **Background Timer** — a `tokio::time::interval` that runs once per
   configurable period (default: 24 hours). It iterates all repos in
   `repos.json`, checks for stale locks (Section 1.3), and enqueues a `Pull`
   job for each repo whose `last_indexed` exceeds a configurable age threshold.

---

## 5. Technology Stack

| Component        | Choice                                           |
|------------------|--------------------------------------------------|
| Language         | Rust (Edition 2024)                              |
| Web Framework    | `axum`                                           |
| Async Runtime    | `tokio` (full features)                          |
| Core Library     | `knot` crate (default features = `["indexer"]`)  |
| File Locking     | `fs2` (`try_lock_exclusive` / `unlock`)          |
| JSON Handling    | `serde` + `serde_json`                           |
| Git Operations   | `std::process::Command` spawning `git` CLI       |
| Configuration    | `clap` + env vars + `.env`                       |
| Logging          | `tracing` + `tracing-subscriber`                 |

### 5.1 Key Dependency Notes

- **`knot` with default features** pulls in `fastembed` + `ort` (ONNX Runtime).
  This is required for both indexing (entity embedding) and search (query
  embedding). The compiled binary will be ~50-80 MB.
- If a future lightweight query-only node is desired, it can depend on
  `knot` with `default-features = false, features = ["only-clients"]`. In this
  mode, `run_search_hybrid_context` will return an error, but `find_callers`,
  `explore_file`, and `deps` work normally.

---

## 6. Startup Lifecycle

```
1. Parse knot-server's own CLI args / env vars (port, workspace dir, DB URLs)
2. Initialize tracing/logging
3. Connect to Qdrant  -> Arc<VectorDb>
4. Connect to Neo4j   -> Arc<GraphDb>
5. Initialize Embedder (loads ONNX model into RAM, ~23 MB) -> Arc<Mutex<Embedder>>
6. Configure Rayon thread pool (for CPU-intensive parsing during indexing)
7. Load repos.json from KNOT_WORKSPACE_DIR (create if not exists)
8. Create mpsc channel for IndexJob queue
9. Spawn the worker_loop task (reads from channel, processes jobs sequentially)
10. Spawn the background timer task (periodic polling / stale-lock cleanup)
11. Mount Axum routes, inject Arc<AppState>
12. Bind to configured address and start listening
```

**Critical ordering:** Steps 3-6 must complete before the server accepts
requests. The Rayon thread pool (step 6) must be configured exactly once per
process via `rayon::ThreadPoolBuilder::new().build_global()`.

---

## 7. Design Principles

- **Shared-Nothing Cluster** — Nodes share no in-memory state. The shared
  filesystem (`KNOT_WORKSPACE_DIR`) is the single source of truth for repository
  metadata and lock coordination.

- **Self-Healing** — Stale locks are automatically detected and cleaned by the
  background timer. No manual operator intervention required for crash recovery.

- **Incremental by Default** — After the initial clone, all subsequent indexing
  runs use `clean = false`. The `IndexState` (per-repo
  `.knot/index_state.json`) tracks SHA-256 hashes of every file, ensuring only
  changed files are re-parsed and re-embedded.

- **Secure by Default** — Webhook endpoints validate request signatures. PAT
  tokens are never stored in plaintext alongside repository data.

- **No Unsafe Code** — All code must use safe Rust, consistent with knot's
  conventions.

---

## 8. Error Handling & Observability

- All endpoints return structured JSON errors with appropriate HTTP status codes:
  - `400` — Invalid request parameters
  - `401` — Invalid webhook signature
  - `404` — Repository not found
  - `409` — Repository already registered
  - `500` — Internal server error
  - `202` — Accepted (async job enqueued)

- The worker logs all indexing operations via `tracing`. Indexing failures update
  the repo's `status` field to `"error"` in `repos.json`, making failures
  visible via `GET /api/repos`.

- Future enhancement: expose a `GET /api/health` endpoint returning node status,
  queue depth, database connectivity, and uptime.

---

## 9. Deployment Topology

```
                    +-----------------+
                    |  Load Balancer  |
                    +--------+--------+
                             |
              +--------------+--------------+
              |              |              |
        +-----+----+  +-----+----+  +-----+----+
        | knot-srv  |  | knot-srv  |  | knot-srv  |
        | node-1    |  | node-2    |  | node-3    |
        +-----+----+  +-----+----+  +-----+----+
              |              |              |
              +--------------+--------------+
                             |
                   +---------+---------+
                   |  Shared Volume    |
                   |  (NFS / EFS)      |
                   |                   |
                   |  repos.json       |
                   |  repos.json.lock  |
                   |  repo-a/          |
                   |    .knot.lock     |
                   |    .knot/         |
                   |    src/...        |
                   |  repo-b/          |
                   |    ...            |
                   +-------------------+

        +-------------------+    +-------------------+
        |     Qdrant        |    |      Neo4j        |
        | (vector search)   |    | (graph queries)   |
        +-------------------+    +-------------------+
```

All nodes connect to the **same** Qdrant and Neo4j instances. Read endpoints
(search, callers, explore, deps) are fully stateless and can be served by any
node. Write operations (indexing) are serialized per-repository via file locks
on the shared volume.

---

## 10. Testing Strategy

Testing follows the same philosophy as the `knot` project:

- **BDD:** E2E tests are written first and must fail before implementation.
- **TDD:** Unit tests in each module before implementation.
- **Regression:** Every bug gets an E2E case before the fix.
- **No `unsafe`:** All test code uses safe Rust.

### 10.1 Unit Tests

Unit tests live inline in the source modules (`#[cfg(test)] mod tests`). They
test individual components in isolation, mocking or stubbing external
dependencies (databases, filesystem, git).

#### 10.1.1 Repository Registry (`registry.rs`)

Tests for `repos.json` management:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_create_empty_registry() {
        let dir = TempDir::new().unwrap();
        let registry = Registry::load_or_create(dir.path()).unwrap();
        assert!(registry.repositories.is_empty());
    }

    #[test]
    fn test_add_repository() {
        let dir = TempDir::new().unwrap();
        let mut registry = Registry::load_or_create(dir.path()).unwrap();
        let repo = RepoEntry {
            id: "test-repo".into(),
            url: "git@github.com:org/test.git".into(),
            auth_type: AuthType::Ssh,
            local_path: dir.path().join("test-repo").to_string_lossy().into(),
            default_branch: "main".into(),
            webhook_secret: None,
            last_indexed: None,
            status: RepoStatus::Idle,
        };
        registry.add(repo.clone()).unwrap();

        // Reload from disk and verify persistence
        let reloaded = Registry::load_or_create(dir.path()).unwrap();
        assert_eq!(reloaded.repositories.len(), 1);
        assert_eq!(reloaded.repositories[0].id, "test-repo");
    }

    #[test]
    fn test_add_duplicate_repository_returns_error() {
        let dir = TempDir::new().unwrap();
        let mut registry = Registry::load_or_create(dir.path()).unwrap();
        let repo = RepoEntry { id: "dup".into(), /* ... */ };
        registry.add(repo.clone()).unwrap();
        let result = registry.add(repo);
        assert!(result.is_err()); // Should return Conflict error
    }

    #[test]
    fn test_remove_repository() {
        let dir = TempDir::new().unwrap();
        let mut registry = Registry::load_or_create(dir.path()).unwrap();
        let repo = RepoEntry { id: "to-remove".into(), /* ... */ };
        registry.add(repo).unwrap();
        registry.remove("to-remove").unwrap();
        assert!(registry.repositories.is_empty());
    }

    #[test]
    fn test_update_status() {
        let dir = TempDir::new().unwrap();
        let mut registry = Registry::load_or_create(dir.path()).unwrap();
        let repo = RepoEntry { id: "r1".into(), status: RepoStatus::Idle, /* ... */ };
        registry.add(repo).unwrap();
        registry.update_status("r1", RepoStatus::Indexing).unwrap();
        assert_eq!(registry.get("r1").unwrap().status, RepoStatus::Indexing);
    }

    #[test]
    fn test_list_repositories() {
        let dir = TempDir::new().unwrap();
        let mut registry = Registry::load_or_create(dir.path()).unwrap();
        registry.add(RepoEntry { id: "a".into(), /* ... */ }).unwrap();
        registry.add(RepoEntry { id: "b".into(), /* ... */ }).unwrap();
        assert_eq!(registry.list().len(), 2);
    }
}
```

#### 10.1.2 File Locking (`locking.rs`)

Tests for `.knot.lock` and `repos.json.lock` coordination:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_acquire_lock_succeeds_on_first_attempt() {
        let dir = TempDir::new().unwrap();
        let lock_path = dir.path().join(".knot.lock");
        let lock = FileLock::try_acquire(&lock_path);
        assert!(lock.is_ok());
    }

    #[test]
    fn test_acquire_lock_fails_when_already_held() {
        let dir = TempDir::new().unwrap();
        let lock_path = dir.path().join(".knot.lock");
        let _lock1 = FileLock::try_acquire(&lock_path).unwrap();
        let lock2 = FileLock::try_acquire(&lock_path);
        assert!(lock2.is_err()); // Cannot acquire — already locked
    }

    #[test]
    fn test_lock_released_on_drop() {
        let dir = TempDir::new().unwrap();
        let lock_path = dir.path().join(".knot.lock");
        {
            let _lock = FileLock::try_acquire(&lock_path).unwrap();
            // Lock is held here
        }
        // Lock dropped — should be acquirable again
        let lock2 = FileLock::try_acquire(&lock_path);
        assert!(lock2.is_ok());
    }

    #[test]
    fn test_stale_lock_detection() {
        let dir = TempDir::new().unwrap();
        let lock_path = dir.path().join(".knot.lock");
        std::fs::File::create(&lock_path).unwrap();

        // Manually set mtime to 2 hours ago
        let two_hours_ago = SystemTime::now() - Duration::from_secs(7200);
        filetime::set_file_mtime(&lock_path, filetime::FileTime::from_system_time(two_hours_ago)).unwrap();

        assert!(is_lock_stale(&lock_path, Duration::from_secs(3600))); // threshold = 1 hour
    }

    #[test]
    fn test_fresh_lock_not_stale() {
        let dir = TempDir::new().unwrap();
        let lock_path = dir.path().join(".knot.lock");
        std::fs::File::create(&lock_path).unwrap();

        assert!(!is_lock_stale(&lock_path, Duration::from_secs(3600)));
    }
}
```

#### 10.1.3 Git Operations (`git.rs`)

Tests for git clone/pull logic (using a local bare repo as fixture):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use std::process::Command;

    /// Helper: create a bare git repo with one commit for testing.
    fn create_test_bare_repo(dir: &Path) -> PathBuf {
        let bare_path = dir.join("test-bare.git");
        Command::new("git").args(["init", "--bare"]).arg(&bare_path).output().unwrap();

        let clone_path = dir.join("tmp-clone");
        Command::new("git").args(["clone"]).arg(&bare_path).arg(&clone_path).output().unwrap();
        std::fs::write(clone_path.join("README.md"), "# Test").unwrap();
        Command::new("git").args(["add", "."]).current_dir(&clone_path).output().unwrap();
        Command::new("git").args(["commit", "-m", "init"]).current_dir(&clone_path).output().unwrap();
        Command::new("git").args(["push"]).current_dir(&clone_path).output().unwrap();

        bare_path
    }

    #[tokio::test]
    async fn test_git_clone_local_repo() {
        let dir = TempDir::new().unwrap();
        let bare = create_test_bare_repo(dir.path());
        let clone_dest = dir.path().join("cloned");

        let repo = RepoEntry {
            url: bare.to_string_lossy().into(),
            local_path: clone_dest.to_string_lossy().into(),
            auth_type: AuthType::Ssh, // Irrelevant for local
            ..Default::default()
        };

        run_git_clone(&repo).await.unwrap();
        assert!(clone_dest.join(".git").exists());
        assert!(clone_dest.join("README.md").exists());
    }

    #[tokio::test]
    async fn test_git_pull_fetches_changes() {
        let dir = TempDir::new().unwrap();
        let bare = create_test_bare_repo(dir.path());
        let clone_dest = dir.path().join("cloned");

        let repo = RepoEntry {
            url: bare.to_string_lossy().into(),
            local_path: clone_dest.to_string_lossy().into(),
            ..Default::default()
        };
        run_git_clone(&repo).await.unwrap();

        // Push a new commit to the bare repo
        let tmp2 = dir.path().join("tmp2");
        Command::new("git").args(["clone"]).arg(&bare).arg(&tmp2).output().unwrap();
        std::fs::write(tmp2.join("new_file.txt"), "content").unwrap();
        Command::new("git").args(["add", "."]).current_dir(&tmp2).output().unwrap();
        Command::new("git").args(["commit", "-m", "add file"]).current_dir(&tmp2).output().unwrap();
        Command::new("git").args(["push"]).current_dir(&tmp2).output().unwrap();

        // Pull and verify
        run_git_pull(&repo).await.unwrap();
        assert!(clone_dest.join("new_file.txt").exists());
    }

    #[tokio::test]
    async fn test_git_clone_invalid_url_returns_error() {
        let dir = TempDir::new().unwrap();
        let repo = RepoEntry {
            url: "https://invalid.example.com/nonexistent.git".into(),
            local_path: dir.path().join("fail").to_string_lossy().into(),
            ..Default::default()
        };
        let result = run_git_clone(&repo).await;
        assert!(result.is_err());
    }
}
```

#### 10.1.4 Webhook Validation (`webhook.rs`)

Tests for signature verification:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gitlab_token_validation_success() {
        let secret = "my-webhook-secret";
        let header_value = "my-webhook-secret";
        assert!(validate_gitlab_token(header_value, secret));
    }

    #[test]
    fn test_gitlab_token_validation_failure() {
        let secret = "my-webhook-secret";
        let header_value = "wrong-token";
        assert!(!validate_gitlab_token(header_value, secret));
    }

    #[test]
    fn test_github_hmac_sha256_validation_success() {
        let secret = "my-secret";
        let body = b"payload-body";
        let signature = compute_hmac_sha256(secret, body);
        let header = format!("sha256={}", hex::encode(&signature));
        assert!(validate_github_signature(&header, body, secret));
    }

    #[test]
    fn test_github_hmac_sha256_validation_failure() {
        let secret = "my-secret";
        let body = b"payload-body";
        let header = "sha256=0000000000000000000000000000000000000000000000000000000000000000";
        assert!(!validate_github_signature(header, body, secret));
    }

    #[test]
    fn test_github_validation_rejects_malformed_header() {
        let secret = "my-secret";
        let body = b"payload-body";
        assert!(!validate_github_signature("not-a-valid-header", body, secret));
    }
}
```

#### 10.1.5 Job Queue (`worker.rs`)

Tests for sequential processing and queue behavior:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn test_job_queue_processes_sequentially() {
        let (tx, rx) = mpsc::channel(16);
        let order = Arc::new(Mutex::new(Vec::new()));

        let order_clone = order.clone();
        let handle = tokio::spawn(async move {
            // Simulated worker that records processing order
            mock_worker_loop(rx, order_clone).await;
        });

        tx.send(IndexJob::Pull { repo_id: "a".into() }).await.unwrap();
        tx.send(IndexJob::Pull { repo_id: "b".into() }).await.unwrap();
        tx.send(IndexJob::Pull { repo_id: "c".into() }).await.unwrap();
        drop(tx); // Close channel to stop worker

        handle.await.unwrap();
        let processed = order.lock().await;
        assert_eq!(*processed, vec!["a", "b", "c"]); // Sequential order
    }

    #[tokio::test]
    async fn test_job_queue_skips_locked_repos() {
        // Verify that when a repo lock is held, the job is skipped
        let dir = TempDir::new().unwrap();
        let lock_path = dir.path().join(".knot.lock");
        let _held_lock = FileLock::try_acquire(&lock_path).unwrap();

        let result = try_process_repository_at(dir.path()).await;
        assert!(result.is_ok()); // Gracefully skipped, not an error
    }
}
```

#### 10.1.6 Axum Handler Tests (`handlers.rs`)

Tests for HTTP request/response correctness using `axum::test`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt; // for `oneshot`

    fn build_test_app(state: Arc<AppState>) -> Router {
        create_router(state)
    }

    #[tokio::test]
    async fn test_list_repos_empty() {
        let state = create_test_state().await;
        let app = build_test_app(state);

        let response = app
            .oneshot(Request::get("/api/repos").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body: serde_json::Value = parse_body(response).await;
        assert_eq!(body["repositories"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn test_register_repo_returns_202() {
        let state = create_test_state().await;
        let app = build_test_app(state);

        let body = serde_json::json!({
            "url": "git@github.com:org/repo.git",
            "auth_type": "ssh"
        });

        let response = app
            .oneshot(
                Request::post("/api/repos")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::ACCEPTED);
    }

    #[tokio::test]
    async fn test_register_duplicate_repo_returns_409() {
        let state = create_test_state().await;
        let app = build_test_app(state.clone());

        let body = serde_json::json!({
            "url": "git@github.com:org/repo.git",
            "auth_type": "ssh"
        });

        // First registration succeeds
        let _ = app.clone()
            .oneshot(Request::post("/api/repos")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&body).unwrap())).unwrap())
            .await.unwrap();

        // Second returns conflict
        let response = app
            .oneshot(Request::post("/api/repos")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&body).unwrap())).unwrap())
            .await.unwrap();

        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn test_delete_nonexistent_repo_returns_404() {
        let state = create_test_state().await;
        let app = build_test_app(state);

        let response = app
            .oneshot(Request::delete("/api/repos/nonexistent").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_webhook_without_signature_returns_401() {
        let state = create_test_state().await;
        let app = build_test_app(state);

        let response = app
            .oneshot(
                Request::post("/api/webhook/some-repo")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_search_missing_query_returns_400() {
        let state = create_test_state().await;
        let app = build_test_app(state);

        let response = app
            .oneshot(Request::get("/api/search").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_callers_missing_entity_returns_400() {
        let state = create_test_state().await;
        let app = build_test_app(state);

        let response = app
            .oneshot(Request::get("/api/callers").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_explore_missing_path_returns_400() {
        let state = create_test_state().await;
        let app = build_test_app(state);

        let response = app
            .oneshot(Request::get("/api/explore").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_sync_nonexistent_repo_returns_404() {
        let state = create_test_state().await;
        let app = build_test_app(state);

        let response = app
            .oneshot(
                Request::post("/api/repos/ghost/sync")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_deps_missing_repo_returns_400() {
        let state = create_test_state().await;
        let app = build_test_app(state);

        let response = app
            .oneshot(Request::get("/api/deps").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
```

#### 10.1.7 Config Construction (`config.rs`)

Tests for server-specific configuration:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_knot_config_from_server_config() {
        let server_cfg = ServerConfig {
            qdrant_url: "http://localhost:6334".into(),
            qdrant_collection: "knot_entities".into(),
            neo4j_uri: "bolt://localhost:7687".into(),
            neo4j_user: "neo4j".into(),
            neo4j_password: "secret".into(),
            ..Default::default()
        };
        let knot_cfg = build_knot_config(&server_cfg, "/repos/my-app", "my-app");

        assert_eq!(knot_cfg.repo_path, "/repos/my-app");
        assert_eq!(knot_cfg.repo_name, "my-app");
        assert!(!knot_cfg.clean);
        assert!(!knot_cfg.watch);
        assert!(!knot_cfg.dry_run);
        assert_eq!(knot_cfg.embed_dim, 384);
    }

    #[test]
    fn test_knot_config_always_incremental() {
        let server_cfg = ServerConfig::default();
        let knot_cfg = build_knot_config(&server_cfg, "/repos/x", "x");
        assert!(!knot_cfg.clean, "knot-server must always index incrementally");
        assert!(!knot_cfg.watch, "knot-server manages scheduling, not knot");
    }
}
```

### 10.2 E2E Integration Tests

E2E tests follow the same pattern as knot's existing test suite: shell scripts
that spin up ephemeral Docker containers (Qdrant + Neo4j on high ports),
exercise the full REST API, and tear everything down on exit.

#### 10.2.1 Infrastructure

**`tests/docker-compose.e2e.yml`** — Identical to knot's E2E compose file,
reusing the same high ports to avoid conflicts:

```yaml
version: '3.8'
services:
  neo4j-e2e:
    image: neo4j:5
    container_name: knot_server_neo4j_e2e
    ports:
      - "17474:7474"
      - "17687:7687"
    environment:
      NEO4J_AUTH: neo4j/e2e_test_password
      NEO4J_server_memory_heap_initial__size: 256m
      NEO4J_server_memory_heap_max__size: 512m
    volumes:
      - ./.e2e_data/neo4j/data:/data
      - ./.e2e_data/neo4j/logs:/logs
    healthcheck:
      test: ["CMD", "cypher-shell", "-u", "neo4j", "-p", "e2e_test_password", "CALL db.ping()"]
      interval: 5s
      timeout: 3s
      retries: 10

  qdrant-e2e:
    image: qdrant/qdrant:latest
    container_name: knot_server_qdrant_e2e
    ports:
      - "16333:6333"
      - "16334:6334"
    volumes:
      - ./.e2e_data/qdrant:/qdrant/storage
    healthcheck:
      test: ["CMD", "wget", "--no-verbose", "--tries=1", "--spider", "http://localhost:6333/health"]
      interval: 5s
      timeout: 3s
      retries: 10

networks:
  default:
    name: knot_server_e2e_network
```

**`tests/fixtures/`** — A small Git repository (pre-created bare repo or a
directory with `.git` initialized) containing Java, TypeScript, and Rust test
files. These are the same fixture files used in knot's own E2E suite
(`tests/testing_files/`), or symlinked from them.

#### 10.2.2 E2E Test Script Structure

Each E2E script follows this lifecycle:

```
1. Start Docker containers (Qdrant + Neo4j)
2. Wait for health checks to pass
3. Build knot-server binary (cargo build --release)
4. Start knot-server as a background process
5. Wait for the server's HTTP port to be ready
6. Run test cases via curl against the REST API
7. Kill the server process
8. Tear down Docker containers
9. Report pass/fail
```

Cleanup is handled via `trap cleanup EXIT INT TERM` (same pattern as knot).

#### 10.2.3 E2E Happy Path: Full Indexing + Query Cycle (`tests/run_e2e.sh`)

This is the core E2E test. It validates the complete lifecycle: register a repo,
wait for indexing, then query the indexed code.

```bash
#!/usr/bin/env bash
# E2E Integration Test for knot-server
# Tests the full lifecycle: register repo -> index -> search -> callers -> explore

set -e
set -u

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
COMPOSE_FILE="$SCRIPT_DIR/docker-compose.e2e.yml"
E2E_DATA_DIR="$SCRIPT_DIR/.e2e_data"
SERVER_PORT=18080
SERVER_PID=""

# Test fixture: a local bare git repo with known source files
FIXTURE_DIR="$SCRIPT_DIR/fixtures/test-repo"

# Database config (matches docker-compose.e2e.yml)
NEO4J_URI="bolt://localhost:17687"
NEO4J_USER="neo4j"
NEO4J_PASSWORD="e2e_test_password"
QDRANT_URL="http://localhost:16334"

echo -e "${GREEN}========================================${NC}"
echo -e "${GREEN}knot-server E2E Integration Test${NC}"
echo -e "${GREEN}========================================${NC}"

cleanup() {
    local exit_code=$?
    if [ -n "$SERVER_PID" ] && kill -0 "$SERVER_PID" 2>/dev/null; then
        kill "$SERVER_PID" 2>/dev/null || true
        wait "$SERVER_PID" 2>/dev/null || true
    fi
    if [ $exit_code -ne 0 ]; then
        echo -e "\n${RED}Tests failed! Containers left running for inspection.${NC}"
        return 0
    fi
    echo -e "\n${YELLOW}Cleaning up...${NC}"
    cd "$SCRIPT_DIR"
    docker compose -f "$COMPOSE_FILE" down -v 2>/dev/null || true
    sudo rm -rf "$E2E_DATA_DIR" 2>/dev/null || rm -rf "$E2E_DATA_DIR" 2>/dev/null || true
    echo -e "${GREEN}Cleanup complete${NC}"
}
trap cleanup EXIT INT TERM

# -------------------------------------------------------
# Step 1: Start Docker containers
# -------------------------------------------------------
echo -e "${YELLOW}[1/7] Starting Docker containers...${NC}"
cd "$SCRIPT_DIR"
docker compose -f "$COMPOSE_FILE" down -v 2>/dev/null || true
sudo rm -rf "$E2E_DATA_DIR" 2>/dev/null || rm -rf "$E2E_DATA_DIR" 2>/dev/null || true
docker compose -f "$COMPOSE_FILE" up -d

# -------------------------------------------------------
# Step 2: Wait for databases
# -------------------------------------------------------
echo -e "${YELLOW}[2/7] Waiting for databases...${NC}"
# (same wait_for_port function as knot's E2E — omitted for brevity)
wait_for_port 17687 "Neo4j" "knot_server_neo4j_e2e"
wait_for_port 16334 "Qdrant" "knot_server_qdrant_e2e"
sleep 5

# -------------------------------------------------------
# Step 3: Build and start knot-server
# -------------------------------------------------------
echo -e "${YELLOW}[3/7] Building and starting knot-server...${NC}"
cd "$PROJECT_ROOT"
cargo build --release 2>&1 | grep -E "(Compiling|Finished|error)" || true

KNOT_SERVER_QDRANT_URL="$QDRANT_URL" \
KNOT_SERVER_NEO4J_URI="$NEO4J_URI" \
KNOT_SERVER_NEO4J_USER="$NEO4J_USER" \
KNOT_NEO4J_PASSWORD="$NEO4J_PASSWORD" \
KNOT_SERVER_PORT="$SERVER_PORT" \
KNOT_WORKSPACE_DIR="$E2E_DATA_DIR/workspace" \
  ./target/release/knot-server &
SERVER_PID=$!

# Wait for the server to be ready
echo "Waiting for knot-server on port $SERVER_PORT..."
for i in $(seq 1 30); do
    if curl -sf "http://localhost:$SERVER_PORT/api/repos" > /dev/null 2>&1; then
        echo -e "${GREEN}✓ knot-server is ready${NC}"
        break
    fi
    if [ "$i" -eq 30 ]; then
        echo -e "${RED}ERROR: knot-server did not start within 30s${NC}"
        exit 1
    fi
    sleep 1
done

BASE_URL="http://localhost:$SERVER_PORT"

# -------------------------------------------------------
# Step 4: Register a repository
# -------------------------------------------------------
echo -e "${YELLOW}[4/7] Registering test repository...${NC}"

REGISTER_RESPONSE=$(curl -sf -w "%{http_code}" -o /tmp/register_body.json \
    -X POST "$BASE_URL/api/repos" \
    -H "Content-Type: application/json" \
    -d "{
        \"url\": \"$FIXTURE_DIR\",
        \"auth_type\": \"ssh\",
        \"default_branch\": \"main\"
    }")

if [ "$REGISTER_RESPONSE" = "202" ]; then
    echo -e "${GREEN}✓ Repository registered (202 Accepted)${NC}"
else
    echo -e "${RED}✗ Expected 202, got $REGISTER_RESPONSE${NC}"
    cat /tmp/register_body.json
    exit 1
fi

REPO_ID=$(cat /tmp/register_body.json | jq -r '.id')
echo "  Repo ID: $REPO_ID"

# -------------------------------------------------------
# Step 5: Wait for indexing to complete
# -------------------------------------------------------
echo -e "${YELLOW}[5/7] Waiting for indexing to complete...${NC}"
for i in $(seq 1 120); do
    STATUS=$(curl -sf "$BASE_URL/api/repos/$REPO_ID" | jq -r '.status')
    if [ "$STATUS" = "idle" ]; then
        LAST_INDEXED=$(curl -sf "$BASE_URL/api/repos/$REPO_ID" | jq -r '.last_indexed')
        if [ "$LAST_INDEXED" != "null" ]; then
            echo -e "${GREEN}✓ Indexing complete (last_indexed: $LAST_INDEXED)${NC}"
            break
        fi
    elif [ "$STATUS" = "error" ]; then
        echo -e "${RED}✗ Indexing failed with error status${NC}"
        exit 1
    fi
    if [ "$i" -eq 120 ]; then
        echo -e "${RED}✗ Indexing did not complete within 120s${NC}"
        exit 1
    fi
    sleep 1
done

# -------------------------------------------------------
# Step 6: Query tests
# -------------------------------------------------------
echo -e "${YELLOW}[6/7] Running query tests...${NC}"

# Test A: Semantic search
echo ""
echo "Test A: Semantic search for 'UserService'..."
SEARCH_RESPONSE=$(curl -sf "$BASE_URL/api/search?q=UserService&repo=$REPO_ID")

if echo "$SEARCH_RESPONSE" | jq -e '.' > /dev/null 2>&1; then
    echo -e "${GREEN}✓ Search returned valid JSON${NC}"
else
    echo -e "${RED}✗ Search returned invalid JSON${NC}"
    echo "$SEARCH_RESPONSE"
    exit 1
fi

if echo "$SEARCH_RESPONSE" | grep -q "UserService"; then
    echo -e "${GREEN}✓ Found UserService in search results${NC}"
else
    echo -e "${RED}✗ UserService not found in search results${NC}"
    echo "$SEARCH_RESPONSE"
    exit 1
fi

# Test B: Find callers
echo ""
echo "Test B: Finding callers of 'AppComponent'..."
CALLERS_RESPONSE=$(curl -sf "$BASE_URL/api/callers?entity=AppComponent&repo=$REPO_ID")

if echo "$CALLERS_RESPONSE" | jq -e '.' > /dev/null 2>&1; then
    echo -e "${GREEN}✓ Callers returned valid JSON${NC}"
else
    echo -e "${RED}✗ Callers returned invalid JSON${NC}"
    exit 1
fi

if echo "$CALLERS_RESPONSE" | grep -q "AppModule"; then
    echo -e "${GREEN}✓ Found AppModule as caller of AppComponent${NC}"
else
    echo -e "${RED}✗ AppModule not found as caller${NC}"
    echo "$CALLERS_RESPONSE"
    exit 1
fi

# Test C: Explore file
echo ""
echo "Test C: Exploring a TypeScript file..."
# URL-encode the file path
TS_FILE_PATH=$(python3 -c "import urllib.parse; print(urllib.parse.quote('$FIXTURE_DIR/test_typescript.ts'))")
EXPLORE_RESPONSE=$(curl -sf "$BASE_URL/api/explore?path=$TS_FILE_PATH&repo=$REPO_ID")

if echo "$EXPLORE_RESPONSE" | jq -e '.' > /dev/null 2>&1; then
    echo -e "${GREEN}✓ Explore returned valid JSON${NC}"
else
    echo -e "${RED}✗ Explore returned invalid JSON${NC}"
    exit 1
fi

if echo "$EXPLORE_RESPONSE" | grep -q "AppComponent"; then
    echo -e "${GREEN}✓ Found AppComponent in file exploration${NC}"
else
    echo -e "${RED}✗ AppComponent not found in explored file${NC}"
    echo "$EXPLORE_RESPONSE"
    exit 1
fi

# Test D: List repos shows the registered repo
echo ""
echo "Test D: Listing repositories..."
LIST_RESPONSE=$(curl -sf "$BASE_URL/api/repos")

if echo "$LIST_RESPONSE" | jq -e ".repositories[] | select(.id == \"$REPO_ID\")" > /dev/null 2>&1; then
    echo -e "${GREEN}✓ Registered repo appears in listing${NC}"
else
    echo -e "${RED}✗ Repo not found in listing${NC}"
    echo "$LIST_RESPONSE"
    exit 1
fi

# Test E: Manual sync returns 202
echo ""
echo "Test E: Triggering manual sync..."
SYNC_RESPONSE=$(curl -sf -w "%{http_code}" -o /dev/null \
    -X POST "$BASE_URL/api/repos/$REPO_ID/sync")

if [ "$SYNC_RESPONSE" = "202" ]; then
    echo -e "${GREEN}✓ Manual sync accepted (202)${NC}"
else
    echo -e "${RED}✗ Expected 202, got $SYNC_RESPONSE${NC}"
    exit 1
fi

# Test F: Deps endpoint
echo ""
echo "Test F: Querying repository dependencies..."
DEPS_RESPONSE=$(curl -sf "$BASE_URL/api/deps?repo=$REPO_ID")

if echo "$DEPS_RESPONSE" | jq -e '.' > /dev/null 2>&1; then
    echo -e "${GREEN}✓ Deps returned valid JSON${NC}"
else
    echo -e "${RED}✗ Deps returned invalid JSON${NC}"
    exit 1
fi

# -------------------------------------------------------
# Step 7: Error handling tests
# -------------------------------------------------------
echo -e "${YELLOW}[7/7] Running error handling tests...${NC}"

# Test G: Search without query param returns 400
echo ""
echo "Test G: Search without query param..."
HTTP_CODE=$(curl -sf -w "%{http_code}" -o /dev/null "$BASE_URL/api/search" 2>/dev/null || echo "400")
if [ "$HTTP_CODE" = "400" ]; then
    echo -e "${GREEN}✓ Missing query param returns 400${NC}"
else
    echo -e "${RED}✗ Expected 400, got $HTTP_CODE${NC}"
    exit 1
fi

# Test H: Delete nonexistent repo returns 404
echo ""
echo "Test H: Delete nonexistent repo..."
HTTP_CODE=$(curl -sf -w "%{http_code}" -o /dev/null -X DELETE "$BASE_URL/api/repos/ghost" 2>/dev/null || echo "404")
if [ "$HTTP_CODE" = "404" ]; then
    echo -e "${GREEN}✓ Nonexistent repo returns 404${NC}"
else
    echo -e "${RED}✗ Expected 404, got $HTTP_CODE${NC}"
    exit 1
fi

# Test I: Webhook without signature returns 401
echo ""
echo "Test I: Webhook without signature..."
HTTP_CODE=$(curl -sf -w "%{http_code}" -o /dev/null \
    -X POST "$BASE_URL/api/webhook/$REPO_ID" \
    -H "Content-Type: application/json" -d '{}' 2>/dev/null || echo "401")
if [ "$HTTP_CODE" = "401" ]; then
    echo -e "${GREEN}✓ Unsigned webhook returns 401${NC}"
else
    echo -e "${RED}✗ Expected 401, got $HTTP_CODE${NC}"
    exit 1
fi

# Test J: Delete the registered repo
echo ""
echo "Test J: Deleting the repository..."
DELETE_CODE=$(curl -sf -w "%{http_code}" -o /dev/null -X DELETE "$BASE_URL/api/repos/$REPO_ID")
if [ "$DELETE_CODE" = "200" ]; then
    echo -e "${GREEN}✓ Repository deleted successfully${NC}"
else
    echo -e "${RED}✗ Expected 200, got $DELETE_CODE${NC}"
    exit 1
fi

# Verify it's gone
LIST_AFTER=$(curl -sf "$BASE_URL/api/repos")
if echo "$LIST_AFTER" | jq -e ".repositories[] | select(.id == \"$REPO_ID\")" > /dev/null 2>&1; then
    echo -e "${RED}✗ Repo still present after deletion${NC}"
    exit 1
else
    echo -e "${GREEN}✓ Repo no longer in listing after deletion${NC}"
fi

# -------------------------------------------------------
# Summary
# -------------------------------------------------------
echo ""
echo -e "${GREEN}========================================${NC}"
echo -e "${GREEN}All E2E tests passed!${NC}"
echo -e "${GREEN}========================================${NC}"
echo ""
echo "Validated features:"
echo "  ✓ Repository registration (POST /api/repos -> 202)"
echo "  ✓ Async git clone + indexing pipeline"
echo "  ✓ Semantic search (GET /api/search)"
echo "  ✓ Reverse dependency lookup (GET /api/callers)"
echo "  ✓ File exploration (GET /api/explore)"
echo "  ✓ Repository listing (GET /api/repos)"
echo "  ✓ Manual sync trigger (POST /api/repos/:id/sync -> 202)"
echo "  ✓ Repository dependencies (GET /api/deps)"
echo "  ✓ Error handling: missing params (400)"
echo "  ✓ Error handling: nonexistent repo (404)"
echo "  ✓ Error handling: unsigned webhook (401)"
echo "  ✓ Repository deletion (DELETE /api/repos/:id)"
echo ""

exit 0
```

#### 10.2.4 E2E Test Runner (`tests/run_all_e2e.sh`)

Orchestrates all E2E suites with Docker cleanup between runs:

```bash
#!/usr/bin/env bash
set -euo pipefail

BLUE='\033[0;34m'
GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[1;33m'
NC='\033[0m'

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$PROJECT_ROOT"

echo -e "${BLUE}========================================${NC}"
echo -e "${BLUE}knot-server E2E Test Suite${NC}"
echo -e "${BLUE}========================================${NC}"

FAILED_TESTS=()
PASSED_TESTS=()

run_test() {
    local test_name="$1"
    local test_script="$2"
    echo -e "\n${YELLOW}[Running: $test_name]${NC}"
    if "$PROJECT_ROOT/tests/$test_script"; then
        echo -e "${GREEN}✓ $test_name PASSED${NC}"
        PASSED_TESTS+=("$test_name")
    else
        echo -e "${RED}✗ $test_name FAILED${NC}"
        FAILED_TESTS+=("$test_name")
    fi
    # Cleanup between suites
    cd "$PROJECT_ROOT/tests"
    docker compose -f docker-compose.e2e.yml down -v 2>/dev/null || true
    sudo rm -rf .e2e_* 2>/dev/null || rm -rf .e2e_* 2>/dev/null || true
    sleep 3
    cd "$PROJECT_ROOT"
}

# Build once
echo -e "\n${YELLOW}Building knot-server...${NC}"
cargo build --release 2>&1 | grep -E "(Compiling|Finished)" || true

# Run suites
run_test "Happy Path: Index + Query" "run_e2e.sh"
# Future suites:
# run_test "Multi-Repo Indexing" "run_multi_repo_e2e.sh"
# run_test "Webhook Integration" "run_webhook_e2e.sh"
# run_test "Cluster Coordination" "run_cluster_e2e.sh"

# Summary
echo -e "\n${BLUE}========================================${NC}"
echo -e "${BLUE}E2E Summary${NC}"
echo -e "${BLUE}========================================${NC}"
for t in "${PASSED_TESTS[@]}"; do echo -e "  ${GREEN}✓${NC} $t"; done
if [ ${#FAILED_TESTS[@]} -gt 0 ]; then
    for t in "${FAILED_TESTS[@]}"; do echo -e "  ${RED}✗${NC} $t"; done
    exit 1
else
    echo -e "\n${GREEN}All E2E tests passed!${NC}"
fi
```

### 10.3 Test Coverage Goals

| Area                    | Unit | E2E | Priority |
|-------------------------|------|-----|----------|
| Registry CRUD           | Yes  | Yes | High     |
| File locking            | Yes  | —   | High     |
| Stale lock recovery     | Yes  | —   | High     |
| Git clone/pull          | Yes  | Yes | High     |
| Webhook validation      | Yes  | Yes | High     |
| Job queue ordering      | Yes  | —   | Medium   |
| Config construction     | Yes  | —   | Medium   |
| `POST /api/repos`       | Yes  | Yes | High     |
| `GET /api/repos`        | Yes  | Yes | High     |
| `DELETE /api/repos/:id` | Yes  | Yes | High     |
| `GET /api/search`       | Yes  | Yes | High     |
| `GET /api/callers`      | Yes  | Yes | High     |
| `GET /api/explore`      | Yes  | Yes | High     |
| `GET /api/deps`         | Yes  | Yes | Medium   |
| `POST /api/webhook/:id` | Yes  | Yes | High     |
| `POST /api/repos/:id/sync` | Yes | Yes | Medium |
| HTTP error codes (400, 401, 404, 409) | Yes | Yes | High |

### 10.4 Running Tests

```bash
# Unit tests only (no Docker required)
cargo test

# Single E2E suite (requires Docker)
./tests/run_e2e.sh

# All E2E suites
./tests/run_all_e2e.sh

# Unit tests + clippy + fmt (pre-commit)
cargo fmt -- --check && cargo clippy --all-targets -- -D warnings && cargo test
```

---

## 11. Implementation Phases

The project is divided into six incremental phases. Each phase produces a
working, testable binary. Later phases build on earlier ones but never break
what was already delivered. Every phase ends with passing unit tests and (from
Phase 2 onward) E2E tests.

### Phase 1 — Project Skeleton & Read Endpoints (MVP)

**Goal:** A running Axum server that can answer code queries against an
already-indexed Qdrant + Neo4j (indexed manually with `knot-indexer`).

**Deliverables:**

1. Initialize the Rust project (`cargo init`), set edition to 2024.
2. `Cargo.toml` with dependencies: `knot`, `axum`, `tokio`, `serde`,
   `serde_json`, `tracing`, `tracing-subscriber`, `clap`, `anyhow`.
3. Server configuration struct (`ServerConfig`) with `clap` + env var support
   for: port, Qdrant URL, Neo4j URI/user/password, workspace dir.
4. Startup lifecycle (steps 1-5 from Section 6): parse config, init logging,
   connect to Qdrant, connect to Neo4j, init Embedder.
5. Shared state (`AppState` with `Arc<VectorDb>`, `Arc<GraphDb>`,
   `Arc<Mutex<Embedder>>`).
6. Read-only endpoints:
   - `GET /api/search?q=...&repo=...&max_results=...`
   - `GET /api/callers?entity=...&repo=...`
   - `GET /api/explore?path=...&repo=...`
   - `GET /api/deps?repo=...&reverse=...&max_depth=...`
7. Proper error handling: 400 for missing params, 500 for internal errors.
   Structured JSON error responses.
8. Unit tests for: config construction, handler parameter validation (400s),
   `build_knot_config` helper.

**Acceptance criteria:**
- `cargo build --release` succeeds.
- `cargo test` passes.
- Server starts and responds to `GET /api/search?q=test` against a
  manually pre-indexed database.

---

### Phase 2 — Repository Registry & Git Operations

**Goal:** The server manages a list of repositories and can clone/pull them.

**Deliverables:**

1. Registry module (`registry.rs`): CRUD operations on `repos.json`.
   - `Registry::load_or_create()`, `add()`, `remove()`, `get()`, `list()`,
     `update_status()`, `update_last_indexed()`.
2. File locking module (`locking.rs`): `FileLock::try_acquire()` with RAII
   drop semantics. `repos.json.lock` for registry writes.
3. Git operations module (`git.rs`): `run_git_clone()`, `run_git_pull()` using
   `std::process::Command`. Support for both SSH and HTTPS+PAT auth.
4. REST endpoints:
   - `POST /api/repos` — Register repo, returns 202.
   - `GET /api/repos` — List all repos with status.
   - `GET /api/repos/:id` — Get single repo details.
   - `DELETE /api/repos/:id` — Remove repo + cleanup.
5. Unit tests for: registry CRUD, file locking (acquire/release/conflict),
   stale lock detection, git clone/pull with local bare repos, duplicate repo
   detection (409), delete nonexistent (404).
6. E2E test infrastructure: `docker-compose.e2e.yml`, test fixtures directory,
   `run_e2e.sh` skeleton with container lifecycle.
7. E2E tests: register a repo via `POST /api/repos`, verify it appears in
   `GET /api/repos`, delete it via `DELETE`, verify it's gone.

**Acceptance criteria:**
- `cargo test` passes (unit tests).
- `./tests/run_e2e.sh` passes: register, list, delete lifecycle.
- Git clone works against a local bare repo fixture.

---

### Phase 3 — Indexing Worker & Async Job Queue

**Goal:** Registered repositories are automatically cloned and indexed. The
server can receive indexing requests and process them sequentially.

**Deliverables:**

1. Job queue module (`worker.rs`): `IndexJob` enum, `mpsc` channel, single
   worker task consuming jobs sequentially.
2. `process_repository()` function (Section 4.3): acquires `.knot.lock`,
   runs git clone/pull, constructs `knot::config::Config` programmatically,
   loads `IndexState`, calls `run_indexing_pipeline`, updates registry.
3. Rayon thread pool configuration at startup (once, via `build_global()`).
4. Wire `POST /api/repos` to enqueue a `Clone` job after registration.
5. Wire `POST /api/repos/:id/sync` to enqueue a `Pull` job. Returns 202.
6. Unit tests for: job queue sequential ordering, config construction for
   knot, lock-skip behavior (job silently skipped when lock held).
7. E2E tests: full happy path — register repo, wait for status to become
   `idle` with non-null `last_indexed`, then run search/callers/explore
   queries against the indexed data and validate results.

**Acceptance criteria:**
- `cargo test` passes.
- `./tests/run_e2e.sh` passes the full lifecycle: register -> index -> query.
- A second `POST /api/repos/:id/sync` triggers incremental re-indexing
  without errors.

---

### Phase 4 — Webhook Support

**Goal:** External Git platforms (GitLab, GitHub, Bitbucket) can notify the
server of new commits, triggering automatic re-indexing.

**Deliverables:**

1. Webhook validation module (`webhook.rs`):
   - `validate_gitlab_token()` — constant-time comparison of `X-Gitlab-Token`.
   - `validate_github_signature()` — HMAC-SHA256 of body vs `X-Hub-Signature-256`.
   - `validate_bitbucket_signature()` — HMAC-SHA256 of body vs `X-Hub-Signature`.
2. `POST /api/webhook/:id` endpoint: validates signature, enqueues `Pull` job.
   Returns 401 on invalid signature, 404 on unknown repo, 202 on success.
3. Per-repo `webhook_secret` field in `repos.json` (set at registration or
   updated via a new `PATCH /api/repos/:id` endpoint).
4. Unit tests for: HMAC computation, signature validation (success, failure,
   malformed header), constant-time comparison.
5. E2E tests: send a webhook with valid signature -> verify 202, send with
   invalid signature -> verify 401, send to unknown repo -> verify 404.

**Acceptance criteria:**
- `cargo test` passes.
- E2E webhook tests pass.
- A simulated GitLab webhook (valid `X-Gitlab-Token`) triggers re-indexing.

---

### Phase 5 — Background Timer & Self-Healing

**Goal:** The server autonomously keeps all repositories up-to-date and
recovers from crashed indexing operations.

**Deliverables:**

1. Background timer task (`scheduler.rs`): `tokio::time::interval` that runs
   at a configurable period (default 24h, env `KNOT_SERVER_POLL_INTERVAL`).
2. Timer logic: iterates all repos in `repos.json`, for each:
   - Checks if `.knot.lock` exists and is stale (mtime > threshold).
   - If stale: removes the lock, logs a warning, enqueues a `Pull` job.
   - If `last_indexed` exceeds the configurable max age: enqueues a `Pull` job.
3. Stale lock threshold configuration (default 60 min, env
   `KNOT_SERVER_STALE_LOCK_TIMEOUT_SECS`).
4. `GET /api/health` endpoint returning: server uptime, queue depth, database
   connectivity status, number of managed repos, number currently indexing.
5. Unit tests for: stale lock detection logic, timer scheduling (mock clock),
   health response structure.
6. E2E tests: artificially create a stale `.knot.lock`, start server with a
   short timer interval (e.g., 5s), verify the lock is cleaned and the repo
   is re-indexed.

**Acceptance criteria:**
- `cargo test` passes.
- E2E stale-lock recovery test passes.
- `GET /api/health` returns valid JSON with all required fields.

---

### Phase 6 — Cluster Hardening & Production Readiness

**Goal:** The server is production-grade for multi-node deployment.

**Deliverables:**

1. `repos.json.lock` serialization for all registry writes (prevents lost
   updates when multiple nodes write concurrently).
2. Graceful shutdown: on `SIGTERM`/`SIGINT`, the server finishes the current
   indexing job (if any), releases all locks, then exits cleanly.
3. Configurable logging levels via `RUST_LOG` env var.
4. Dockerfile for the server binary (multi-stage build).
5. `docker-compose.yml` for local development: knot-server + Qdrant + Neo4j.
6. README.md with: quickstart, configuration reference, API documentation,
   deployment guide for cluster mode.
7. CI configuration (GitHub Actions): `cargo fmt`, `cargo clippy`, `cargo test`,
   `./tests/run_all_e2e.sh`.
8. E2E tests: simulate two servers writing to `repos.json` concurrently
   (verify no lost updates), test graceful shutdown mid-indexing.

**Acceptance criteria:**
- `cargo fmt -- --check` passes.
- `cargo clippy --all-targets -- -D warnings` passes.
- `cargo test` passes.
- `./tests/run_all_e2e.sh` passes all suites.
- Docker image builds and runs successfully.
- README.md is complete and accurate.

---

### Phase Summary

| Phase | Name                          | Key Outcome                                   | Depends On |
|-------|-------------------------------|-----------------------------------------------|------------|
| 1     | Skeleton & Read Endpoints     | Server answers code queries                   | —          |
| 2     | Registry & Git Operations     | Server manages repos, clones them              | Phase 1    |
| 3     | Indexing Worker & Job Queue   | Automatic indexing after registration          | Phase 2    |
| 4     | Webhook Support               | Real-time re-indexing on push                  | Phase 3    |
| 5     | Background Timer & Self-Heal  | Autonomous polling + crash recovery            | Phase 3    |
| 6     | Cluster Hardening & Prod      | Multi-node safe, Docker, CI, docs              | Phase 4, 5 |

Phases 4 and 5 are independent of each other and can be developed in parallel.
Phase 6 requires both 4 and 5 to be complete.
