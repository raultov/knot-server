use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::locking::acquire_file_lock;
use crate::models::{RegistryData, RepoEntry, RepoStatus};

pub struct Registry {
    data: RegistryData,
    json_path: PathBuf,
    lock_path: PathBuf,
    last_load: SystemTime,
}

impl Registry {
    pub fn load_or_create(workspace_dir: &Path) -> anyhow::Result<Self> {
        fs::create_dir_all(workspace_dir)?;

        let json_path = workspace_dir.join("repos.json");
        let lock_path = workspace_dir.join("repos.json.lock");

        let _lock = acquire_file_lock(&lock_path)?;

        let data = if json_path.exists() {
            let content = fs::read_to_string(&json_path)?;
            if content.trim().is_empty() {
                RegistryData {
                    repositories: Vec::new(),
                }
            } else {
                serde_json::from_str(&content).unwrap_or(RegistryData {
                    repositories: Vec::new(),
                })
            }
        } else {
            RegistryData {
                repositories: Vec::new(),
            }
        };

        Ok(Self {
            data,
            json_path,
            lock_path,
            last_load: SystemTime::now(),
        })
    }

    fn save(&self) -> anyhow::Result<()> {
        use std::io::Write;

        let _lock = acquire_file_lock(&self.lock_path)?;
        let json = serde_json::to_string_pretty(&self.data)?;
        let temp_path = self.json_path.with_extension("json.tmp");
        {
            let mut f = fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&temp_path)?;
            f.write_all(json.as_bytes())?;
            f.sync_all()?;
        }
        if self.json_path.exists() {
            let _ = fs::remove_file(&self.json_path);
        }
        fs::rename(&temp_path, &self.json_path)?;
        Ok(())
    }

    fn reload_if_stale(&mut self) -> anyhow::Result<()> {
        if !self.json_path.exists() {
            return Ok(());
        }
        let mtime = match fs::metadata(&self.json_path) {
            Ok(m) => m.modified().ok(),
            Err(_) => None,
        };
        let stale = match mtime {
            Some(t) => t > self.last_load,
            None => fs::metadata(&self.json_path).map(|m| m.len()).unwrap_or(0) != 0,
        };
        if stale
            && let Ok(content) = fs::read_to_string(&self.json_path)
            && let Ok(parsed) = serde_json::from_str::<RegistryData>(&content)
        {
            self.data = parsed;
            self.last_load = SystemTime::now();
        }
        Ok(())
    }

    /// Atomically add `entry` or replace an existing one with the same id.
    /// Returns `true` if an existing entry was replaced, `false` if the
    /// entry was newly added.
    ///
    /// The replacement happens before the save so the registry is never
    /// observable in a state with two entries sharing the same id, even on
    /// a crash between the two mutations (the on-disk `repos.json` is only
    /// written once, atomically with respect to the lock).
    pub fn add_or_replace(&mut self, entry: RepoEntry) -> anyhow::Result<bool> {
        let replaced = self.data.repositories.iter().position(|r| r.id == entry.id);
        let was_replaced = replaced.is_some();
        if let Some(idx) = replaced {
            self.data.repositories.remove(idx);
        }
        self.data.repositories.push(entry);
        self.save()?;
        Ok(was_replaced)
    }

    pub fn remove(&mut self, id: &str) -> anyhow::Result<()> {
        let index = self
            .data
            .repositories
            .iter()
            .position(|r| r.id == id)
            .ok_or_else(|| anyhow::anyhow!("Repository '{}' not found", id))?;
        self.data.repositories.remove(index);
        self.save()?;
        Ok(())
    }

    pub fn get(&mut self, id: &str) -> Option<&RepoEntry> {
        let _ = self.reload_if_stale();
        self.data.repositories.iter().find(|r| r.id == id)
    }

    pub fn list(&mut self) -> &[RepoEntry] {
        let _ = self.reload_if_stale();
        &self.data.repositories
    }

    pub fn update_status(&mut self, id: &str, status: RepoStatus) -> anyhow::Result<()> {
        let entry = self
            .data
            .repositories
            .iter_mut()
            .find(|r| r.id == id)
            .ok_or_else(|| anyhow::anyhow!("Repository '{}' not found", id))?;
        entry.status = status;
        self.save()?;
        Ok(())
    }

    pub fn update_last_indexed(&mut self, id: &str) -> anyhow::Result<()> {
        let entry = self
            .data
            .repositories
            .iter_mut()
            .find(|r| r.id == id)
            .ok_or_else(|| anyhow::anyhow!("Repository '{}' not found", id))?;
        entry.last_indexed = Some(crate::time_utils::chrono_now());
        self.save()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{AuthType, RepoEntry, RepoStatus};
    use tempfile::TempDir;

    fn make_entry(id: &str, url: &str, local_path: &str) -> RepoEntry {
        RepoEntry {
            id: id.to_string(),
            url: url.to_string(),
            auth_type: AuthType::Ssh,
            local_path: local_path.to_string(),
            branch: "main".to_string(),
            webhook_secret: None,
            last_indexed: None,
            status: RepoStatus::Indexed,
        }
    }

    #[test]
    fn test_create_empty_registry() {
        let dir = TempDir::new().unwrap();
        let mut registry = Registry::load_or_create(dir.path()).unwrap();
        assert!(registry.list().is_empty());
    }

    #[test]
    fn test_add_repository() {
        let dir = TempDir::new().unwrap();
        let mut registry = Registry::load_or_create(dir.path()).unwrap();
        let repo = make_entry("test-repo", "git@github.com:org/test.git", "/tmp/test-repo");
        registry.add_or_replace(repo.clone()).unwrap();

        let mut reloaded = Registry::load_or_create(dir.path()).unwrap();
        assert_eq!(reloaded.list().len(), 1);
        assert_eq!(reloaded.list()[0].id, "test-repo");
    }

    #[test]
    fn test_add_or_replace_inserts_new_entry() {
        let dir = TempDir::new().unwrap();
        let mut registry = Registry::load_or_create(dir.path()).unwrap();
        let repo = make_entry("new-repo", "git@github.com:org/new.git", "/tmp/new");
        let replaced = registry.add_or_replace(repo).unwrap();
        assert!(!replaced);
        assert_eq!(registry.list().len(), 1);
        assert_eq!(registry.list()[0].url, "git@github.com:org/new.git");
    }

    #[test]
    fn test_add_or_replace_overwrites_existing_entry() {
        let dir = TempDir::new().unwrap();
        let mut registry = Registry::load_or_create(dir.path()).unwrap();
        let old = make_entry("dup", "git@github.com:org/old.git", "/tmp/dup");
        registry.add_or_replace(old).unwrap();

        let updated = make_entry("dup", "git@github.com:org/new.git", "/tmp/dup");
        let replaced = registry.add_or_replace(updated).unwrap();
        assert!(replaced);
        assert_eq!(registry.list().len(), 1);
        assert_eq!(registry.list()[0].url, "git@github.com:org/new.git");

        // Reload to confirm the replacement was persisted.
        let mut reloaded = Registry::load_or_create(dir.path()).unwrap();
        assert_eq!(reloaded.list().len(), 1);
        assert_eq!(reloaded.list()[0].url, "git@github.com:org/new.git");
    }

    #[test]
    fn test_remove_repository() {
        let dir = TempDir::new().unwrap();
        let mut registry = Registry::load_or_create(dir.path()).unwrap();
        let repo = make_entry("to-remove", "git@github.com:org/remove.git", "/tmp/remove");
        registry.add_or_replace(repo).unwrap();
        registry.remove("to-remove").unwrap();
        assert!(registry.list().is_empty());
    }

    #[test]
    fn test_remove_nonexistent_returns_error() {
        let dir = TempDir::new().unwrap();
        let mut registry = Registry::load_or_create(dir.path()).unwrap();
        let result = registry.remove("ghost");
        assert!(result.is_err());
    }

    #[test]
    fn test_update_status() {
        let dir = TempDir::new().unwrap();
        let mut registry = Registry::load_or_create(dir.path()).unwrap();
        let repo = make_entry("r1", "git@github.com:org/r1.git", "/tmp/r1");
        registry.add_or_replace(repo).unwrap();
        registry.update_status("r1", RepoStatus::Indexing).unwrap();
        assert_eq!(registry.get("r1").unwrap().status, RepoStatus::Indexing);
    }

    #[test]
    fn test_update_last_indexed() {
        let dir = TempDir::new().unwrap();
        let mut registry = Registry::load_or_create(dir.path()).unwrap();
        let repo = make_entry("r1", "git@github.com:org/r1.git", "/tmp/r1");
        registry.add_or_replace(repo).unwrap();
        assert!(registry.get("r1").unwrap().last_indexed.is_none());
        registry.update_last_indexed("r1").unwrap();
        assert!(registry.get("r1").unwrap().last_indexed.is_some());
    }

    #[test]
    fn test_list_repositories() {
        let dir = TempDir::new().unwrap();
        let mut registry = Registry::load_or_create(dir.path()).unwrap();
        registry
            .add_or_replace(make_entry("a", "git@github.com:org/a.git", "/tmp/a"))
            .unwrap();
        registry
            .add_or_replace(make_entry("b", "git@github.com:org/b.git", "/tmp/b"))
            .unwrap();
        assert_eq!(registry.list().len(), 2);
    }

    #[test]
    fn test_get_repository() {
        let dir = TempDir::new().unwrap();
        let mut registry = Registry::load_or_create(dir.path()).unwrap();
        let repo = make_entry("target", "git@github.com:org/target.git", "/tmp/target");
        registry.add_or_replace(repo).unwrap();
        let found = registry.get("target").unwrap();
        assert_eq!(found.id, "target");
        assert!(registry.get("nonexistent").is_none());
    }
}
