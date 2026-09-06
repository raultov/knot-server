use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use crate::locking::{is_lock_stale, remove_stale_lock};
use crate::models::AppState;
use crate::models::{IndexJob, RepoStatus};

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

/// Remove a stale lock, re-queue the repo, and enqueue a Pull job. Returns
/// `Some(enqueued)` when a stale lock was found (regardless of whether the
/// channel send succeeded), `None` when the lock is absent or not yet stale.
fn recover_stale_lock(
    state: &Arc<AppState>,
    repo: &crate::models::RepoEntry,
    lock_path: &Path,
    threshold: Duration,
) -> Option<bool> {
    if !lock_path.exists() || !is_lock_stale(lock_path, threshold) {
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
fn pickup_pending_repo(state: &Arc<AppState>, repo: &crate::models::RepoEntry) -> bool {
    let git_dir = Path::new(&repo.local_path).join(".git");
    let job = if git_dir.exists() {
        IndexJob::Pull {
            repo_id: repo.id.clone(),
        }
    } else {
        IndexJob::Clone {
            repo_id: repo.id.clone(),
        }
    };
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
        let lock_path = Path::new(&repo.local_path).join(".knot.lock");
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
