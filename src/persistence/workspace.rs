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
    /// Front-end view state that belongs to the workspace rather than to any
    /// one project: which main view was last on screen and Mission control's
    /// scope. Absent from files written before it existed, and inert for the
    /// TUI, which has no such views but writes back whatever it loaded, so
    /// switching front-ends loses nothing.
    #[serde(default)]
    pub ui: WorkspaceUi,
}

impl Default for WorkspaceState {
    fn default() -> Self {
        WorkspaceState {
            version: WORKSPACE_VERSION,
            projects: Vec::new(),
            active: 0,
            ui: WorkspaceUi::default(),
        }
    }
}

/// The workspace-level view state a front-end remembers across restarts.
///
/// Every field defaults on its own, so a file from an older build (no `ui`
/// at all) or a partial one (`"ui": {"view": "mission"}`) still loads with
/// the rest at their defaults.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceUi {
    /// The main view last on screen.
    #[serde(default)]
    pub view: MainView,
    /// What Mission control shows.
    #[serde(default)]
    pub mission_scope: MissionScope,
}

/// The desktop app's two main views (design A1 ⇄ A2).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MainView {
    /// One project at a time: its agents, git strip and terminal.
    #[default]
    Projects,
    /// Every live session across all projects, as tiles.
    Mission,
}

impl MainView {
    /// The other view: where the view-switch chord goes.
    pub fn toggled(self) -> MainView {
        match self {
            MainView::Projects => MainView::Mission,
            MainView::Mission => MainView::Projects,
        }
    }
}

/// How far back Mission control's "earlier" cards reach. The live tiles are
/// never limited by it: an active session is always shown.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecentScope {
    /// Activity in the last 24 hours (the design's default).
    #[default]
    Day,
    /// Activity in the last 7 days.
    Week,
    /// Any recorded activity at all.
    All,
}

impl RecentScope {
    /// Every scope, in menu order.
    pub const ALL: [RecentScope; 3] = [RecentScope::Day, RecentScope::Week, RecentScope::All];

    /// The window [`crate::app::activity::is_recent`] measures against.
    pub fn window_secs(self) -> u64 {
        use crate::app::activity::{RECENT_WINDOW_24H, RECENT_WINDOW_7D, RECENT_WINDOW_ALL};
        match self {
            RecentScope::Day => RECENT_WINDOW_24H,
            RecentScope::Week => RECENT_WINDOW_7D,
            RecentScope::All => RECENT_WINDOW_ALL,
        }
    }
}

/// Mission control's scope menu: the recency window plus an optional project
/// filter.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissionScope {
    /// How far back the "earlier" cards reach.
    #[serde(default)]
    pub window: RecentScope,
    /// Only this project, by its absolute repository root (stable across
    /// reordering and restarts, unlike an index). `None` is every project. A
    /// root that is no longer open reads as `None`.
    #[serde(default)]
    pub project: Option<String>,
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
            ui: WorkspaceUi::default(),
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
            ui: WorkspaceUi {
                view: MainView::Mission,
                mission_scope: MissionScope {
                    window: RecentScope::Week,
                    project: Some("/b/two".to_string()),
                },
            },
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
    fn a_file_from_before_the_ui_state_still_loads() {
        let old: WorkspaceState =
            serde_json::from_str(r#"{"version":1,"projects":["/a"],"active":0}"#).unwrap();
        assert_eq!(old.ui, WorkspaceUi::default());
        assert_eq!(old.ui.view, MainView::Projects);
        assert_eq!(old.ui.mission_scope.window, RecentScope::Day);
        // A partial `ui` keeps its siblings at their defaults.
        let partial: WorkspaceState = serde_json::from_str(
            r#"{"version":1,"projects":[],"active":0,"ui":{"view":"mission"}}"#,
        )
        .unwrap();
        assert_eq!(partial.ui.view, MainView::Mission);
        assert_eq!(partial.ui.mission_scope, MissionScope::default());
        let partial: WorkspaceState =
            serde_json::from_str(r#"{"version":1,"ui":{"mission_scope":{"window":"all"}}}"#)
                .unwrap();
        assert_eq!(partial.ui.mission_scope.window, RecentScope::All);
        assert_eq!(partial.ui.mission_scope.project, None);
    }

    #[test]
    fn scope_windows_are_the_activity_windows() {
        use crate::app::activity::{RECENT_WINDOW_24H, RECENT_WINDOW_7D, RECENT_WINDOW_ALL};
        assert_eq!(RecentScope::Day.window_secs(), RECENT_WINDOW_24H);
        assert_eq!(RecentScope::Week.window_secs(), RECENT_WINDOW_7D);
        assert_eq!(RecentScope::All.window_secs(), RECENT_WINDOW_ALL);
        assert_eq!(MainView::Projects.toggled(), MainView::Mission);
        assert_eq!(MainView::Mission.toggled(), MainView::Projects);
    }

    #[test]
    fn save_creates_parent_dir() {
        let fs = FakeFs::new();
        let path = Path::new("/home/user/.flightdeck/workspace.json");
        save_workspace(&fs, path, &WorkspaceState::default()).expect("save");
        assert!(fs.exists(Path::new("/home/user/.flightdeck")));
    }
}
