use std::fs::{self, File};
use std::path::{Path};
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

pub fn acquire_file_lock(lock_path: &Path) -> anyhow::Result<FileLock> {
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = File::create(lock_path)?;
    file.try_lock_exclusive()?;
    Ok(FileLock::new(file))
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
    fn test_lock_creates_parent_directory() {
        let dir = TempDir::new().unwrap();
        let lock_path = dir.path().join("subdir").join(".knot.lock");
        let lock = acquire_file_lock(&lock_path);
        assert!(lock.is_ok());
        assert!(lock_path.exists());
    }
}
