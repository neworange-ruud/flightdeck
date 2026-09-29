//! Workspace persistence: the set of open project folders, remembered across
//! restarts (multi-project). Stored per-user in `~/.flightdeck/workspace.json`
//! (NOT inside any single project's `.flightdeck/`, so it spans repositories).
//!
//! This is best-effort: a missing/unreadable file simply means "no remembered
//! projects", and a save failure never interrupts the UI. Recovery of each
//! project's own tabs still goes through the per-project `state.json`
//! (SPECS §9/§10) — this file only records *which* projects were open.

use crate::contracts::{FileSystem, FlightDeckError, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The current workspace-file schema version.
pub const WORKSPACE_VERSION: u32 = 1;

/// Persisted workspace: the absolute roots of the open projects and which one
/// was active. Paths are stored absolute (unlike per-project `state.json` which
/// stores relative paths) because a workspace spans repositories.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceState {
    pub version: u32,
    /// Absolute project root paths, in tab order.
    #[serde(default)]
    pub projects: Vec<String>,
    /// Index of the active project within `projects`.
    #[serde(default)]
    pub active: usize,
}

impl Default for WorkspaceState {
    fn default() -> Self {
        WorkspaceState {
            version: WORKSPACE_VERSION,
            projects: Vec::new(),
            active: 0,
        }
    }
}

/// The per-user workspace file path, `~/.flightdeck/workspace.json`. Returns
/// `None` when neither `$HOME` nor `%USERPROFILE%` is set (so the caller simply
/// skips workspace persistence rather than failing).
pub fn workspace_state_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(
        PathBuf::from(home)
            .join(".flightdeck")
            .join("workspace.json"),
    )
}

/// Load and deserialize the workspace file.
pub fn load_workspace(fs: &dyn FileSystem, path: &Path) -> Result<WorkspaceState> {
    let contents = fs.read_to_string(path).map_err(|e| {
        FlightDeckError::State(format!(
            "failed to read workspace file {}: {e}",
            path.display()
        ))
    })?;
    let state: WorkspaceState = serde_json::from_str(&contents)
        .map_err(|e| FlightDeckError::State(format!("failed to parse workspace file: {e}")))?;
    Ok(state)
}

/// Serialize and write the workspace file, creating `~/.flightdeck/` if needed.
pub fn save_workspace(fs: &dyn FileSystem, path: &Path, state: &WorkspaceState) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !fs.exists(parent) {
            fs.create_dir_all(parent)?;
        }
    }
    let json = serde_json::to_string_pretty(state)
        .map_err(|e| FlightDeckError::State(format!("failed to serialize workspace: {e}")))?;
    fs.write(path, &json)
        .map_err(|e| FlightDeckError::State(format!("failed to write workspace file: {e}")))?;
    Ok(())
}

/// The projects remembered from the last session that are still folders on
/// disk, the one that was active first. What a front-end launched outside any
/// repository (from Finder, a launcher) offers to reopen. Best-effort like the
/// rest of this module: a missing or unreadable file is an empty list.
pub fn recent_projects(fs: &dyn FileSystem, path: &Path) -> Vec<PathBuf> {
    let Ok(saved) = load_workspace(fs, path) else {
        return Vec::new();
    };
    let mut roots: Vec<PathBuf> = Vec::new();
    for (i, root) in saved.projects.iter().enumerate() {
        let root = PathBuf::from(root);
        if !fs.is_dir(&root) || roots.contains(&root) {
            continue;
        }
        if i == saved.active {
            roots.insert(0, root);
        } else {
            roots.push(root);
        }
    }
    roots
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeFs;
    use std::path::Path;

    #[test]
    fn recent_projects_lists_existing_folders_active_first() {
        let fs = FakeFs::new();
        let path = Path::new("/home/user/.flightdeck/workspace.json");
        for dir in ["/a/one", "/b/two", "/c/three"] {
            fs.create_dir_all(Path::new(dir)).expect("dir");
        }
        let state = WorkspaceState {
            version: WORKSPACE_VERSION,
            projects: vec![
                "/a/one".into(),
                "/gone/away".into(),
                "/b/two".into(),
                "/a/one".into(),
                "/c/three".into(),
            ],
            active: 2,
        };
        save_workspace(&fs, path, &state).expect("save");
        assert_eq!(
            recent_projects(&fs, path),
            vec![
                PathBuf::from("/b/two"),
                PathBuf::from("/a/one"),
                PathBuf::from("/c/three")
            ]
        );
    }

    #[test]
    fn recent_projects_without_a_workspace_file_is_empty() {
        let fs = FakeFs::new();
        assert!(recent_projects(&fs, Path::new("/nope/workspace.json")).is_empty());
    }

    #[test]
    fn round_trip_save_then_load() {
        let fs = FakeFs::new();
        let path = Path::new("/home/user/.flightdeck/workspace.json");
        let state = WorkspaceState {
            version: WORKSPACE_VERSION,
            projects: vec!["/a/one".to_string(), "/b/two".to_string()],
            active: 1,
        };
        save_workspace(&fs, path, &state).expect("save");
        let loaded = load_workspace(&fs, path).expect("load");
        assert_eq!(loaded, state);
    }

    #[test]
    fn load_missing_file_is_err() {
        let fs = FakeFs::new();
        let path = Path::new("/home/user/.flightdeck/workspace.json");
        assert!(load_workspace(&fs, path).is_err());
    }

    #[test]
    fn save_creates_parent_dir() {
        let fs = FakeFs::new();
        let path = Path::new("/home/user/.flightdeck/workspace.json");
        save_workspace(&fs, path, &WorkspaceState::default()).expect("save");
        assert!(fs.exists(Path::new("/home/user/.flightdeck")));
    }
}
