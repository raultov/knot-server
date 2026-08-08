use std::path::{Path, PathBuf};
use std::sync::Arc;

use tracing::Instrument;

use crate::cleanup::CleanupScope;
use crate::locking::acquire_file_lock;
use crate::models::{IndexJob, RepoEntry};

/// The git-level operation the worker performs for a job, decided up front by
/// [`decide_job_plan`] instead of being inferred ad-hoc from the on-disk state.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum GitAction {
    /// Remote repo, start from a clean directory (`git clone`).
    FreshClone,
    /// Remote repo with an existing `.git`, incremental `git fetch`/reset.
    Pull,
    /// Local working-tree source, mirror it into the workspace.
    LocalSync,
}

/// The full plan for a job: whether to wipe existing artifacts (databases +
/// local directory) before the git action, and which git action to run.
///
/// Extracting this decision into a pure function makes the previously implicit
/// pull-vs-clone choice (which ignored the job type entirely) testable and
/// removes the race that let a background cleanup delete `local_path` while the
/// worker was mid-fetch.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct JobPlan {
    /// Delete databases + local directory before the git action.
    pub wipe_before: bool,
    pub action: GitAction,
}

/// Decide what a job should do based on its type and the current on-disk state.
///
/// Semantics (design doc §2.2):
/// - `Clone` means "start from scratch": always wipe, then fresh-clone (or
///   local-sync for a local source).
/// - `Pull` is incremental: pull when `.git` exists, but **fall back to a
///   fresh-clone** (with wipe) when the directory is gone, so a manual sync of
///   an errored repo without a directory recovers instead of failing with
///   "cannot pull".
/// - A local source always uses `LocalSync`; it only wipes for a `Clone`.
pub(crate) fn decide_job_plan(job: &IndexJob, git_dir_exists: bool, is_local: bool) -> JobPlan {
    if is_local {
        return JobPlan {
            wipe_before: matches!(job, IndexJob::Clone { .. }),
            action: GitAction::LocalSync,
        };
    }
    match job {
        IndexJob::Clone { .. } => JobPlan {
            wipe_before: true,
            action: GitAction::FreshClone,
        },
        IndexJob::Pull { .. } => {
            if git_dir_exists {
                JobPlan {
                    wipe_before: false,
                    action: GitAction::Pull,
                }
            } else {
                JobPlan {
                    wipe_before: true,
                    action: GitAction::FreshClone,
                }
            }
        }
    }
}

