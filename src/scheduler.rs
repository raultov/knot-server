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

        // Check for stale locks
        if lock_path.exists() {
            let threshold = Duration::from_secs(stale_lock_timeout_secs);
            if is_lock_stale(&lock_path, threshold) {
                tracing::warn!(
                    "Stale lock detected for {} at {} (removing and re-enqueuing)",
                    repo.id,
                    lock_path.display()
                );
                remove_stale_lock(&lock_path);
                stale_locks_recovered += 1;
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
                } else {
                    jobs_enqueued += 1;
                }
                continue;
            }
        }

        // Pick up Pending repos that have no lock — prevent them from
        // remaining stuck forever when the startup recovery loop missed
        // them (e.g. a single pending repo after a clean shutdown).
        // The Pending → Queued transition is atomic (CAS) so duplicate
        // enqueues are impossible even when the worker is slow to start.
        if repo.status == RepoStatus::Pending && !lock_path.exists() {
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
                    } else {
                        jobs_enqueued += 1;
                    }
                }
                Ok(false) => {
                    tracing::debug!(
                        "Repo '{}' no longer Pending, scheduler skipped enqueue",
                        repo.id
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        "Failed to claim Pending repo '{}' for enqueue: {e}",
                        repo.id
                    );
                }
            }
            continue;
        }

        // Check if re-indexing is overdue
        if let Some(ref last_indexed_str) = repo.last_indexed {
            // Parse the ISO 8601 timestamp
            if let Ok(elapsed) = crate::time_utils::elapsed_since_iso8601(last_indexed_str)
                && elapsed > Duration::from_secs(max_index_age_secs)
            {
                tracing::info!(
                    "Repository {} last indexed {} ago (threshold: {}s), enqueuing Pull job",
                    repo.id,
                    elapsed.as_secs(),
                    max_index_age_secs
                );
                let job = IndexJob::Pull {
                    repo_id: repo.id.clone(),
                };
                if state.job_tx.try_send(job).is_ok() {
                    jobs_enqueued += 1;
                }
            }
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
    use std::time::Duration;
    use tempfile::TempDir;

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
