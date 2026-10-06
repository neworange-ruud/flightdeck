//! Saved remotes: `~/.flightdeck/remotes.json`
//! (`specs/DESKTOP_REMOTE_CONTROL_PLAN.md` §2.4, R6).
//!
//! One record per controlled instance this machine has paired with, so a
//! reconnect needs no new code. The token in it is a bearer secret, so the
//! file is hardened to owner-only (`0600`) on Unix after every write, exactly
//! as the phone pairing state is ([`crate::remote::state`]): the
//! [`FileSystem`] seam has no chmod, so that one call is `std::fs` layered on
//! top, best-effort, and skipped under the in-memory test filesystem. No
//! keychain crate (R6).
//!
//! Load is forgiving like every per-user file FlightDeck keeps: missing or
//! unreadable means "nothing saved yet", never an error in the way of the app.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::contracts::{FileSystem, FlightDeckError, Result};

use super::exchange::AccessToken;

/// The file's format version.
pub const REMOTES_FILE_VERSION: u32 = 1;

/// One paired instance.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedRemote {
    /// What the window calls it.
    pub label: String,
    /// `host:port`, normalised ([`super::normalize_address`]).
    pub address: String,
    /// The `flightdeck_web` cookie's value.
    pub token: String,
    /// When this client last attached, Unix seconds.
    #[serde(default)]
    pub last_seen: Option<u64>,
    /// The viewer id the host gave it last time.
    #[serde(default)]
    pub viewer_id: Option<String>,
    /// The host's FlightDeck version at the last attach.
    #[serde(default)]
    pub host_version: Option<String>,
    /// The one-time plain-WebSocket warning was accepted for this remote.
    #[serde(default)]
    pub warned_unencrypted: bool,
    /// Where "Open Worktree in VS Code" connects over SSH: a `Host` alias
    /// from `~/.ssh/config` or `user@host`. Unset, it is the host's own
    /// account at this remote's address ([`crate::host::vscode::ssh_target`]).
    /// Only ever set by hand, so it is kept across a re-pair.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh_target: Option<String>,
}

impl SavedRemote {
    /// The token, as the link takes it.
    pub fn access_token(&self) -> AccessToken {
        AccessToken::new(self.token.clone())
    }
}

/// The whole file.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemotesFile {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub remotes: Vec<SavedRemote>,
}

impl RemotesFile {
    /// The record for `address`, if saved.
    pub fn get(&self, address: &str) -> Option<&SavedRemote> {
        self.remotes.iter().find(|r| r.address == address)
    }

    /// Save `remote`, replacing any record for the same address (a re-pair
    /// keeps the warning already accepted and a hand-set SSH target).
    pub fn upsert(&mut self, mut remote: SavedRemote) {
        match self
            .remotes
            .iter_mut()
            .find(|r| r.address == remote.address)
        {
            Some(existing) => {
                remote.warned_unencrypted |= existing.warned_unencrypted;
                if remote.ssh_target.is_none() {
                    remote.ssh_target = existing.ssh_target.take();
                }
                *existing = remote;
            }
            None => self.remotes.push(remote),
        }
    }

    /// Forget `address`. Returns whether a record went.
    pub fn forget(&mut self, address: &str) -> bool {
        let before = self.remotes.len();
        self.remotes.retain(|r| r.address != address);
        self.remotes.len() != before
    }

    /// Record an attach to `address`.
    pub fn touch(
        &mut self,
        address: &str,
        now_unix_secs: u64,
        viewer_id: Option<String>,
        host_version: Option<String>,
    ) {
        if let Some(remote) = self.remotes.iter_mut().find(|r| r.address == address) {
            remote.last_seen = Some(now_unix_secs);
            if viewer_id.is_some() {
                remote.viewer_id = viewer_id;
            }
            if host_version.is_some() {
                remote.host_version = host_version;
            }
        }
    }

    /// Most recently used first, never-used last in the order saved.
    pub fn by_recent(&self) -> Vec<&SavedRemote> {
        let mut list: Vec<&SavedRemote> = self.remotes.iter().collect();
        list.sort_by_key(|r| std::cmp::Reverse(r.last_seen));
        list
    }
}

/// `~/.flightdeck/remotes.json`, or `None` with no home directory.
pub fn remotes_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(PathBuf::from(home).join(".flightdeck").join("remotes.json"))
}

