//! End-to-end channel crypto for FlightDeck Remote (spec §7).
//!
//! This is the encryption layer the relay cannot see into. The relay routes an
//! [`EncryptedEnvelope`](flightdeck_remote_protocol::relay::EncryptedEnvelope) by
//! `pairing_id` alone; its `ciphertext` is sealed here and opened only on the two
//! endpoints (phone & desktop). This module turns the pairing bootstrap material
//! (§5.2) plus the two devices' P-256 identity keys into a pair of directional
//! AEAD keys, and seals/opens the per-message payloads.
//!
//! # Scheme (pinned in spec §7)
//!
//! 1. **Shared input keying material (IKM).** A *static-static* P-256 ECDH between
//!    the two device identity keys (the same keypairs used for relay auth in
//!    [`crate::remote::identity`]). Both endpoints can compute the identical
//!    32-byte shared secret — its big-endian **x-coordinate** — from their own
//!    private scalar and the peer's public key. This input is available on **both**
//!    pairing paths (QR and 4-digit code), because both devices always exchange
//!    identity public keys during bootstrap.
//!
//!    *Forward secrecy:* v1 uses the long-lived identity keys directly, so the
//!    channel has **no forward secrecy** — compromise of a device key exposes past
//!    traffic. This is a deliberate v1 simplification; ephemeral-key rotation is a
//!    deferred item (PRD §13). It is called out here so it is not mistaken for an
//!    oversight.
//!
//! 2. **Salt binds the bootstrap secret.** HKDF's salt is the pairing bootstrap
//!    secret: the 32-byte random `pairing_secret` carried in the **QR** payload,
//!    or the **claim-token** bytes for the 4-digit **code** path. The salt is what
//!    ties the derived keys to *this* pairing act (so a stolen identity key alone,
//!    without having observed the bootstrap, still cannot derive the channel keys).
//!    This layer is agnostic to which one it is handed — the caller passes the
//!    right bytes as `salt`.
//!
//! 3. **KDF.** HKDF-SHA256(ikm, salt) is expanded twice, once per direction, into
//!    two independent 32-byte keys:
//!    * `info = "flightdeck-remote-e2e-v1:" ‖ pairing_id ‖ ":d2p"` → desktop→phone
//!    * `info = "flightdeck-remote-e2e-v1:" ‖ pairing_id ‖ ":p2d"` → phone→desktop
//!
//! 4. **AEAD.** ChaCha20-Poly1305 with a fresh random 12-byte nonce per message
//!    (carried in the envelope's `nonce` field). The **AAD** is the UTF-8 of the
//!    canonical header string `pairing_id ‖ ":" ‖ seq ‖ ":" ‖ sender ‖ ":" ‖
//!    sent_at_ms` (decimal integers; `sender` is `desktop`/`phone`). Binding the
//!    header as AAD means the relay cannot alter routing/ordering/attribution
//!    without the receiver's `open` failing.
//!
//! The construction is byte-compatible with the CryptoKit implementation on iOS
//! (`E2EChannel.swift`); the checked-in cross-language vectors
//! (`remote/protocol/tests/fixtures/e2e_crypto/vectors.json`) are the contract.

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use chacha20poly1305::aead::{Aead, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, KeyInit, Nonce};
use hkdf::Hkdf;
use p256::ecdh::diffie_hellman;
use p256::{PublicKey, SecretKey};
use rand_core::{OsRng, RngCore};
use sha2::Sha256;

use crate::contracts::{FlightDeckError, Result};
use flightdeck_remote_protocol::Role;

/// Length of a derived AEAD key (ChaCha20-Poly1305 key).
pub const KEY_LEN: usize = 32;
/// Length of the per-message AEAD nonce.
pub const NONCE_LEN: usize = 12;

/// The fixed protocol label mixed into every derived key (spec §7). Bumping this
/// string is how a future incompatible E2E change is fenced off.
const INFO_PREFIX: &str = "flightdeck-remote-e2e-v1";

/// A derived, ready-to-use end-to-end channel for one pairing on one endpoint.
///
/// Holds the two directional keys plus this endpoint's [`Role`] (which selects the
/// send key). Construct it with [`E2eChannel::derive`]; then [`E2eChannel::seal`]
/// outgoing payloads and [`E2eChannel::open`] incoming ones. The private key
/// material is consumed at derive time and never stored here.
pub struct E2eChannel {
    /// This endpoint's role — chooses which directional key `seal` uses.
    role: Role,
    /// Pairing id, bound into every AAD and into key derivation.
    pairing_id: String,
    /// Desktop→phone key.
    d2p_key: [u8; KEY_LEN],
    /// Phone→desktop key.
    p2d_key: [u8; KEY_LEN],
}

