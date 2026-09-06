use crate::models::{IndexJob, RepoEntry};

/// The git-level operation the worker performs for a job, decided up front by
/// [`decide_job_plan`] instead of being inferred ad-hoc from the on-disk state.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum GitAction {
    /// Remote repo, start from a clean directory (`git clone`).
    FreshClone,
    /// Remote repo with an existing `.git`, incremental `git fetch`/reset.
    Pull,
    /// Local working-tree source, mirror it into the workspace.
    LocalSync,
}

/// The full plan for a job: whether to wipe existing artifacts (databases +
/// local directory) before the git action, and which git action to run.
///
/// Extracting this decision into a pure function makes the previously implicit
/// pull-vs-clone choice (which ignored the job type entirely) testable and
/// removes the race that let a background cleanup delete `local_path` while the
/// worker was mid-fetch.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct JobPlan {
    /// Delete databases + local directory before the git action.
    pub wipe_before: bool,
    pub action: GitAction,
}

/// Decide what a job should do based on its type and the current on-disk state.
///
/// Semantics (design doc §2.2):
/// - `Clone` means "start from scratch": always wipe, then fresh-clone (or
///   local-sync for a local source).
/// - `Pull` is incremental: pull when `.git` exists, but **fall back to a
///   fresh-clone** (with wipe) when the directory is gone, so a manual sync of
///   an errored repo without a directory recovers instead of failing with
///   "cannot pull".
/// - A local source always uses `LocalSync`; it only wipes for a `Clone`.
pub(crate) fn decide_job_plan(job: &IndexJob, git_dir_exists: bool, is_local: bool) -> JobPlan {
    if is_local {
        return JobPlan {
            wipe_before: matches!(job, IndexJob::Clone { .. }),
            action: GitAction::LocalSync,
        };
    }
    match job {
        IndexJob::Clone { .. } => JobPlan {
            wipe_before: true,
            action: GitAction::FreshClone,
        },
        IndexJob::Pull { .. } => {
            if git_dir_exists {
                JobPlan {
                    wipe_before: false,
                    action: GitAction::Pull,
                }
            } else {
                JobPlan {
                    wipe_before: true,
                    action: GitAction::FreshClone,
                }
            }
        }
    }
}

/// Whether a failed job should trigger a destructive wipe of the repo's
/// databases and local directory.
///
/// Policy (design doc §2.3, confirmed 2026-07-06): only wipe repos that never
/// indexed successfully. A repo that was already indexed and fails a
/// transient pull keeps its index and directory (recovery is still available by
/// re-registering, which enqueues a `Clone` = wipe + fresh).
pub(crate) fn should_wipe_on_failure(entry: &RepoEntry) -> bool {
    entry.last_indexed.is_none()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{AuthType, RepoStatus};

    #[test]
    fn test_decide_plan_clone_job_always_fresh_clone_even_if_git_exists() {
        let job = IndexJob::Clone {
            repo_id: "x".into(),
        };
        let plan = decide_job_plan(&job, true, false);
        assert_eq!(
            plan,
            JobPlan {
                wipe_before: true,
                action: GitAction::FreshClone
            }
        );
    }

    #[test]
    fn test_decide_plan_clone_job_on_missing_dir_is_fresh_clone() {
        let job = IndexJob::Clone {
            repo_id: "x".into(),
        };
        let plan = decide_job_plan(&job, false, false);
        assert_eq!(
            plan,
            JobPlan {
                wipe_before: true,
                action: GitAction::FreshClone
            }
        );
    }

    #[test]
    fn test_decide_plan_pull_job_with_git_dir_pulls_without_wipe() {
        let job = IndexJob::Pull {
            repo_id: "x".into(),
        };
        let plan = decide_job_plan(&job, true, false);
        assert_eq!(
            plan,
            JobPlan {
                wipe_before: false,
                action: GitAction::Pull
            }
        );
    }

    #[test]
    fn test_decide_plan_pull_job_without_git_dir_falls_back_to_fresh_clone() {
        let job = IndexJob::Pull {
            repo_id: "x".into(),
        };
        let plan = decide_job_plan(&job, false, false);
        assert_eq!(
            plan,
            JobPlan {
                wipe_before: true,
                action: GitAction::FreshClone
            }
        );
    }

    #[test]
    fn test_decide_plan_local_repo_clone_job_wipes_and_syncs() {
        let job = IndexJob::Clone {
            repo_id: "x".into(),
        };
        // is_local=true: git_dir_exists is irrelevant.
        for git_dir_exists in [true, false] {
            let plan = decide_job_plan(&job, git_dir_exists, true);
            assert_eq!(
                plan,
                JobPlan {
                    wipe_before: true,
                    action: GitAction::LocalSync
                }
            );
        }
    }

    #[test]
    fn test_decide_plan_local_repo_pull_job_syncs_without_wipe() {
        let job = IndexJob::Pull {
            repo_id: "x".into(),
        };
        for git_dir_exists in [true, false] {
            let plan = decide_job_plan(&job, git_dir_exists, true);
            assert_eq!(
                plan,
                JobPlan {
                    wipe_before: false,
                    action: GitAction::LocalSync
                }
            );
        }
    }

    #[test]
    fn test_should_wipe_on_failure_true_when_never_indexed() {
        let entry = RepoEntry {
            id: "x".into(),
            url: "https://example.com/x.git".into(),
            local_path: "/tmp/x".into(),
            auth_type: AuthType::Ssh,
            branch: "main".into(),
            webhook_secret: None,
            last_indexed: None,
            status: RepoStatus::Error,
        };
        assert!(should_wipe_on_failure(&entry));
    }

    #[test]
    fn test_should_wipe_on_failure_false_when_previously_indexed() {
        let entry = RepoEntry {
            id: "x".into(),
            url: "https://example.com/x.git".into(),
            local_path: "/tmp/x".into(),
            auth_type: AuthType::Ssh,
            branch: "main".into(),
            webhook_secret: None,
            last_indexed: Some("2026-07-06T00:00:00Z".into()),
            status: RepoStatus::Error,
        };
        assert!(!should_wipe_on_failure(&entry));
    }
}
