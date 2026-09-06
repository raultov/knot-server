//! Per-repository progress snapshots persisted to the shared workspace so
//! that nodes without an in-process `ProgressTracker` (i.e. every node that
//! is not currently indexing this repo) can still report the real
//! progress of a peer.
//!
//! Layout: `<workspace>/progress/<repo_id>.json`
//!
//! Writes are atomic (temp file + rename) so concurrent readers never
//! observe a partially written snapshot. Reads treat a snapshot whose
//! `updated_at` is older than `MAX_AGE_SECS` as missing — this protects
//! against a crashed indexer leaving a stale file behind (the analogous
//! cleanup for repo locks already exists in `crate::locking`).

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::models::RepoStatus;
use crate::time_utils;

/// Default staleness window for a snapshot. A snapshot whose `updated_at`
/// is older than this is considered missing on read.
pub const MAX_AGE_SECS: u64 = 60;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PersistedProgress {
    pub repo_id: String,
    /// Status written by the indexing node. Stays `Indexing` for the
    /// duration of an indexing run so a reader whose registry copy lags
    /// behind still reports a coherent status.
    pub status: RepoStatus,
    pub stage: String,
    pub total_files: u64,
    pub parsed_files: u64,
    pub percent_complete: f32,
    pub entities_ingested: u64,
    pub batches_ingested: u64,
    pub error: Option<String>,
    /// ISO 8601 timestamp, refreshed by the writer every time the snapshot
    /// is updated. Readers use this to ignore stale snapshots.
    pub updated_at: String,
}

impl PersistedProgress {
    /// Construct a snapshot from the in-memory tracker. `status` is
    /// supplied by the worker (which knows the canonical
    /// `RepoStatus::Indexing` for an active run).
    pub fn from_tracker(
        repo_id: &str,
        status: RepoStatus,
        snap: &knot::pipeline::progress::IndexingProgress,
    ) -> Self {
        Self {
            repo_id: repo_id.to_string(),
            status,
            stage: format_stage(snap.stage),
            total_files: snap.total_files,
            parsed_files: snap.parsed_files,
            percent_complete: snap.percent_complete,
            entities_ingested: snap.entities_ingested,
            batches_ingested: snap.batches_ingested,
            error: snap.error.clone(),
            updated_at: time_utils::chrono_now(),
        }
    }
}

/// Helper to consistently format the `IndexingStage` enum into a string.
pub fn format_stage(stage: knot::pipeline::progress::IndexingStage) -> String {
    serde_json::to_value(stage)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| "idle".to_string())
}

/// `<workspace>/progress`
pub fn progress_dir(workspace: &Path) -> PathBuf {
    workspace.join("progress")
}

/// `<workspace>/progress/<repo_id>.json`
pub fn snapshot_path(workspace: &Path, repo_id: &str) -> PathBuf {
    progress_dir(workspace).join(format!("{repo_id}.json"))
}

/// Persist a snapshot atomically (temp file + rename) so a concurrent
/// reader never observes a half-written file.
pub fn write_snapshot(workspace: &Path, p: &PersistedProgress) -> anyhow::Result<()> {
    let dir = progress_dir(workspace);
    fs::create_dir_all(&dir)?;
    let path = snapshot_path(workspace, &p.repo_id);
    let temp_path = dir.join(format!(".{}.json.tmp", p.repo_id));
    let json = serde_json::to_string_pretty(p)?;
    crate::fs_utils::write_file_atomically_with_temp(&path, &temp_path, &json)?;
    Ok(())
}

/// Read the snapshot for `repo_id` if it exists, is well-formed, and is
/// not older than `MAX_AGE_SECS`. Returns `None` in every other case
/// (including on read / parse errors), so the caller can fall back to
/// the idle response without error handling.
pub fn read_snapshot(workspace: &Path, repo_id: &str) -> Option<PersistedProgress> {
    read_snapshot_with_max_age(workspace, repo_id, MAX_AGE_SECS)
}

