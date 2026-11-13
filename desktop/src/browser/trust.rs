//! The certificates a person chose to trust anyway (`specs/SESSION_BROWSER_PRD.md`
//! B21): per browser profile, the `host:port`s whose self-signed (or
//! otherwise untrusted) certificate **Continue anyway** accepted.
//!
//! Kept in `~/.flightdeck/browser-trust.json`, next to the saved remotes, as
//! `{ "<profile hex>": ["localhost:8443", …] }`, so a session's dev server is
//! accepted again after a restart. It belongs to the profile: deleting the
//! profile (B19) deletes its entry ([`TrustStore::forget`]).

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};

/// The file, in FlightDeck's own folder.
pub fn default_path() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(|home| {
            PathBuf::from(home)
                .join(".flightdeck")
                .join("browser-trust.json")
        })
}

/// A profile identifier as the file names it.
pub fn profile_key(profile: &[u8; 16]) -> String {
    profile.iter().map(|b| format!("{b:02x}")).collect()
}

/// See the module docs. Reads and writes the whole (small) file each time,
/// so two windows never keep stale copies.
pub struct TrustStore {
    path: Option<PathBuf>,
}

impl TrustStore {
    /// The store at `path`; `None` keeps nothing.
    pub fn at(path: Option<PathBuf>) -> TrustStore {
        TrustStore { path }
    }

    /// The `host:port`s profile `profile` trusts.
    pub fn allowed(&self, profile: &[u8; 16]) -> HashSet<(String, u16)> {
        read(self.path.as_deref())
            .get(&profile_key(profile))
            .into_iter()
            .flatten()
            .filter_map(|entry| split(entry))
            .collect()
    }

    /// Trust `host:port` in profile `profile` from now on.
    pub fn allow(&self, profile: &[u8; 16], host: &str, port: u16) {
        let Some(path) = self.path.as_deref() else {
            return;
        };
        let mut all = read(Some(path));
        all.entry(profile_key(profile))
            .or_default()
            .insert(format!("{}:{port}", host.to_ascii_lowercase()));
        write(path, &all);
    }

    /// Forget everything profile `profile` trusted (the profile is deleted).
    pub fn forget(&self, profile: &[u8; 16]) {
        let Some(path) = self.path.as_deref() else {
            return;
        };
        let mut all = read(Some(path));
        if all.remove(&profile_key(profile)).is_some() {
            write(path, &all);
        }
    }
}

type Entries = BTreeMap<String, BTreeSet<String>>;

/// The file's entries; a missing or unreadable file is none (nothing is
/// trusted that the person did not choose).
fn read(path: Option<&Path>) -> Entries {
    path.and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn write(path: &Path, all: &Entries) {
    // Nothing trusted is no file at all.
    if all.values().all(BTreeSet::is_empty) {
        let _ = std::fs::remove_file(path);
        return;
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(text) = serde_json::to_string_pretty(all) {
        let _ = std::fs::write(path, text);
    }
}

/// `host:port` (IPv6 bracketed) into its parts.
fn split(entry: &str) -> Option<(String, u16)> {
    let (host, port) = entry.rsplit_once(':')?;
    Some((
        host.trim_matches(['[', ']']).to_string(),
        port.parse().ok()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_profile_remembers_what_it_trusted_and_forgets_it_with_the_profile() {
        let dir = tempfile::tempdir().unwrap();
        let store = TrustStore::at(Some(dir.path().join("browser-trust.json")));
        let (a, b) = ([1u8; 16], [2u8; 16]);
        assert!(store.allowed(&a).is_empty());
        store.allow(&a, "LocalHost", 8443);
        store.allow(&a, "::1", 9443);
        store.allow(&b, "dev.test", 443);
        let allowed = store.allowed(&a);
        assert!(allowed.contains(&("localhost".to_string(), 8443)));
        assert!(allowed.contains(&("::1".to_string(), 9443)));
        assert!(
            !allowed.contains(&("dev.test".to_string(), 443)),
            "another profile's"
        );
        store.forget(&a);
        assert!(store.allowed(&a).is_empty());
        assert_eq!(
            store.allowed(&b).len(),
            1,
            "the other profile keeps its own"
        );
    }

    #[test]
    fn no_file_and_a_broken_file_trust_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("browser-trust.json");
        assert!(TrustStore::at(Some(path.clone()))
            .allowed(&[0; 16])
            .is_empty());
        std::fs::write(&path, "{ not json").unwrap();
        assert!(TrustStore::at(Some(path)).allowed(&[0; 16]).is_empty());
        let none = TrustStore::at(None);
        none.allow(&[0; 16], "x", 1);
        assert!(none.allowed(&[0; 16]).is_empty());
    }

    #[test]
    fn a_profile_is_named_by_its_bytes_in_hex() {
        let mut id = [0u8; 16];
        id[0] = 0xab;
        id[15] = 0x01;
        assert_eq!(profile_key(&id), format!("ab{}01", "0".repeat(28)));
    }
}
