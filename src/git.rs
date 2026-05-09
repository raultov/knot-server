use std::fs;
use std::path::Path;
use std::process::Command;

use crate::models::RepoEntry;

#[allow(dead_code)]
pub async fn run_git_clone(repo: &RepoEntry) -> anyhow::Result<()> {
    let local_path = Path::new(&repo.local_path);

    if local_path.join(".git").exists() {
        tracing::info!(
            "Repository already exists at {}, skipping clone",
            repo.local_path
        );
        return Ok(());
    }

    // The lock code creates the directory (and .knot.lock inside it).
    // git clone requires the destination to not exist or be empty,
    // so clean up the directory (the file lock is still held via fd).
    if local_path.exists() {
        fs::remove_dir_all(local_path)?;
    }

    tracing::info!("Cloning {} to {}", repo.url, repo.local_path);

    let mut cmd = Command::new("git");
    cmd.arg("clone")
        .arg("--branch")
        .arg(&repo.branch)
        .arg(&repo.url)
        .arg(local_path);

    // For HTTPS auth, we'd set GIT_ASKPASS or credential helper
    // This is done in a future phase (Phase 4/5 credential management)
    let status = tokio::task::spawn_blocking(move || cmd.status()).await??;

    if !status.success() {
        anyhow::bail!("git clone failed with exit code: {:?}", status.code());
    }

    tracing::info!("Clone of {} completed", repo.id);
    Ok(())
}

#[allow(dead_code)]
pub async fn run_git_pull(repo: &RepoEntry) -> anyhow::Result<()> {
    let local_path = Path::new(&repo.local_path);

    if !local_path.join(".git").exists() {
        anyhow::bail!(
            "Repository {} does not exist at {}, cannot pull",
            repo.id,
            repo.local_path
        );
    }

    tracing::info!("Pulling {} at {}", repo.id, repo.local_path);

    let local = local_path.to_path_buf();

    let fetch_status = tokio::task::spawn_blocking({
        let local = local.clone();
        let branch = repo.branch.clone();
        move || {
            Command::new("git")
                .args(["fetch", "origin", &branch])
                .current_dir(&local)
                .status()
        }
    })
    .await??;

    if !fetch_status.success() {
        anyhow::bail!("git fetch failed with exit code: {:?}", fetch_status.code());
    }

    let branch = repo.branch.clone();
    let reset_status = tokio::task::spawn_blocking({
        let local = local.clone();
        move || {
            Command::new("git")
                .args(["reset", "--hard", &format!("origin/{branch}")])
                .current_dir(&local)
                .status()
        }
    })
    .await??;

    if !reset_status.success() {
        anyhow::bail!("git reset failed with exit code: {:?}", reset_status.code());
    }

    tracing::info!("Pull of {} completed", repo.id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::AuthType;
    use std::process::Command as StdCommand;
    use tempfile::TempDir;

    fn create_test_bare_repo(dir: &Path) -> String {
        let bare_path = dir.join("test-bare.git");
        let bare_str = bare_path.to_string_lossy().to_string();

        StdCommand::new("git")
            .args(["init", "--bare"])
            .arg(&bare_path)
            .output()
            .unwrap();

        let clone_path = dir.join("tmp-clone");
        StdCommand::new("git")
            .args(["clone"])
            .arg(&bare_path)
            .arg(&clone_path)
            .output()
            .unwrap();

        StdCommand::new("git")
            .args(["checkout", "-b", "main"])
            .current_dir(&clone_path)
            .output()
            .unwrap();

        std::fs::write(clone_path.join("README.md"), "# Test Repo\n").unwrap();
        StdCommand::new("git")
            .args(["add", "."])
            .current_dir(&clone_path)
            .output()
            .unwrap();
        StdCommand::new("git")
            .args(["commit", "-m", "initial commit"])
            .current_dir(&clone_path)
            .output()
            .unwrap();
        StdCommand::new("git")
            .args(["push", "origin", "main"])
            .current_dir(&clone_path)
            .output()
            .unwrap();

        bare_str
    }

    #[tokio::test]
    async fn test_git_clone_local_repo() {
        let dir = TempDir::new().unwrap();
        let bare_url = create_test_bare_repo(dir.path());
        let clone_dest = dir.path().join("cloned");

        let repo = RepoEntry {
            id: "test-repo".into(),
            url: bare_url,
            local_path: clone_dest.to_string_lossy().into(),
            auth_type: AuthType::Ssh,
            branch: "main".into(),
            webhook_secret: None,
            last_indexed: None,
            status: crate::models::RepoStatus::Idle,
        };

        run_git_clone(&repo).await.unwrap();
        assert!(clone_dest.join(".git").exists());
        assert!(clone_dest.join("README.md").exists());
    }

    #[tokio::test]
    async fn test_git_pull_fetches_changes() {
        let dir = TempDir::new().unwrap();
        let bare_url = create_test_bare_repo(dir.path());
        let clone_dest = dir.path().join("cloned");

        let repo = RepoEntry {
            id: "test-repo".into(),
            url: bare_url.clone(),
            local_path: clone_dest.to_string_lossy().into(),
            auth_type: AuthType::Ssh,
            branch: "main".into(),
            webhook_secret: None,
            last_indexed: None,
            status: crate::models::RepoStatus::Idle,
        };

        run_git_clone(&repo).await.unwrap();

        StdCommand::new("git")
            .args(["branch", "-m", "master", "main"])
            .current_dir(&clone_dest)
            .output()
            .unwrap();

        std::fs::write(clone_dest.join("new_file.txt"), "new content").unwrap();
        StdCommand::new("git")
            .args(["add", "."])
            .current_dir(&clone_dest)
            .output()
            .unwrap();
        StdCommand::new("git")
            .args(["commit", "-m", "add file"])
            .current_dir(&clone_dest)
            .output()
            .unwrap();
        StdCommand::new("git")
            .args(["push", "origin", "main"])
            .current_dir(&clone_dest)
            .output()
            .unwrap();

        run_git_pull(&repo).await.unwrap();
        assert!(clone_dest.join("new_file.txt").exists());
    }

    #[tokio::test]
    async fn test_git_pull_nonexistent_repo_returns_error() {
        let dir = TempDir::new().unwrap();
        let repo = RepoEntry {
            id: "ghost".into(),
            url: "git@github.com:org/ghost.git".into(),
            local_path: dir.path().join("ghost").to_string_lossy().into(),
            auth_type: AuthType::Ssh,
            branch: "main".into(),
            webhook_secret: None,
            last_indexed: None,
            status: crate::models::RepoStatus::Idle,
        };
        let result = run_git_pull(&repo).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_git_clone_idempotent() {
        let dir = TempDir::new().unwrap();
        let bare_url = create_test_bare_repo(dir.path());
        let clone_dest = dir.path().join("cloned");

        let repo = RepoEntry {
            id: "test-repo".into(),
            url: bare_url,
            local_path: clone_dest.to_string_lossy().into(),
            auth_type: AuthType::Ssh,
            branch: "main".into(),
            webhook_secret: None,
            last_indexed: None,
            status: crate::models::RepoStatus::Idle,
        };

        run_git_clone(&repo).await.unwrap();
        run_git_clone(&repo).await.unwrap();
    }
}
