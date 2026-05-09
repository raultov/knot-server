use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use crate::locking::{is_lock_stale, remove_stale_lock};
use crate::models::IndexJob;
use crate::state::AppState;

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
        tracing::info!("Scheduler: checking repositories for stale locks and overdue indexing");

        let repos = {
            let registry = state.registry.lock().unwrap();
            registry.list().to_vec()
        };

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
                    let job = IndexJob::Pull {
                        repo_id: repo.id.clone(),
                    };
                    let _ = state.job_tx.try_send(job);
                    continue;
                }
            }

            // Check if re-indexing is overdue
            if let Some(ref last_indexed_str) = repo.last_indexed {
                // Parse the ISO 8601 timestamp
                if let Ok(elapsed) = elapsed_since_iso8601(last_indexed_str)
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
                    let _ = state.job_tx.try_send(job);
                }
            }
        }
    }
}

fn elapsed_since_iso8601(ts: &str) -> anyhow::Result<Duration> {
    // Simple ISO 8601 parser: YYYY-MM-DDTHH:MM:SSZ
    let ts = ts.trim();
    let (date_part, time_part) = if let Some(pos) = ts.find('T') {
        (&ts[..pos], &ts[pos + 1..])
    } else {
        anyhow::bail!("Invalid ISO 8601 timestamp: missing 'T'");
    };

    let time_part = time_part.trim_end_matches('Z');

    let date_parts: Vec<&str> = date_part.split('-').collect();
    let time_parts: Vec<&str> = time_part.split(':').collect();

    if date_parts.len() != 3 || time_parts.len() < 2 {
        anyhow::bail!("Invalid ISO 8601 timestamp format");
    }

    let year: i64 = date_parts[0].parse()?;
    let month: u32 = date_parts[1].parse()?;
    let day: u32 = date_parts[2].parse()?;
    let hour: u64 = time_parts[0].parse()?;
    let min: u64 = time_parts[1].parse()?;
    let sec: u64 = if time_parts.len() > 2 {
        time_parts[2].parse()?
    } else {
        0
    };

    let days = days_since_epoch(year, month, day);
    let total_secs = days as u64 * 86400 + hour * 3600 + min * 60 + sec;

    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    if total_secs > now_secs {
        // Timestamp is in the future
        return Ok(Duration::ZERO);
    }

    Ok(Duration::from_secs(now_secs - total_secs))
}

fn days_since_epoch(year: i64, month: u32, day: u32) -> i64 {
    let mut days = 0i64;
    for y in 1970..year {
        days += if is_leap(y) { 366 } else { 365 };
    }
    let month_days = if is_leap(year) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };
    for m in 1..month as usize {
        days += month_days[m - 1] as i64;
    }
    days += day as i64 - 1;
    days
}

fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tempfile::TempDir;

    #[test]
    fn test_elapsed_since_iso8601() {
        let ts = "2020-01-01T00:00:00Z";
        let elapsed = elapsed_since_iso8601(ts);
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
        let days = recent / 86400;
        let remaining = recent % 86400;
        let hours = remaining / 3600;
        let remaining = remaining % 3600;
        let minutes = remaining / 60;
        let seconds = remaining % 60;

        let mut year = 1970i64;
        let mut days_left = days as i64;
        loop {
            let diy = if is_leap(year) { 366 } else { 365 };
            if days_left < diy {
                break;
            }
            days_left -= diy;
            year += 1;
        }
        let md = if is_leap(year) {
            [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
        } else {
            [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
        };
        let mut month = 1u32;
        for &m in &md {
            if days_left < m as i64 {
                break;
            }
            days_left -= m as i64;
            month += 1;
        }
        let day = days_left as u32 + 1;

        let ts = format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
            year, month, day, hours, minutes, seconds
        );

        let elapsed = elapsed_since_iso8601(&ts).unwrap();
        assert!(
            elapsed.as_secs() <= 120,
            "Expected <=120s, got {}",
            elapsed.as_secs()
        );
    }

    #[test]
    fn test_elapsed_invalid_format() {
        assert!(elapsed_since_iso8601("not-a-timestamp").is_err());
        assert!(elapsed_since_iso8601("").is_err());
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
