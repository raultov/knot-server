mod plan;
mod state;

pub(crate) use plan::{GitAction, decide_job_plan, should_wipe_on_failure};
pub(crate) use state::{StateSource, load_index_state_with_recovery};

use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::Instrument;

use crate::cleanup::CleanupScope;
use crate::locking::{FileLock, acquire_file_lock};
use crate::models::{IndexJob, RepoEntry};

/// Look up a repo entry from the registry, logging and returning `None` on any failure.
fn take_repo_entry(state: &Arc<crate::models::AppState>, repo_id: &str) -> Option<RepoEntry> {
    let mut registry = match state.registry.lock() {
        Ok(guard) => guard,
        Err(e) => {
            tracing::error!("Registry lock poisoned: {}", e);
            return None;
        }
    };
    match registry.get(repo_id) {
        Some(entry) => Some(entry.clone()),
        None => {
            tracing::error!(
                "Repository '{}' not found in registry, skipping job",
                repo_id
            );
            None
        }
    }
}

/// Transition the repo from `Queued` to its in-progress status. Returns `true`
/// when the claim succeeded and the job should proceed.
fn claim_queued_job(state: &Arc<crate::models::AppState>, repo_id: &str, job: &IndexJob) -> bool {
    let new_status = match job {
        IndexJob::Clone { .. } => crate::models::RepoStatus::Cloning,
        IndexJob::Pull { .. } => crate::models::RepoStatus::Pulling,
    };
    let mut registry = match state.registry.lock() {
        Ok(guard) => guard,
        Err(e) => {
            tracing::error!("Registry lock poisoned during status claim: {}", e);
            return false;
        }
    };
    match registry.transition_status(repo_id, &[crate::models::RepoStatus::Queued], new_status) {
        Ok(true) => true,
        Ok(false) => {
            tracing::info!(
                "Dropping stale job {:?} for '{}' — repo no longer Queued",
                job,
                repo_id
            );
            false
        }
        Err(e) => {
            tracing::error!("Failed to claim job for '{}': {e}", repo_id);
            false
        }
    }
}

