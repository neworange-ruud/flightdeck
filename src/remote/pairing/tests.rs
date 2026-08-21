//! Tests for the desktop pairing state machine, QR payload/art, and the
//! reconciled salt contract (identical channels on both pairing paths).

use super::*;
use p256::elliptic_curve::sec1::ToEncodedPoint;
use p256::SecretKey;

// Fixed, test-only P-256 scalars (trivially valid: small, below the group
// order). One is the desktop identity+KA key, the other the phone's KA key.
const DESKTOP_SCALAR: [u8; 32] = [
    0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff, 0x01,
    0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10, 0x11,
];
const PHONE_SCALAR: [u8; 32] = [
    0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x2b, 0x2c, 0x2d, 0x2e, 0x2f, 0x30,
    0x31, 0x32, 0x33, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x3b, 0x3c, 0x3d, 0x3e, 0x3f, 0x40,
];

fn public_x963(scalar: &[u8; 32]) -> Vec<u8> {
    SecretKey::from_slice(scalar)
        .expect("valid scalar")
        .public_key()
        .to_encoded_point(false)
        .as_bytes()
        .to_vec()
}

// ── state machine transitions ───────────────────────────────────────────────

#[test]
fn happy_path_transitions_idle_offering_displaying_established() {
    let mut s = PairingSession::begin("wss://relay.example/v1");
    assert!(matches!(s.phase(), PairingPhase::Offering));
    assert_eq!(s.hint().len(), 4, "hint is a 4-digit code");
    assert!(s.hint().chars().all(|c| c.is_ascii_digit()));

    let pairing = PairingId::new("pair_1");
    s.on_offered(pairing.clone(), "4729".to_string(), 120_000);
    match s.phase() {
        PairingPhase::Displaying {
            code,
            qr_payload,
            expires_at_ms,
        } => {
            assert_eq!(code, "4729");
            assert!(qr_payload.starts_with("fdr1:"));
            assert_eq!(*expires_at_ms, 120_000);
        }
        other => panic!("expected Displaying, got {other:?}"),
    }

    let peer_ka = STANDARD.encode(public_x963(&PHONE_SCALAR));
    let became = s.on_claimed(pairing.clone(), Some(peer_ka));
    assert!(became, "claim with a peer KA key establishes the pairing");
    assert!(s.is_established());
    assert!(matches!(s.phase(), PairingPhase::Established { .. }));
}

