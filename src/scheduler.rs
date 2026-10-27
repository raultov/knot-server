use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use crate::locking::{is_lock_held, is_lock_stale, remove_stale_lock, repo_lock_path};
use crate::models::AppState;
use crate::models::{IndexJob, RepoEntry, RepoStatus};

pub async fn scheduler_loop(
    state: Arc<AppState>,
    poll_interval_secs: u64,
    stale_lock_timeout_secs: u64,
    max_index_age_secs: u64,
) {
    let mut interval = tokio::time::interval(Duration::from_secs(poll_interval_secs));
    // Skip the immediate first tick
    interval.tick().await;

    loop {
        interval.tick().await;
        run_scheduler_iteration(&state, stale_lock_timeout_secs, max_index_age_secs);
    }
}

/// The job that resumes an interrupted repository: an incremental `Pull`
/// when a checkout exists, otherwise a `Clone` (the worker wipes and starts
/// from scratch).
pub(crate) fn recovery_job(repo: &RepoEntry) -> IndexJob {
    let repo_id = repo.id.clone();
    if Path::new(&repo.local_path).join(".git").exists() {
        IndexJob::Pull { repo_id }
    } else {
        IndexJob::Clone { repo_id }
    }
}

/// Periodically re-enqueue repositories left in an in-progress status by a
/// worker that died, independently of the (by default daily) scheduler poll.
pub async fn stuck_repo_loop(state: Arc<AppState>, check_interval_secs: u64) {
    let mut interval = tokio::time::interval(Duration::from_secs(check_interval_secs));
    interval.tick().await;
    let mut suspects = HashSet::new();
    loop {
        interval.tick().await;
        heal_stuck_repos(&state, &mut suspects);
    }
}

/// One self-healing pass. A worker holds the repository lock for the whole
/// job, so a repo in `cloning` / `pulling` / `indexing` whose lock nobody
/// holds has no job behind it: its worker crashed, or it was claimed by a
/// second knot-server that exited before serving (issue #10). Such a repo is
/// only re-enqueued when two consecutive passes see it, which rules out the
/// instant between a worker's claim and its lock. `suspects` carries the repos
/// seen on the previous pass. Returns the number of repos re-enqueued.
pub(crate) fn heal_stuck_repos(state: &Arc<AppState>, suspects: &mut HashSet<String>) -> usize {
    let repos = {
        let mut registry = state.registry.lock().unwrap();
        registry.list().to_vec()
    };

    let mut seen_now = HashSet::new();
    let mut healed = 0;
    for repo in repos.iter().filter(|r| is_in_progress(&r.status)) {
        let lock_path = repo_lock_path(Path::new(&state.workspace_dir), &repo.id);
        if is_lock_held(&lock_path) {
            continue;
        }
        if !suspects.contains(&repo.id) {
            seen_now.insert(repo.id.clone());
            continue;
        }
        if requeue_stuck_repo(state, repo) {
            healed += 1;
        }
    }
    *suspects = seen_now;
    healed
}

fn is_in_progress(status: &RepoStatus) -> bool {
    matches!(
        status,
        RepoStatus::Cloning | RepoStatus::Pulling | RepoStatus::Indexing
    )
}

/// Move a stuck repo back to `Queued` and enqueue its recovery job. The
/// transition is a CAS, so a repo that moved on since the pass listed it is
/// left alone. Returns `true` when a job was enqueued.
fn requeue_stuck_repo(state: &Arc<AppState>, repo: &RepoEntry) -> bool {
    let mut registry = state.registry.lock().unwrap();
    let in_progress = [
        RepoStatus::Cloning,
        RepoStatus::Pulling,
        RepoStatus::Indexing,
    ];
    if !matches!(
        registry.transition_status(&repo.id, &in_progress, RepoStatus::Queued),
        Ok(true)
    ) {
        return false;
    }
    let job = recovery_job(repo);
    tracing::warn!(
        "Recovering stuck repo '{}' (was {}): no worker holds its lock, enqueuing {:?}",
        repo.id,
        repo.status,
        job
    );
    if state.job_tx.try_send(job).is_err() {
        let _ = registry.update_status(&repo.id, RepoStatus::Pending);
        return false;
    }
    true
}

