//! Server-side pending-event queue with gapless sequencing, cumulative-ack
//! pruning, resume-replay, and a bounded drop-oldest overflow policy.
//!
//! This is the "never lose an event, never send blind" machinery of spec §6,
//! from the relay's side. One [`SenderQueue`] holds the outbound envelopes of a
//! single `(pairing_id, sender_role)` stream:
//!
//! - **Gapless monotonic seq (§6.1).** Envelopes carry a per-stream `seq`
//!   starting at 1. The relay accepts `seq == high_water + 1`, tolerates a
//!   re-send of the current high-water `seq` as an idempotent no-op (reconnect
//!   races), and rejects anything else as a protocol error (see the §6 v1
//!   amendment in `specs/REMOTE_PROTOCOL.md`).
//! - **Hold while offline / un-acked.** Accepted envelopes are buffered so a
//!   peer that reconnects can [`SenderQueue::replay`] them.
//! - **Cumulative ack (§6.2).** [`SenderQueue::ack`] prunes everything `<=
//!   cursor`.
//! - **Bounded, drop-oldest (§6 amendment).** At most `max_len` un-acked
//!   envelopes are held; a push past the bound drops the oldest and flags
//!   overflow so the caller can emit an advisory `rate_limited` error.
//!
//! The buffer never inspects `ciphertext` — it stores whole [`EncryptedEnvelope`]
//! values opaquely and hands them back verbatim.

use std::collections::VecDeque;

use flightdeck_remote_protocol::EncryptedEnvelope;

/// Why an inbound envelope was refused by a [`SenderQueue`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueError {
    /// `seq` regressed below, or skipped past, the expected next value. The
    /// stream's gapless invariant would be broken; the envelope is dropped and
    /// the sender should receive a `bad_frame` error.
    SeqViolation {
        /// The `seq` the relay expected (`high_water + 1`).
        expected: u64,
        /// The `seq` that actually arrived.
        got: u64,
    },
}

/// Result of servicing a `resume { from_seq }` against a [`SenderQueue`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumeOutcome {
    /// The buffered envelopes with `seq > from_seq`, in order (possibly empty).
    /// Delivering these leaves no hole: `from_seq + 1` is either the oldest
    /// retained envelope or the receiver is already caught up.
    Replay(Vec<EncryptedEnvelope>),
    /// The receiver's next-needed seq (`from_seq + 1`) falls **below** the oldest
    /// seq still buffered: earlier envelopes were shed by drop-oldest overflow
    /// (see [`AppendOutcome::Accepted`]'s `overflow`) and are gone for good.
    /// Replaying would hand the receiver a stream with a hole, which its gapless
    /// enforcement rejects — the receiver stalls forever. The caller must instead
    /// signal a resync so the receiver abandons its stale cursor and requests a
    /// fresh snapshot (remote-control-0ef.7).
    Resync,
}

/// Result of accepting an envelope into a [`SenderQueue`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppendOutcome {
    /// The envelope advanced the stream by one and was buffered. `overflow` is
    /// true if the bound was hit and the oldest buffered envelope was dropped to
    /// make room.
    Accepted {
        /// Whether a drop-oldest eviction occurred on this push.
        overflow: bool,
    },
    /// The envelope re-sent the current high-water `seq`; ignored as an
    /// idempotent no-op. Nothing was buffered or dropped.
    Duplicate,
}

/// The buffered outbound envelopes of one `(pairing, sender)` stream.
#[derive(Debug)]
pub struct SenderQueue {
    /// Highest `seq` accepted so far (0 before the first envelope).
    high_water: u64,
    /// Highest `seq` acknowledged (pruned) by the receiving peer.
    ack_cursor: u64,
    /// Buffered envelopes, ascending by `seq`, all with `seq > ack_cursor`.
    buf: VecDeque<EncryptedEnvelope>,
    /// Maximum number of un-acked envelopes to retain.
    max_len: usize,
}

