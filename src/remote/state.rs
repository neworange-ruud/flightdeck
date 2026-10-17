//! Persisted FlightDeck Remote state — `~/.flightdeck/remote.json`.
//!
//! This per-user file (NOT inside any single project's `.flightdeck/`, because a
//! device's identity and pairings span every repository) holds three things:
//!
//! 1. the device's ECDSA P-256 **private key** (base64-standard of the 32-byte
//!    scalar), from which the public key and stable device id are re-derived on
//!    load (see [`crate::remote::identity`]);
//! 2. the set of **pairings** with their per-direction sequence cursors, so
//!    `resume` after a reconnect asks the relay only for envelopes newer than
//!    what we already hold, and outbound `seq` stays gapless across restarts
//!    (spec §6 — a cursor rewind would break the peer's dedup); and
//! 3. an optional **relay URL override** (config still wins when set).
//!
//! Load/save go through the [`FileSystem`] trait and are **best-effort**, exactly
//! like [`crate::persistence::workspace`]: a missing/unreadable file simply means
//! "fresh device", and a save failure never interrupts the app.
//!
//! ## File permissions
//!
//! The file contains a private key, so on save the real filesystem additionally
//! hardens it to owner-only (`0600`) on Unix. That hardening is a direct
//! `std::fs` call layered on top of the trait write (the [`FileSystem`] seam has
//! no chmod); it is best-effort and silently skipped under the in-memory test
//! filesystem, whose paths never exist on disk.

use crate::contracts::{FileSystem, FlightDeckError, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Current `remote.json` schema version.
pub const REMOTE_STATE_VERSION: u32 = 1;

/// One phone <-> desktop pairing and its delivery cursors. All routing is keyed
/// by [`Self::pairing_id`]; the cursors implement the spec's resume/ack/dedup
/// contract (§6) for this desktop's view of the pairing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pairing {
    /// The relay-assigned pairing id shared by both devices.
    pub pairing_id: String,
    /// The peer (phone) device id, once known.
    #[serde(default)]
    pub peer_device_id: Option<String>,
    /// Highest `seq` this desktop has assigned to an outbound envelope. The next
    /// outbound envelope uses `last_sent_seq + 1` (envelopes start at 1).
    #[serde(default)]
    pub last_sent_seq: u64,
    /// Highest outbound `seq` the peer has acknowledged (cumulative). Envelopes
    /// at or below this are safe for the relay to drop.
    #[serde(default)]
    pub last_acked_by_peer: u64,
    /// Highest incoming `seq` this desktop has durably handled. Sent as
    /// `from_seq` on `resume`; incoming envelopes at or below it are duplicates.
    #[serde(default)]
    pub last_received_seq: u64,
    /// The peer (phone) **key-agreement** public key, base64 (standard, padded)
    /// X9.63 uncompressed SEC1 (65 bytes) — as delivered in
    /// `pairing_claimed.peer_key_agreement_public_key` (spec §5.2). Fed into the
    /// static-static ECDH that bootstraps the E2E channel (spec §7.1). `None`
    /// until the phone claims the pairing.
    #[serde(default)]
    pub peer_key_agreement_public_key: Option<String>,
    /// The pairing bootstrap **salt** source: the effective `claim_token` string
    /// the relay issued (spec §5.2). The E2E salt is its UTF-8 bytes — the one
    /// value both endpoints share on *both* the QR and 4-digit-code paths, so
    /// the derivation is deterministic regardless of how the phone paired (spec
    /// §7.1, reconciled contract). `None` until an offer is minted.
    #[serde(default)]
    pub claim_token: Option<String>,
    /// Whether the E2E channel is live: set once the phone has claimed and this
    /// desktop has recorded the peer KA key. On the next launch a pairing with
    /// this set (plus a peer KA key + claim token) has its real `E2eChannel`
    /// reconstructed at startup instead of the passthrough sealer.
    #[serde(default)]
    pub established: bool,
    /// The phone's **identity** public key, base64 (standard, padded) X9.63 —
    /// delivered in the desktop-bound `pairing_claimed.peer_device_public_key`
    /// (spec §5.2, v4). The direct link (spec §5.10) verifies the phone's
    /// `auth_response` signature against it, so the Mac authenticates the phone
    /// itself with no relay in the loop. `None` for pairings claimed through a
    /// pre-v4 relay and for every record written before the direct link existed.
    #[serde(default)]
    pub peer_device_public_key: Option<String>,
    /// Which data plane carries this pairing's envelopes (spec §5.2 "Direct
    /// endpoint & direct mode"). Absent in every record written before the
    /// direct link existed, which therefore loads as [`PairingTransport::Relay`]
    /// — exactly how those pairings behave. Switching means re-pairing.
    #[serde(default)]
    pub transport: PairingTransport,
}