#[test]
fn offer_ok_ignored_unless_offering() {
    let mut s = PairingSession::begin("wss://r/v1");
    let pairing = PairingId::new("pair_1");
    s.on_offered(pairing.clone(), "1111".to_string(), 1);
    // A second (stray) offer_ok while displaying is a no-op.
    s.on_offered(pairing, "2222".to_string(), 2);
    match s.phase() {
        PairingPhase::Displaying { code, .. } => assert_eq!(code, "1111"),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn claim_without_peer_key_fails() {
    let mut s = PairingSession::begin("wss://r/v1");
    let pairing = PairingId::new("pair_1");
    s.on_offered(pairing.clone(), "4729".to_string(), 120_000);
    let became = s.on_claimed(pairing, None);
    assert!(!became);
    assert!(matches!(s.phase(), PairingPhase::Failed { .. }));
}

#[test]
fn claim_for_other_pairing_is_ignored() {
    let mut s = PairingSession::begin("wss://r/v1");
    s.on_offered(PairingId::new("pair_1"), "4729".to_string(), 120_000);
    let peer_ka = STANDARD.encode(public_x963(&PHONE_SCALAR));
    let became = s.on_claimed(PairingId::new("pair_other"), Some(peer_ka));
    assert!(!became);
    assert!(matches!(s.phase(), PairingPhase::Displaying { .. }));
}

#[test]
fn seconds_remaining_saturates_at_zero() {
    let mut s = PairingSession::begin("wss://r/v1");
    s.on_offered(PairingId::new("pair_1"), "4729".to_string(), 10_000);
    assert_eq!(s.seconds_remaining(0), Some(10));
    assert_eq!(s.seconds_remaining(9_000), Some(1));
    assert_eq!(s.seconds_remaining(20_000), Some(0));
}

// ── QR payload format (matches ios/.../PairingModels.swift) ──────────────────

#[test]
fn qr_payload_matches_ios_format_byte_for_byte() {
    let payload = build_qr_payload(
        "4729",
        "cGFpcmluZy1zZWNyZXQtMzItYnl0ZXMtZXhhY3RseQ",
        "wss://relay.example/v1",
    );
    // Prefix is plain ASCII so a scanner rejects non-FlightDeck codes instantly.
    let b64 = payload.strip_prefix("fdr1:").expect("fdr1: prefix");
    // The body is base64url (no padding) of the exact snake_case JSON, keys in
    // declaration order — the cross-language contract the iOS decoder consumes.
    let decoded = URL_SAFE_NO_PAD.decode(b64).expect("valid base64url");
    let expected = concat!(
        "{\"claim_token\":\"4729\",",
        "\"pairing_secret\":\"cGFpcmluZy1zZWNyZXQtMzItYnl0ZXMtZXhhY3RseQ\",",
        "\"relay_url\":\"wss://relay.example/v1\"}"
    );
    assert_eq!(String::from_utf8(decoded).unwrap(), expected);
    // No '+' or '/' in the outer encoding (url-safe alphabet).
    assert!(!b64.contains('+') && !b64.contains('/') && !b64.contains('='));
}

#[test]
fn begin_produces_a_scannable_qr_payload() {
    let mut s = PairingSession::begin("wss://relay.example/v1");
    s.on_offered(PairingId::new("pair_1"), "4729".to_string(), 120_000);
    let PairingPhase::Displaying { qr_payload, .. } = s.phase() else {
        panic!("expected Displaying");
    };
    assert!(
        qr_art(qr_payload).is_some(),
        "the payload must encode as a QR"
    );
}

// ── QR half-block art ────────────────────────────────────────────────────────

#[test]
fn qr_art_has_consistent_square_dimensions() {
    let art = qr_art("fdr1:hello-world-known-payload").expect("encodes");
    // Two vertical modules per text row → rows == ceil(width / 2).
    assert_eq!(art.rows.len(), art.width.div_ceil(2));
    // Every row is exactly `width` cells wide (padded square incl. quiet zone).
    for row in &art.rows {
        assert_eq!(row.chars().count(), art.width);
    }
    // Only the four half-block glyphs (plus space) appear.
    for row in &art.rows {
        for ch in row.chars() {
            assert!(
                matches!(ch, ' ' | '▀' | '▄' | '█'),
                "unexpected glyph {ch:?}"
            );
        }
    }
}

// ── salt contract: identical channels on both pairing paths ──────────────────

#[test]
fn desktop_and_phone_derive_identical_channels_from_claim_token_salt() {
    // The reconciled contract: salt = claim_token bytes on BOTH paths. Here the
    // desktop derives via the pairing session's `build_channel`; a simulated
    // phone derives with the same salt and Role::Phone. If the salt contract
    // holds, the two round-trip in both directions.
    let pairing_id = "pair_salt";
    let claim_token = "4729";

    let phone_ka_b64 = STANDARD.encode(public_x963(&PHONE_SCALAR));
    let (seal, open) =
        build_channel(&DESKTOP_SCALAR, &phone_ka_b64, pairing_id, claim_token).expect("derive");

    // Simulated phone endpoint: its peer is the desktop's identity/KA key.
    let phone = E2eChannel::derive(
        &PHONE_SCALAR,
        &public_x963(&DESKTOP_SCALAR),
        pairing_id,
        claim_token.as_bytes(),
        Role::Phone,
    )
    .expect("phone derive");

    // desktop → phone
    let (nonce, ct) = seal(br#"{"type":"snapshot"}"#, 1, 1_752_412_802_000).expect("seal d2p");
    let opened = phone
        .open(1, Role::Desktop, 1_752_412_802_000, &nonce, &ct)
        .expect("phone opens d2p");
    assert_eq!(opened, br#"{"type":"snapshot"}"#);

    // phone → desktop
    let (n2, c2) = phone
        .seal(br#"{"type":"reply"}"#, 1, 1_752_412_900_000)
        .expect("seal p2d");
    let opened2 = open(1, Role::Phone, 1_752_412_900_000, &n2, &c2).expect("desktop opens p2d");
    assert_eq!(opened2, br#"{"type":"reply"}"#);
}

#[test]
fn wrong_claim_token_salt_cannot_open() {
    let pairing_id = "pair_salt";
    let phone_ka_b64 = STANDARD.encode(public_x963(&PHONE_SCALAR));
    let (seal, _open) =
        build_channel(&DESKTOP_SCALAR, &phone_ka_b64, pairing_id, "4729").expect("derive");

    // Phone derived with a DIFFERENT claim token (salt) → different keys.
    let phone = E2eChannel::derive(
        &PHONE_SCALAR,
        &public_x963(&DESKTOP_SCALAR),
        pairing_id,
        b"9999",
        Role::Phone,
    )
    .expect("phone derive");

    let (nonce, ct) = seal(b"secret", 1, 1).expect("seal");
    assert!(phone.open(1, Role::Desktop, 1, &nonce, &ct).is_err());
}

// ── 4-digit code generation ─────────────────────────────────────────────────

#[test]
fn four_digit_code_is_always_four_ascii_digits() {
    for _ in 0..2_000 {
        let code = four_digit_code();
        assert_eq!(code.len(), 4);
        assert!(code.chars().all(|c| c.is_ascii_digit()));
    }
}