/// Remove a stale lock, re-queue the repo, and enqueue a Pull job. Returns
/// `Some(enqueued)` when a stale lock was found (regardless of whether the
/// channel send succeeded), `None` when the lock is absent or not yet stale.
fn recover_stale_lock(
    state: &Arc<AppState>,
    repo: &crate::models::RepoEntry,
    lock_path: &Path,
    threshold: Duration,
) -> Option<bool> {
    // An old lock file can still be held by a long job: removing it would let
    // another node lock a new file at the same path and index concurrently.
    if !lock_path.exists() || !is_lock_stale(lock_path, threshold) || is_lock_held(lock_path) {
        return None;
    }
    tracing::warn!(
        "Stale lock detected for {} at {} (removing and re-enqueuing)",
        repo.id,
        lock_path.display()
    );
    remove_stale_lock(lock_path);
    {
        let mut registry = state.registry.lock().unwrap();
        let _ = registry.update_status(&repo.id, RepoStatus::Queued);
    }
    let job = IndexJob::Pull {
        repo_id: repo.id.clone(),
    };
    if state.job_tx.try_send(job).is_err() {
        let mut registry = state.registry.lock().unwrap();
        let _ = registry.update_status(&repo.id, RepoStatus::Pending);
        Some(false)
    } else {
        Some(true)
    }
}

/// Pick up a `Pending` repo that has no lock and enqueue it. The `Pending →
/// Queued` transition is a CAS so duplicate enqueues are impossible. Returns
/// `true` when a job was successfully enqueued.
///
/// The caller is responsible for verifying `status == Pending && !lock_exists`
/// before calling this function.
fn pickup_pending_repo(state: &Arc<AppState>, repo: &RepoEntry) -> bool {
    let job = recovery_job(repo);
    let mut registry = state.registry.lock().unwrap();
    match registry.transition_status(&repo.id, &[RepoStatus::Pending], RepoStatus::Queued) {
        Ok(true) => {
            tracing::info!(
                "Picking up Pending repo '{}' (no lock), enqueuing {:?}",
                repo.id,
                job
            );
            if state.job_tx.try_send(job).is_err() {
                let _ = registry.update_status(&repo.id, RepoStatus::Pending);
                false
            } else {
                true
            }
        }
        Ok(false) => {
            tracing::debug!(
                "Repo '{}' no longer Pending, scheduler skipped enqueue",
                repo.id
            );
            false
        }
        Err(e) => {
            tracing::warn!(
                "Failed to claim Pending repo '{}' for enqueue: {e}",
                repo.id
            );
            false
        }
    }
}

/// Enqueue a Pull job when the repo's last-indexed timestamp is older than the
/// threshold. Returns `true` when a job was successfully enqueued.
fn enqueue_if_overdue(
    state: &Arc<AppState>,
    repo: &crate::models::RepoEntry,
    max_index_age_secs: u64,
) -> bool {
    let Some(ref last_indexed_str) = repo.last_indexed else {
        return false;
    };
    let Ok(elapsed) = crate::time_utils::elapsed_since_iso8601(last_indexed_str) else {
        return false;
    };
    if elapsed <= Duration::from_secs(max_index_age_secs) {
        return false;
    }
    tracing::info!(
        "Repository {} last indexed {} ago (threshold: {}s), enqueuing Pull job",
        repo.id,
        elapsed.as_secs(),
        max_index_age_secs
    );
    let job = IndexJob::Pull {
        repo_id: repo.id.clone(),
    };
    state.job_tx.try_send(job).is_ok()
}