impl Pairing {
    /// A fresh pairing with zeroed cursors.
    pub fn new(pairing_id: impl Into<String>) -> Self {
        Pairing {
            pairing_id: pairing_id.into(),
            peer_device_id: None,
            last_sent_seq: 0,
            last_acked_by_peer: 0,
            last_received_seq: 0,
            peer_key_agreement_public_key: None,
            claim_token: None,
            established: false,
            peer_device_public_key: None,
            transport: PairingTransport::Relay,
        }
    }

    /// Whether this pairing has everything needed to reconstruct its E2E channel
    /// (a recorded peer KA key + salt source, and the `established` flag). The
    /// startup go-live in `lib.rs` builds the real sealer only for these.
    pub fn is_e2e_ready(&self) -> bool {
        self.established
            && self.peer_key_agreement_public_key.is_some()
            && self.claim_token.is_some()
    }

    /// Whether this pairing's data plane is the direct link (spec §5.10).
    pub fn is_direct(&self) -> bool {
        self.transport == PairingTransport::Direct
    }
}

/// The data plane a pairing's envelopes travel on (spec §5.2, §5.10).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PairingTransport {
    /// Envelopes, resume and acks go through the relay (§6) — every pairing made
    /// before v4, and every pairing whose offer carried no `direct_endpoint`.
    #[default]
    Relay,
    /// The phone connects to this Mac's direct-link WebSocket over Tailscale and
    /// the Mac's own outbox replaces the relay's queue. The relay is still used
    /// for push-token registration, revoke and `push_notify`, never for this
    /// pairing's envelopes.
    Direct,
}

/// The whole persisted remote state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteState {
    /// Schema version.
    pub version: u32,
    /// Base64 (standard, padded) of the 32-byte P-256 private scalar. Empty
    /// until a device identity is generated.
    #[serde(default)]
    pub device_private_key: String,
    /// Optional relay URL override. When set and non-empty it takes precedence
    /// over the configured URL (a per-device escape hatch for dev/staging).
    #[serde(default)]
    pub relay_url: Option<String>,
    /// Known pairings, in no particular order.
    #[serde(default)]
    pub pairings: Vec<Pairing>,
}

impl Default for RemoteState {
    fn default() -> Self {
        RemoteState {
            version: REMOTE_STATE_VERSION,
            device_private_key: String::new(),
            relay_url: None,
            pairings: Vec::new(),
        }
    }
}

impl RemoteState {
    /// Find a pairing by id.
    pub fn pairing(&self, pairing_id: &str) -> Option<&Pairing> {
        self.pairings.iter().find(|p| p.pairing_id == pairing_id)
    }

    /// Find a pairing by id, mutably.
    pub fn pairing_mut(&mut self, pairing_id: &str) -> Option<&mut Pairing> {
        self.pairings
            .iter_mut()
            .find(|p| p.pairing_id == pairing_id)
    }

    /// All pairing ids this device wants to activate on a connection.
    pub fn pairing_ids(&self) -> Vec<String> {
        self.pairings.iter().map(|p| p.pairing_id.clone()).collect()
    }

    /// Whether `pairing_id` is a direct-mode pairing (spec §5.10). Unknown ids
    /// are not.
    pub fn is_direct(&self, pairing_id: &str) -> bool {
        self.pairing(pairing_id).is_some_and(Pairing::is_direct)
    }
}

/// The per-user remote-state path, `~/.flightdeck/remote.json`. Returns `None`
/// when neither `$HOME` nor `%USERPROFILE%` is set (the caller then simply skips
/// remote persistence rather than failing), matching the workspace-file idiom.
pub fn remote_state_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(PathBuf::from(home).join(".flightdeck").join("remote.json"))
}

/// Load and deserialize the remote-state file.
pub fn load_remote_state(fs: &dyn FileSystem, path: &Path) -> Result<RemoteState> {
    let contents = fs.read_to_string(path).map_err(|e| {
        FlightDeckError::State(format!(
            "failed to read remote file {}: {e}",
            path.display()
        ))
    })?;
    let state: RemoteState = serde_json::from_str(&contents)
        .map_err(|e| FlightDeckError::State(format!("failed to parse remote file: {e}")))?;
    Ok(state)
}

/// Serialize and write the remote-state file, creating `~/.flightdeck/` if
/// needed, then hardening perms to owner-only on Unix (best-effort — see the
/// module docs).
pub fn save_remote_state(fs: &dyn FileSystem, path: &Path, state: &RemoteState) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !fs.exists(parent) {
            fs.create_dir_all(parent)?;
        }
    }
    let json = serde_json::to_string_pretty(state)
        .map_err(|e| FlightDeckError::State(format!("failed to serialize remote state: {e}")))?;
    fs.write(path, &json)
        .map_err(|e| FlightDeckError::State(format!("failed to write remote file: {e}")))?;
    harden_permissions(path);
    Ok(())
}