impl SenderQueue {
    /// Create an empty queue bounded to `max_len` un-acked envelopes. `max_len`
    /// is clamped to at least 1.
    pub fn new(max_len: usize) -> Self {
        Self {
            high_water: 0,
            ack_cursor: 0,
            buf: VecDeque::new(),
            max_len: max_len.max(1),
        }
    }

    /// Rehydrate a queue from a persisted snapshot so a durable [`RelayStore`]
    /// can reuse this type's canonical append/resume/ack logic instead of
    /// re-expressing it in SQL — the two would otherwise drift
    /// (remote-control-tvc). `buffer` must be the retained (un-acked, un-dropped)
    /// envelopes in ascending-`seq` order, exactly as [`Self::append`] /
    /// [`Self::ack`] leave the internal buffer; `high_water` and `ack_cursor` are
    /// the stream's persisted cursors. `max_len` is clamped to at least 1, as in
    /// [`Self::new`].
    ///
    /// [`RelayStore`]: crate::store::RelayStore
    pub fn from_snapshot(
        high_water: u64,
        ack_cursor: u64,
        buffer: Vec<EncryptedEnvelope>,
        max_len: usize,
    ) -> Self {
        Self {
            high_water,
            ack_cursor,
            buf: VecDeque::from(buffer),
            max_len: max_len.max(1),
        }
    }

    /// Highest `seq` accepted so far (the stream's high-water mark).
    pub fn high_water(&self) -> u64 {
        self.high_water
    }

    /// Highest contiguous `seq` acknowledged by the peer.
    pub fn ack_cursor(&self) -> u64 {
        self.ack_cursor
    }

    /// Number of envelopes currently buffered.
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    /// Whether the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Accept (or reject / dedup) an inbound envelope by its `seq`.
    pub fn append(&mut self, env: EncryptedEnvelope) -> Result<AppendOutcome, QueueError> {
        let expected = self.high_water + 1;
        if env.seq == expected {
            let overflow = self.buf.len() >= self.max_len;
            if overflow {
                // Drop the oldest un-acked envelope to make room. The dropped
                // envelope is not counted as acked; recovery is the sender's
                // responsibility (spec §6 amendment).
                self.buf.pop_front();
            }
            self.high_water = env.seq;
            self.buf.push_back(env);
            Ok(AppendOutcome::Accepted { overflow })
        } else if self.high_water > 0 && env.seq == self.high_water {
            // Idempotent re-send of the current head; tolerate silently.
            Ok(AppendOutcome::Duplicate)
        } else {
            Err(QueueError::SeqViolation {
                expected,
                got: env.seq,
            })
        }
    }