/// A single scheduler poll: scan every repo for stale locks, pick up stuck
/// `Pending` repos, and enqueue overdue re-indexing. Extracted into its own
/// function so the whole iteration is one `scheduler_poll` span; the body is
/// fully synchronous (no `.await`), so the span guard is held safely.
#[tracing::instrument(
    name = "scheduler_poll",
    skip_all,
    fields(
        otel.kind = "internal",
        repos_checked = tracing::field::Empty,
        jobs_enqueued = tracing::field::Empty,
        stale_locks_recovered = tracing::field::Empty,
    )
)]
fn run_scheduler_iteration(
    state: &Arc<AppState>,
    stale_lock_timeout_secs: u64,
    max_index_age_secs: u64,
) {
    tracing::info!("Scheduler: checking repositories for stale locks and overdue indexing");

    let repos = {
        let mut registry = state.registry.lock().unwrap();
        registry.list().to_vec()
    };

    let mut jobs_enqueued: u64 = 0;
    let mut stale_locks_recovered: u64 = 0;

    for repo in &repos {
        let lock_path = repo_lock_path(Path::new(&state.workspace_dir), &repo.id);
        let threshold = Duration::from_secs(stale_lock_timeout_secs);

        if let Some(enqueued) = recover_stale_lock(state, repo, &lock_path, threshold) {
            stale_locks_recovered += 1;
            if enqueued {
                jobs_enqueued += 1;
            }
            continue;
        }

        if repo.status == RepoStatus::Pending && !lock_path.exists() {
            if pickup_pending_repo(state, repo) {
                jobs_enqueued += 1;
            }
            continue;
        }

        if enqueue_if_overdue(state, repo, max_index_age_secs) {
            jobs_enqueued += 1;
        }
    }

    let span = tracing::Span::current();
    span.record("repos_checked", repos.len() as u64);
    span.record("jobs_enqueued", jobs_enqueued);
    span.record("stale_locks_recovered", stale_locks_recovered);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::tests_common::{create_test_state_with_rx, make_test_repo as make_repo};
    use std::time::Duration;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_enqueue_if_overdue_no_last_indexed() {
        let dir = TempDir::new().unwrap();
        let workspace = dir.path().join("ws");
        std::fs::create_dir_all(&workspace).unwrap();
        let (state, _rx) = create_test_state_with_rx(&workspace).await;

        let repo = make_repo("r1", &workspace.join("r1"), RepoStatus::Indexed);
        // no last_indexed → must return false
        assert!(!enqueue_if_overdue(&state, &repo, 3600));
    }

    #[tokio::test]
    async fn test_enqueue_if_overdue_recent_timestamp_returns_false() {
        let dir = TempDir::new().unwrap();
        let workspace = dir.path().join("ws");
        std::fs::create_dir_all(&workspace).unwrap();
        let (state, _rx) = create_test_state_with_rx(&workspace).await;

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let mut repo = make_repo("r2", &workspace.join("r2"), RepoStatus::Indexed);
        repo.last_indexed = Some(crate::time_utils::format_iso8601(now - 60));

        assert!(!enqueue_if_overdue(&state, &repo, 3600));
    }

    #[tokio::test]
    async fn test_enqueue_if_overdue_old_timestamp_returns_true() {
        let dir = TempDir::new().unwrap();
        let workspace = dir.path().join("ws");
        std::fs::create_dir_all(&workspace).unwrap();
        let (state, mut rx) = create_test_state_with_rx(&workspace).await;

        let mut repo = make_repo("r3", &workspace.join("r3"), RepoStatus::Indexed);
        repo.last_indexed = Some("2020-01-01T00:00:00Z".into());

        assert!(enqueue_if_overdue(&state, &repo, 3600));
        assert!(rx.try_recv().is_ok(), "expected a job in the channel");
    }

    #[tokio::test]
    async fn test_pickup_pending_repo_enqueues_and_transitions() {
        let dir = TempDir::new().unwrap();
        let workspace = dir.path().join("ws");
        std::fs::create_dir_all(&workspace).unwrap();
        let (state, mut rx) = create_test_state_with_rx(&workspace).await;

        let repo = make_repo("r4", &workspace.join("r4"), RepoStatus::Pending);
        state
            .registry
            .lock()
            .unwrap()
            .add_or_replace(repo.clone())
            .unwrap();

        let enqueued = pickup_pending_repo(&state, &repo);
        assert!(enqueued);
        assert!(rx.try_recv().is_ok(), "expected a job in the channel");

        let status = state
            .registry
            .lock()
            .unwrap()
            .get("r4")
            .unwrap()
            .status
            .clone();
        assert_eq!(status, RepoStatus::Queued);
    }

    #[tokio::test]
    async fn test_pickup_pending_repo_noop_when_already_claimed() {
        let dir = TempDir::new().unwrap();
        let workspace = dir.path().join("ws");
        std::fs::create_dir_all(&workspace).unwrap();
        let (state, mut rx) = create_test_state_with_rx(&workspace).await;

        // Register as Indexed (not Pending) — CAS must fail → no enqueue
        let repo = make_repo("r5", &workspace.join("r5"), RepoStatus::Indexed);
        state
            .registry
            .lock()
            .unwrap()
            .add_or_replace(repo.clone())
            .unwrap();

        let enqueued = pickup_pending_repo(&state, &repo);
        assert!(!enqueued);
        assert!(rx.try_recv().is_err(), "expected no job in the channel");
    }

    fn add_repo(state: &Arc<AppState>, workspace: &Path, id: &str, status: RepoStatus) {
        let repo = make_repo(id, &workspace.join(id), status);
        state.registry.lock().unwrap().add_or_replace(repo).unwrap();
    }

    fn status_of(state: &Arc<AppState>, id: &str) -> RepoStatus {
        state
            .registry
            .lock()
            .unwrap()
            .get(id)
            .unwrap()
            .status
            .clone()
    }

    /// Issue #10: a repo left in `cloning` with no worker holding its lock is
    /// re-enqueued, but only on the second consecutive sighting.
    #[tokio::test]
    async fn test_heal_stuck_repos_requeues_on_second_sighting() {
        let dir = TempDir::new().unwrap();
        let workspace = dir.path().join("ws");
        std::fs::create_dir_all(&workspace).unwrap();
        let (state, mut rx) = create_test_state_with_rx(&workspace).await;
        add_repo(&state, &workspace, "stuck", RepoStatus::Cloning);
        add_repo(&state, &workspace, "done", RepoStatus::Indexed);

        let mut suspects = HashSet::new();
        assert_eq!(heal_stuck_repos(&state, &mut suspects), 0);
        assert_eq!(status_of(&state, "stuck"), RepoStatus::Cloning);
        assert!(
            rx.try_recv().is_err(),
            "the first sighting must not enqueue"
        );

        assert_eq!(heal_stuck_repos(&state, &mut suspects), 1);
        assert_eq!(status_of(&state, "stuck"), RepoStatus::Queued);
        match rx.try_recv() {
            Ok(IndexJob::Clone { repo_id }) => assert_eq!(repo_id, "stuck"),
            other => panic!("expected a Clone job for 'stuck', got {other:?}"),
        }
        assert!(rx.try_recv().is_err(), "only the stuck repo is re-enqueued");
        assert_eq!(status_of(&state, "done"), RepoStatus::Indexed);
    }

    #[tokio::test]
    async fn test_heal_stuck_repos_leaves_a_locked_repo_alone() {
        let dir = TempDir::new().unwrap();
        let workspace = dir.path().join("ws");
        std::fs::create_dir_all(&workspace).unwrap();
        let (state, mut rx) = create_test_state_with_rx(&workspace).await;
        add_repo(&state, &workspace, "busy", RepoStatus::Indexing);
        let _held = crate::locking::acquire_file_lock(&repo_lock_path(&workspace, "busy")).unwrap();

        let mut suspects = HashSet::new();
        for _ in 0..3 {
            assert_eq!(heal_stuck_repos(&state, &mut suspects), 0);
        }
        assert_eq!(status_of(&state, "busy"), RepoStatus::Indexing);
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_heal_stuck_repos_forgets_a_repo_that_moved_on() {
        let dir = TempDir::new().unwrap();
        let workspace = dir.path().join("ws");
        std::fs::create_dir_all(&workspace).unwrap();
        let (state, mut rx) = create_test_state_with_rx(&workspace).await;
        add_repo(&state, &workspace, "claimed", RepoStatus::Cloning);

        let mut suspects = HashSet::new();
        heal_stuck_repos(&state, &mut suspects);
        // The worker finished in between: the next passes start from scratch.
        state
            .registry
            .lock()
            .unwrap()
            .update_status("claimed", RepoStatus::Indexed)
            .unwrap();
        assert_eq!(heal_stuck_repos(&state, &mut suspects), 0);
        assert!(suspects.is_empty());

        state
            .registry
            .lock()
            .unwrap()
            .update_status("claimed", RepoStatus::Pulling)
            .unwrap();
        assert_eq!(
            heal_stuck_repos(&state, &mut suspects),
            0,
            "a new in-progress status needs two sightings again"
        );
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_recover_stale_lock_never_removes_a_held_lock() {
        let dir = TempDir::new().unwrap();
        let workspace = dir.path().join("ws");
        std::fs::create_dir_all(&workspace).unwrap();
        let (state, mut rx) = create_test_state_with_rx(&workspace).await;
        add_repo(&state, &workspace, "long", RepoStatus::Indexing);
        let lock_path = repo_lock_path(&workspace, "long");
        let held = crate::locking::acquire_file_lock(&lock_path).unwrap();
        let old = std::time::SystemTime::now() - Duration::from_secs(7200);
        std::fs::File::options()
            .write(true)
            .open(&lock_path)
            .unwrap()
            .set_modified(old)
            .unwrap();
        let repo = state.registry.lock().unwrap().get("long").unwrap().clone();

        let threshold = Duration::from_secs(3600);
        assert_eq!(
            recover_stale_lock(&state, &repo, &lock_path, threshold),
            None
        );
        assert!(lock_path.exists(), "a held lock file must stay");
        assert!(rx.try_recv().is_err());

        drop(held);
        assert_eq!(
            recover_stale_lock(&state, &repo, &lock_path, threshold),
            Some(true),
            "an old lock nobody holds is still recovered"
        );
    }

    #[test]
    fn test_elapsed_since_iso8601() {
        let ts = "2020-01-01T00:00:00Z";
        let elapsed = crate::time_utils::elapsed_since_iso8601(ts);
        assert!(elapsed.is_ok());
        let secs = elapsed.unwrap().as_secs();
        // Should be several years (from 2020 to now)
        assert!(secs > 100_000_000, "Expected >100M seconds, got {secs}");
    }

    #[test]
    fn test_elapsed_recent_timestamp() {
        // A timestamp just 60 seconds ago
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let recent = now - 60;

        // Build ISO 8601 from recent
        let ts = crate::time_utils::format_iso8601(recent);

        let elapsed = crate::time_utils::elapsed_since_iso8601(&ts).unwrap();
        assert!(
            elapsed.as_secs() <= 120,
            "Expected <=120s, got {}",
            elapsed.as_secs()
        );
    }

    #[test]
    fn test_elapsed_invalid_format() {
        assert!(crate::time_utils::elapsed_since_iso8601("not-a-timestamp").is_err());
        assert!(crate::time_utils::elapsed_since_iso8601("").is_err());
    }

    #[test]
    fn test_stale_lock_cleanup_integration() {
        let dir = TempDir::new().unwrap();
        let lock_path = dir.path().join(".knot.lock");
        std::fs::File::create(&lock_path).unwrap();

        // Lock is fresh
        assert!(!is_lock_stale(&lock_path, Duration::from_secs(3600)));

        // Remove it cleanly
        assert!(remove_stale_lock(&lock_path));
        assert!(!lock_path.exists());
    }
}
