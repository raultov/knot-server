use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::Duration;

use fs2::FileExt;

pub struct FileLock {
    file: File,
}

impl FileLock {
    pub fn new(file: File) -> Self {
        Self { file }
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

/// Workspace subdirectory holding the per-repository lock files.
///
/// It lives outside every repository directory on purpose: the worker wipes a
/// repository's local dir under the lock (fresh clone, failure cleanup), and a
/// lock file inside that dir would be unlinked while held. The holder keeps its
/// lock on the orphaned inode, but another node would then create a new file at
/// the same path and lock it too, so two nodes would index the same repository
/// concurrently. The dot prefix keeps it from colliding with a repository whose
/// generated id is `locks`.
const LOCKS_DIR: &str = ".locks";

/// `<workspace>/.locks/<repo_id>.lock`
pub fn repo_lock_path(workspace_dir: &Path, repo_id: &str) -> PathBuf {
    workspace_dir
        .join(LOCKS_DIR)
        .join(format!("{repo_id}.lock"))
}

pub fn acquire_file_lock(lock_path: &Path) -> anyhow::Result<FileLock> {
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = File::create(lock_path)?;
    file.try_lock_exclusive()?;
    Ok(FileLock::new(file))
}

/// Attempts [`acquire_file_lock_with_retry`] makes before reporting contention.
const LOCK_ATTEMPTS: u32 = 5;
const LOCK_RETRY_DELAY: Duration = Duration::from_millis(20);

/// [`acquire_file_lock`] with a short retry. [`is_lock_held`] takes the lock
/// for a few microseconds to probe it, so a worker that tried exactly then
/// would otherwise mistake the probe for another node indexing the repository.
pub async fn acquire_file_lock_with_retry(lock_path: &Path) -> anyhow::Result<FileLock> {
    let mut attempt = 1;
    loop {
        match acquire_file_lock(lock_path) {
            Ok(lock) => return Ok(lock),
            Err(e) if attempt >= LOCK_ATTEMPTS => return Err(e),
            Err(_) => {
                attempt += 1;
                tokio::time::sleep(LOCK_RETRY_DELAY).await;
            }
        }
    }
}

/// Whether a live process (another node, or a worker of this one) holds the
/// lock right now. A missing lock file is never held. The probe takes the lock
/// without blocking and releases it at once; any probe error counts as held,
/// so callers err on the side of leaving the repository alone.
pub fn is_lock_held(lock_path: &Path) -> bool {
    let Ok(file) = File::open(lock_path) else {
        return false;
    };
    match file.try_lock_exclusive() {
        Ok(()) => {
            let _ = FileExt::unlock(&file);
            false
        }
        Err(_) => true,
    }
}

pub fn is_lock_stale(lock_path: &Path, threshold: Duration) -> bool {
    if let Ok(metadata) = fs::metadata(lock_path)
        && let Ok(modified) = metadata.modified()
        && let Ok(elapsed) = modified.elapsed()
    {
        return elapsed > threshold;
    }
    false
}

pub fn remove_stale_lock(lock_path: &Path) -> bool {
    if lock_path.exists() {
        if let Err(e) = fs::remove_file(lock_path) {
            tracing::warn!("Failed to remove stale lock {}: {e}", lock_path.display());
            return false;
        }
        tracing::warn!("Removed stale lock: {}", lock_path.display());
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tempfile::TempDir;

    #[test]
    fn test_acquire_lock_succeeds_on_first_attempt() {
        let dir = TempDir::new().unwrap();
        let lock_path = dir.path().join(".knot.lock");
        let lock = acquire_file_lock(&lock_path);
        assert!(lock.is_ok());
    }

    #[test]
    fn test_acquire_lock_fails_when_already_held() {
        let dir = TempDir::new().unwrap();
        let lock_path = dir.path().join(".knot.lock");
        let _lock1 = acquire_file_lock(&lock_path).unwrap();
        let lock2 = acquire_file_lock(&lock_path);
        assert!(lock2.is_err());
    }

    #[test]
    fn test_lock_released_on_drop() {
        let dir = TempDir::new().unwrap();
        let lock_path = dir.path().join(".knot.lock");
        {
            let _lock = acquire_file_lock(&lock_path).unwrap();
        }
        let lock2 = acquire_file_lock(&lock_path);
        assert!(lock2.is_ok());
    }

    #[test]
    fn test_fresh_lock_not_stale() {
        let dir = TempDir::new().unwrap();
        let lock_path = dir.path().join(".knot.lock");
        File::create(&lock_path).unwrap();
        assert!(!is_lock_stale(&lock_path, Duration::from_secs(3600)));
    }

    #[test]
    fn test_remove_stale_lock_removes_file() {
        let dir = TempDir::new().unwrap();
        let lock_path = dir.path().join(".knot.lock");
        File::create(&lock_path).unwrap();
        assert!(lock_path.exists());
        assert!(remove_stale_lock(&lock_path));
        assert!(!lock_path.exists());
    }

    #[test]
    fn test_remove_stale_lock_nonexistent() {
        let dir = TempDir::new().unwrap();
        let lock_path = dir.path().join("nonexistent.lock");
        assert!(!remove_stale_lock(&lock_path));
    }

    #[test]
    fn test_repo_lock_path_is_outside_repo_dir() {
        let workspace = Path::new("/ws");
        let lock_path = repo_lock_path(workspace, "alpha");
        assert_eq!(lock_path, workspace.join(".locks").join("alpha.lock"));
        let repo_dir = crate::models::repo_local_path("/ws", "alpha");
        assert!(!lock_path.starts_with(&repo_dir));
    }

    #[test]
    fn test_repo_lock_path_does_not_collide_with_repo_named_locks() {
        let workspace = Path::new("/ws");
        let repo_dir = crate::models::repo_local_path("/ws", "locks");
        assert!(!repo_lock_path(workspace, "alpha").starts_with(&repo_dir));
    }

    #[test]
    fn test_lock_survives_wipe_of_repo_dir() {
        let workspace = TempDir::new().unwrap();
        let ws = workspace.path().to_string_lossy().into_owned();
        let repo_dir = crate::models::repo_local_path(&ws, "alpha");
        fs::create_dir_all(&repo_dir).unwrap();
        let lock_path = repo_lock_path(workspace.path(), "alpha");
        let _held = acquire_file_lock(&lock_path).unwrap();

        // The worker wipes the repo dir under the lock before a fresh clone.
        fs::remove_dir_all(&repo_dir).unwrap();

        assert!(lock_path.exists());
        assert!(
            acquire_file_lock(&lock_path).is_err(),
            "a second node must not acquire the lock after the repo dir is wiped"
        );
    }

    #[test]
    fn test_is_lock_held_tracks_the_holder() {
        let dir = TempDir::new().unwrap();
        let lock_path = dir.path().join("alpha.lock");
        assert!(!is_lock_held(&lock_path), "a missing file is never held");

        let held = acquire_file_lock(&lock_path).unwrap();
        assert!(is_lock_held(&lock_path));
        drop(held);
        assert!(!is_lock_held(&lock_path), "the file outlives the lock");
        assert!(
            acquire_file_lock(&lock_path).is_ok(),
            "the probe must release the lock it took"
        );
    }

    #[tokio::test]
    async fn test_acquire_with_retry_waits_out_a_brief_holder() {
        let dir = TempDir::new().unwrap();
        let lock_path = dir.path().join("alpha.lock");
        let held = acquire_file_lock(&lock_path).unwrap();
        let release = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            drop(held);
        });

        assert!(acquire_file_lock_with_retry(&lock_path).await.is_ok());
        release.await.unwrap();
    }

    #[tokio::test]
    async fn test_acquire_with_retry_reports_a_lasting_holder() {
        let dir = TempDir::new().unwrap();
        let lock_path = dir.path().join("alpha.lock");
        let _held = acquire_file_lock(&lock_path).unwrap();
        assert!(acquire_file_lock_with_retry(&lock_path).await.is_err());
    }

    #[test]
    fn test_lock_creates_parent_directory() {
        let dir = TempDir::new().unwrap();
        let lock_path = dir.path().join("subdir").join(".knot.lock");
        let lock = acquire_file_lock(&lock_path);
        assert!(lock.is_ok());
        assert!(lock_path.exists());
    }
}
