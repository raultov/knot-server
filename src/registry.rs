use std::fs;
use std::path::{Path, PathBuf};

use crate::locking::acquire_file_lock;
use crate::models::{RegistryData, RepoEntry, RepoStatus};

pub struct Registry {
    data: RegistryData,
    #[allow(dead_code)]
    workspace_dir: PathBuf,
    json_path: PathBuf,
    lock_path: PathBuf,
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
            workspace_dir: workspace_dir.to_path_buf(),
            json_path,
            lock_path,
        })
    }

    fn save(&self) -> anyhow::Result<()> {
        let _lock = acquire_file_lock(&self.lock_path)?;
        let json = serde_json::to_string_pretty(&self.data)?;
        fs::write(&self.json_path, json)?;
        Ok(())
    }

    pub fn add(&mut self, entry: RepoEntry) -> anyhow::Result<()> {
        if self.data.repositories.iter().any(|r| r.id == entry.id) {
            anyhow::bail!("Repository '{}' already exists", entry.id);
        }
        self.data.repositories.push(entry);
        self.save()?;
        Ok(())
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

    pub fn get(&self, id: &str) -> Option<&RepoEntry> {
        self.data.repositories.iter().find(|r| r.id == id)
    }

    pub fn list(&self) -> &[RepoEntry] {
        &self.data.repositories
    }

    #[allow(dead_code)]
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

    #[allow(dead_code)]
    pub fn update_last_indexed(&mut self, id: &str) -> anyhow::Result<()> {
        let entry = self
            .data
            .repositories
            .iter_mut()
            .find(|r| r.id == id)
            .ok_or_else(|| anyhow::anyhow!("Repository '{}' not found", id))?;
        entry.last_indexed = Some(chrono_now());
        self.save()?;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn workspace_dir(&self) -> &Path {
        &self.workspace_dir
    }
}

#[allow(dead_code)]
fn chrono_now() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    // Simple ISO 8601 formatting without chrono dependency
    let days = secs / 86400;
    let remaining = secs % 86400;
    let hours = remaining / 3600;
    let remaining = remaining % 3600;
    let minutes = remaining / 60;
    let seconds = remaining % 60;

    // Calculate year/month/day from days since epoch (approximate, good enough)
    let mut year = 1970i64;
    let mut days_left = days as i64;
    loop {
        let days_in_year = if is_leap(year) { 366 } else { 365 };
        if days_left < days_in_year {
            break;
        }
        days_left -= days_in_year;
        year += 1;
    }

    let month_days = if is_leap(year) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };
    let mut month = 1u32;
    for &md in &month_days {
        if days_left < md as i64 {
            break;
        }
        days_left -= md as i64;
        month += 1;
    }
    let day = days_left as u32 + 1;

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, month, day, hours, minutes, seconds
    )
}

#[allow(dead_code)]
fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
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
        let registry = Registry::load_or_create(dir.path()).unwrap();
        assert!(registry.list().is_empty());
    }

    #[test]
    fn test_add_repository() {
        let dir = TempDir::new().unwrap();
        let mut registry = Registry::load_or_create(dir.path()).unwrap();
        let repo = make_entry("test-repo", "git@github.com:org/test.git", "/tmp/test-repo");
        registry.add(repo.clone()).unwrap();

        let reloaded = Registry::load_or_create(dir.path()).unwrap();
        assert_eq!(reloaded.list().len(), 1);
        assert_eq!(reloaded.list()[0].id, "test-repo");
    }

    #[test]
    fn test_add_duplicate_repository_returns_error() {
        let dir = TempDir::new().unwrap();
        let mut registry = Registry::load_or_create(dir.path()).unwrap();
        let repo = make_entry("dup", "git@github.com:org/dup.git", "/tmp/dup");
        registry.add(repo.clone()).unwrap();
        let result = registry.add(repo);
        assert!(result.is_err());
    }

    #[test]
    fn test_remove_repository() {
        let dir = TempDir::new().unwrap();
        let mut registry = Registry::load_or_create(dir.path()).unwrap();
        let repo = make_entry("to-remove", "git@github.com:org/remove.git", "/tmp/remove");
        registry.add(repo).unwrap();
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
        registry.add(repo).unwrap();
        registry.update_status("r1", RepoStatus::Indexing).unwrap();
        assert_eq!(registry.get("r1").unwrap().status, RepoStatus::Indexing);
    }

    #[test]
    fn test_update_last_indexed() {
        let dir = TempDir::new().unwrap();
        let mut registry = Registry::load_or_create(dir.path()).unwrap();
        let repo = make_entry("r1", "git@github.com:org/r1.git", "/tmp/r1");
        registry.add(repo).unwrap();
        assert!(registry.get("r1").unwrap().last_indexed.is_none());
        registry.update_last_indexed("r1").unwrap();
        assert!(registry.get("r1").unwrap().last_indexed.is_some());
    }

    #[test]
    fn test_list_repositories() {
        let dir = TempDir::new().unwrap();
        let mut registry = Registry::load_or_create(dir.path()).unwrap();
        registry
            .add(make_entry("a", "git@github.com:org/a.git", "/tmp/a"))
            .unwrap();
        registry
            .add(make_entry("b", "git@github.com:org/b.git", "/tmp/b"))
            .unwrap();
        assert_eq!(registry.list().len(), 2);
    }

    #[test]
    fn test_get_repository() {
        let dir = TempDir::new().unwrap();
        let mut registry = Registry::load_or_create(dir.path()).unwrap();
        let repo = make_entry("target", "git@github.com:org/target.git", "/tmp/target");
        registry.add(repo).unwrap();
        let found = registry.get("target").unwrap();
        assert_eq!(found.id, "target");
        assert!(registry.get("nonexistent").is_none());
    }
}