    /// The retained (un-acked, un-dropped) envelopes in ascending-`seq` order —
    /// the buffer half of the snapshot a durable store persists after a canonical
    /// mutation (remote-control-tvc). Pairs with [`Self::from_snapshot`],
    /// [`Self::high_water`], and [`Self::ack_cursor`].
    pub fn buffered(&self) -> impl Iterator<Item = &EncryptedEnvelope> + '_ {
        self.buf.iter()
    }

    /// Return, in order, every buffered envelope with `seq > from_seq`. Used to
    /// service a `resume { from_seq }`. Does not mutate the queue — replay is
    /// idempotent, so a client may resume repeatedly (double-resume yields the
    /// same set, and yields nothing once `from_seq` has caught up).
    pub fn replay(&self, from_seq: u64) -> Vec<EncryptedEnvelope> {
        self.buf
            .iter()
            .filter(|e| e.seq > from_seq)
            .cloned()
            .collect()
    }

    /// Service a `resume { from_seq }`, distinguishing a clean replay from a
    /// drop-induced gap that requires a resync (remote-control-0ef.7).
    ///
    /// Drop-oldest overflow ([`Self::append`]) sheds the lowest un-acked
    /// envelopes **without** advancing [`Self::ack_cursor`], so the buffer's
    /// front `seq` can sit strictly above `ack_cursor + 1`. A receiver that
    /// resumes from a `from_seq` older than that front is asking for envelopes
    /// the relay no longer holds; those seqs will never arrive. Rather than
    /// [`Self::replay`] a hole the receiver stalls on, return
    /// [`ResumeOutcome::Resync`] so the caller can tell the receiver to request
    /// a fresh snapshot.
    ///
    /// **Recovery path.** The session maps `Resync` onto the same
    /// `SeqViolation` advisory the enqueue path already uses: the receiver
    /// zeroes its cursor for this pairing and asks its peer for a fresh
    /// snapshot (restarting the peer's stream), so no new wire frame is needed.
    pub fn resume(&self, from_seq: u64) -> ResumeOutcome {
        if let Some(front) = self.buf.front() {
            // The front can sit above `ack_cursor + 1` in exactly one case:
            // drop-oldest overflow shed un-acked seqs (see [`Self::append`]).
            // Cumulative ack ([`Self::ack`]) also advances the front, but
            // *contiguously* (front becomes `ack_cursor + 1`), and those seqs
            // were delivered and acknowledged — not lost. So a hole that forces
            // a resync exists only when BOTH hold: the front is above the ack
            // watermark's successor (an overflow drop happened) AND the receiver
            // is asking for a seq below that front. Without the overflow guard,
            // a plain `ack`-pruned resume (`from_seq` below an acked front) would
            // be misread as a gap.
            let overflow_gap = front.seq > self.ack_cursor + 1;
            if overflow_gap && from_seq + 1 < front.seq {
                return ResumeOutcome::Resync;
            }
        }
        ResumeOutcome::Replay(self.replay(from_seq))
    }

    /// Prune every buffered envelope with `seq <= cursor` (cumulative ack,
    /// §6.2). A `cursor` at or below the current ack point is a no-op; a cursor
    /// beyond `high_water` is clamped.
    pub fn ack(&mut self, cursor: u64) {
        let cursor = cursor.min(self.high_water);
        if cursor <= self.ack_cursor {
            return;
        }
        self.ack_cursor = cursor;
        while let Some(front) = self.buf.front() {
            if front.seq <= cursor {
                self.buf.pop_front();
            } else {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flightdeck_remote_protocol::{PairingId, Role};

    fn env(seq: u64) -> EncryptedEnvelope {
        EncryptedEnvelope {
            pairing_id: PairingId::new("pair_test"),
            seq,
            sender: Role::Desktop,
            sent_at_ms: 1000 + seq as i64,
            nonce: "bm9uY2U=".into(),
            // Intentionally not valid base64/ciphertext in some tests — the
            // queue must never care.
            ciphertext: format!("ciphertext-{seq}"),
        }
    }

    #[test]
    fn accepts_gapless_sequence_from_one() {
        let mut q = SenderQueue::new(100);
        for seq in 1..=5 {
            assert_eq!(
                q.append(env(seq)),
                Ok(AppendOutcome::Accepted { overflow: false })
            );
        }
        assert_eq!(q.high_water(), 5);
        assert_eq!(q.len(), 5);
    }

    #[test]
    fn rejects_gap() {
        let mut q = SenderQueue::new(100);
        assert!(q.append(env(1)).is_ok());
        assert_eq!(
            q.append(env(3)),
            Err(QueueError::SeqViolation {
                expected: 2,
                got: 3
            })
        );
        // High-water unchanged after a rejected gap.
        assert_eq!(q.high_water(), 1);
    }

    #[test]
    fn rejects_regression() {
        let mut q = SenderQueue::new(100);
        assert!(q.append(env(1)).is_ok());
        assert!(q.append(env(2)).is_ok());
        assert_eq!(
            q.append(env(1)),
            Err(QueueError::SeqViolation {
                expected: 3,
                got: 1
            })
        );
    }

    #[test]
    fn tolerates_duplicate_of_current_head() {
        let mut q = SenderQueue::new(100);
        assert!(q.append(env(1)).is_ok());
        assert!(q.append(env(2)).is_ok());
        assert_eq!(q.append(env(2)), Ok(AppendOutcome::Duplicate));
        assert_eq!(q.high_water(), 2);
        assert_eq!(q.len(), 2);
    }

    #[test]
    fn first_envelope_must_be_one() {
        let mut q = SenderQueue::new(100);
        assert_eq!(
            q.append(env(5)),
            Err(QueueError::SeqViolation {
                expected: 1,
                got: 5
            })
        );
    }

    #[test]
    fn replay_returns_strictly_above_from_seq() {
        let mut q = SenderQueue::new(100);
        for seq in 1..=5 {
            q.append(env(seq)).unwrap();
        }
        let replayed = q.replay(2);
        assert_eq!(
            replayed.iter().map(|e| e.seq).collect::<Vec<_>>(),
            vec![3, 4, 5]
        );
        // Double-resume from the same point is identical (idempotent).
        assert_eq!(q.replay(2).len(), 3);
        // Resuming from the head yields nothing.
        assert!(q.replay(5).is_empty());
    }

    #[test]
    fn ack_prunes_cumulatively() {
        let mut q = SenderQueue::new(100);
        for seq in 1..=5 {
            q.append(env(seq)).unwrap();
        }
        q.ack(3);
        assert_eq!(q.ack_cursor(), 3);
        assert_eq!(
            q.replay(0).iter().map(|e| e.seq).collect::<Vec<_>>(),
            vec![4, 5]
        );
        // Older ack is a no-op.
        q.ack(1);
        assert_eq!(q.ack_cursor(), 3);
        // Ack beyond high-water clamps.
        q.ack(999);
        assert_eq!(q.ack_cursor(), 5);
        assert!(q.is_empty());
    }

    #[test]
    fn overflow_drops_oldest() {
        let mut q = SenderQueue::new(3);
        assert_eq!(
            q.append(env(1)),
            Ok(AppendOutcome::Accepted { overflow: false })
        );
        assert_eq!(
            q.append(env(2)),
            Ok(AppendOutcome::Accepted { overflow: false })
        );
        assert_eq!(
            q.append(env(3)),
            Ok(AppendOutcome::Accepted { overflow: false })
        );
        // Fourth push exceeds the bound: oldest (seq 1) is evicted.
        assert_eq!(
            q.append(env(4)),
            Ok(AppendOutcome::Accepted { overflow: true })
        );
        assert_eq!(q.len(), 3);
        let seqs: Vec<u64> = q.replay(0).iter().map(|e| e.seq).collect();
        assert_eq!(seqs, vec![2, 3, 4], "oldest dropped, newest retained");
        // Sequencing continues gaplessly despite the drop.
        assert_eq!(q.high_water(), 4);
    }

    #[test]
    fn resume_replays_when_no_gap() {
        let mut q = SenderQueue::new(100);
        for seq in 1..=5 {
            q.append(env(seq)).unwrap();
        }
        // A resume from a point the buffer still covers replays cleanly.
        match q.resume(2) {
            ResumeOutcome::Replay(v) => {
                assert_eq!(v.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![3, 4, 5]);
            }
            ResumeOutcome::Resync => panic!("no drop occurred; expected a clean replay"),
        }
        // Resuming from the head yields an empty (still clean) replay.
        assert_eq!(q.resume(5), ResumeOutcome::Replay(vec![]));
    }

    #[test]
    fn resume_signals_resync_after_drop_oldest() {
        // remote-control-0ef.7: a drop-oldest overflow leaves the buffer's front
        // above ack_cursor + 1. A receiver resuming from before that front asked
        // for shed envelopes → it must be told to RESYNC, not handed a hole.
        let mut q = SenderQueue::new(3);
        for seq in 1..=5 {
            q.append(env(seq)).unwrap();
        }
        // Buffer now holds seq 3,4,5 (1 and 2 were dropped). ack_cursor is still 0.
        assert_eq!(
            q.replay(0).iter().map(|e| e.seq).collect::<Vec<_>>(),
            vec![3, 4, 5]
        );
        // A fresh receiver (from_seq 0) needs seq 1, which is gone → resync.
        assert_eq!(q.resume(0), ResumeOutcome::Resync);
        // A receiver that last saw seq 1 needs seq 2, also gone → resync.
        assert_eq!(q.resume(1), ResumeOutcome::Resync);
        // A receiver that last saw seq 2 needs seq 3, which is the front → clean.
        match q.resume(2) {
            ResumeOutcome::Replay(v) => {
                assert_eq!(v.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![3, 4, 5]);
            }
            ResumeOutcome::Resync => panic!("seq 3 is retained; expected a clean replay"),
        }
    }

    #[test]
    fn resume_on_empty_or_acked_queue_never_resyncs() {
        // An empty queue (no drops) is always a clean, empty replay.
        let q = SenderQueue::new(3);
        assert_eq!(q.resume(0), ResumeOutcome::Replay(vec![]));

        // After a clean ack (no drop), resuming from the ack point is clean.
        let mut q = SenderQueue::new(100);
        for seq in 1..=5 {
            q.append(env(seq)).unwrap();
        }
        q.ack(5);
        assert_eq!(q.resume(5), ResumeOutcome::Replay(vec![]));
    }

    #[test]
    fn from_snapshot_round_trips_and_continues() {
        // remote-control-tvc: a queue rehydrated from a persisted snapshot must
        // behave exactly like the live queue it was snapshotted from — the whole
        // point of letting a durable store reuse this logic instead of re-doing it.
        let mut live = SenderQueue::new(100);
        for seq in 1..=5 {
            live.append(env(seq)).unwrap();
        }
        live.ack(2); // prune 1,2; buffer = [3,4,5], high_water 5, ack_cursor 2.

        let restored = SenderQueue::from_snapshot(
            live.high_water(),
            live.ack_cursor(),
            live.buffered().cloned().collect(),
            100,
        );
        assert_eq!(restored.high_water(), 5);
        assert_eq!(restored.ack_cursor(), 2);
        assert_eq!(
            restored.buffered().map(|e| e.seq).collect::<Vec<_>>(),
            vec![3, 4, 5]
        );

        // The rehydrated queue continues the stream gaplessly and dedups the head.
        let mut restored = restored;
        assert_eq!(restored.append(env(5)), Ok(AppendOutcome::Duplicate));
        assert_eq!(
            restored.append(env(6)),
            Ok(AppendOutcome::Accepted { overflow: false })
        );
        assert_eq!(restored.high_water(), 6);
    }

    #[test]
    fn from_snapshot_preserves_overflow_gap_resync() {
        // A snapshot taken after a drop-oldest overflow must still signal Resync
        // for a resume from before the retained front (remote-control-tvc + 0ef.7).
        let mut live = SenderQueue::new(3);
        for seq in 1..=5 {
            live.append(env(seq)).unwrap(); // drops 1,2; buffer = [3,4,5].
        }
        let restored = SenderQueue::from_snapshot(
            live.high_water(),
            live.ack_cursor(),
            live.buffered().cloned().collect(),
            3,
        );
        assert_eq!(restored.resume(0), ResumeOutcome::Resync);
        assert_eq!(restored.resume(1), ResumeOutcome::Resync);
        match restored.resume(2) {
            ResumeOutcome::Replay(v) => {
                assert_eq!(v.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![3, 4, 5])
            }
            ResumeOutcome::Resync => panic!("seq 3 is retained; expected a clean replay"),
        }
    }

    #[test]
    fn resume_from_before_an_acked_front_is_clean_not_resync() {
        // Regression (remote-control-0ef.7): ack-pruning advances the buffer
        // front contiguously (front == ack_cursor + 1). A resume from *before*
        // that front must NOT be misread as an overflow gap — those seqs were
        // delivered and acknowledged, so replaying the retained tail is correct.
        let mut q = SenderQueue::new(100);
        for seq in 1..=3 {
            q.append(env(seq)).unwrap();
        }
        q.ack(1); // prune seq 1; front is now seq 2, ack_cursor = 1.
        match q.resume(0) {
            ResumeOutcome::Replay(v) => {
                assert_eq!(v.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![2, 3]);
            }
            ResumeOutcome::Resync => panic!("an ack-prune is not an overflow gap"),
        }
    }
}
