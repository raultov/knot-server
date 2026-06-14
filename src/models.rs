use crate::registry::Registry;
use knot::db::graph::GraphDb;
use knot::db::vector::VectorDb;
use knot::pipeline::embed::Embedder;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use utoipa::ToSchema;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum AuthType {
    Ssh,
    Https,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum RepoStatus {
    Pending,
    #[serde(alias = "idle")]
    Indexed,
    Cloning,
    Pulling,
    Indexing,
    Error,
}

impl std::fmt::Display for RepoStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => write!(f, "pending"),
            Self::Indexed => write!(f, "indexed"),
            Self::Cloning => write!(f, "cloning"),
            Self::Pulling => write!(f, "pulling"),
            Self::Indexing => write!(f, "indexing"),
            Self::Error => write!(f, "error"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RepoEntry {
    pub id: String,
    pub url: String,
    pub auth_type: AuthType,
    pub local_path: String,
    #[serde(default = "default_branch")]
    pub branch: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(write_only)]
    pub webhook_secret: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_indexed: Option<String>,
    #[serde(default = "default_status")]
    pub status: RepoStatus,
}

fn default_branch() -> String {
    "main".to_string()
}

fn default_status() -> RepoStatus {
    RepoStatus::Indexed
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryData {
    pub repositories: Vec<RepoEntry>,
}

#[derive(Debug, Clone)]
pub enum IndexJob {
    Clone { repo_id: String },
    Pull { repo_id: String },
}

impl IndexJob {
    pub fn repo_id(&self) -> &str {
        match self {
            Self::Clone { repo_id } | Self::Pull { repo_id } => repo_id,
        }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct RegisterRepoRequest {
    /// Git repository URL (HTTPS, SSH, or local absolute path)
    #[schema(example = "https://github.com/raultov/knot.git")]
    pub url: String,
    /// Authentication method
    #[serde(default = "default_auth_type")]
    pub auth_type: AuthType,
    /// Branch to clone
    #[schema(example = "master")]
    #[serde(default = "default_branch")]
    pub branch: String,
    /// Shared secret for webhook signature validation (HMAC-SHA256)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub webhook_secret: Option<String>,
}

fn default_auth_type() -> AuthType {
    AuthType::Ssh
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct RegisterRepoResponse {
    pub id: String,
    pub message: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct RepoListResponse {
    pub repositories: Vec<RepoEntry>,
}

impl RegisterRepoRequest {
    pub fn generate_id(&self) -> String {
        let url = &self.url;
        let raw_name: &str = if url.contains("://") {
            url.rsplit('/').next().unwrap_or(url)
        } else if let Some(pos) = url.find(':') {
            let after_colon = &url[pos + 1..];
            after_colon.rsplit('/').next().unwrap_or(after_colon)
        } else {
            url.rsplit('/').next().unwrap_or(url)
        };
        let trimmed = raw_name.trim_end_matches('/');
        let name = trimmed.strip_suffix(".git").unwrap_or(trimmed);
        name.chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' {
                    c
                } else {
                    '-'
                }
            })
            .collect()
    }
}

pub fn repo_local_path(workspace_dir: &str, repo_id: &str) -> String {
    PathBuf::from(workspace_dir)
        .join(repo_id)
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_repo_id_generation_github_ssh() {
        let req = RegisterRepoRequest {
            url: "git@github.com:company/backend.git".into(),
            auth_type: AuthType::Ssh,
            branch: "main".into(),
            webhook_secret: None,
        };
        assert_eq!(req.generate_id(), "backend");
    }

    #[test]
    fn test_repo_id_generation_https() {
        let req = RegisterRepoRequest {
            url: "https://github.com/company/my-service.git".into(),
            auth_type: AuthType::Https,
            branch: "main".into(),
            webhook_secret: None,
        };
        assert_eq!(req.generate_id(), "my-service");
    }

    #[test]
    fn test_repo_id_generation_no_git_suffix() {
        let req = RegisterRepoRequest {
            url: "git@github.com:org/library".into(),
            auth_type: AuthType::Ssh,
            branch: "main".into(),
            webhook_secret: None,
        };
        assert_eq!(req.generate_id(), "library");
    }

    #[test]
    fn test_repo_local_path() {
        let path = repo_local_path("/var/lib/knot/repos", "my-repo");
        assert_eq!(path, "/var/lib/knot/repos/my-repo");
    }

    #[test]
    fn test_repo_status_display() {
        assert_eq!(RepoStatus::Pending.to_string(), "pending");
        assert_eq!(RepoStatus::Indexed.to_string(), "indexed");
        assert_eq!(RepoStatus::Cloning.to_string(), "cloning");
        assert_eq!(RepoStatus::Pulling.to_string(), "pulling");
        assert_eq!(RepoStatus::Indexing.to_string(), "indexing");
        assert_eq!(RepoStatus::Error.to_string(), "error");
    }

    #[test]
    fn test_repo_status_deserialization_alias() {
        let json = r#"{"status": "idle"}"#;
        #[derive(Deserialize)]
        struct Wrapper {
            status: RepoStatus,
        }
        let parsed: Wrapper = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.status, RepoStatus::Indexed);
    }

    #[test]
    fn test_index_job_repo_id() {
        let job = IndexJob::Clone {
            repo_id: "test".into(),
        };
        assert_eq!(job.repo_id(), "test");

        let job = IndexJob::Pull {
            repo_id: "test2".into(),
        };
        assert_eq!(job.repo_id(), "test2");
    }

    #[test]
    fn test_register_repo_defaults() {
        let req = RegisterRepoRequest {
            url: "git@github.com:org/repo.git".into(),
            auth_type: AuthType::Ssh,
            branch: "main".into(),
            webhook_secret: None,
        };
        assert_eq!(req.auth_type, AuthType::Ssh);
        assert_eq!(req.branch, "main");
        assert_eq!(req.webhook_secret, None);
    }
}

pub struct AppState {
    pub vector_db: Arc<VectorDb>,
    pub graph_db: Arc<GraphDb>,
    pub embedder: Option<Arc<Mutex<Embedder>>>,
    pub workspace_dir: String,
    pub registry: Arc<Mutex<Registry>>,
    pub job_tx: tokio::sync::mpsc::Sender<IndexJob>,
    pub qdrant_url: String,
    pub qdrant_collection: String,
    pub neo4j_uri: String,
    pub neo4j_user: String,
    pub neo4j_password: String,
    pub embed_dim: u64,
    pub rayon_threads: Option<usize>,
    pub batch_size: usize,
    pub ingest_concurrency: usize,
    pub start_time: std::time::Instant,
}