impl std::fmt::Debug for E2eChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never render the key material.
        f.debug_struct("E2eChannel")
            .field("role", &self.role)
            .field("pairing_id", &self.pairing_id)
            .finish_non_exhaustive()
    }
}

impl E2eChannel {
    /// Derive the channel from the local device's P-256 private scalar, the peer's
    /// X9.63 public key (65 bytes, `0x04 ‖ x ‖ y` — the same encoding
    /// [`crate::remote::identity`] publishes), the `pairing_id`, the bootstrap
    /// `salt` (QR `pairing_secret` or the code path's claim-token bytes), and this
    /// endpoint's `role`.
    ///
    /// Both endpoints, feeding in their own private key + the peer's public key,
    /// arrive at the identical `d2p`/`p2d` key pair.
    pub fn derive(
        identity_private_scalar: &[u8],
        peer_public_key_x963: &[u8],
        pairing_id: &str,
        salt: &[u8],
        role: Role,
    ) -> Result<Self> {
        let secret = SecretKey::from_slice(identity_private_scalar)
            .map_err(|e| FlightDeckError::State(format!("e2e: invalid local private key: {e}")))?;
        let peer = PublicKey::from_sec1_bytes(peer_public_key_x963)
            .map_err(|e| FlightDeckError::State(format!("e2e: invalid peer public key: {e}")))?;

        // Static-static ECDH → the shared secret's x-coordinate is the HKDF IKM.
        let shared = diffie_hellman(secret.to_nonzero_scalar(), peer.as_affine());
        let ikm = shared.raw_secret_bytes();

        let hk = Hkdf::<Sha256>::new(Some(salt), ikm.as_slice());
        let d2p_key = expand_key(&hk, pairing_id, "d2p")?;
        let p2d_key = expand_key(&hk, pairing_id, "p2d")?;

        Ok(Self {
            role,
            pairing_id: pairing_id.to_string(),
            d2p_key,
            p2d_key,
        })
    }

    /// Seal an outgoing plaintext payload. Uses this endpoint's send key (chosen by
    /// [`Role`]) and a fresh random nonce. Returns `(nonce_b64, ciphertext_b64)`
    /// ready to drop into an
    /// [`EncryptedEnvelope`](flightdeck_remote_protocol::relay::EncryptedEnvelope);
    /// `ciphertext` is the AEAD output with its 16-byte tag appended.
    pub fn seal(
        &self,
        plaintext_json: &[u8],
        seq: u64,
        sent_at_ms: i64,
    ) -> Result<(String, String)> {
        let mut nonce = [0u8; NONCE_LEN];
        OsRng.fill_bytes(&mut nonce);
        self.seal_with_nonce(plaintext_json, seq, sent_at_ms, &nonce)
    }

    /// Seal with a caller-supplied nonce. **Test/vector hook only** — production
    /// code must use [`E2eChannel::seal`] so every message gets a fresh random
    /// nonce (nonce reuse under a fixed key is catastrophic for ChaCha20-Poly1305).
    pub fn seal_with_nonce(
        &self,
        plaintext_json: &[u8],
        seq: u64,
        sent_at_ms: i64,
        nonce: &[u8; NONCE_LEN],
    ) -> Result<(String, String)> {
        // The sender of an outgoing message is always this endpoint's role.
        let key = self.key_for_sender(self.role);
        let aad = header_aad(&self.pairing_id, seq, self.role, sent_at_ms);
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&key));
        let ciphertext = cipher
            .encrypt(
                Nonce::from_slice(nonce),
                Payload {
                    msg: plaintext_json,
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|_| FlightDeckError::State("e2e: seal failed".to_string()))?;
        Ok((STANDARD.encode(nonce), STANDARD.encode(ciphertext)))
    }

    /// Open an incoming envelope. `sender` is the envelope's `sender` field (the
    /// peer's role, which selects the receive key and is bound into the AAD). Fails
    /// if the key is wrong, the ciphertext was tampered with, or any AAD header
    /// field (`seq`/`sender`/`sent_at_ms`, or the channel's `pairing_id`) does not
    /// match what was sealed.
    pub fn open(
        &self,
        seq: u64,
        sender: Role,
        sent_at_ms: i64,
        nonce_b64: &str,
        ciphertext_b64: &str,
    ) -> Result<Vec<u8>> {
        let nonce = STANDARD
            .decode(nonce_b64)
            .map_err(|e| FlightDeckError::State(format!("e2e: nonce not base64: {e}")))?;
        if nonce.len() != NONCE_LEN {
            return Err(FlightDeckError::State(format!(
                "e2e: nonce must be {NONCE_LEN} bytes, got {}",
                nonce.len()
            )));
        }
        let ciphertext = STANDARD
            .decode(ciphertext_b64)
            .map_err(|e| FlightDeckError::State(format!("e2e: ciphertext not base64: {e}")))?;

        let key = self.key_for_sender(sender);
        let aad = header_aad(&self.pairing_id, seq, sender, sent_at_ms);
        let cipher = ChaCha20Poly1305::new(Key::from_slice(&key));
        cipher
            .decrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: &ciphertext,
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|_| {
                FlightDeckError::State(
                    "e2e: open failed (wrong key, tampered, or bad header)".to_string(),
                )
            })
    }

    /// The directional key a given sender's messages are encrypted under.
    fn key_for_sender(&self, sender: Role) -> [u8; KEY_LEN] {
        match sender {
            Role::Desktop => self.d2p_key,
            Role::Phone => self.p2d_key,
        }
    }
}

