//! Restart-survival test for the persistent [`SqliteStore`] (remote-control-b0f).
//!
//! The bug this guards against: the relay's state lived only in memory, so every
//! restart/redeploy wiped device registrations, pairings, claim tokens, and
//! per-pairing sequence high-water marks — a previously-paired desktop + phone
//! then failed auth with "unknown device" and hung until re-pairing.
//!
//! This test drives the store through its public trait, then **drops the store
//! (closing the SQLite file) and reopens it at the same path** — the closest
//! in-process analogue of a relay process restart — and asserts every category
//! of state survived, that a claim which expired while the relay was "down" is
//! *not* resurrected, and that the persisted sequence high-water marks still
//! reject a gap after reload.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use flightdeck_relay::claims::ClaimError;
use flightdeck_relay::queue::{AppendOutcome, QueueError, ResumeOutcome};
use flightdeck_relay::store::{RelayStore, SqliteStore};
use flightdeck_remote_protocol::{ApnsEnvironment, DeviceId, EncryptedEnvelope, PairingId, Role};

/// A unique temp path that removes its parent directory on drop, so the test
/// leaves nothing behind and never collides with a parallel run.
struct TempDbPath {
    dir: PathBuf,
    db: PathBuf,
}

impl TempDbPath {
    fn new() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "relay-store-persistence-{}-{}",
            std::process::id(),
            nanos
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let db = dir.join("relay-state.db");
        Self { dir, db }
    }
}