/// Read the file; anything wrong with it reads as nothing saved.
pub fn load_remotes(fs: &dyn FileSystem, path: &Path) -> RemotesFile {
    fs.read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Write the file, creating `~/.flightdeck/` if needed, then harden it to
/// owner-only on Unix.
pub fn save_remotes(fs: &dyn FileSystem, path: &Path, file: &RemotesFile) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !fs.exists(parent) {
            fs.create_dir_all(parent)?;
        }
    }
    let file = RemotesFile {
        version: REMOTES_FILE_VERSION,
        remotes: file.remotes.clone(),
    };
    let json = serde_json::to_string_pretty(&file)
        .map_err(|e| FlightDeckError::State(format!("failed to serialize remotes: {e}")))?;
    fs.write(path, &json)
        .map_err(|e| FlightDeckError::State(format!("failed to write remotes file: {e}")))?;
    harden(path);
    Ok(())
}

#[cfg(unix)]
fn harden(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn harden(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeFs;

    fn remote(address: &str) -> SavedRemote {
        SavedRemote {
            label: "studio".to_string(),
            address: address.to_string(),
            token: "s3cret".to_string(),
            last_seen: None,
            viewer_id: None,
            host_version: None,
            warned_unencrypted: false,
            ssh_target: None,
        }
    }

    #[test]
    fn a_re_pair_keeps_the_ssh_target_set_by_hand() {
        let mut file = RemotesFile::default();
        file.upsert(SavedRemote {
            ssh_target: Some("buildbox".to_string()),
            ..remote("10.0.0.2:7420")
        });
        file.upsert(SavedRemote {
            token: "new".to_string(),
            ..remote("10.0.0.2:7420")
        });
        let saved = file.get("10.0.0.2:7420").unwrap();
        assert_eq!(saved.token, "new");
        assert_eq!(saved.ssh_target.as_deref(), Some("buildbox"));

        // Absent from the file when unset, and an old file still loads.
        let json = serde_json::to_string(&remote("h:1")).unwrap();
        assert!(!json.contains("ssh_target"), "{json}");
    }

    #[test]
    fn a_missing_or_broken_file_is_nothing_saved() {
        let fs = FakeFs::new();
        let path = Path::new("/home/u/.flightdeck/remotes.json");
        assert_eq!(load_remotes(&fs, path), RemotesFile::default());
        fs.write(path, "{ not json").unwrap();
        assert_eq!(load_remotes(&fs, path), RemotesFile::default());
    }

    #[test]
    fn records_round_trip_and_are_keyed_by_address() {
        let fs = FakeFs::new();
        let path = Path::new("/home/u/.flightdeck/remotes.json");
        let mut file = RemotesFile::default();
        file.upsert(remote("192.168.2.20:7420"));
        file.upsert(SavedRemote {
            warned_unencrypted: true,
            ..remote("10.0.0.5:7420")
        });
        // A re-pair replaces the token and keeps the accepted warning.
        file.upsert(SavedRemote {
            token: "new".to_string(),
            ..remote("10.0.0.5:7420")
        });
        save_remotes(&fs, path, &file).unwrap();

        let back = load_remotes(&fs, path);
        assert_eq!(back.version, REMOTES_FILE_VERSION);
        assert_eq!(back.remotes.len(), 2);
        let again = back.get("10.0.0.5:7420").unwrap();
        assert_eq!(again.token, "new");
        assert!(again.warned_unencrypted);
    }

    #[test]
    fn touching_orders_by_recent_use_and_forgetting_removes() {
        let mut file = RemotesFile::default();
        file.upsert(remote("a:7420"));
        file.upsert(remote("b:7420"));
        file.touch("b:7420", 100, Some("v1".into()), Some("1.2.0".into()));
        let order: Vec<&str> = file
            .by_recent()
            .iter()
            .map(|r| r.address.as_str())
            .collect();
        assert_eq!(order, vec!["b:7420", "a:7420"]);
        assert_eq!(
            file.get("b:7420").unwrap().host_version.as_deref(),
            Some("1.2.0")
        );
        assert!(file.forget("b:7420"));
        assert!(!file.forget("b:7420"));
        assert_eq!(file.remotes.len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn the_real_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".flightdeck").join("remotes.json");
        let mut file = RemotesFile::default();
        file.upsert(remote("a:7420"));
        save_remotes(&crate::contracts::real::RealFs, &path, &file).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
