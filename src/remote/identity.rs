//! Per-device identity for FlightDeck Remote.
//!
//! Every desktop holds one long-lived ECDSA **P-256** keypair. The relay
//! authenticates the connection by a challenge it signs (spec §5.1), and derives
//! nothing else from it — the relay is zero-knowledge. The same curve and the
//! same wire encodings are used by the iOS app (whose key lives in the Secure
//! Enclave, which is why the protocol mandates P-256 rather than Ed25519).
//!
//! Encodings, byte-for-byte identical across every device (spec §5.1):
//! * **public key** — base64 (standard, padded) of the X9.63 uncompressed SEC1
//!   point: 65 bytes, `0x04 ‖ x ‖ y`.
//! * **signature** — base64 (standard, padded) of the raw `r ‖ s` form: 64 bytes.
//! * **device id** — base64url **without padding** of `SHA-256(public_key)`
//!   (the 65-byte X9.63 form), matching the iOS convention.
//!
//! The private scalar is persisted (base64, standard) inside
//! [`crate::remote::state`]'s `remote.json`; this module turns those bytes back
//! into a usable signer and never logs or exposes them.

use crate::contracts::{FileSystem, FlightDeckError, Result};
use crate::remote::state::{load_remote_state, save_remote_state, RemoteState};
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use sha2::{Digest, Sha256};
use std::path::Path;

/// Length of the X9.63 uncompressed public-key encoding (`0x04 ‖ x ‖ y`).
pub const PUBLIC_KEY_LEN: usize = 65;
/// Length of the raw `r ‖ s` ECDSA signature.
pub const SIGNATURE_LEN: usize = 64;

/// A loaded, usable device identity: the signing key plus its cached public
/// encoding and derived device id.
pub struct DeviceIdentity {
    signing: SigningKey,
    public_key_x963: [u8; PUBLIC_KEY_LEN],
    device_id: String,
}

impl std::fmt::Debug for DeviceIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never render the private key.
        f.debug_struct("DeviceIdentity")
            .field("device_id", &self.device_id)
            .finish_non_exhaustive()
    }
}

impl DeviceIdentity {
    /// Generate a brand-new random identity from the OS CSPRNG.
    pub fn generate() -> Self {
        let signing = SigningKey::random(&mut rand_core::OsRng);
        Self::from_signing_key(signing)
    }

    /// Rebuild an identity from its 32-byte private scalar.
    pub fn from_private_bytes(bytes: &[u8]) -> Result<Self> {
        let signing = SigningKey::from_slice(bytes)
            .map_err(|e| FlightDeckError::State(format!("invalid device private key: {e}")))?;
        Ok(Self::from_signing_key(signing))
    }

    /// Rebuild an identity from the base64 (standard, padded) private scalar.
    pub fn from_private_base64(b64: &str) -> Result<Self> {
        let bytes = STANDARD
            .decode(b64)
            .map_err(|e| FlightDeckError::State(format!("device key is not valid base64: {e}")))?;
        Self::from_private_bytes(&bytes)
    }

    fn from_signing_key(signing: SigningKey) -> Self {
        let point = signing.verifying_key().to_encoded_point(false);
        let mut public_key_x963 = [0u8; PUBLIC_KEY_LEN];
        // P-256's uncompressed point is always 65 bytes; copy defensively.
        let bytes = point.as_bytes();
        public_key_x963.copy_from_slice(&bytes[..PUBLIC_KEY_LEN]);
        let device_id = derive_device_id(&public_key_x963);
        DeviceIdentity {
            signing,
            public_key_x963,
            device_id,
        }
    }

    /// The 32-byte private scalar (for persistence). Handle with care.
    pub fn private_key_bytes(&self) -> Vec<u8> {
        self.signing.to_bytes().to_vec()
    }

    /// The private scalar as base64 (standard, padded), for `remote.json`.
    pub fn private_key_base64(&self) -> String {
        STANDARD.encode(self.signing.to_bytes())
    }

    /// The X9.63 uncompressed public key (65 bytes, `0x04 ‖ x ‖ y`).
    pub fn public_key_x963(&self) -> &[u8; PUBLIC_KEY_LEN] {
        &self.public_key_x963
    }

    /// The public key as base64 (standard, padded) — the `device_public_key`
    /// wire encoding.
    pub fn public_key_base64(&self) -> String {
        STANDARD.encode(self.public_key_x963)
    }

    /// The stable device id (base64url-no-pad of `SHA-256(public_key)`).
    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    /// Sign raw message bytes, returning the raw `r ‖ s` (64-byte) signature.
    /// ECDSA P-256 over SHA-256 of the message (deterministic, RFC 6979).
    pub fn sign(&self, message: &[u8]) -> [u8; SIGNATURE_LEN] {
        let sig: Signature = self.signing.sign(message);
        let mut out = [0u8; SIGNATURE_LEN];
        out.copy_from_slice(sig.to_bytes().as_slice());
        out
    }

    /// Sign a base64 (standard, padded) challenge nonce as delivered in
    /// `auth_challenge`, returning the base64 (standard, padded) signature for
    /// `auth_response`. The signature covers the exact decoded nonce bytes.
    pub fn sign_nonce_base64(&self, nonce_b64: &str) -> Result<String> {
        let nonce = STANDARD
            .decode(nonce_b64)
            .map_err(|e| FlightDeckError::State(format!("challenge nonce is not base64: {e}")))?;
        Ok(STANDARD.encode(self.sign(&nonce)))
    }
}