/// Best-effort owner-only (`0600`) hardening for the private-key file. No-op off
/// Unix and silently ignored when the path is not a real on-disk file (tests).
#[cfg(unix)]
fn harden_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn harden_permissions(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeFs;

    #[test]
    fn round_trip_save_then_load() {
        let fs = FakeFs::new();
        let path = Path::new("/home/user/.flightdeck/remote.json");
        let mut state = RemoteState {
            version: REMOTE_STATE_VERSION,
            device_private_key: "AAAA".to_string(),
            relay_url: Some("ws://127.0.0.1:8080/ws".to_string()),
            pairings: vec![Pairing {
                pairing_id: "pair_a".to_string(),
                peer_device_id: Some("dev_phone".to_string()),
                last_sent_seq: 7,
                last_acked_by_peer: 5,
                last_received_seq: 12,
                peer_key_agreement_public_key: Some("BPeerKaKey".to_string()),
                claim_token: Some("4729".to_string()),
                established: true,
                peer_device_public_key: Some("BPhoneIdKey".to_string()),
                transport: PairingTransport::Direct,
            }],
        };
        save_remote_state(&fs, path, &state).expect("save");
        let loaded = load_remote_state(&fs, path).expect("load");
        assert_eq!(loaded, state);

        // Mutating a cursor and re-saving must not disturb the key.
        state.pairing_mut("pair_a").unwrap().last_received_seq = 13;
        save_remote_state(&fs, path, &state).expect("save 2");
        let loaded = load_remote_state(&fs, path).expect("load 2");
        assert_eq!(loaded.device_private_key, "AAAA");
        assert_eq!(loaded.pairing("pair_a").unwrap().last_received_seq, 13);
    }

    #[test]
    fn missing_file_is_err_but_default_is_usable() {
        let fs = FakeFs::new();
        let path = Path::new("/home/user/.flightdeck/remote.json");
        assert!(load_remote_state(&fs, path).is_err());
        // Callers fall back to default(), which is a valid empty device.
        let d = RemoteState::default();
        assert!(d.device_private_key.is_empty());
        assert!(d.pairings.is_empty());
    }

    #[test]
    fn save_creates_parent_dir() {
        let fs = FakeFs::new();
        let path = Path::new("/home/user/.flightdeck/remote.json");
        save_remote_state(&fs, path, &RemoteState::default()).expect("save");
        assert!(fs.exists(Path::new("/home/user/.flightdeck")));
    }

    #[test]
    fn tolerates_partial_json() {
        // A hand-written file with only the key present must load, defaulting the
        // rest — so a future field never bricks an older file.
        let fs = FakeFs::new().with_file(
            "/home/user/.flightdeck/remote.json",
            r#"{"version":1,"device_private_key":"KEY"}"#,
        );
        let path = Path::new("/home/user/.flightdeck/remote.json");
        let loaded = load_remote_state(&fs, path).expect("load");
        assert_eq!(loaded.device_private_key, "KEY");
        assert!(loaded.pairings.is_empty());
        assert_eq!(loaded.relay_url, None);
    }

    /// A pairing record written before the direct link existed carries neither
    /// `transport` nor `peer_device_public_key`; it must load as a relay-mode
    /// pairing with no phone identity key — exactly how it behaves (spec §5.2:
    /// pre-v4 pairings are relay mode, re-pair to switch).
    #[test]
    fn a_pre_direct_pairing_record_loads_as_relay_mode() {
        let fs = FakeFs::new().with_file(
            "/home/user/.flightdeck/remote.json",
            r#"{"version":1,"device_private_key":"KEY","pairings":[
                {"pairing_id":"pair_old","peer_device_id":"dev","last_sent_seq":9,
                 "peer_key_agreement_public_key":"BKa","claim_token":"1234","established":true}]}"#,
        );
        let path = Path::new("/home/user/.flightdeck/remote.json");
        let loaded = load_remote_state(&fs, path).expect("load");
        let p = loaded.pairing("pair_old").expect("pairing");
        assert_eq!(p.transport, PairingTransport::Relay);
        assert!(!p.is_direct());
        assert_eq!(p.peer_device_public_key, None);
        assert_eq!(p.last_sent_seq, 9);
        assert!(!loaded.is_direct("pair_old"));
    }

    /// The transport is persisted in snake_case.
    #[test]
    fn transport_is_persisted() {
        let fs = FakeFs::new();
        let path = Path::new("/home/user/.flightdeck/remote.json");
        let mut state = RemoteState::default();
        let mut p = Pairing::new("pair_d");
        p.transport = PairingTransport::Direct;
        state.pairings.push(p);
        save_remote_state(&fs, path, &state).expect("save");
        let raw = fs.file_contents(path).expect("written");
        assert!(raw.contains(r#""transport": "direct""#), "{raw}");
        let loaded = load_remote_state(&fs, path).expect("load");
        assert!(loaded.is_direct("pair_d"));
    }
}
