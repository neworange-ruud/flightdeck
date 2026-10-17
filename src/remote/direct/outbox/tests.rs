//! Outbox unit tests: bounds, ack, replay, the two `seq_violation` cases, the
//! healed overflow gap, epoch restart, inbound dedup, and persistence (atomic
//! document, debounced writes, the seq reservation that survives a crash).

use super::*;
use crate::testing::FakeFs;
use flightdeck_remote_protocol::PairingId;

fn env(seq: u64) -> EncryptedEnvelope {
    env_sized(seq, 8)
}

fn env_sized(seq: u64, payload: usize) -> EncryptedEnvelope {
    EncryptedEnvelope {
        pairing_id: PairingId::new("pair_d"),
        seq,
        sender: Role::Desktop,
        sent_at_ms: 1_000 + seq as i64,
        nonce: "NNNN".to_string(),
        ciphertext: "c".repeat(payload),
    }
}

fn seqs(envs: &[EncryptedEnvelope]) -> Vec<u64> {
    envs.iter().map(|e| e.seq).collect()
}

fn small(max_envelopes: usize) -> OutboxLimits {
    OutboxLimits {
        max_envelopes,
        max_bytes: usize::MAX,
    }
}

#[test]
fn default_limits_are_the_spec_bounds() {
    let l = OutboxLimits::default();
    assert_eq!(l.max_envelopes, 1000);
    assert_eq!(l.max_bytes, 8 * 1024 * 1024);
}

#[test]
fn count_bound_drops_the_oldest_first() {
    let mut o = Outbox::new(small(3));
    for s in 1..=5 {
        o.push(env(s));
    }
    assert_eq!(o.len(), 3);
    assert_eq!(o.front_seq(), Some(3));
    assert_eq!(o.high_water(), 5);
    assert_eq!(seqs(&o.after(0)), vec![3, 4, 5]);
}

#[test]
fn byte_bound_drops_the_oldest_first_but_keeps_the_newest() {
    // Each envelope costs 4 (nonce) + 96 = 100 bytes; room for two.
    let mut o = Outbox::new(OutboxLimits {
        max_envelopes: 1000,
        max_bytes: 250,
    });
    for s in 1..=4 {
        let out = o.push(env_sized(s, 96));
        if s > 2 {
            assert_eq!(out.dropped, 1, "push {s} sheds exactly one");
        }
    }
    assert_eq!(seqs(&o.after(0)), vec![3, 4]);
    assert_eq!(o.bytes(), 200);
    // A single envelope larger than the whole bound still goes in, alone.
    o.push(env_sized(5, 10_000));
    assert_eq!(seqs(&o.after(0)), vec![5]);
}

#[test]
fn the_full_default_byte_bound_holds() {
    let mut o = Outbox::new(OutboxLimits::default());
    // 100 KiB each: 8 MiB holds 81 of them (cost includes the 4-byte nonce).
    for s in 1..=200 {
        o.push(env_sized(s, 100 * 1024));
    }
    assert!(o.bytes() <= MAX_BYTES);
    assert_eq!(o.high_water(), 200);
    assert_eq!(o.after(0).last().map(|e| e.seq), Some(200));
    assert!(o.len() < 200);
}

#[test]
fn ack_trims_cumulatively_and_never_regresses() {
    let mut o = Outbox::new(OutboxLimits::default());
    for s in 1..=5 {
        o.push(env(s));
    }
    assert!(o.ack(3));
    assert_eq!(seqs(&o.after(0)), vec![4, 5]);
    assert!(!o.ack(2), "a stale ack is a no-op");
    assert_eq!(o.ack_cursor(), 3);
    assert!(o.ack(99), "a cursor past the high-water mark clamps");
    assert_eq!(o.ack_cursor(), 5);
    assert!(o.is_empty());
}

#[test]
fn resume_replays_strictly_above_from_seq() {
    let mut o = Outbox::new(OutboxLimits::default());
    for s in 1..=4 {
        o.push(env(s));
    }
    assert_eq!(o.resume(0), ResumeOutcome::Replay(o.after(0)));
    match o.resume(2) {
        ResumeOutcome::Replay(r) => assert_eq!(seqs(&r), vec![3, 4]),
        other => panic!("{other:?}"),
    }
    assert_eq!(o.resume(4), ResumeOutcome::Replay(vec![]));
}

