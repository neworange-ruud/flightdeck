//! Global (user-wide) remembered choices: `~/.flightdeck/global-state.json`.
//!
//! Distinct from `config.toml` (settings the user edits) and from the
//! per-project `state.json`: this file holds answers FlightDeck itself asked
//! for once, which apply to every project. Best-effort like the workspace
//! file: a missing or unreadable file is "nothing remembered", and a write
//! failure never interrupts the UI. Writes go through [`FileSystem::write`],
//! which is atomic.

use crate::contracts::{FileSystem, FlightDeckError, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The current global-state schema version.
pub const GLOBAL_STATE_VERSION: u32 = 1;

/// What the user answered to the Windows Defender exclusion offer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "choice", rename_all = "snake_case")]
pub enum DefenderExclusionChoice {
    /// Said yes, once and for all.
    Accepted,
    /// Said no while running `version`; asked again after the next version.
    Declined { version: String },
}

/// Persisted global state. Every field defaults on its own so a file from an
/// older or newer build still loads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GlobalState {
    pub version: u32,
    /// The answer to the Defender exclusion offer; `None` = never answered.
    #[serde(default)]
    pub defender_exclusion: Option<DefenderExclusionChoice>,
}

impl Default for GlobalState {
    fn default() -> Self {
        GlobalState {
            version: GLOBAL_STATE_VERSION,
            defender_exclusion: None,
        }
    }
}

/// The per-user global-state path, `~/.flightdeck/global-state.json`. `None`
/// when neither `$HOME` nor `%USERPROFILE%` is set.
pub fn global_state_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(
        PathBuf::from(home)
            .join(".flightdeck")
            .join("global-state.json"),
    )
}

/// Load the global state; a missing or unparsable file is the default.
pub fn load_global_state(fs: &dyn FileSystem, path: &Path) -> GlobalState {
    fs.read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

/// Serialize and write the global state, creating `~/.flightdeck/` if needed.
pub fn save_global_state(fs: &dyn FileSystem, path: &Path, state: &GlobalState) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !fs.exists(parent) {
            fs.create_dir_all(parent)?;
        }
    }
    let json = serde_json::to_string_pretty(state)
        .map_err(|e| FlightDeckError::State(format!("failed to serialize global state: {e}")))?;
    fs.write(path, &json)
        .map_err(|e| FlightDeckError::State(format!("failed to write global state: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeFs;

    fn path() -> PathBuf {
        PathBuf::from("/home/u/.flightdeck/global-state.json")
    }

    #[test]
    fn a_missing_file_is_the_default() {
        let fs = FakeFs::new();
        assert_eq!(load_global_state(&fs, &path()), GlobalState::default());
    }

    #[test]
    fn garbage_is_the_default() {
        let fs = FakeFs::new().with_file(path(), "{ not json");
        assert_eq!(load_global_state(&fs, &path()), GlobalState::default());
    }

    #[test]
    fn a_state_round_trips() {
        let fs = FakeFs::new();
        let state = GlobalState {
            defender_exclusion: Some(DefenderExclusionChoice::Declined {
                version: "1.2.3".to_string(),
            }),
            ..GlobalState::default()
        };
        save_global_state(&fs, &path(), &state).unwrap();
        assert_eq!(load_global_state(&fs, &path()), state);
    }

    #[test]
    fn the_on_disk_shape_is_stable() {
        let fs = FakeFs::new();
        let state = GlobalState {
            defender_exclusion: Some(DefenderExclusionChoice::Accepted),
            ..GlobalState::default()
        };
        save_global_state(&fs, &path(), &state).unwrap();
        let raw = fs.file_contents(&path()).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["defender_exclusion"]["choice"], "accepted");
        assert_eq!(v["version"], 1);
    }

    #[test]
    fn a_file_without_the_field_loads() {
        let fs = FakeFs::new().with_file(path(), r#"{"version":1}"#);
        assert_eq!(load_global_state(&fs, &path()).defender_exclusion, None);
    }
}
