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
    configured_embed_model: &str,
    clean: bool,
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

    match knot::pipeline::state::IndexState::load_for_indexer(
        repo_path,
        configured_embed_model,
        clean,
    ) {
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
        // Persist through knot's own `save` so the fixture always carries the
        // current schema version, rather than pinning a literal that breaks
        // every time knot bumps `CURRENT_STATE_VERSION`.
        let mut state = knot::pipeline::state::IndexState::default();
        state
            .file_hashes
            .insert("a.rs".to_string(), "h1".to_string());
        state
            .file_hashes
            .insert("b.rs".to_string(), "h2".to_string());
        state.save(repo_path).unwrap();

        let loaded =
            load_index_state_with_recovery(repo_path, true, "AllMiniLML6V2", false).unwrap();

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
        let loaded = load_index_state_with_recovery(
            dir.path().to_str().unwrap(),
            true,
            "AllMiniLML6V2",
            false,
        )
        .unwrap();

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

        let loaded = load_index_state_with_recovery(
            dir.path().to_str().unwrap(),
            true,
            "AllMiniLML6V2",
            false,
        )
        .unwrap();

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

        let loaded = load_index_state_with_recovery(
            dir.path().to_str().unwrap(),
            true,
            "AllMiniLML6V2",
            false,
        )
        .unwrap();

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

        let result = load_index_state_with_recovery(
            dir.path().to_str().unwrap(),
            false,
            "AllMiniLML6V2",
            false,
        );

        assert!(result.is_err());
    }

    #[test]
    fn test_model_mismatch_on_local_repo_falls_back_to_full_reindex() {
        // A state file persisted under model A, loaded under model B: for a
        // local repo the `LoadErrorFallback` branch deletes the stale state
        // and re-indexes fully — exactly the right recovery for a model
        // switch on a local repo (decided in S1 of the plan).
        let dir = TempDir::new().unwrap();
        let repo_path = dir.path().to_str().unwrap();
        let mut state = knot::pipeline::state::IndexState {
            embed_model: Some("AllMiniLML6V2".to_string()),
            ..Default::default()
        };
        state
            .file_hashes
            .insert("a.rs".to_string(), "h1".to_string());
        state.save(repo_path).unwrap();

        let loaded =
            load_index_state_with_recovery(repo_path, true, "BGEBaseENV15", false).unwrap();

        match loaded.source {
            StateSource::LoadErrorFallback { error } => {
                assert!(error.contains("AllMiniLML6V2"), "{error}");
                assert!(error.contains("BGEBaseENV15"), "{error}");
            }
            other => panic!("expected LoadErrorFallback, got {other:?}"),
        }
        assert!(loaded.state.file_hashes.is_empty());
        assert!(
            !Path::new(repo_path)
                .join(".knot")
                .join("index_state.json")
                .exists(),
            "the stale state was deleted so the next run re-indexes cleanly"
        );
    }

    #[test]
    fn test_model_mismatch_on_remote_repo_fails_the_job() {
        // For a remote repo there is no recovery: the model mismatch must
        // fail the job with an actionable error instead of silently mixing
        // vectors from two models.
        let dir = TempDir::new().unwrap();
        let repo_path = dir.path().to_str().unwrap();
        let state = knot::pipeline::state::IndexState {
            embed_model: Some("AllMiniLML6V2".to_string()),
            ..Default::default()
        };
        state.save(repo_path).unwrap();
        let result = load_index_state_with_recovery(repo_path, false, "BGEBaseENV15", false);

        let err = match result {
            Ok(loaded) => panic!("expected Err, got {:?}", loaded.source),
            Err(e) => e.to_string(),
        };
        assert!(err.contains("AllMiniLML6V2"), "{err}");
        assert!(err.contains("BGEBaseENV15"), "{err}");
    }

    #[test]
    fn test_matching_model_loads_ok() {
        // Same model + `clean = false`: the normal incremental path must be
        // untouched by the new guard.
        let dir = TempDir::new().unwrap();
        let repo_path = dir.path().to_str().unwrap();
        let mut state = knot::pipeline::state::IndexState {
            embed_model: Some("AllMiniLML6V2".to_string()),
            ..Default::default()
        };
        state
            .file_hashes
            .insert("a.rs".to_string(), "h1".to_string());
        state.save(repo_path).unwrap();

        let loaded =
            load_index_state_with_recovery(repo_path, true, "AllMiniLML6V2", false).unwrap();

        match loaded.source {
            StateSource::LoadedOk { entries, .. } => assert_eq!(entries, 1),
            other => panic!("expected LoadedOk, got {other:?}"),
        }
    }

    #[test]
    fn test_model_mismatch_with_clean_true_loads_ok() {
        // `clean = true` means a deliberate full re-index: the persisted
        // model is allowed to differ.
        let dir = TempDir::new().unwrap();
        let repo_path = dir.path().to_str().unwrap();
        let mut state = knot::pipeline::state::IndexState {
            embed_model: Some("AllMiniLML6V2".to_string()),
            ..Default::default()
        };
        state
            .file_hashes
            .insert("a.rs".to_string(), "h1".to_string());
        state.save(repo_path).unwrap();

        let loaded = load_index_state_with_recovery(repo_path, true, "BGEBaseENV15", true).unwrap();

        match loaded.source {
            StateSource::LoadedOk { entries, .. } => assert_eq!(entries, 1),
            other => panic!("expected LoadedOk, got {other:?}"),
        }
    }
}