#[test]
fn resume_after_acks_is_a_clean_replay_not_a_gap() {
    // Acks advance the front contiguously; resuming below an acked front is
    // not an overflow hole (the relay's `SenderQueue` rule).
    let mut o = Outbox::new(OutboxLimits::default());
    for s in 1..=5 {
        o.push(env(s));
    }
    o.ack(3);
    match o.resume(1) {
        ResumeOutcome::Replay(r) => assert_eq!(seqs(&r), vec![4, 5]),
        other => panic!("{other:?}"),
    }
}

#[test]
fn resume_below_an_overflowed_front_is_a_seq_violation_then_heals() {
    let mut o = Outbox::new(small(3));
    for s in 1..=5 {
        o.push(env(s));
    }
    // Retained 3,4,5; 1 and 2 were shed un-acked.
    match o.resume(2) {
        ResumeOutcome::Replay(r) => assert_eq!(seqs(&r), vec![3, 4, 5]),
        other => panic!("seq 3 is the retained front → clean replay, got {other:?}"),
    }
    assert_eq!(o.resume(1), ResumeOutcome::Resync, "needs shed seq 2");
    // The phone's §6.3 recovery is `resume { from_seq: 0 }`; it must now be
    // served, not answered with another seq_violation forever.
    match o.resume(0) {
        ResumeOutcome::Replay(r) => assert_eq!(seqs(&r), vec![3, 4, 5]),
        other => panic!("the healed gap must replay the retained tail, got {other:?}"),
    }
}

#[test]
fn resume_above_the_high_water_mark_is_a_seq_violation() {
    let mut o = Outbox::new(OutboxLimits::default());
    o.push(env(1));
    o.push(env(2));
    assert_eq!(o.resume(7), ResumeOutcome::Resync);
    // A stream that has never emitted anything has nothing to contradict.
    let mut fresh = Outbox::new(OutboxLimits::default());
    assert_eq!(fresh.resume(7), ResumeOutcome::Replay(vec![]));
}

#[test]
fn a_rewound_seq_restarts_the_epoch() {
    let mut o = Outbox::new(OutboxLimits::default());
    for s in 1..=4 {
        o.push(env(s));
    }
    let out = o.push(env(1));
    assert!(out.restarted);
    assert_eq!(seqs(&o.after(0)), vec![1]);
    assert_eq!(o.high_water(), 1);
    assert_eq!(o.ack_cursor(), 0);
}

#[test]
fn inbound_dedup_applies_each_seq_once_and_accepts_a_restart() {
    // The relay client's rule verbatim (spec §6.4). Note its edge: seq 1 over
    // a held cursor — even a cursor of exactly 1 — counts as a restart, so the
    // duplicates below start from seq 2 (a repeated seq 1 is caught one layer
    // up, by the command-id ledger).
    let mut o = Outbox::new(OutboxLimits::default());
    assert!(o.accept_inbound(1));
    assert!(o.accept_inbound(2));
    assert!(!o.accept_inbound(2), "duplicate");
    assert!(o.accept_inbound(3));
    assert!(!o.accept_inbound(3), "duplicate");
    assert!(!o.accept_inbound(2), "an old seq is a duplicate…");
    assert!(o.accept_inbound(5), "a forward jump is accepted");
    assert!(
        o.accept_inbound(1),
        "…but seq 1 over a held cursor is a restart"
    );
    assert_eq!(o.last_received(), 1);
}

// --- persistence ------------------------------------------------------------

fn fs() -> Arc<dyn FileSystem + Send + Sync> {
    Arc::new(FakeFs::new())
}

const ROOT: &str = "/home/u/.flightdeck/remote-direct";

#[test]
fn pairing_dirs_are_escaped_and_injective() {
    let root = Path::new(ROOT);
    assert_eq!(pairing_dir(root, "pair_ab-1"), root.join("pair_5fab-1"));
    assert_eq!(pairing_dir(root, "../x"), root.join("_2e_2e_2fx"));
    assert_ne!(pairing_dir(root, "a_2f"), pairing_dir(root, "a/"));
    assert_eq!(pairing_dir(root, ""), root.join("_empty"));
}

#[test]
fn a_clean_flush_round_trips_without_a_seq_jump() {
    let fs = fs();
    let root = Path::new(ROOT);
    let mut f = OutboxFile::open(fs.clone(), root, "pair_d", OutboxLimits::default());
    for s in 1..=3 {
        f.push(env(s));
    }
    f.ack(1);
    f.accept_inbound(4);
    f.flush_clean().unwrap();

    let g = OutboxFile::open(fs, root, "pair_d", OutboxLimits::default());
    assert_eq!(
        g.outbox().high_water(),
        3,
        "an orderly restart continues the stream"
    );
    assert_eq!(g.outbox().ack_cursor(), 1);
    assert_eq!(g.outbox().last_received(), 4);
    assert_eq!(seqs(&g.outbox().after(0)), vec![2, 3]);
    assert!(!g.is_dirty());
}