/// HKDF-Expand one directional key. `direction` is `"d2p"` or `"p2d"`.
fn expand_key(hk: &Hkdf<Sha256>, pairing_id: &str, direction: &str) -> Result<[u8; KEY_LEN]> {
    let info = format!("{INFO_PREFIX}:{pairing_id}:{direction}");
    let mut key = [0u8; KEY_LEN];
    hk.expand(info.as_bytes(), &mut key)
        .map_err(|e| FlightDeckError::State(format!("e2e: hkdf expand failed: {e}")))?;
    Ok(key)
}

/// The canonical AAD string bound to every message (spec §7):
/// `pairing_id ":" seq ":" sender ":" sent_at_ms`.
fn header_aad(pairing_id: &str, seq: u64, sender: Role, sent_at_ms: i64) -> String {
    format!("{pairing_id}:{seq}:{}:{sent_at_ms}", role_str(sender))
}

/// The wire spelling of a [`Role`] (matches its `snake_case` serde form).
fn role_str(role: Role) -> &'static str {
    match role {
        Role::Desktop => "desktop",
        Role::Phone => "phone",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::elliptic_curve::sec1::ToEncodedPoint;
    use p256::SecretKey;
    use serde_json::json;
    use std::path::PathBuf;

    // Fixed, test-only private scalars (never used outside tests). Both are small
    // and therefore trivially below the P-256 group order, hence valid scalars.
    const DESKTOP_SCALAR: [u8; 32] = [
        0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f,
        0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e,
        0x1f, 0x20,
    ];
    const PHONE_SCALAR: [u8; 32] = [
        0x20, 0x1f, 0x1e, 0x1d, 0x1c, 0x1b, 0x1a, 0x19, 0x18, 0x17, 0x16, 0x15, 0x14, 0x13, 0x12,
        0x11, 0x10, 0x0f, 0x0e, 0x0d, 0x0c, 0x0b, 0x0a, 0x09, 0x08, 0x07, 0x06, 0x05, 0x04, 0x03,
        0x02, 0x01,
    ];

    fn public_x963(scalar: &[u8; 32]) -> Vec<u8> {
        let sk = SecretKey::from_slice(scalar).expect("valid scalar");
        sk.public_key().to_encoded_point(false).as_bytes().to_vec()
    }

    fn channels_for(pairing_id: &str, salt: &[u8]) -> (E2eChannel, E2eChannel) {
        let desktop = E2eChannel::derive(
            &DESKTOP_SCALAR,
            &public_x963(&PHONE_SCALAR),
            pairing_id,
            salt,
            Role::Desktop,
        )
        .expect("desktop derive");
        let phone = E2eChannel::derive(
            &PHONE_SCALAR,
            &public_x963(&DESKTOP_SCALAR),
            pairing_id,
            salt,
            Role::Phone,
        )
        .expect("phone derive");
        (desktop, phone)
    }

    #[test]
    fn both_endpoints_derive_identical_keys() {
        let (desktop, phone) = channels_for("pair_x", b"salt-bytes-here");
        assert_eq!(desktop.d2p_key, phone.d2p_key);
        assert_eq!(desktop.p2d_key, phone.p2d_key);
        // The two directions must be distinct keys.
        assert_ne!(desktop.d2p_key, desktop.p2d_key);
    }

    #[test]
    fn desktop_to_phone_round_trips() {
        let (desktop, phone) = channels_for("pair_x", b"salt-bytes-here");
        let plaintext = br#"{"type":"status_update","updates":[]}"#;
        let (nonce, ct) = desktop.seal(plaintext, 1, 1_752_412_802_000).expect("seal");
        let opened = phone
            .open(1, Role::Desktop, 1_752_412_802_000, &nonce, &ct)
            .expect("open");
        assert_eq!(opened, plaintext);
    }

    #[test]
    fn phone_to_desktop_round_trips() {
        let (desktop, phone) = channels_for("pair_x", b"salt-bytes-here");
        let plaintext = br#"{"command_id":"cmd_1","type":"reply","text":"go"}"#;
        let (nonce, ct) = phone.seal(plaintext, 7, 1_752_412_900_000).expect("seal");
        let opened = desktop
            .open(7, Role::Phone, 1_752_412_900_000, &nonce, &ct)
            .expect("open");
        assert_eq!(opened, plaintext);
    }

    #[test]
    fn tampered_seq_in_aad_is_rejected() {
        let (desktop, phone) = channels_for("pair_x", b"salt-bytes-here");
        let (nonce, ct) = desktop.seal(b"hello", 1, 42).expect("seal");
        // Same ciphertext, but claim a different seq: AAD no longer authenticates.
        assert!(phone.open(2, Role::Desktop, 42, &nonce, &ct).is_err());
        // Wrong sent_at_ms likewise fails.
        assert!(phone.open(1, Role::Desktop, 43, &nonce, &ct).is_err());
        // The honest header still opens.
        assert!(phone.open(1, Role::Desktop, 42, &nonce, &ct).is_ok());
    }

    #[test]
    fn tampered_ciphertext_is_rejected() {
        let (desktop, phone) = channels_for("pair_x", b"salt-bytes-here");
        let (nonce, ct) = desktop.seal(b"hello world", 1, 42).expect("seal");
        let mut raw = STANDARD.decode(&ct).unwrap();
        let last = raw.len() - 1;
        raw[last] ^= 0x01;
        let tampered = STANDARD.encode(&raw);
        assert!(phone.open(1, Role::Desktop, 42, &nonce, &tampered).is_err());
    }

    #[test]
    fn wrong_pairing_secret_yields_wrong_keys() {
        let (desktop, _) = channels_for("pair_x", b"salt-one");
        let (_, phone_other) = channels_for("pair_x", b"salt-two");
        let (nonce, ct) = desktop.seal(b"secret", 1, 1).expect("seal");
        // Different bootstrap salt → different keys → cannot open.
        assert!(phone_other.open(1, Role::Desktop, 1, &nonce, &ct).is_err());
    }

    #[test]
    fn wrong_direction_key_cannot_open() {
        // A desktop-sealed message claimed as phone-sent uses the wrong key.
        let (desktop, phone) = channels_for("pair_x", b"salt-bytes-here");
        let (nonce, ct) = desktop.seal(b"m", 1, 1).expect("seal");
        assert!(phone.open(1, Role::Phone, 1, &nonce, &ct).is_err());
    }

    // ---------------------------------------------------------------------
    // Cross-language contract: generate + lock the shared test vectors.
    // ---------------------------------------------------------------------

    /// Absolute path to the checked-in cross-language vectors.
    fn vectors_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("remote/protocol/tests/fixtures/e2e_crypto/vectors.json")
    }

    /// Build one vector case: derived keys + a set of fixed-nonce seal outputs in
    /// both directions, plus a tamper descriptor for the Swift side to check.
    fn build_case(name: &str, pairing_id: &str, salt: &[u8]) -> serde_json::Value {
        let (desktop, phone) = channels_for(pairing_id, salt);

        // Fixed nonces so the sealed bytes are reproducible across languages.
        let n_d2p: [u8; NONCE_LEN] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
        let n_p2d: [u8; NONCE_LEN] = [11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0];

        let d2p_plain = br#"{"type":"status_update","updates":[{"session_id":"s1"}]}"#;
        let p2d_plain = br#"{"command_id":"cmd_00000001","type":"reply","text":"Yes, run it."}"#;

        let d2p_seq = 1u64;
        let d2p_ts = 1_752_412_802_000i64;
        let p2d_seq = 1u64;
        let p2d_ts = 1_752_412_810_000i64;

        let (d2p_nonce_b64, d2p_ct_b64) = desktop
            .seal_with_nonce(d2p_plain, d2p_seq, d2p_ts, &n_d2p)
            .expect("seal d2p");
        let (p2d_nonce_b64, p2d_ct_b64) = phone
            .seal_with_nonce(p2d_plain, p2d_seq, p2d_ts, &n_p2d)
            .expect("seal p2d");

        // Sanity: our own opens succeed (keeps the generator honest).
        assert_eq!(
            phone
                .open(d2p_seq, Role::Desktop, d2p_ts, &d2p_nonce_b64, &d2p_ct_b64)
                .unwrap(),
            d2p_plain
        );
        assert_eq!(
            desktop
                .open(p2d_seq, Role::Phone, p2d_ts, &p2d_nonce_b64, &p2d_ct_b64)
                .unwrap(),
            p2d_plain
        );

        json!({
            "name": name,
            "pairing_id": pairing_id,
            "role_input": {
                "desktop_private_scalar_hex": hex(&DESKTOP_SCALAR),
                "phone_private_scalar_hex": hex(&PHONE_SCALAR),
                "desktop_public_x963_b64": STANDARD.encode(public_x963(&DESKTOP_SCALAR)),
                "phone_public_x963_b64": STANDARD.encode(public_x963(&PHONE_SCALAR)),
            },
            "salt_b64": STANDARD.encode(salt),
            "info_prefix": INFO_PREFIX,
            "derived": {
                "d2p_key_hex": hex(&desktop.d2p_key),
                "p2d_key_hex": hex(&desktop.p2d_key),
            },
            "messages": [
                {
                    "direction": "d2p",
                    "sender": "desktop",
                    "seq": d2p_seq,
                    "sent_at_ms": d2p_ts,
                    "aad": header_aad(pairing_id, d2p_seq, Role::Desktop, d2p_ts),
                    "plaintext_utf8": String::from_utf8(d2p_plain.to_vec()).unwrap(),
                    "nonce_b64": d2p_nonce_b64,
                    "ciphertext_b64": d2p_ct_b64,
                },
                {
                    "direction": "p2d",
                    "sender": "phone",
                    "seq": p2d_seq,
                    "sent_at_ms": p2d_ts,
                    "aad": header_aad(pairing_id, p2d_seq, Role::Phone, p2d_ts),
                    "plaintext_utf8": String::from_utf8(p2d_plain.to_vec()).unwrap(),
                    "nonce_b64": p2d_nonce_b64,
                    "ciphertext_b64": p2d_ct_b64,
                }
            ],
            // The Swift side must confirm this altered header fails to open.
            "tamper": {
                "based_on_message_index": 0,
                "tampered_seq": d2p_seq + 1,
                "expect": "reject"
            }
        })
    }

    fn hex(bytes: &[u8]) -> String {
        let mut s = String::with_capacity(bytes.len() * 2);
        for b in bytes {
            s.push_str(&format!("{b:02x}"));
        }
        s
    }

    /// Generate the cross-language vectors and lock them: on first run (file
    /// absent) write it; thereafter assert the regenerated bytes match the
    /// checked-in file, so any drift in either the scheme or the file is caught.
    #[test]
    fn generate_and_lock_cross_language_vectors() {
        let vectors = json!({
            "scheme": {
                "ikm": "P-256 static-static ECDH shared secret x-coordinate (big-endian, 32 bytes)",
                "kdf": "HKDF-SHA256(ikm, salt); info = \"flightdeck-remote-e2e-v1:\" + pairing_id + \":d2p\"|\":p2d\"",
                "aead": "ChaCha20-Poly1305, 12-byte nonce, ciphertext includes the 16-byte tag",
                "aad": "utf8(pairing_id + \":\" + seq + \":\" + sender + \":\" + sent_at_ms)"
            },
            "cases": [
                // QR path: salt is the 32-byte random pairing_secret.
                build_case(
                    "qr_pairing_secret",
                    "pair_qr_000001",
                    &[
                        0xa0, 0xa1, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xab,
                        0xac, 0xad, 0xae, 0xaf, 0xb0, 0xb1, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7,
                        0xb8, 0xb9, 0xba, 0xbb, 0xbc, 0xbd, 0xbe, 0xbf,
                    ],
                ),
                // Code path: salt is the claim-token bytes (its UTF-8).
                build_case("code_claim_token", "pair_code_00002", b"claim-482913-abcdef"),
            ]
        });

        let mut serialized = serde_json::to_string_pretty(&vectors).expect("serialize vectors");
        serialized.push('\n');

        let path = vectors_path();
        if path.exists() {
            let on_disk = std::fs::read_to_string(&path).expect("read vectors");
            assert_eq!(
                on_disk, serialized,
                "e2e_crypto/vectors.json is out of date. Delete it and re-run this test to regenerate."
            );
        } else {
            std::fs::create_dir_all(path.parent().unwrap()).expect("mkdir fixtures");
            std::fs::write(&path, &serialized).expect("write vectors");
        }
    }
}
