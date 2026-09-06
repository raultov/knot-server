use std::path::Path;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum StateSource {
    LoadedOk { entries: usize, bytes: u64 },
    Missing,
    LegacyCleared,
    LoadErrorFallback { error: String },
}

pub(crate) struct LoadedState {
    pub state: knot::pipeline::state::IndexState,
    pub source: StateSource,
}

pub(crate) fn load_index_state_with_recovery(
    repo_path: &str,
    is_local: bool,
) -> anyhow::Result<LoadedState> {
    let state_file = Path::new(repo_path).join(".knot").join("index_state.json");

    if is_local && crate::local_sync::clear_stale_index_state(repo_path) {
        return Ok(LoadedState {
            state: knot::pipeline::state::IndexState::default(),
            source: StateSource::LegacyCleared,
        });
    }

    if !state_file.exists() {
        return Ok(LoadedState {
            state: knot::pipeline::state::IndexState::default(),
            source: StateSource::Missing,
        });
    }

    let bytes = std::fs::metadata(&state_file).map(|m| m.len()).unwrap_or(0);

    match knot::pipeline::state::IndexState::load(repo_path) {
        Ok(state) => {
            let entries = state.file_hashes.len();
            Ok(LoadedState {
                state,
                source: StateSource::LoadedOk { entries, bytes },
            })
        }
        Err(e) if is_local => {
            let _ = std::fs::remove_file(&state_file);
            Ok(LoadedState {
                state: knot::pipeline::state::IndexState::default(),
                source: StateSource::LoadErrorFallback {
                    error: format!("{e:#}"),
                },
            })
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_load_state_returns_loaded_ok_when_state_is_valid() {
        let dir = TempDir::new().unwrap();
        let repo_path = dir.path().to_str().unwrap();
        let knot_dir = dir.path().join(".knot");
        std::fs::create_dir_all(&knot_dir).unwrap();
        let raw = r#"{"version":4,"file_hashes":{"a.rs":"h1","b.rs":"h2"}}"#;
        std::fs::write(knot_dir.join("index_state.json"), raw).unwrap();

        let loaded = load_index_state_with_recovery(repo_path, true).unwrap();

        match loaded.source {
            StateSource::LoadedOk { entries, bytes } => {
                assert_eq!(entries, 2);
                assert!(bytes > 0);
            }
            other => panic!("expected LoadedOk, got {other:?}"),
        }
        assert_eq!(loaded.state.file_hashes.len(), 2);
    }

    #[test]
    fn test_load_state_returns_missing_when_state_absent() {
        let dir = TempDir::new().unwrap();
        let loaded = load_index_state_with_recovery(dir.path().to_str().unwrap(), true).unwrap();

        assert!(matches!(loaded.source, StateSource::Missing));
        assert!(loaded.state.file_hashes.is_empty());
    }

    #[test]
    fn test_load_state_returns_legacy_cleared_for_local_repo_with_v0_state() {
        let dir = TempDir::new().unwrap();
        let knot_dir = dir.path().join(".knot");
        std::fs::create_dir_all(&knot_dir).unwrap();
        let raw = r#"{"file_hashes":{"a.rs":"h1"}}"#;
        std::fs::write(knot_dir.join("index_state.json"), raw).unwrap();

        let loaded = load_index_state_with_recovery(dir.path().to_str().unwrap(), true).unwrap();

        assert!(matches!(loaded.source, StateSource::LegacyCleared));
        assert!(loaded.state.file_hashes.is_empty());
        assert!(
            !knot_dir.join("index_state.json").exists(),
            "The legacy file was deleted"
        );
    }

    #[test]
    fn test_load_state_returns_error_fallback_when_json_is_corrupt() {
        let dir = TempDir::new().unwrap();
        let knot_dir = dir.path().join(".knot");
        std::fs::create_dir_all(&knot_dir).unwrap();
        let raw = r#"{"version":4,"file_hashes":NOT_VALID_JSON}"#;
        std::fs::write(knot_dir.join("index_state.json"), raw).unwrap();

        let loaded = load_index_state_with_recovery(dir.path().to_str().unwrap(), true).unwrap();

        match loaded.source {
            StateSource::LoadErrorFallback { error } => {
                assert!(!error.is_empty());
            }
            other => panic!("expected LoadErrorFallback, got {other:?}"),
        }
        assert!(loaded.state.file_hashes.is_empty());
        assert!(
            !knot_dir.join("index_state.json").exists(),
            "The corrupted file was deleted to avoid blocking the next run"
        );
    }

    #[test]
    fn test_load_state_for_remote_repo_propagates_errors() {
        let dir = TempDir::new().unwrap();
        let knot_dir = dir.path().join(".knot");
        std::fs::create_dir_all(&knot_dir).unwrap();
        let raw = r#"{"version":1,"file_hashes":{}}"#;
        std::fs::write(knot_dir.join("index_state.json"), raw).unwrap();

        let result = load_index_state_with_recovery(dir.path().to_str().unwrap(), false);

        assert!(result.is_err());
    }
}
