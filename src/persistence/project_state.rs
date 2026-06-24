//! Load and save `state.json` (SPECS §9). Stores relative paths; serde-backed.

use crate::contracts::{FileSystem, ProjectState, Result};
use std::path::Path;

/// A fresh, empty project state for `base_branch` (SPECS §9).
pub fn default_state(base_branch: &str) -> ProjectState {
    let _ = base_branch;
    todo!("T5: fresh ProjectState at STATE_VERSION, project_root_relative '.', no tabs")
}

/// Load and deserialize `state.json` (SPECS §9).
pub fn load_state(fs: &dyn FileSystem, path: &Path) -> Result<ProjectState> {
    let _ = (fs, path);
    todo!("T5: read via fs, serde_json deserialize")
}

/// Serialize and write `state.json` (SPECS §9).
pub fn save_state(fs: &dyn FileSystem, path: &Path, state: &ProjectState) -> Result<()> {
    let _ = (fs, path, state);
    todo!("T5: serde_json serialize, write via fs")
}
