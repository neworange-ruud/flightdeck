//! First-run initialization of `.flightdeck/` (SPECS §7).
//!
//! Creates the metadata directory, `config.toml`, `state.json`, and
//! `worktrees/` if missing. Idempotent: does not duplicate work if already
//! present. The `.gitignore` update is handled separately by [`crate::fs::ignore`]
//! and orchestrated by startup (SPECS §7 step 8).

use crate::config::load::serialize_config;
use crate::config::schema::default_config;
use crate::contracts::{FileSystem, ProjectState, Result, STATE_VERSION};
use std::path::Path;

/// What first-run init created (each `true` only if it was missing before).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InitOutcome {
    pub created_flightdeck_dir: bool,
    pub created_config: bool,
    pub created_state: bool,
    pub created_worktrees_dir: bool,
}

/// Ensure `.flightdeck/` and its contents exist under `repo_root` (SPECS §7).
pub fn initialize(
    fs: &dyn FileSystem,
    repo_root: &Path,
    project_name: &str,
    base_branch: &str,
) -> Result<InitOutcome> {
    let mut outcome = InitOutcome::default();

    let flightdeck_dir = repo_root.join(".flightdeck");
    let config_path = flightdeck_dir.join("config.toml");
    let state_path = flightdeck_dir.join("state.json");
    let worktrees_dir = flightdeck_dir.join("worktrees");

    // 1. Create .flightdeck/ if missing
    if !fs.exists(&flightdeck_dir) {
        fs.create_dir_all(&flightdeck_dir)?;
        outcome.created_flightdeck_dir = true;
    }

    // 2. Create config.toml if missing
    if !fs.exists(&config_path) {
        let cfg = default_config(project_name, base_branch);
        let toml_str = serialize_config(&cfg)?;
        fs.write(&config_path, &toml_str)?;
        outcome.created_config = true;
    }

    // 3. Create state.json if missing
    if !fs.exists(&state_path) {
        let state = ProjectState {
            version: STATE_VERSION,
            project_root_relative: ".".to_string(),
            base_branch: base_branch.to_string(),
            tabs: vec![],
        };
        let json = serde_json::to_string_pretty(&state)
            .map_err(|e| crate::contracts::FlightDeckError::State(e.to_string()))?;
        fs.write(&state_path, &json)?;
        outcome.created_state = true;
    }

    // 4. Create worktrees/ dir if missing
    if !fs.exists(&worktrees_dir) {
        fs.create_dir_all(&worktrees_dir)?;
        outcome.created_worktrees_dir = true;
    }

    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeFs;
    use std::path::Path;

    #[test]
    fn init_creates_all_artifacts_on_fresh_fs() {
        let fs = FakeFs::new();
        let repo = Path::new("/repo");

        let outcome = initialize(&fs, repo, "my-project", "main").unwrap();

        assert!(outcome.created_flightdeck_dir);
        assert!(outcome.created_config);
        assert!(outcome.created_state);
        assert!(outcome.created_worktrees_dir);
    }

    #[test]
    fn init_creates_flightdeck_dir() {
        let fs = FakeFs::new();
        let repo = Path::new("/repo");
        initialize(&fs, repo, "proj", "main").unwrap();
        assert!(fs.exists(Path::new("/repo/.flightdeck")));
    }

    #[test]
    fn init_creates_config_toml() {
        let fs = FakeFs::new();
        let repo = Path::new("/repo");
        initialize(&fs, repo, "proj", "main").unwrap();
        let config_path = Path::new("/repo/.flightdeck/config.toml");
        assert!(fs.exists(config_path));
        let contents = fs.file_contents(config_path).unwrap();
        assert!(contents.contains("proj"));
        assert!(contents.contains("opencode"));
    }

    #[test]
    fn init_writes_containers_section_off_by_default() {
        let fs = FakeFs::new();
        let repo = Path::new("/repo");
        initialize(&fs, repo, "proj", "main").unwrap();
        let contents = fs
            .file_contents(Path::new("/repo/.flightdeck/config.toml"))
            .unwrap();
        // The section is present so the feature is discoverable...
        assert!(contents.contains("[containers]"), "config: {contents}");
        // ...and disabled by default (parse it back to be unambiguous).
        let cfg = crate::config::load::parse_config(&contents).unwrap();
        assert!(!cfg.containers.enabled, "containers must default to off");
        assert_eq!(cfg.containers.runtime, "podman");
    }

    #[test]
    fn init_creates_state_json() {
        let fs = FakeFs::new();
        let repo = Path::new("/repo");
        initialize(&fs, repo, "proj", "main").unwrap();
        let state_path = Path::new("/repo/.flightdeck/state.json");
        assert!(fs.exists(state_path));
        let contents = fs.file_contents(state_path).unwrap();
        assert!(contents.contains("\"version\""));
        assert!(contents.contains("\"tabs\""));
    }

    #[test]
    fn init_creates_worktrees_dir() {
        let fs = FakeFs::new();
        let repo = Path::new("/repo");
        initialize(&fs, repo, "proj", "main").unwrap();
        assert!(fs.exists(Path::new("/repo/.flightdeck/worktrees")));
    }

    #[test]
    fn init_is_idempotent_returns_all_false_on_second_call() {
        let fs = FakeFs::new();
        let repo = Path::new("/repo");

        // First call — everything gets created
        let first = initialize(&fs, repo, "proj", "main").unwrap();
        assert!(first.created_flightdeck_dir);
        assert!(first.created_config);
        assert!(first.created_state);
        assert!(first.created_worktrees_dir);

        // Capture the file contents before the second call
        let config_before = fs
            .file_contents(Path::new("/repo/.flightdeck/config.toml"))
            .unwrap();
        let state_before = fs
            .file_contents(Path::new("/repo/.flightdeck/state.json"))
            .unwrap();

        // Second call — nothing should be created
        let second = initialize(&fs, repo, "proj", "main").unwrap();
        assert!(!second.created_flightdeck_dir);
        assert!(!second.created_config);
        assert!(!second.created_state);
        assert!(!second.created_worktrees_dir);

        // Files must not have been overwritten
        let config_after = fs
            .file_contents(Path::new("/repo/.flightdeck/config.toml"))
            .unwrap();
        let state_after = fs
            .file_contents(Path::new("/repo/.flightdeck/state.json"))
            .unwrap();
        assert_eq!(config_before, config_after);
        assert_eq!(state_before, state_after);
    }

    #[test]
    fn init_state_json_contains_correct_base_branch() {
        let fs = FakeFs::new();
        let repo = Path::new("/repo");
        initialize(&fs, repo, "proj", "develop").unwrap();
        let state_str = fs
            .file_contents(Path::new("/repo/.flightdeck/state.json"))
            .unwrap();
        let state: serde_json::Value = serde_json::from_str(&state_str).unwrap();
        assert_eq!(state["base_branch"], "develop");
        assert_eq!(state["version"], 1);
    }
}