impl Drop for TempDbPath {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Unwrap a clean replay's envelopes, failing the test on a resync signal.
fn replayed(outcome: ResumeOutcome) -> Vec<EncryptedEnvelope> {
    match outcome {
        ResumeOutcome::Replay(v) => v,
        ResumeOutcome::Resync => panic!("expected a clean replay, got Resync"),
    }
}

fn envelope(pairing: &PairingId, sender: Role, seq: u64) -> EncryptedEnvelope {
    EncryptedEnvelope {
        pairing_id: pairing.clone(),
        seq,
        sender,
        sent_at_ms: 1_700_000_000_000 + seq as i64,
        nonce: "bm9uY2U=".into(),
        ciphertext: format!("ciphertext-{sender:?}-{seq}"),
    }
}

#[tokio::test]
async fn sqlite_store_survives_a_restart() {
    let path = TempDbPath::new();

    let desktop = DeviceId::new("dev_mac_1234");
    let phone = DeviceId::new("dev_phone_5678");

    // --- First "boot": open the store and populate every category of state. ---
    let pairing;
    {
        let store = SqliteStore::open(&path.db, 1000).expect("open sqlite store");

        // Device identity + key-agreement public keys.
        store
            .register_device(desktop.clone(), "desktop-pubkey".into())
            .await;
        store
            .register_key_agreement_key(desktop.clone(), "desktop-ka".into())
            .await;
        store
            .register_device(phone.clone(), "phone-pubkey".into())
            .await;

        // Pairing membership + announced machine name.
        pairing = store.create_pairing(desktop.clone()).await;
        let peer = store
            .add_phone_to_pairing(&pairing, phone.clone())
            .await
            .expect("attach phone");
        assert_eq!(peer, desktop);
        store
            .set_machine_name(&pairing, "Ruud's MacBook Pro".into())
            .await;

        // Two claim tokens: one non-expired, one that will be expired by the
        // time we redeem it after "restart".
        store
            .issue_claim(
                "live-token".into(),
                pairing.clone(),
                desktop.clone(),
                9_000_000_000_000,
            )
            .await;
        store
            .issue_claim(
                "expired-token".into(),
                pairing.clone(),
                desktop.clone(),
                1_000, // long past
            )
            .await;

        // Queues in both directions, with the desktop stream partly acked so the
        // ack_cursor (not just the high-water) must persist too.
        for seq in 1..=3 {
            assert_eq!(
                store.enqueue(envelope(&pairing, Role::Desktop, seq)).await,
                Ok(AppendOutcome::Accepted { overflow: false })
            );
        }
        store
            .enqueue(envelope(&pairing, Role::Phone, 1))
            .await
            .expect("enqueue phone 1");
        // Ack desktop up to seq 1: prunes seq 1, leaves 2 and 3, high-water = 3.
        store.ack(&pairing, Role::Desktop, 1).await;

        // Push token.
        store
            .register_push_token(
                pairing.clone(),
                "apns-token".into(),
                ApnsEnvironment::Sandbox,
            )
            .await;

        // Store dropped here → SQLite connection closed (simulated restart).
    }

    // --- Second "boot": reopen the SAME file and assert everything survived. ---
    let store = SqliteStore::open(&path.db, 1000).expect("reopen sqlite store");

    // Device pubkeys + key-agreement key.
    assert_eq!(
        store.device_public_key(&desktop).await,
        Some("desktop-pubkey".into()),
        "desktop identity key must survive restart"
    );
    assert_eq!(
        store.device_key_agreement_key(&desktop).await,
        Some("desktop-ka".into()),
        "desktop KA key must survive restart"
    );
    assert_eq!(
        store.device_public_key(&phone).await,
        Some("phone-pubkey".into()),
        "phone identity key must survive restart"
    );

    // Pairing membership + machine name.
    let members = store
        .pairing_members(&pairing)
        .await
        .expect("pairing must survive restart");
    assert_eq!(members.desktop, desktop);
    assert_eq!(members.phone, Some(phone.clone()));
    assert_eq!(members.machine_name.as_deref(), Some("Ruud's MacBook Pro"));

    // Non-expired claim still redeems; expired one is rejected (not resurrected).
    let redeemed = store
        .redeem_claim("live-token", 2_000_000)
        .await
        .expect("live claim must survive restart and redeem");
    assert_eq!(redeemed.pairing_id, pairing);
    assert_eq!(redeemed.desktop_device, desktop);
    assert_eq!(
        store.redeem_claim("expired-token", 2_000_000).await,
        Err(ClaimError::Expired),
        "a claim that expired while the relay was down must not resurrect"
    );

    // Queue contents + sequence high-water marks survived.
    let desktop_seqs: Vec<u64> = replayed(store.resume(&pairing, Role::Desktop, 0).await)
        .iter()
        .map(|e| e.seq)
        .collect();
    assert_eq!(
        desktop_seqs,
        vec![2, 3],
        "desktop queue must survive with its ack_cursor honored (seq 1 pruned)"
    );
    let phone_seqs: Vec<u64> = replayed(store.resume(&pairing, Role::Phone, 0).await)
        .iter()
        .map(|e| e.seq)
        .collect();
    assert_eq!(phone_seqs, vec![1], "phone queue must survive");

    // The persisted high-water mark (3 for desktop) must still reject a gap and
    // accept the true next seq — this is the state whose loss caused the seq
    // divergence bug (remote-control-bbf) after an in-memory relay restart.
    assert_eq!(
        store.enqueue(envelope(&pairing, Role::Desktop, 5)).await,
        Err(QueueError::SeqViolation {
            expected: 4,
            got: 5
        }),
        "high-water mark must survive so a post-restart gap is still rejected"
    );
    assert_eq!(
        store.enqueue(envelope(&pairing, Role::Desktop, 4)).await,
        Ok(AppendOutcome::Accepted { overflow: false }),
        "the true next seq (high_water + 1) must be accepted after restart"
    );

    // Push token survived.
    assert_eq!(
        store.push_token(&pairing).await,
        Some(("apns-token".into(), ApnsEnvironment::Sandbox)),
        "push token must survive restart"
    );

    // The persisted pairing counter must keep advancing (no id reuse) across the
    // restart: the next pairing id is numbered above the first.
    let second = store.create_pairing(desktop.clone()).await;
    assert_ne!(second, pairing);
    assert!(
        second.as_str().starts_with("pair_0002_"),
        "pairing counter must persist and advance across restart, got {second}"
    );
}