#[test]
fn a_crash_after_a_debounced_flush_numbers_past_anything_sent() {
    let fs = fs();
    let root = Path::new(ROOT);
    let mut f = OutboxFile::open(fs.clone(), root, "pair_d", OutboxLimits::default());
    f.push(env(1)); // passes the (empty) reservation → written through
    f.push(env(2));
    f.flush().unwrap(); // debounced flush: reserves ahead
    f.push(env(3)); // within the reservation: not written yet
    assert!(f.is_dirty());
    drop(f); // "crash": envelope 3 never reached disk

    let g = OutboxFile::open(fs, root, "pair_d", OutboxLimits::default());
    assert_eq!(seqs(&g.outbox().after(0)), vec![1, 2]);
    assert!(
        g.outbox().high_water() >= 3 + SEQ_RESERVE - 1,
        "the floor must clear every seq the dead process could have used, got {}",
        g.outbox().high_water()
    );
}

#[test]
fn a_push_past_the_reservation_is_written_through_immediately() {
    let fs = fs();
    let root = Path::new(ROOT);
    let mut f = OutboxFile::open(fs.clone(), root, "pair_d", OutboxLimits::default());
    f.push(env(1));
    assert!(
        !f.is_dirty(),
        "the first push passes the empty reservation and is written"
    );
    for s in 2..=SEQ_RESERVE + 1 {
        f.push(env(s));
    }
    assert!(f.is_dirty());
    // seq SEQ_RESERVE + 2 passes the reservation made by the push of seq 1.
    f.push(env(SEQ_RESERVE + 2));
    assert!(!f.is_dirty());
    drop(f);
    let g = OutboxFile::open(fs, root, "pair_d", OutboxLimits::default());
    assert_eq!(
        g.outbox().after(0).last().map(|e| e.seq),
        Some(SEQ_RESERVE + 2)
    );
}

#[test]
fn a_corrupt_or_foreign_file_starts_empty() {
    let fake = FakeFs::new();
    let root = Path::new(ROOT);
    let dir = pairing_dir(root, "pair_d");
    let fake = fake.with_file(dir.join("outbox.json").to_str().unwrap(), "{ not json");
    let fs: Arc<dyn FileSystem + Send + Sync> = Arc::new(fake);
    let f = OutboxFile::open(fs.clone(), root, "pair_d", OutboxLimits::default());
    assert_eq!(f.outbox().high_water(), 0);
    assert!(f.outbox().is_empty());

    // A document recorded for a different pairing is not this one's.
    let mut other = OutboxFile::open(fs.clone(), root, "pair_other", OutboxLimits::default());
    other.push(env(9));
    other.flush_clean().unwrap();
    let raw = fs
        .read_to_string(&pairing_dir(root, "pair_other").join("outbox.json"))
        .unwrap();
    fs.write(&dir.join("outbox.json"), &raw).unwrap();
    let f = OutboxFile::open(fs, root, "pair_d", OutboxLimits::default());
    assert_eq!(f.outbox().high_water(), 0);
}

#[test]
fn load_reapplies_the_bounds() {
    let fs = fs();
    let root = Path::new(ROOT);
    let mut f = OutboxFile::open(fs.clone(), root, "pair_d", OutboxLimits::default());
    for s in 1..=10 {
        f.push(env(s));
    }
    f.flush_clean().unwrap();
    let g = OutboxFile::open(fs, root, "pair_d", small(4));
    assert_eq!(seqs(&g.outbox().after(0)), vec![7, 8, 9, 10]);
    assert_eq!(g.outbox().high_water(), 10);
}

#[test]
fn delete_removes_the_pairing_directory() {
    let fs = fs();
    let root = Path::new(ROOT);
    let mut f = OutboxFile::open(fs.clone(), root, "pair_d", OutboxLimits::default());
    f.push(env(1));
    assert!(fs.exists(&pairing_dir(root, "pair_d")));
    f.delete().unwrap();
    assert!(!fs.exists(&pairing_dir(root, "pair_d")));
    let g = OutboxFile::open(fs, root, "pair_d", OutboxLimits::default());
    assert_eq!(g.outbox().high_water(), 0);
}