pub async fn worker_loop(
    mut rx: tokio::sync::mpsc::Receiver<IndexJob>,
    state: Arc<crate::models::AppState>,
) {
    while let Some(job) = rx.recv().await {
        let repo_id = job.repo_id().to_string();
        tracing::info!("Worker picked up job: {:?} for {}", job, repo_id);

        let Some(repo) = take_repo_entry(&state, &repo_id) else {
            continue;
        };

        if !claim_queued_job(&state, &repo_id, &job) {
            continue;
        }

        // Root span for the whole job. Jobs are consumed off a mpsc queue, so
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

/// Attempt to acquire the file lock for a repository. If the lock is held by
/// another process, reverts the registry status to `Queued` and returns `None`
/// so the caller can exit gracefully without failing the job.
fn acquire_lock_or_requeue(
    state: &Arc<crate::models::AppState>,
    repo: &RepoEntry,
    lock_path: &Path,
) -> Option<FileLock> {
    match acquire_file_lock(lock_path) {
        Ok(lock) => {
            tracing::info!("Worker: acquired file lock for '{}'", repo.id);
            Some(lock)
        }
        Err(_) => {
            tracing::info!("Worker: '{}' locked by another node, skipping", repo.id);
            if let Ok(mut registry) = state.registry.lock() {
                let _ = registry.transition_status(
                    &repo.id,
                    &[
                        crate::models::RepoStatus::Cloning,
                        crate::models::RepoStatus::Pulling,
                    ],
                    crate::models::RepoStatus::Queued,
                );
            }
            tracing::warn!(
                "Lock for repo '{}' is held by another node; status claim was reverted to Queued",
                repo.id
            );
            None
        }
    }
}

/// Wipe databases and local directory before the git action when the plan
/// requires it. Runs under the file lock so no re-registration race can occur.
async fn wipe_before_action(
    state: &Arc<crate::models::AppState>,
    repo: &RepoEntry,
    job_plan: &plan::JobPlan,
    job: &IndexJob,
) {
    if job_plan.wipe_before {
        tracing::info!(
            "Worker: wiping databases + local dir for '{}' before {:?} (job {:?})",
            repo.id,
            job_plan.action,
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
}

fn action_label(action: &GitAction) -> &'static str {
    match action {
        GitAction::LocalSync => "local sync",
        GitAction::Pull => "pull",
        GitAction::FreshClone => "clone",
    }
}

/// Execute the git-level operation (clone / pull / local-sync) for the plan
/// action, emitting a single "starting" and "complete" log regardless of which
/// branch is taken.
async fn run_git_phase(repo: &RepoEntry, action: &GitAction) -> anyhow::Result<()> {
    tracing::info!(
        "Worker: starting {} for '{}' from {}",
        action_label(action),
        repo.id,
        repo.url
    );
    match action {
        GitAction::LocalSync => {
            let src = repo.url.clone();
            let dst = repo.local_path.clone();
            tokio::task::spawn_blocking(move || {
                crate::local_sync::sync_local_working_tree(&src, &dst)
            })
            .instrument(tracing::info_span!("git_sync"))
            .await??;
        }
        GitAction::Pull => {
            crate::git::run_git_pull(repo)
                .instrument(tracing::info_span!("git_pull"))
                .await?;
        }
        GitAction::FreshClone => {
            crate::git::run_git_clone(repo)
                .instrument(tracing::info_span!("git_clone"))
                .await?;
        }
    }
    tracing::info!(
        "Worker: {} complete for '{}'",
        action_label(action),
        repo.id
    );
    Ok(())
}

fn mark_indexing(state: &Arc<crate::models::AppState>, repo: &RepoEntry) -> anyhow::Result<()> {
    let mut registry = state
        .registry
        .lock()
        .map_err(|e| anyhow::anyhow!("Registry lock poisoned: {}", e))?;
    registry.update_status(&repo.id, crate::models::RepoStatus::Indexing)?;
    tracing::info!("Worker: status=indexing for '{}'", repo.id);
    Ok(())
}

fn build_knot_config(
    repo: &RepoEntry,
    state: &Arc<crate::models::AppState>,
) -> knot::config::Config {
    knot::config::Config {
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
    }
}

/// Log the outcome of loading the `IndexState`, preserving the original
/// severity (info for normal cases, warn for recovery paths).
fn log_state_source(source: &StateSource, repo_id: &str) {
    match source {
        StateSource::LoadedOk { entries, bytes } => {
            tracing::info!(
                "IndexState loaded for '{}' ({} entries, {} bytes on disk)",
                repo_id,
                entries,
                bytes
            );
        }
        StateSource::Missing => {
            tracing::info!(
                "IndexState file absent for '{}' — full indexing will run",
                repo_id
            );
        }
        StateSource::LegacyCleared => {
            tracing::warn!(
                "Removed stale .knot/index_state.json for local repo '{}' \
                 (older knot format); the next pipeline run will do a clean re-index",
                repo_id
            );
        }
        StateSource::LoadErrorFallback { error } => {
            tracing::warn!(
                "IndexState::load failed for local repo '{}': {}; \
                 removed the file and forcing full re-index",
                repo_id,
                error
            );
        }
    }
}

/// Run the indexing pipeline with progress tracking and clean up the progress
/// snapshot once the pipeline completes (regardless of outcome).
async fn run_pipeline_with_progress(
    state: &Arc<crate::models::AppState>,
    repo: &RepoEntry,
    knot_cfg: &knot::config::Config,
    index_state: &mut knot::pipeline::state::IndexState,
) -> anyhow::Result<()> {
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
    let pipeline_result = knot::pipeline::runner::run_indexing_pipeline_with_progress(
        knot_cfg,
        &state.vector_db,
        &state.graph_db,
        index_state,
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
    Ok(())
}

fn mark_indexed(state: &Arc<crate::models::AppState>, repo: &RepoEntry) -> anyhow::Result<()> {
    let mut registry = state
        .registry
        .lock()
        .map_err(|e| anyhow::anyhow!("Registry lock poisoned: {}", e))?;
    registry.update_status(&repo.id, crate::models::RepoStatus::Indexed)?;
    registry.update_last_indexed(&repo.id)?;
    crate::metrics::set_last_success(&repo.id);
    tracing::info!("Worker: status=indexed for '{}'", repo.id);
    Ok(())
}

async fn process_repository(
    repo: &RepoEntry,
    state: &Arc<crate::models::AppState>,
    job: &IndexJob,
) -> anyhow::Result<()> {
    // 1. Acquire exclusive file lock
    let lock_path = PathBuf::from(&repo.local_path).join(".knot.lock");
    let Some(_lock) = acquire_lock_or_requeue(state, repo, &lock_path) else {
        return Ok(());
    };

    // 2. Decide plan and wipe artifacts if needed (under the lock, so no race with re-registration)
    let is_local = crate::local_sync::is_local_path(&repo.url);
    let exists = Path::new(&repo.local_path).join(".git").exists();
    let job_plan = decide_job_plan(job, exists, is_local);
    tracing::info!("Worker: job plan for '{}': {:?}", repo.id, job_plan);
    wipe_before_action(state, repo, &job_plan, job).await;

    // 3. Execute the git phase (status was set to Cloning/Pulling by worker_loop)
    run_git_phase(repo, &job_plan.action).await?;

    // 4. Mark Indexing
    mark_indexing(state, repo)?;

    // 5. Build config and load IndexState
    let knot_cfg = build_knot_config(repo, state);
    let mut loaded = load_index_state_with_recovery(&repo.local_path, is_local)?;
    log_state_source(&loaded.source, &repo.id);

    // 6. Run the indexing pipeline
    run_pipeline_with_progress(state, repo, &knot_cfg, &mut loaded.state).await?;

    // 7. Mark Indexed
    mark_indexed(state, repo)?;
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
    use crate::handlers::tests_common::make_test_repo;
    use crate::models::{AuthType, RepoStatus};
    use std::sync::Mutex;
    use tempfile::TempDir;

    async fn create_test_state(workspace: &Path) -> Arc<crate::models::AppState> {
        create_test_state_with_rx(workspace).await.0
    }

    async fn create_test_state_with_rx(
        workspace: &Path,
    ) -> (
        Arc<crate::models::AppState>,
        tokio::sync::mpsc::Receiver<IndexJob>,
    ) {
        crate::handlers::tests_common::create_test_state_with_rx(workspace).await
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

        let repo = make_test_repo(
            "nonexistent",
            &workspace.join("nonexistent"),
            RepoStatus::Indexed,
        );

        let result = process_repository(
            &repo,
            &state,
            &IndexJob::Clone {
                repo_id: "nonexistent".into(),
            },
        )
        .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_take_repo_entry_returns_entry_when_present() {
        let dir = TempDir::new().unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let state = create_test_state(&workspace).await;

        let repo = make_test_repo("my-repo", &workspace.join("my-repo"), RepoStatus::Queued);
        state
            .registry
            .lock()
            .unwrap()
            .add_or_replace(repo.clone())
            .unwrap();

        let result = take_repo_entry(&state, "my-repo");
        assert!(result.is_some());
        assert_eq!(result.unwrap().id, "my-repo");
    }

    #[tokio::test]
    async fn test_take_repo_entry_returns_none_when_absent() {
        let dir = TempDir::new().unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let state = create_test_state(&workspace).await;

        let result = take_repo_entry(&state, "does-not-exist");
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_claim_queued_job_succeeds_from_queued() {
        let dir = TempDir::new().unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let state = create_test_state(&workspace).await;

        let repo = make_test_repo("repo-a", &workspace.join("repo-a"), RepoStatus::Queued);
        state.registry.lock().unwrap().add_or_replace(repo).unwrap();

        let job = IndexJob::Pull {
            repo_id: "repo-a".into(),
        };
        let claimed = claim_queued_job(&state, "repo-a", &job);
        assert!(claimed);

        let status = state
            .registry
            .lock()
            .unwrap()
            .get("repo-a")
            .unwrap()
            .status
            .clone();
        assert_eq!(status, RepoStatus::Pulling);
    }

    #[tokio::test]
    async fn test_claim_queued_job_fails_from_non_queued() {
        let dir = TempDir::new().unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let state = create_test_state(&workspace).await;

        let repo = make_test_repo("repo-b", &workspace.join("repo-b"), RepoStatus::Indexed);
        state.registry.lock().unwrap().add_or_replace(repo).unwrap();

        let job = IndexJob::Pull {
            repo_id: "repo-b".into(),
        };
        let claimed = claim_queued_job(&state, "repo-b", &job);
        assert!(!claimed);

        let status = state
            .registry
            .lock()
            .unwrap()
            .get("repo-b")
            .unwrap()
            .status
            .clone();
        // Status must remain unchanged
        assert_eq!(status, RepoStatus::Indexed);
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

    // ---- Worker integration (TempDir + local bare repo) ----

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

    #[tokio::test]
    async fn test_failed_registration_can_be_recovered_by_reregistering() {
        use crate::models::RegisterRepoRequest;
        use axum::Json;
        use axum::extract::State;
        use axum::http::StatusCode;

        let dir = TempDir::new().unwrap();
        let bare_url = create_bare_repo(dir.path());
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

        let resp = crate::handlers::repo::register_repo_handler(
            State(state.clone()),
            Json(mk_req(broken_url)),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
        let job = job_rx.recv().await.expect("Clone job enqueued");

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

        let resp2 = crate::handlers::repo::register_repo_handler(
            State(state.clone()),
            Json(mk_req(&bare_url)),
        )
        .await;
        assert_eq!(resp2.status(), StatusCode::ACCEPTED);
        let job2 = job_rx.recv().await.expect("second Clone job enqueued");
        assert!(matches!(job2, IndexJob::Clone { .. }));

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

    #[tokio::test]
    async fn test_lock_contention_reverts_claimed_status() {
        let dir = TempDir::new().unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let state = create_test_state(&workspace).await;

        let local_path = workspace.join("alpha");
        std::fs::create_dir_all(&local_path).unwrap();

        let repo = make_test_repo("alpha", &local_path, RepoStatus::Pulling);
        state
            .registry
            .lock()
            .unwrap()
            .add_or_replace(repo.clone())
            .unwrap();

        let lock_path = local_path.join(".knot.lock");
        let _holder = acquire_file_lock(&lock_path).unwrap();

        let result = process_repository(
            &repo,
            &state,
            &IndexJob::Pull {
                repo_id: "alpha".into(),
            },
        )
        .await;
        assert!(result.is_ok());

        let mut registry = state.registry.lock().unwrap();
        let updated = registry.get("alpha").unwrap();
        assert_eq!(updated.status, RepoStatus::Queued);
    }

    #[tokio::test]
    async fn test_lock_contention_preserves_holder_status() {
        let dir = TempDir::new().unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let state = create_test_state(&workspace).await;

        let local_path = workspace.join("alpha");
        std::fs::create_dir_all(&local_path).unwrap();

        let repo = make_test_repo("alpha", &local_path, RepoStatus::Indexing);
        state
            .registry
            .lock()
            .unwrap()
            .add_or_replace(repo.clone())
            .unwrap();

        let lock_path = local_path.join(".knot.lock");
        let _holder = acquire_file_lock(&lock_path).unwrap();

        let result = process_repository(
            &repo,
            &state,
            &IndexJob::Pull {
                repo_id: "alpha".into(),
            },
        )
        .await;
        assert!(result.is_ok());

        let mut registry = state.registry.lock().unwrap();
        let updated = registry.get("alpha").unwrap();
        assert_eq!(updated.status, RepoStatus::Indexing);
    }
}
