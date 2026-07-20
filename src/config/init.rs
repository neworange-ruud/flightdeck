//! First-run initialization of `.flightdeck/` (SPECS §7).
//!
//! Creates the metadata directory, `config.toml`, `state.json`, and
//! `worktrees/` if missing. Idempotent: does not duplicate work if already
//! present. The `.gitignore` update is handled separately by [`crate::fs::ignore`]
//! and orchestrated by startup (SPECS §7 step 8).

use crate::config::load::{minimal_project_config, serialize_global_config, GLOBAL_CONFIG_HEADER};
use crate::config::schema::default_global_config;
use crate::contracts::{FileSystem, Result};
use std::path::Path;

/// What first-run init created (each `true` only if it was missing before).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InitOutcome {
    pub created_flightdeck_dir: bool,
    pub created_config: bool,
    pub created_state: bool,
    pub created_worktrees_dir: bool,
    pub created_hooks: bool,
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
    let hooks_path = flightdeck_dir.join(crate::hooks::HOOKS_FILE_NAME);

    // 1. Create .flightdeck/ if missing
    if !fs.exists(&flightdeck_dir) {
        fs.create_dir_all(&flightdeck_dir)?;
        outcome.created_flightdeck_dir = true;
    }

    // 2. Create config.toml if missing. Only project identity is written; every
    //    other setting is inherited from the global base until a project chooses
    //    to override it (SPECS §8).
    if !fs.exists(&config_path) {
        fs.write(
            &config_path,
            &minimal_project_config(project_name, base_branch),
        )?;
        outcome.created_config = true;
    }

    // 3. Create state.json if missing
    if !fs.exists(&state_path) {
        let state = crate::persistence::project_state::default_state(base_branch);
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

    // 5. Create the default (empty, only commented) hooks.toml if missing, so it
    //    exists in the repo for the user to opt into (SPECS §7). It is gitignored
    //    by default (see `crate::fs::ignore`), so the user consciously un-ignores
    //    and commits it to share hooks with their team.
    if !fs.exists(&hooks_path) {
        fs.write(&hooks_path, crate::hooks::DEFAULT_HOOKS_TEMPLATE)?;
        outcome.created_hooks = true;
    }

    Ok(outcome)
}

/// Ensure the per-user global `config.toml` at `global_path` exists, writing the
/// fully-documented default base (all sections except `[project]`) if missing
/// (SPECS §8). Idempotent: an existing file is left untouched. Returns whether
/// the file was created. Creates the parent (`~/.flightdeck/`) if needed.
pub fn ensure_global_config(fs: &dyn FileSystem, global_path: &Path) -> Result<bool> {
    if fs.exists(global_path) {
        return Ok(false);
    }
    if let Some(parent) = global_path.parent() {
        if !fs.exists(parent) {
            fs.create_dir_all(parent)?;
        }
    }
    let contents = serialize_global_config(&default_global_config())?;
    // Guard against a template that somehow lost its header (keeps the file
    // self-describing for anyone who opens it by hand).
    debug_assert!(contents.starts_with(GLOBAL_CONFIG_HEADER));
    fs.write(global_path, &contents)?;
    Ok(true)
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
        assert!(outcome.created_hooks);
    }

    #[test]
    fn init_creates_default_hooks_file() {
        let fs = FakeFs::new();
        let repo = Path::new("/repo");
        initialize(&fs, repo, "proj", "main").unwrap();
        let hooks_path = Path::new("/repo/.flightdeck/hooks.toml");
        assert!(fs.exists(hooks_path));
        let contents = fs.file_contents(hooks_path).unwrap();
        // The shipped default documents both hooks but defines no commands.
        assert!(contents.contains("[worktree_created]"), "hooks: {contents}");
        assert!(contents.contains("[worktree_update]"), "hooks: {contents}");
        let hooks = crate::hooks::parse_hooks(&contents).unwrap();
        assert!(hooks.is_empty(), "default hooks must define no commands");
    }

    #[test]
    fn init_creates_flightdeck_dir() {
        let fs = FakeFs::new();
        let repo = Path::new("/repo");
        initialize(&fs, repo, "proj", "main").unwrap();
        assert!(fs.exists(Path::new("/repo/.flightdeck")));
    }

    #[test]
    fn init_creates_minimal_project_config() {
        let fs = FakeFs::new();
        let repo = Path::new("/repo");
        initialize(&fs, repo, "proj", "main").unwrap();
        let config_path = Path::new("/repo/.flightdeck/config.toml");
        assert!(fs.exists(config_path));
        let contents = fs.file_contents(config_path).unwrap();
        // Only project identity is written; agents/containers now live in the
        // global base and are inherited (SPECS §8).
        assert!(contents.contains("[project]"), "config: {contents}");
        assert!(contents.contains("proj"));
        assert!(
            !contents.contains("opencode"),
            "project config must be minimal"
        );
        assert!(
            !contents.contains("[containers]"),
            "project config must be minimal"
        );
    }

    #[test]
    fn ensure_global_config_writes_documented_base() {
        let fs = FakeFs::new();
        let global = Path::new("/home/user/.flightdeck/config.toml");
        assert!(ensure_global_config(&fs, global).unwrap());
        let contents = fs.file_contents(global).unwrap();
        // The global base carries every section EXCEPT [project]...
        assert!(!contents.contains("[project]"), "global: {contents}");
        assert!(contents.contains("[containers]"), "global: {contents}");
        assert!(contents.contains("opencode"), "global: {contents}");
        // ...containers disabled by default (parse it back to be unambiguous).
        let cfg = crate::config::load::parse_config(&contents).unwrap();
        assert!(!cfg.containers.enabled, "containers must default to off");
        assert_eq!(cfg.containers.runtime, "podman");
        assert!(cfg.notifications.enabled, "notifications default on");
        // Idempotent: a second call leaves the file untouched.
        assert!(!ensure_global_config(&fs, global).unwrap());
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
        assert!(first.created_hooks);

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
        assert!(!second.created_hooks);

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
