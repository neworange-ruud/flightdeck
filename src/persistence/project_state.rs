//! Load and save `state.json` (SPECS §9). Stores relative paths; serde-backed.

use crate::contracts::{FileSystem, FlightDeckError, ProjectState, Result, STATE_VERSION};
use std::path::Path;

/// A fresh, empty project state for `base_branch` (SPECS §9).
pub fn default_state(base_branch: &str) -> ProjectState {
    ProjectState {
        version: STATE_VERSION,
        project_root_relative: ".".into(),
        base_branch: base_branch.into(),
        tabs: vec![],
    }
}

/// Load and deserialize `state.json` (SPECS §9).
pub fn load_state(fs: &dyn FileSystem, path: &Path) -> Result<ProjectState> {
    let contents = fs.read_to_string(path).map_err(|e| {
        FlightDeckError::State(format!("failed to read state file {}: {e}", path.display()))
    })?;
    let state: ProjectState = serde_json::from_str(&contents)
        .map_err(|e| FlightDeckError::State(format!("failed to parse state file: {e}")))?;
    Ok(state)
}

/// Serialize and write `state.json` (SPECS §9).
pub fn save_state(fs: &dyn FileSystem, path: &Path, state: &ProjectState) -> Result<()> {
    let json = serde_json::to_string_pretty(state)
        .map_err(|e| FlightDeckError::State(format!("failed to serialize state: {e}")))?;
    fs.write(path, &json)
        .map_err(|e| FlightDeckError::State(format!("failed to write state file: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::domain::{TabState, STATE_VERSION};
    use crate::testing::FakeFs;
    use std::path::Path;

    fn sample_tab() -> TabState {
        TabState {
            id: "tab-1".to_string(),
            name: "My Task".to_string(),
            slug: "my-task".to_string(),
            agent: "opencode".to_string(),
            branch: "flightdeck/my-task".to_string(),
            worktree_path_relative: ".flightdeck/worktrees/my-task".to_string(),
            base_branch: "main".to_string(),
            base_commit_sha: "abc123".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            attached_existing_branch: false,
            recovered: false,
            last_known_status: "running".to_string(),
            manual_status: None,
            containerized: false,
            container_image: None,
        }
    }

    #[test]
    fn default_state_has_correct_shape() {
        let s = default_state("main");
        assert_eq!(s.version, STATE_VERSION);
        assert_eq!(s.project_root_relative, ".");
        assert_eq!(s.base_branch, "main");
        assert!(s.tabs.is_empty());
    }

    #[test]
    fn round_trip_save_then_load() {
        let fs = FakeFs::new();
        let path = Path::new("/repo/.flightdeck/state.json");

        let mut original = default_state("main");
        original.tabs.push(sample_tab());

        save_state(&fs, path, &original).expect("save should succeed");
        let loaded = load_state(&fs, path).expect("load should succeed");

        assert_eq!(original, loaded);
    }

    #[test]
    fn load_state_missing_file_returns_err() {
        let fs = FakeFs::new();
        let path = Path::new("/repo/.flightdeck/state.json");

        let result = load_state(&fs, path);
        assert!(result.is_err(), "missing file should yield Err");
        // Must be a State error
        match result.unwrap_err() {
            FlightDeckError::State(_) => {}
            other => panic!("expected State error, got: {other:?}"),
        }
    }

    #[test]
    fn load_state_malformed_json_returns_state_err() {
        let fs = FakeFs::new().with_file("/repo/.flightdeck/state.json", "not valid json {{{{");
        let path = Path::new("/repo/.flightdeck/state.json");

        let result = load_state(&fs, path);
        assert!(result.is_err());
        match result.unwrap_err() {
            FlightDeckError::State(_) => {}
            other => panic!("expected State error, got: {other:?}"),
        }
    }

    #[test]
    fn save_state_writes_valid_json() {
        let fs = FakeFs::new();
        let path = Path::new("/repo/.flightdeck/state.json");
        let state = default_state("develop");

        save_state(&fs, path, &state).expect("save should succeed");

        let contents = fs
            .file_contents(path)
            .expect("file should exist after save");
        // Must be parseable JSON
        let parsed: serde_json::Value =
            serde_json::from_str(&contents).expect("saved content is valid JSON");
        assert_eq!(parsed["base_branch"], "develop");
        assert_eq!(parsed["version"], STATE_VERSION);
    }
}
