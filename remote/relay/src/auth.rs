//! Device authentication: the ECDSA P-256 / SHA-256 challenge-response from
//! spec §5.1.
//!
//! The relay stores only **public** keys. On connect it sends a random 32-byte
//! nonce ([`AuthChallenge`]); the endpoint signs the nonce bytes with its
//! Secure-Enclave / keystore-resident private key and returns the raw `r ‖ s`
//! signature. This module verifies that signature against the registered public
//! key. It holds no private key material and performs no decryption — the relay
//! is a zero-knowledge broker.
//!
//! Encodings (identical for every device, per the spec):
//! - public key: base64(standard, padded) of the X9.63 uncompressed SEC1 point
//!   (65 bytes, `0x04 ‖ x ‖ y`) — CryptoKit `x963Representation`.
//! - signature: base64(standard, padded) of the raw `r ‖ s` form (64 bytes) —
//!   CryptoKit `ECDSASignature.rawRepresentation`.
//!
//! `p256`'s [`Verifier`] impl hashes the message with SHA-256 internally, which
//! is exactly what CryptoKit's `sign(data:)` does on the phone, so the nonce
//! bytes are passed verbatim and no explicit SHA-256 step is needed here.
//!
//! [`AuthChallenge`]: flightdeck_remote_protocol::RelayFrame::AuthChallenge

use base64::Engine as _;
use p256::ecdsa::{signature::Verifier, Signature, VerifyingKey};

/// Length of the challenge nonce, in bytes (spec §5.1: "32-byte random nonce").
pub const NONCE_LEN: usize = 32;

/// Why an authentication attempt (or a key registration) was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthError {
    /// The registered/presented public key was not valid base64 or not a valid
    /// SEC1 P-256 point.
    BadPublicKey,
    /// The signature was not valid base64 or not a 64-byte `r ‖ s` P-256 sig.
    BadSignature,
    /// The signature did not verify against the public key over the nonce.
    VerificationFailed,
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            AuthError::BadPublicKey => "malformed device public key",
            AuthError::BadSignature => "malformed signature",
            AuthError::VerificationFailed => "signature verification failed",
        };
        f.write_str(s)
    }
}

impl std::error::Error for AuthError {}

fn b64() -> base64::engine::general_purpose::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

/// Decode + validate a device public key without verifying anything. Used at
/// registration time (`pairing_offer` / `pairing_claim`) to reject a garbage key
/// before it is ever stored.
pub fn parse_public_key(public_key_b64: &str) -> Result<VerifyingKey, AuthError> {
    let bytes = b64()
        .decode(public_key_b64)
        .map_err(|_| AuthError::BadPublicKey)?;
    VerifyingKey::from_sec1_bytes(&bytes).map_err(|_| AuthError::BadPublicKey)
}

/// Verify a challenge signature. Returns `Ok(())` iff `signature_b64` is a valid
/// P-256 signature over `nonce` produced by the private key matching
/// `public_key_b64`.
pub fn verify_challenge(
    public_key_b64: &str,
    nonce: &[u8],
    signature_b64: &str,
) -> Result<(), AuthError> {
    let verifying_key = parse_public_key(public_key_b64)?;
    let sig_bytes = b64()
        .decode(signature_b64)
        .map_err(|_| AuthError::BadSignature)?;
    let signature = Signature::from_slice(&sig_bytes).map_err(|_| AuthError::BadSignature)?;
    verifying_key
        .verify(nonce, &signature)
        .map_err(|_| AuthError::VerificationFailed)
}

/// Fill a fresh random challenge nonce. Uses the OS CSPRNG via `getrandom`;
/// panics only if the OS entropy source is unavailable (unrecoverable).
pub fn random_nonce() -> [u8; NONCE_LEN] {
    let mut nonce = [0u8; NONCE_LEN];
    getrandom::getrandom(&mut nonce).expect("OS CSPRNG unavailable");
    nonce
}

/// Base64-encode bytes with the standard padded alphabet (the wire convention
/// for every binary field on the relay plane).
pub fn encode_b64(bytes: &[u8]) -> String {
    b64().encode(bytes)
}

/// Decode a standard padded base64 string, e.g. an `auth_challenge` nonce.
pub fn decode_b64(s: &str) -> Option<Vec<u8>> {
    b64().decode(s).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::ecdsa::{signature::Signer, SigningKey};
    use rand_core::OsRng;

    /// Generate a keypair and return (public_key_b64, signing_key).
    fn keypair() -> (String, SigningKey) {
        let sk = SigningKey::random(&mut OsRng);
        let vk = sk.verifying_key();
        let pk_b64 = encode_b64(vk.to_encoded_point(false).as_bytes());
        (pk_b64, sk)
    }

    fn sign_b64(sk: &SigningKey, msg: &[u8]) -> String {
        let sig: Signature = sk.sign(msg);
        encode_b64(&sig.to_bytes())
    }

    #[test]
    fn good_signature_verifies() {
        let (pk, sk) = keypair();
        let nonce = random_nonce();
        let sig = sign_b64(&sk, &nonce);
        assert_eq!(verify_challenge(&pk, &nonce, &sig), Ok(()));
    }

    #[test]
    fn signature_from_wrong_key_fails() {
        let (pk, _sk) = keypair();
        let (_pk2, sk2) = keypair();
        let nonce = random_nonce();
        let sig = sign_b64(&sk2, &nonce); // signed by a different device
        assert_eq!(
            verify_challenge(&pk, &nonce, &sig),
            Err(AuthError::VerificationFailed)
        );
    }

    #[test]
    fn replayed_nonce_does_not_verify_against_new_challenge() {
        let (pk, sk) = keypair();
        let old_nonce = random_nonce();
        let sig_over_old = sign_b64(&sk, &old_nonce);
        // A fresh challenge nonce; the attacker replays the signature over the
        // *old* nonce. It must not verify against the new one.
        let new_nonce = random_nonce();
        assert_ne!(old_nonce, new_nonce);
        assert_eq!(
            verify_challenge(&pk, &new_nonce, &sig_over_old),
            Err(AuthError::VerificationFailed)
        );
    }

    #[test]
    fn garbage_public_key_is_rejected() {
        let nonce = random_nonce();
        assert_eq!(
            verify_challenge("not base64!!!", &nonce, "AAAA"),
            Err(AuthError::BadPublicKey)
        );
        // Valid base64 but not a SEC1 point.
        assert_eq!(
            verify_challenge(&encode_b64(&[1, 2, 3]), &nonce, "AAAA"),
            Err(AuthError::BadPublicKey)
        );
    }

    #[test]
    fn garbage_signature_is_rejected() {
        let (pk, _sk) = keypair();
        let nonce = random_nonce();
        assert_eq!(
            verify_challenge(&pk, &nonce, "!!!not-base64"),
            Err(AuthError::BadSignature)
        );
        // Right encoding, wrong length.
        assert_eq!(
            verify_challenge(&pk, &nonce, &encode_b64(&[0u8; 10])),
            Err(AuthError::BadSignature)
        );
    }

    #[test]
    fn nonce_is_32_bytes_and_changes() {
        let a = random_nonce();
        let b = random_nonce();
        assert_eq!(a.len(), NONCE_LEN);
        assert_ne!(a, b, "nonces must not repeat");
    }
}
