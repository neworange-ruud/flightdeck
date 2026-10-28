//! Global (user-wide) remembered choices: `~/.flightdeck/global-state.json`.
//!
//! Distinct from `config.toml` (settings the user edits) and from the
//! per-project `state.json`: this file holds answers FlightDeck itself asked
//! for once, which apply to every project. Best-effort like the workspace
//! file: a missing or unreadable file is "nothing remembered", and a write
//! failure never interrupts the UI. Writes go through [`FileSystem::write`],
//! which is atomic. A file that is not valid JSON loads as empty, and is copied
//! to `global-state.json.bak` before the next save replaces it.

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

/// Persisted global state. Every field defaults on its own, so a file from an
/// older build still loads; a file from a newer build loads too: a Defender
/// answer this build does not recognise reads as "never answered", and fields
/// it does not know are kept in `extra` and written back unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GlobalState {
    #[serde(default = "current_version")]
    pub version: u32,
    /// The answer to the Defender exclusion offer; `None` = never answered (or
    /// an answer from a newer build that this one cannot read).
    #[serde(default, deserialize_with = "lenient_choice")]
    pub defender_exclusion: Option<DefenderExclusionChoice>,
    /// Top-level fields this build does not know, preserved across a save.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

fn current_version() -> u32 {
    GLOBAL_STATE_VERSION
}

/// Read the Defender answer, treating one this build cannot parse as absent
/// rather than failing the whole file.
fn lenient_choice<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<DefenderExclusionChoice>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(raw.and_then(|value| serde_json::from_value(value).ok()))
}

impl Default for GlobalState {
    fn default() -> Self {
        GlobalState {
            version: GLOBAL_STATE_VERSION,
            defender_exclusion: None,
            extra: serde_json::Map::new(),
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
/// A file that exists but cannot be parsed is copied to `<name>.bak` first,
/// so the save does not silently destroy it.
pub fn save_global_state(fs: &dyn FileSystem, path: &Path, state: &GlobalState) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !fs.exists(parent) {
            fs.create_dir_all(parent)?;
        }
    }
    if let Ok(existing) = fs.read_to_string(path) {
        if serde_json::from_str::<GlobalState>(&existing).is_err() {
            let mut backup = path.as_os_str().to_owned();
            backup.push(".bak");
            let _ = fs.write(Path::new(&backup), &existing);
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
    fn a_file_without_a_version_loads() {
        let fs = FakeFs::new().with_file(path(), r#"{"defender_exclusion":{"choice":"accepted"}}"#);
        let state = load_global_state(&fs, &path());
        assert_eq!(state.version, GLOBAL_STATE_VERSION);
        assert_eq!(
            state.defender_exclusion,
            Some(DefenderExclusionChoice::Accepted)
        );
    }

    #[test]
    fn an_unknown_choice_does_not_wipe_the_other_fields() {
        let fs = FakeFs::new().with_file(
            path(),
            r#"{"version":1,"defender_exclusion":{"choice":"snoozed","days":3},"other":7}"#,
        );
        let state = load_global_state(&fs, &path());
        assert_eq!(state.defender_exclusion, None);
        assert_eq!(state.extra["other"], 7);
    }

    #[test]
    fn unknown_fields_survive_a_load_and_save() {
        let fs =
            FakeFs::new().with_file(path(), r#"{"version":1,"future":{"a":[1,2]},"flag":true}"#);
        let mut state = load_global_state(&fs, &path());
        state.defender_exclusion = Some(DefenderExclusionChoice::Accepted);
        save_global_state(&fs, &path(), &state).unwrap();
        let raw = fs.file_contents(&path()).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["future"]["a"][1], 2);
        assert_eq!(v["flag"], true);
        assert_eq!(v["defender_exclusion"]["choice"], "accepted");
    }

    #[test]
    fn a_corrupt_file_is_backed_up_before_it_is_replaced() {
        let fs = FakeFs::new().with_file(path(), "{ not json");
        save_global_state(&fs, &path(), &GlobalState::default()).unwrap();
        let backup = PathBuf::from("/home/u/.flightdeck/global-state.json.bak");
        assert_eq!(fs.file_contents(&backup).as_deref(), Some("{ not json"));
        assert!(fs.file_contents(&path()).unwrap().contains("\"version\""));
    }

    #[test]
    fn a_readable_file_is_not_backed_up() {
        let fs = FakeFs::new();
        save_global_state(&fs, &path(), &GlobalState::default()).unwrap();
        save_global_state(&fs, &path(), &GlobalState::default()).unwrap();
        let backup = PathBuf::from("/home/u/.flightdeck/global-state.json.bak");
        assert_eq!(fs.file_contents(&backup), None);
    }

    #[test]
    fn a_file_without_the_field_loads() {
        let fs = FakeFs::new().with_file(path(), r#"{"version":1}"#);
        assert_eq!(load_global_state(&fs, &path()).defender_exclusion, None);
    }
}