/// Derive the device id from an X9.63 public key: base64url-no-pad of its
/// SHA-256 digest. A free function so callers can compute a peer's id from a
/// received public key without a full identity.
pub fn derive_device_id(public_key_x963: &[u8]) -> String {
    let digest = Sha256::digest(public_key_x963);
    URL_SAFE_NO_PAD.encode(digest)
}

/// Load the device identity from `remote.json`, generating and persisting a new
/// keypair on first run (or when the stored key is unreadable). Returns the
/// identity together with the full [`RemoteState`] (pairings + cursors) so the
/// caller can hand the pairings to the relay client.
///
/// Best-effort like the workspace file: a save failure is swallowed (the app
/// still runs this session, it just may re-generate next launch).
pub fn load_or_create_identity(
    fs: &dyn FileSystem,
    path: &Path,
) -> Result<(DeviceIdentity, RemoteState)> {
    let mut state = load_remote_state(fs, path).unwrap_or_default();

    if !state.device_private_key.is_empty() {
        if let Ok(identity) = DeviceIdentity::from_private_base64(&state.device_private_key) {
            return Ok((identity, state));
        }
        // A corrupt key: fall through and mint a fresh one rather than wedging.
    }

    let identity = DeviceIdentity::generate();
    state.device_private_key = identity.private_key_base64();
    let _ = save_remote_state(fs, path, &state);
    Ok((identity, state))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeFs;
    use p256::ecdsa::signature::Verifier;
    use p256::ecdsa::VerifyingKey;

    #[test]
    fn public_key_is_65_byte_x963_uncompressed() {
        let id = DeviceIdentity::generate();
        let pk = id.public_key_x963();
        assert_eq!(pk.len(), 65);
        assert_eq!(pk[0], 0x04, "uncompressed SEC1 point must start with 0x04");
        // base64 of 65 bytes is 88 chars (padded).
        assert_eq!(id.public_key_base64().len(), 88);
    }

    #[test]
    fn signature_is_64_bytes_and_verifies_with_public_key() {
        let id = DeviceIdentity::generate();
        let nonce = b"the-exact-challenge-nonce-bytes";
        let sig_bytes = id.sign(nonce);
        assert_eq!(sig_bytes.len(), 64);

        // A fresh verifier built from ONLY the published x963 public key must
        // accept the signature — this is exactly what the relay does.
        let vk = VerifyingKey::from_sec1_bytes(id.public_key_x963()).expect("parse pubkey");
        let sig = Signature::from_slice(&sig_bytes).expect("parse sig");
        assert!(vk.verify(nonce, &sig).is_ok());
        // A different message must not verify.
        assert!(vk.verify(b"other", &sig).is_err());
    }

    #[test]
    fn base64_signature_round_trips_through_the_wire_encoding() {
        let id = DeviceIdentity::generate();
        // A relay-style base64 nonce.
        let nonce_raw = [9u8; 32];
        let nonce_b64 = STANDARD.encode(nonce_raw);
        let sig_b64 = id.sign_nonce_base64(&nonce_b64).expect("sign");
        let sig_bytes = STANDARD.decode(&sig_b64).expect("decode sig");
        assert_eq!(sig_bytes.len(), 64);
        let vk = VerifyingKey::from_sec1_bytes(id.public_key_x963()).unwrap();
        let sig = Signature::from_slice(&sig_bytes).unwrap();
        assert!(vk.verify(&nonce_raw, &sig).is_ok());
    }

    #[test]
    fn private_key_round_trip_preserves_identity() {
        let id = DeviceIdentity::generate();
        let device_id = id.device_id().to_string();
        let pub_b64 = id.public_key_base64();

        let restored = DeviceIdentity::from_private_base64(&id.private_key_base64()).unwrap();
        assert_eq!(restored.device_id(), device_id);
        assert_eq!(restored.public_key_base64(), pub_b64);
    }

    #[test]
    fn device_id_is_url_safe_no_pad_sha256() {
        let id = DeviceIdentity::generate();
        // 32-byte SHA-256 in url-safe-no-pad base64 is 43 chars.
        assert_eq!(id.device_id().len(), 43);
        assert!(!id.device_id().contains('='), "no padding");
        assert!(!id.device_id().contains('+') && !id.device_id().contains('/'));
        // Deterministic given the public key.
        assert_eq!(id.device_id(), derive_device_id(id.public_key_x963()));
    }

    #[test]
    fn load_or_create_persists_then_reloads_same_identity() {
        let fs = FakeFs::new();
        let path = Path::new("/home/user/.flightdeck/remote.json");

        let (id1, _s1) = load_or_create_identity(&fs, path).expect("first");
        let first_device = id1.device_id().to_string();

        // Second load must return the SAME device (key was persisted).
        let (id2, s2) = load_or_create_identity(&fs, path).expect("second");
        assert_eq!(id2.device_id(), first_device);
        assert!(!s2.device_private_key.is_empty());
    }

    #[test]
    fn corrupt_key_is_regenerated_not_fatal() {
        let fs = FakeFs::new().with_file(
            "/home/user/.flightdeck/remote.json",
            r#"{"version":1,"device_private_key":"!!not-base64!!"}"#,
        );
        let path = Path::new("/home/user/.flightdeck/remote.json");
        let (id, _s) = load_or_create_identity(&fs, path).expect("recovers");
        assert_eq!(id.device_id().len(), 43);
    }
}
