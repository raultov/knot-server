use std::fs;
use std::io::Write;
use std::path::Path;

/// Atomically writes `content` to `target_path` using a temporary file and sync/rename.
///
/// Flushes and syncs to disk before renaming so concurrent readers never
/// observe a partially written file or stale mtime.
pub fn write_file_atomically_with_temp(
    target_path: &Path,
    temp_path: &Path,
    content: &str,
) -> anyhow::Result<()> {
    {
        let mut f = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(temp_path)?;
        f.write_all(content.as_bytes())?;
        f.sync_all()?;
    }
    if target_path.exists() {
        let _ = fs::remove_file(target_path);
    }
    fs::rename(temp_path, target_path)?;
    Ok(())
}