/// Whether a failed job should trigger a destructive wipe of the repo's
/// databases and local directory.
///
/// Policy (design doc §2.3, confirmed 2026-07-06): only wipe repos that never
/// indexed successfully. A repo that was already indexed and fails a
/// transient pull keeps its index and directory (recovery is still available by
/// re-registering, which enqueues a `Clone` = wipe + fresh).
pub(crate) fn should_wipe_on_failure(entry: &RepoEntry) -> bool {
    entry.last_indexed.is_none()
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum StateSource {
    LoadedOk { entries: usize, bytes: u64 },
    Missing,
    LegacyCleared,
    LoadErrorFallback { error: String },
}

pub(crate) struct LoadedState {
    pub state: knot::pipeline::state::IndexState,
    pub source: StateSource,
}

pub(crate) fn load_index_state_with_recovery(
    repo_path: &str,
    is_local: bool,
) -> anyhow::Result<LoadedState> {
    let state_file = Path::new(repo_path).join(".knot").join("index_state.json");

    if is_local && crate::local_sync::clear_stale_index_state(repo_path) {
        return Ok(LoadedState {
            state: knot::pipeline::state::IndexState::default(),
            source: StateSource::LegacyCleared,
        });
    }

    if !state_file.exists() {
        return Ok(LoadedState {
            state: knot::pipeline::state::IndexState::default(),
            source: StateSource::Missing,
        });
    }

    let bytes = std::fs::metadata(&state_file).map(|m| m.len()).unwrap_or(0);

    match knot::pipeline::state::IndexState::load(repo_path) {
        Ok(state) => {
            let entries = state.file_hashes.len();
            Ok(LoadedState {
                state,
                source: StateSource::LoadedOk { entries, bytes },
            })
        }
        Err(e) if is_local => {
            let _ = std::fs::remove_file(&state_file);
            Ok(LoadedState {
                state: knot::pipeline::state::IndexState::default(),
                source: StateSource::LoadErrorFallback {
                    error: format!("{e:#}"),
                },
            })
        }
        Err(e) => Err(e),
    }
}

#[expect(clippy::cognitive_complexity, reason = "deferred refactoring")]
#[expect(clippy::too_many_lines, reason = "deferred refactoring")]
pub async fn worker_loop(
    mut rx: tokio::sync::mpsc::Receiver<IndexJob>,
    state: Arc<crate::models::AppState>,
) {
    while let Some(job) = rx.recv().await {
        let repo_id = job.repo_id().to_string();
        tracing::info!("Worker picked up job: {:?} for {}", job, repo_id);

        let repo = {
            let mut registry = match state.registry.lock() {
                Ok(guard) => guard,
                Err(e) => {
                    tracing::error!("Registry lock poisoned: {}", e);
                    continue;
                }
            };
            match registry.get(&repo_id) {
                Some(entry) => entry.clone(),
                None => {
                    tracing::error!(
                        "Repository '{}' not found in registry, skipping job",
                        repo_id
                    );
                    continue;
                }
            }
        };

        let new_status = match &job {
            IndexJob::Clone { .. } => crate::models::RepoStatus::Cloning,
            IndexJob::Pull { .. } => crate::models::RepoStatus::Pulling,
        };
        {
            let mut registry = match state.registry.lock() {
                Ok(guard) => guard,
                Err(e) => {
                    tracing::error!("Registry lock poisoned during status claim: {}", e);
                    continue;
                }
            };
            match registry.transition_status(
                &repo_id,
                &[crate::models::RepoStatus::Queued],
                new_status,
            ) {
                Ok(true) => {}
                Ok(false) => {
                    tracing::info!(
                        "Dropping stale job {:?} for '{}' — repo no longer Queued",
                        job,
                        repo_id
                    );
                    continue;
                }
                Err(e) => {
                    tracing::error!("Failed to claim job for '{}': {e}", repo_id);
                    continue;
                }
            }
        }

        // Root span for the whole job. Jobs are consumed off an mpsc queue, so
        // this is a *separate trace* from the HTTP request that enqueued the
        // job (context propagation via IndexJob is future work).
        let kind_str = match &job {
            IndexJob::Clone { .. } => "clone",
            IndexJob::Pull { .. } => "pull",
        };
        let job_span = tracing::info_span!(
            "indexing_job",
            repo_id = %repo_id,
            kind = kind_str,
            otel.kind = "internal",
            result = tracing::field::Empty,
            otel.status_code = tracing::field::Empty,
        );

        let start = std::time::Instant::now();
        let result = process_repository(&repo, &state, &job)
            .instrument(job_span.clone())
            .await;
        let ok = result.is_ok();
        job_span.record("result", if ok { "ok" } else { "err" });
        if let Err(e) = &result {
            job_span.record("otel.status_code", "ERROR");
            tracing::error!("Indexing failed for {}: {e:#}", repo.id);
            handle_job_failure(&state, &repo).await;
        }
        let kind = match &job {
            IndexJob::Clone { .. } => crate::metrics::JobKind::Clone,
            IndexJob::Pull { .. } => crate::metrics::JobKind::Pull,
        };
        crate::metrics::record_indexing_job(&repo_id, kind, ok, start.elapsed());
    }
}

/// Handle a failed indexing job: always mark the repo `Error` and drop its
/// progress snapshot + in-memory tracker; additionally wipe databases and the
/// local directory when the repo never indexed successfully (see
/// [`should_wipe_on_failure`]). Called from [`worker_loop`] after
/// `process_repository` has returned (i.e. the file lock is already released).
pub(crate) async fn handle_job_failure(state: &Arc<crate::models::AppState>, repo: &RepoEntry) {
    {
        let mut registry = match state.registry.lock() {
            Ok(guard) => guard,
            Err(e) => {
                tracing::error!("Registry lock poisoned during error handling: {}", e);
                return;
            }
        };
        let _ = registry.update_status(&repo.id, crate::models::RepoStatus::Error);
    }

    let wipe = should_wipe_on_failure(repo);
    if wipe {
        tracing::warn!(
            "Job for '{}' failed and it was never indexed: wiping databases + local dir",
            repo.id
        );
    } else {
        tracing::info!(
            "Job for '{}' failed but it was previously indexed: keeping index + local dir",
            repo.id
        );
    }
    crate::cleanup::cleanup_repo_artifacts(
        state,
        &repo.id,
        CleanupScope {
            databases: wipe,
            local_dir: wipe,
            progress: true,
        },
    )
    .await;
}

#[expect(clippy::cognitive_complexity, reason = "deferred refactoring")]
#[expect(clippy::too_many_lines, reason = "deferred refactoring")]
async fn process_repository(
    repo: &RepoEntry,
    state: &Arc<crate::models::AppState>,
    job: &IndexJob,
) -> anyhow::Result<()> {
    // 1. Acquire exclusive file lock
    let lock_path = PathBuf::from(&repo.local_path).join(".knot.lock");
    let _lock = match acquire_file_lock(&lock_path) {
        Ok(lock) => {
            tracing::info!("Worker: acquired file lock for '{}'", repo.id);
            lock
        }
        Err(_) => {
            tracing::info!("Worker: '{}' locked by another node, skipping", repo.id);
            return Ok(());
        }
    };

    // 2. Decide what to do from the job type + on-disk state. The worker is the
    //    sole owner of destructive cleanup: any wipe happens here, under the
    //    file lock, serialized with the git/index work — this is what removes
    //    the re-registration race (a background task can no longer delete
    //    `local_path` mid-fetch).
    let is_local = crate::local_sync::is_local_path(&repo.url);
    let exists = Path::new(&repo.local_path).join(".git").exists();
    let plan = decide_job_plan(job, exists, is_local);
    tracing::info!("Worker: job plan for '{}': {:?}", repo.id, plan);

    if plan.wipe_before {
        tracing::info!(
            "Worker: wiping databases + local dir for '{}' before {:?} (job {:?})",
            repo.id,
            plan.action,
            job
        );
        crate::cleanup::cleanup_repo_artifacts(
            state,
            &repo.id,
            CleanupScope {
                databases: true,
                local_dir: true,
                progress: false,
            },
        )
        .await;
    }

    // 3. Execute the git phase. Status was already set to Cloning/Pulling by
    //    worker_loop via the Queued transition.
    match plan.action {
        GitAction::LocalSync => {
            tracing::info!(
                "Worker: syncing local working tree for '{}' from {}",
                repo.id,
                repo.url
            );
            let src = repo.url.clone();
            let dst = repo.local_path.clone();
            tokio::task::spawn_blocking(move || {
                crate::local_sync::sync_local_working_tree(&src, &dst)
            })
            .instrument(tracing::info_span!("git_sync"))
            .await??;
            tracing::info!("Worker: local sync complete for '{}'", repo.id);
        }
        GitAction::Pull => {
            tracing::info!("Worker: pulling '{}' from {}", repo.id, repo.url);
            crate::git::run_git_pull(repo)
                .instrument(tracing::info_span!("git_pull"))
                .await?;
            tracing::info!("Worker: pull complete for '{}'", repo.id);
        }
        GitAction::FreshClone => {
            tracing::info!("Worker: cloning '{}' from {}", repo.id, repo.url);
            crate::git::run_git_clone(repo)
                .instrument(tracing::info_span!("git_clone"))
                .await?;
            tracing::info!("Worker: clone complete for '{}'", repo.id);
        }
    }

    // 3. Update status to indexing
    {
        let mut registry = state
            .registry
            .lock()
            .map_err(|e| anyhow::anyhow!("Registry lock poisoned: {}", e))?;
        registry.update_status(&repo.id, crate::models::RepoStatus::Indexing)?;
        tracing::info!("Worker: status=indexing for '{}'", repo.id);
    }

    // 4. Build knot Config programmatically
    let knot_cfg = knot::config::Config {
        repo_path: repo.local_path.clone(),
        repo_name: repo.id.clone(),
        qdrant_url: state.qdrant_url.clone(),
        qdrant_collection: state.qdrant_collection.clone(),
        neo4j_uri: state.neo4j_uri.clone(),
        neo4j_user: state.neo4j_user.clone(),
        neo4j_password: state.neo4j_password.clone(),
        custom_queries_path: None,
        embed_dim: state.embed_dim,
        batch_size: state.batch_size,
        clean: false,
        dependency_repos: Vec::new(),
        watch: false,
        dry_run: false,
        custom_ca_certs: None,
        output_format: knot::config::OutputFormat::Markdown,
        ingest_concurrency: state.ingest_concurrency,
        rayon_threads: state.rayon_threads,
        include_config_files: false,
        embedder_reset_interval: 0,
    };

    // 5. Load IndexState
    //    For local paths, defend against a stale on-disk state file from an
    //    older `knot` version (no `version` field → version=0 < current).
    //    `local_sync` preserves `.knot/` across syncs (both copy_tree skips it
    //    and prune_tree explicitly protects it) because it is the indexer's
    //    incremental state, so a knot-version transition would otherwise block
    //    every future sync. Clear the stale file and, if load still fails for
    //    any other reason, fall back to a fresh state rather than failing the
    //    whole local sync job.
    let loaded = load_index_state_with_recovery(&repo.local_path, is_local)?;
    match &loaded.source {
        StateSource::LoadedOk { entries, bytes } => {
            tracing::info!(
                "IndexState loaded for '{}' ({} entries, {} bytes on disk)",
                repo.id,
                entries,
                bytes
            );
        }
        StateSource::Missing => {
            tracing::info!(
                "IndexState file absent for '{}' — full indexing will run",
                repo.id
            );
        }
        StateSource::LegacyCleared => {
            tracing::warn!(
                "Removed stale .knot/index_state.json for local repo '{}' \
                 (older knot format); the next pipeline run will do a clean re-index",
                repo.id
            );
        }
        StateSource::LoadErrorFallback { error } => {
            tracing::warn!(
                "IndexState::load failed for local repo '{}': {}; \
                 removed the file and forcing full re-index",
                repo.id,
                error
            );
        }
    }
    let mut index_state = loaded.state;

    // 6. Run the indexing pipeline with progress tracking
    let tracker = {
        let mut map = state
            .progress_trackers
            .lock()
            .map_err(|e| anyhow::anyhow!("Progress tracker lock poisoned: {}", e))?;
        Arc::clone(map.entry(repo.id.clone()).or_default())
    };

    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let persister =
        spawn_progress_persister(state, repo.id.clone(), Arc::clone(&tracker), cancel.clone());

    tracing::info!("Worker: starting indexing pipeline for '{}'", repo.id);
    // The knot pipeline runs parse → embed → graph-ingest internally as one
    // call; a single child span captures the whole indexing phase (finer-grained
    // per-stage spans would require instrumenting the `knot` library — future
    // work, see TRACING_PLAN §4.1/Future work).
    let pipeline_result = knot::pipeline::runner::run_indexing_pipeline_with_progress(
        &knot_cfg,
        &state.vector_db,
        &state.graph_db,
        &mut index_state,
        tracker,
    )
    .instrument(tracing::info_span!("index_pipeline"))
    .await;

    cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    if let Some(handle) = persister {
        handle.await.ok();
    }

    crate::progress_store::remove_snapshot(&PathBuf::from(&state.workspace_dir), &repo.id);

    pipeline_result?;
    tracing::info!("Worker: indexing pipeline complete for '{}'", repo.id);

    // 7. Update registry
    {
        let mut registry = state
            .registry
            .lock()
            .map_err(|e| anyhow::anyhow!("Registry lock poisoned: {}", e))?;
        registry.update_status(&repo.id, crate::models::RepoStatus::Indexed)?;
        registry.update_last_indexed(&repo.id)?;
        crate::metrics::set_last_success(&repo.id);
        tracing::info!("Worker: status=indexed for '{}'", repo.id);
    }

    tracing::info!("Worker: job completed for '{}'", repo.id);
    Ok(())
}

fn spawn_progress_persister(
    state: &Arc<crate::models::AppState>,
    repo_id: String,
    tracker: Arc<knot::pipeline::progress::ProgressTracker>,
    cancel: Arc<std::sync::atomic::AtomicBool>,
) -> Option<tokio::task::JoinHandle<()>> {
    use std::sync::atomic::Ordering;
    use std::time::Duration;
    let workspace = PathBuf::from(&state.workspace_dir);
    Some(tokio::spawn(async move {
        let mut last_signature: Option<String> = None;
        while !cancel.load(Ordering::Relaxed) {
            let snap = tracker.snapshot();
            let stage = crate::progress_store::format_stage(snap.stage);
            crate::metrics::set_indexing_progress(&repo_id, &stage, &snap);
            let signature = format!(
                "{:?}|{:.3}|{}|{}|{}|{}",
                snap.stage,
                snap.percent_complete,
                snap.parsed_files,
                snap.total_files,
                snap.entities_ingested,
                snap.batches_ingested
            );
            if last_signature.as_deref() != Some(signature.as_str()) {
                let persisted = crate::progress_store::PersistedProgress::from_tracker(
                    &repo_id,
                    crate::models::RepoStatus::Indexing,
                    &snap,
                );
                if let Err(e) = crate::progress_store::write_snapshot(&workspace, &persisted) {
                    tracing::warn!(
                        "Worker: failed to persist progress snapshot for {}: {e:#}",
                        repo_id
                    );
                } else {
                    last_signature = Some(signature);
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }))
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{AuthType, RepoStatus};
    use crate::registry::Registry;
    use knot::db::graph::ConnectExt;
    use knot::db::vector::VectorConnectExt;
    use std::collections::HashMap;
    use std::sync::Mutex;
    use tempfile::TempDir;

    async fn create_test_state(workspace: &Path) -> Arc<crate::models::AppState> {
        let registry = Registry::load_or_create(workspace).unwrap();

        let graph_db =
            knot::db::graph::GraphDb::connect("bolt://localhost:9999", "neo4j", "badpassword")
                .await
                .expect("connect for test db");
        let vector_db =
            knot::db::vector::VectorDb::connect("http://localhost:9999", "test_collection", 384)
                .await
                .expect("connect for test vector db");
        Arc::new(crate::models::AppState {
            vector_db: Arc::new(vector_db),
            graph_db: Arc::new(graph_db),
            embedder: None,
            workspace_dir: workspace.to_string_lossy().into(),
            registry: Arc::new(Mutex::new(registry)),
            job_tx: tokio::sync::mpsc::channel(16).0,
            qdrant_url: "http://localhost:6334".into(),
            qdrant_collection: "knot_entities".into(),
            neo4j_uri: "bolt://localhost:7687".into(),
            neo4j_user: "neo4j".into(),
            neo4j_password: "secret".into(),
            embed_dim: 384,
            rayon_threads: None,
            batch_size: 64,
            ingest_concurrency: 4,
            start_time: std::time::Instant::now(),
            progress_trackers: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    #[tokio::test]
    async fn test_job_queue_processes_sequentially() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<IndexJob>(16);
        let order = Arc::new(Mutex::new(Vec::new()));

        let order_clone = order.clone();
        let handle = tokio::spawn(async move {
            while let Some(job) = rx.recv().await {
                order_clone.lock().unwrap().push(job.repo_id().to_string());
            }
        });

        tx.send(IndexJob::Pull {
            repo_id: "a".into(),
        })
        .await
        .unwrap();
        tx.send(IndexJob::Pull {
            repo_id: "b".into(),
        })
        .await
        .unwrap();
        tx.send(IndexJob::Pull {
            repo_id: "c".into(),
        })
        .await
        .unwrap();
        drop(tx);

        handle.await.unwrap();
        let processed = order.lock().unwrap();
        assert_eq!(*processed, vec!["a", "b", "c"]);
    }

    #[tokio::test]
    async fn test_job_queue_skips_locked_repos() {
        let dir = TempDir::new().unwrap();
        let lock_path = dir.path().join(".knot.lock");

        // Acquire a lock first
        let _held_lock = acquire_file_lock(&lock_path).unwrap();

        // Try to acquire again — should fail gracefully
        let result = acquire_file_lock(&lock_path);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_process_repository_nonexistent_skips() {
        let dir = TempDir::new().unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();

        let state = create_test_state(&workspace).await;

        let repo = RepoEntry {
            id: "nonexistent".into(),
            url: "https://invalid.example.com/nonexistent.git".into(),
            local_path: workspace.join("nonexistent").to_string_lossy().into(),
            auth_type: AuthType::Ssh,
            branch: "main".into(),
            webhook_secret: None,
            last_indexed: None,
            status: RepoStatus::Indexed,
        };

        // Should fail during git clone but not panic
        let result = process_repository(
            &repo,
            &state,
            &IndexJob::Clone {
                repo_id: "nonexistent".into(),
            },
        )
        .await;
        // The error is expected — we don't have a real git remote
        // The test verifies the function runs without panicking
        assert!(result.is_err());
    }

    #[test]
    fn test_load_state_returns_loaded_ok_when_state_is_valid() {
        let dir = TempDir::new().unwrap();
        let repo_path = dir.path().to_str().unwrap();
        let knot_dir = dir.path().join(".knot");
        std::fs::create_dir_all(&knot_dir).unwrap();
        let raw = r#"{"version":4,"file_hashes":{"a.rs":"h1","b.rs":"h2"}}"#;
        std::fs::write(knot_dir.join("index_state.json"), raw).unwrap();

        let loaded = load_index_state_with_recovery(repo_path, true).unwrap();

        match loaded.source {
            StateSource::LoadedOk { entries, bytes } => {
                assert_eq!(entries, 2);
                assert!(bytes > 0);
            }
            other => panic!("expected LoadedOk, got {other:?}"),
        }
        assert_eq!(loaded.state.file_hashes.len(), 2);
    }

    #[test]
    fn test_load_state_returns_missing_when_state_absent() {
        let dir = TempDir::new().unwrap();
        let loaded = load_index_state_with_recovery(dir.path().to_str().unwrap(), true).unwrap();

        assert!(matches!(loaded.source, StateSource::Missing));
        assert!(loaded.state.file_hashes.is_empty());
    }

    #[test]
    fn test_load_state_returns_legacy_cleared_for_local_repo_with_v0_state() {
        let dir = TempDir::new().unwrap();
        let knot_dir = dir.path().join(".knot");
        std::fs::create_dir_all(&knot_dir).unwrap();
        let raw = r#"{"file_hashes":{"a.rs":"h1"}}"#;
        std::fs::write(knot_dir.join("index_state.json"), raw).unwrap();

        let loaded = load_index_state_with_recovery(dir.path().to_str().unwrap(), true).unwrap();

        assert!(matches!(loaded.source, StateSource::LegacyCleared));
        assert!(loaded.state.file_hashes.is_empty());
        assert!(
            !knot_dir.join("index_state.json").exists(),
            "El archivo legacy debe haberse eliminado"
        );
    }

    #[test]
    fn test_load_state_returns_error_fallback_when_json_is_corrupt() {
        let dir = TempDir::new().unwrap();
        let knot_dir = dir.path().join(".knot");
        std::fs::create_dir_all(&knot_dir).unwrap();
        let raw = r#"{"version":4,"file_hashes":NOT_VALID_JSON}"#;
        std::fs::write(knot_dir.join("index_state.json"), raw).unwrap();

        let loaded = load_index_state_with_recovery(dir.path().to_str().unwrap(), true).unwrap();

        match loaded.source {
            StateSource::LoadErrorFallback { error } => {
                assert!(!error.is_empty());
            }
            other => panic!("expected LoadErrorFallback, got {other:?}"),
        }
        assert!(loaded.state.file_hashes.is_empty());
        assert!(
            !knot_dir.join("index_state.json").exists(),
            "El archivo corrupto debe haberse eliminado para no atascar al siguiente run"
        );
    }

    #[test]
    fn test_load_state_for_remote_repo_propagates_errors() {
        let dir = TempDir::new().unwrap();
        let knot_dir = dir.path().join(".knot");
        std::fs::create_dir_all(&knot_dir).unwrap();
        let raw = r#"{"version":1,"file_hashes":{}}"#;
        std::fs::write(knot_dir.join("index_state.json"), raw).unwrap();

        let result = load_index_state_with_recovery(dir.path().to_str().unwrap(), false);

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_process_repository_adds_tracker_to_state() {
        let dir = TempDir::new().unwrap();

        let src_dir = dir.path().join("src-repo");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::create_dir_all(src_dir.join(".git")).unwrap();
        std::fs::create_dir_all(src_dir.join(".knot")).unwrap();
        let raw = r#"{"version":4,"file_hashes":{"a.rs":"h1"}}"#;
        std::fs::write(src_dir.join(".knot").join("index_state.json"), raw).unwrap();

        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();

        let state = create_test_state(&workspace).await;

        let repo = RepoEntry {
            id: "test-repo".into(),
            url: src_dir.to_string_lossy().into(),
            local_path: workspace.join("test-repo").to_string_lossy().into(),
            auth_type: AuthType::Ssh,
            branch: "main".into(),
            webhook_secret: None,
            last_indexed: None,
            status: RepoStatus::Indexed,
        };

        state
            .registry
            .lock()
            .unwrap()
            .add_or_replace(repo.clone())
            .unwrap();

        let _ = process_repository(
            &repo,
            &state,
            &IndexJob::Pull {
                repo_id: "test-repo".into(),
            },
        )
        .await;

        let map = state.progress_trackers.lock().unwrap();
        assert!(
            map.contains_key("test-repo"),
            "tracker was not added to state.progress_trackers"
        );
    }
    #[tokio::test]
    async fn test_spawn_progress_persister_writes_and_exits() {
        let dir = TempDir::new().unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let state = create_test_state(&workspace).await;

        let repo_id = "progress-repo".to_string();
        let tracker = Arc::new(knot::pipeline::progress::ProgressTracker::new());
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));

        let handle =
            spawn_progress_persister(&state, repo_id.clone(), tracker.clone(), cancel.clone())
                .unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(150)).await;

        let progress_file = workspace.join("progress").join("progress-repo.json");
        assert!(
            progress_file.exists(),
            "Snapshot file should have been written"
        );

        cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        handle.await.unwrap();
    }

    // ---- Pure decision functions (Bug A / issue #7) ----

    #[test]
    fn test_decide_plan_clone_job_always_fresh_clone_even_if_git_exists() {
        let job = IndexJob::Clone {
            repo_id: "x".into(),
        };
        let plan = decide_job_plan(&job, true, false);
        assert_eq!(
            plan,
            JobPlan {
                wipe_before: true,
                action: GitAction::FreshClone
            }
        );
    }

    #[test]
    fn test_decide_plan_clone_job_on_missing_dir_is_fresh_clone() {
        let job = IndexJob::Clone {
            repo_id: "x".into(),
        };
        let plan = decide_job_plan(&job, false, false);
        assert_eq!(
            plan,
            JobPlan {
                wipe_before: true,
                action: GitAction::FreshClone
            }
        );
    }

    #[test]
    fn test_decide_plan_pull_job_with_git_dir_pulls_without_wipe() {
        let job = IndexJob::Pull {
            repo_id: "x".into(),
        };
        let plan = decide_job_plan(&job, true, false);
        assert_eq!(
            plan,
            JobPlan {
                wipe_before: false,
                action: GitAction::Pull
            }
        );
    }

    #[test]
    fn test_decide_plan_pull_job_without_git_dir_falls_back_to_fresh_clone() {
        let job = IndexJob::Pull {
            repo_id: "x".into(),
        };
        let plan = decide_job_plan(&job, false, false);
        assert_eq!(
            plan,
            JobPlan {
                wipe_before: true,
                action: GitAction::FreshClone
            }
        );
    }

    #[test]
    fn test_decide_plan_local_repo_clone_job_wipes_and_syncs() {
        let job = IndexJob::Clone {
            repo_id: "x".into(),
        };
        // is_local=true: git_dir_exists is irrelevant.
        for git_dir_exists in [true, false] {
            let plan = decide_job_plan(&job, git_dir_exists, true);
            assert_eq!(
                plan,
                JobPlan {
                    wipe_before: true,
                    action: GitAction::LocalSync
                }
            );
        }
    }

    #[test]
    fn test_decide_plan_local_repo_pull_job_syncs_without_wipe() {
        let job = IndexJob::Pull {
            repo_id: "x".into(),
        };
        for git_dir_exists in [true, false] {
            let plan = decide_job_plan(&job, git_dir_exists, true);
            assert_eq!(
                plan,
                JobPlan {
                    wipe_before: false,
                    action: GitAction::LocalSync
                }
            );
        }
    }

    #[test]
    fn test_should_wipe_on_failure_true_when_never_indexed() {
        let entry = RepoEntry {
            id: "x".into(),
            url: "https://example.com/x.git".into(),
            local_path: "/tmp/x".into(),
            auth_type: AuthType::Ssh,
            branch: "main".into(),
            webhook_secret: None,
            last_indexed: None,
            status: RepoStatus::Error,
        };
        assert!(should_wipe_on_failure(&entry));
    }

    #[test]
    fn test_should_wipe_on_failure_false_when_previously_indexed() {
        let entry = RepoEntry {
            id: "x".into(),
            url: "https://example.com/x.git".into(),
            local_path: "/tmp/x".into(),
            auth_type: AuthType::Ssh,
            branch: "main".into(),
            webhook_secret: None,
            last_indexed: Some("2026-07-06T00:00:00Z".into()),
            status: RepoStatus::Error,
        };
        assert!(!should_wipe_on_failure(&entry));
    }

    // ---- Worker integration (TempDir + local bare repo) ----

    /// Create a local bare git repo with one commit on `main` and return its
    /// path. A bare repo is classified as *remote* by `is_local_path`
    /// (`is_bare_git_repo` short-circuits), so the worker exercises the git
    /// clone/pull branch rather than local-sync.
    fn create_bare_repo(dir: &Path) -> String {
        use std::process::Command as Cmd;
        let bare = dir.join("bare.git");
        Cmd::new("git")
            .args(["init", "--bare", "-b", "main"])
            .arg(&bare)
            .output()
            .unwrap();

        let seed = dir.join("seed");
        Cmd::new("git")
            .args(["clone"])
            .arg(&bare)
            .arg(&seed)
            .output()
            .unwrap();
        Cmd::new("git")
            .args(["checkout", "-b", "main"])
            .current_dir(&seed)
            .output()
            .unwrap();
        std::fs::write(seed.join("README.md"), "# seed\n").unwrap();
        Cmd::new("git")
            .args(["add", "."])
            .current_dir(&seed)
            .output()
            .unwrap();
        Cmd::new("git")
            .args([
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-m",
                "init",
            ])
            .current_dir(&seed)
            .output()
            .unwrap();
        Cmd::new("git")
            .args(["push", "origin", "main"])
            .current_dir(&seed)
            .output()
            .unwrap();
        bare.to_string_lossy().into_owned()
    }

    #[tokio::test]
    async fn test_process_clone_job_wipes_existing_local_dir_before_git_action() {
        let dir = TempDir::new().unwrap();
        let bare_url = create_bare_repo(dir.path());
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let state = create_test_state(&workspace).await;

        let local_path = workspace.join("clone-wipe");
        // Pre-populate the local dir with a fake .git and a STALE marker that
        // does not exist in origin.
        std::fs::create_dir_all(local_path.join(".git")).unwrap();
        std::fs::write(local_path.join("STALE.txt"), b"stale").unwrap();

        let repo = RepoEntry {
            id: "clone-wipe".into(),
            url: bare_url,
            local_path: local_path.to_string_lossy().into(),
            auth_type: AuthType::Ssh,
            branch: "main".into(),
            webhook_secret: None,
            last_indexed: None,
            status: RepoStatus::Indexed,
        };
        state
            .registry
            .lock()
            .unwrap()
            .add_or_replace(repo.clone())
            .unwrap();

        // The pipeline will fail against the stub DBs, but the git phase (wipe
        // + fresh clone) runs first and is what we assert on.
        let _ = process_repository(
            &repo,
            &state,
            &IndexJob::Clone {
                repo_id: "clone-wipe".into(),
            },
        )
        .await;

        assert!(
            !local_path.join("STALE.txt").exists(),
            "STALE marker must be wiped and not restored by the fresh clone"
        );
        assert!(
            local_path.join("README.md").exists(),
            "fresh clone must have repopulated the working tree from origin"
        );
    }

    #[tokio::test]
    async fn test_process_pull_job_with_missing_dir_does_fresh_clone_instead_of_failing() {
        let dir = TempDir::new().unwrap();
        let bare_url = create_bare_repo(dir.path());
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let state = create_test_state(&workspace).await;

        let local_path = workspace.join("pull-fallback");
        // No local directory at all.
        assert!(!local_path.exists());

        let repo = RepoEntry {
            id: "pull-fallback".into(),
            url: bare_url,
            local_path: local_path.to_string_lossy().into(),
            auth_type: AuthType::Ssh,
            branch: "main".into(),
            webhook_secret: None,
            last_indexed: None,
            status: RepoStatus::Error,
        };
        state
            .registry
            .lock()
            .unwrap()
            .add_or_replace(repo.clone())
            .unwrap();

        let result = process_repository(
            &repo,
            &state,
            &IndexJob::Pull {
                repo_id: "pull-fallback".into(),
            },
        )
        .await;

        // The only acceptable failure is a downstream (DB/pipeline) error, never
        // the "cannot pull" bail from run_git_pull.
        if let Err(e) = &result {
            let msg = format!("{e:#}");
            assert!(
                !msg.contains("cannot pull"),
                "Pull on a missing dir must fall back to fresh-clone, got: {msg}"
            );
        }
        assert!(
            local_path.join(".git").exists(),
            "fallback fresh-clone must have created the local repo"
        );
    }

    #[tokio::test]
    async fn test_worker_failure_on_never_indexed_repo_cleans_dir_and_snapshot_but_keeps_entry() {
        let dir = TempDir::new().unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let state = create_test_state(&workspace).await;

        let repo = RepoEntry {
            id: "gamma".into(),
            url: "https://invalid.example/x.git".into(),
            local_path: crate::models::repo_local_path(&state.workspace_dir, "gamma"),
            auth_type: AuthType::Ssh,
            branch: "main".into(),
            webhook_secret: None,
            last_indexed: None,
            status: RepoStatus::Indexing,
        };
        state
            .registry
            .lock()
            .unwrap()
            .add_or_replace(repo.clone())
            .unwrap();

        // Partial local directory + a persisted progress snapshot + tracker.
        std::fs::create_dir_all(&repo.local_path).unwrap();
        std::fs::write(Path::new(&repo.local_path).join("partial"), b"x").unwrap();
        let persisted = crate::progress_store::PersistedProgress {
            repo_id: "gamma".into(),
            status: RepoStatus::Indexing,
            stage: "parsing".into(),
            total_files: 0,
            parsed_files: 0,
            percent_complete: 0.0,
            entities_ingested: 0,
            batches_ingested: 0,
            error: None,
            updated_at: crate::time_utils::chrono_now(),
        };
        crate::progress_store::write_snapshot(&workspace, &persisted).unwrap();
        state.progress_trackers.lock().unwrap().insert(
            "gamma".into(),
            Arc::new(knot::pipeline::progress::ProgressTracker::new()),
        );

        handle_job_failure(&state, &repo).await;

        {
            let mut registry = state.registry.lock().unwrap();
            let entry = registry
                .get("gamma")
                .expect("entry must remain in registry");
            assert_eq!(entry.status, RepoStatus::Error);
        }
        assert!(
            !Path::new(&repo.local_path).exists(),
            "local dir must be removed for a never-indexed failure"
        );
        assert!(
            !crate::progress_store::snapshot_path(&workspace, "gamma").exists(),
            "progress snapshot must be removed"
        );
        assert!(
            !state
                .progress_trackers
                .lock()
                .unwrap()
                .contains_key("gamma"),
            "in-memory tracker must be removed"
        );
    }

    async fn create_test_state_with_rx(
        workspace: &Path,
    ) -> (
        Arc<crate::models::AppState>,
        tokio::sync::mpsc::Receiver<IndexJob>,
    ) {
        let registry = Registry::load_or_create(workspace).unwrap();
        let graph_db =
            knot::db::graph::GraphDb::connect("bolt://localhost:9999", "neo4j", "badpassword")
                .await
                .expect("connect for test db");
        let vector_db =
            knot::db::vector::VectorDb::connect("http://localhost:9999", "test_collection", 384)
                .await
                .expect("connect for test vector db");
        let (job_tx, job_rx) = tokio::sync::mpsc::channel::<IndexJob>(16);
        (
            Arc::new(crate::models::AppState {
                vector_db: Arc::new(vector_db),
                graph_db: Arc::new(graph_db),
                embedder: None,
                workspace_dir: workspace.to_string_lossy().into(),
                registry: Arc::new(Mutex::new(registry)),
                job_tx,
                qdrant_url: "http://localhost:6334".into(),
                qdrant_collection: "knot_entities".into(),
                neo4j_uri: "bolt://localhost:7687".into(),
                neo4j_user: "neo4j".into(),
                neo4j_password: "secret".into(),
                embed_dim: 384,
                rayon_threads: None,
                batch_size: 64,
                ingest_concurrency: 4,
                start_time: std::time::Instant::now(),
                progress_trackers: Arc::new(Mutex::new(HashMap::new())),
            }),
            job_rx,
        )
    }

    /// Fase 3 BDD: a registration whose clone fails can be fully recovered by
    /// re-registering with a corrected URL that derives the same id.
    #[tokio::test]
    async fn test_failed_registration_can_be_recovered_by_reregistering() {
        use crate::models::RegisterRepoRequest;
        use axum::Json;
        use axum::extract::State;
        use axum::http::StatusCode;

        let dir = TempDir::new().unwrap();
        let bare_url = create_bare_repo(dir.path()); // .../bare.git → id "bare"
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let (state, mut job_rx) = create_test_state_with_rx(&workspace).await;

        let broken_url = "https://invalid.example/bare.git";
        let mk_req = |url: &str| RegisterRepoRequest {
            url: url.into(),
            auth_type: AuthType::Ssh,
            branch: "main".into(),
            webhook_secret: None,
        };
        let id = mk_req(broken_url).generate_id();
        assert_eq!(id, "bare");
        assert_eq!(mk_req(&bare_url).generate_id(), id, "URLs must share an id");

        // 1. Register with the broken URL → 202, one Clone job.
        let resp = crate::handlers::repo::register_repo_handler(
            State(state.clone()),
            Json(mk_req(broken_url)),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
        let job = job_rx.recv().await.expect("Clone job enqueued");

        // 2. Worker processes and fails; failure handling wipes + keeps entry.
        let repo = state.registry.lock().unwrap().get(&id).unwrap().clone();
        let res = process_repository(&repo, &state, &job).await;
        assert!(res.is_err(), "clone of a broken URL must fail");
        handle_job_failure(&state, &repo).await;

        assert_eq!(
            state.registry.lock().unwrap().get(&id).unwrap().status,
            RepoStatus::Error
        );
        assert!(
            !Path::new(&repo.local_path).exists(),
            "never-indexed failure must leave no local dir"
        );

        // 3. Re-register with the corrected URL (same id) → 202 re-registered.
        let resp2 = crate::handlers::repo::register_repo_handler(
            State(state.clone()),
            Json(mk_req(&bare_url)),
        )
        .await;
        assert_eq!(resp2.status(), StatusCode::ACCEPTED);
        let job2 = job_rx.recv().await.expect("second Clone job enqueued");
        assert!(matches!(job2, IndexJob::Clone { .. }));

        // 4. Worker processes: the git phase now succeeds (dir is cloned). The
        //    pipeline may still fail against the stub DBs, but that is no longer
        //    a git error.
        let repo2 = state.registry.lock().unwrap().get(&id).unwrap().clone();
        let res2 = process_repository(&repo2, &state, &job2).await;
        if let Err(e) = &res2 {
            let msg = format!("{e:#}");
            assert!(
                !msg.contains("git clone failed") && !msg.contains("cannot pull"),
                "recovery must get past the git phase, got: {msg}"
            );
        }
        assert!(
            Path::new(&repo2.local_path).join(".git").exists(),
            "recovered repo must be cloned on disk"
        );
    }

    #[tokio::test]
    async fn test_worker_failure_on_previously_indexed_repo_keeps_dir() {
        let dir = TempDir::new().unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let state = create_test_state(&workspace).await;

        let repo = RepoEntry {
            id: "delta".into(),
            url: "https://invalid.example/x.git".into(),
            local_path: crate::models::repo_local_path(&state.workspace_dir, "delta"),
            auth_type: AuthType::Ssh,
            branch: "main".into(),
            webhook_secret: None,
            last_indexed: Some("2026-07-06T00:00:00Z".into()),
            status: RepoStatus::Indexing,
        };
        state
            .registry
            .lock()
            .unwrap()
            .add_or_replace(repo.clone())
            .unwrap();

        std::fs::create_dir_all(&repo.local_path).unwrap();
        std::fs::write(Path::new(&repo.local_path).join("keep"), b"x").unwrap();

        handle_job_failure(&state, &repo).await;

        {
            let mut registry = state.registry.lock().unwrap();
            let entry = registry
                .get("delta")
                .expect("entry must remain in registry");
            assert_eq!(entry.status, RepoStatus::Error);
        }
        assert!(
            Path::new(&repo.local_path).exists(),
            "a previously-indexed repo must keep its local dir on failure"
        );
    }
}