/// Test-friendly variant that lets callers control the staleness window.
pub fn read_snapshot_with_max_age(
    workspace: &Path,
    repo_id: &str,
    max_age_secs: u64,
) -> Option<PersistedProgress> {
    let path = snapshot_path(workspace, repo_id);
    let content = fs::read_to_string(&path).ok()?;
    let parsed: PersistedProgress = match serde_json::from_str(&content) {
        Ok(p) => p,
        Err(_) => return None,
    };
    let elapsed = time_utils::elapsed_since_iso8601(&parsed.updated_at).ok()?;
    if elapsed.as_secs() > max_age_secs {
        return None;
    }
    Some(parsed)
}

/// Best-effort delete. Silently ignores missing files.
pub fn remove_snapshot(workspace: &Path, repo_id: &str) {
    let path = snapshot_path(workspace, repo_id);
    if path.exists() {
        let _ = fs::remove_file(&path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn sample(repo_id: &str, updated_at: &str) -> PersistedProgress {
        PersistedProgress {
            repo_id: repo_id.to_string(),
            status: RepoStatus::Indexing,
            stage: "parsing".to_string(),
            total_files: 100,
            parsed_files: 57,
            percent_complete: 57.0,
            entities_ingested: 900,
            batches_ingested: 3,
            error: None,
            updated_at: updated_at.to_string(),
        }
    }

    #[test]
    fn test_write_then_read_roundtrip() {
        let dir = TempDir::new().unwrap();
        let snap = sample("alpha", &time_utils::chrono_now());
        write_snapshot(dir.path(), &snap).unwrap();

        let read_back = read_snapshot(dir.path(), "alpha").expect("snapshot must exist");
        assert_eq!(read_back, snap);
    }

    #[test]
    fn test_read_missing_returns_none() {
        let dir = TempDir::new().unwrap();
        assert!(read_snapshot(dir.path(), "alpha").is_none());
    }

    #[test]
    fn test_read_corrupt_returns_none() {
        let dir = TempDir::new().unwrap();
        fs::create_dir_all(progress_dir(dir.path())).unwrap();
        fs::write(snapshot_path(dir.path(), "alpha"), b"NOT VALID JSON").unwrap();
        assert!(
            read_snapshot(dir.path(), "alpha").is_none(),
            "corrupt snapshots must not break the handler"
        );
    }

    #[test]
    fn test_read_stale_returns_none() {
        let dir = TempDir::new().unwrap();
        // Year 2000 is "very old" for any reasonable threshold.
        let snap = sample("alpha", "2000-01-01T00:00:00Z");
        write_snapshot(dir.path(), &snap).unwrap();

        // File is on disk...
        assert!(snapshot_path(dir.path(), "alpha").exists());
        // ...but the staleness guard returns None.
        assert!(read_snapshot(dir.path(), "alpha").is_none());
    }

    #[test]
    fn test_remove_snapshot_deletes_file() {
        let dir = TempDir::new().unwrap();
        let snap = sample("alpha", &time_utils::chrono_now());
        write_snapshot(dir.path(), &snap).unwrap();
        assert!(snapshot_path(dir.path(), "alpha").exists());

        remove_snapshot(dir.path(), "alpha");
        assert!(!snapshot_path(dir.path(), "alpha").exists());
    }

    #[test]
    fn test_remove_snapshot_missing_is_noop() {
        let dir = TempDir::new().unwrap();
        // Must not panic or error.
        remove_snapshot(dir.path(), "never-existed");
    }

    #[test]
    fn test_atomic_write_never_leaves_partial_file_visible() {
        let dir = TempDir::new().unwrap();
        // Multiple writes in quick succession all produce a valid file
        // that reads back as one of the written snapshots.
        for i in 0..10 {
            let mut snap = sample("alpha", &time_utils::chrono_now());
            snap.parsed_files = i;
            snap.percent_complete = i as f32;
            write_snapshot(dir.path(), &snap).unwrap();
            let read_back = read_snapshot(dir.path(), "alpha").expect("snapshot must exist");
            assert_eq!(read_back.parsed_files, i);
        }
    }
}
