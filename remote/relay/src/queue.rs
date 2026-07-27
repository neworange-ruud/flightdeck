//! Server-side pending-event queue with gapless sequencing, cumulative-ack
//! pruning, resume-replay, and a bounded drop-oldest overflow policy.
//!
//! This is the "never lose an event, never send blind" machinery of spec §6,
//! from the relay's side. One [`SenderQueue`] holds the outbound envelopes of a
//! single `(pairing_id, sender_role)` stream:
//!
//! - **Gapless monotonic seq (§6.1).** Envelopes carry a per-stream `seq`. The
//!   relay accepts `seq == high_water + 1`, tolerates a re-send of the current
//!   high-water `seq` as an idempotent no-op (reconnect races), and rejects a
//!   forward *gap* as a protocol error (see the §6 v1 amendment in
//!   `specs/REMOTE_PROTOCOL.md`).
//! - **The sender owns its cursor; the relay follows it.** Two cases would
//!   otherwise deadlock a sender against the relay's watermark, so neither is an
//!   error (remote-control-arg):
//!   - a stream the relay has never seen (`high_water == 0`) **adopts** whatever
//!     `seq` the first envelope carries, instead of demanding 1. This is the
//!     relay-restarted-with-an-empty-store case: the sender kept its persisted
//!     cursor at 59 and the receiver kept its matching one, so starting the
//!     relay's watermark at 60 is exactly right — and demanding 1 would reject
//!     every envelope forever.
//!   - a sender whose `seq` **rewinds** below the watermark lost its cursor
//!     (reinstall, restored backup) and restarted its stream. [`SenderQueue::reset`]
//!     abandons the old epoch so the restarted one is accepted.
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
    /// `seq` skipped **past** the expected next value on a stream the relay is
    /// already tracking: the sender is ahead of us and the envelopes in between
    /// never arrived. Delivering this one would break the stream's gapless
    /// invariant, so it is dropped and the sender is told to re-sync.
    Gap {
        /// The `seq` the relay expected (`high_water + 1`).
        expected: u64,
        /// The `seq` that actually arrived.
        got: u64,
    },
    /// `seq` fell **below** the watermark: the sender lost its outbound cursor
    /// (reinstall, restored backup, cleared state) and restarted its stream from
    /// the beginning. Not a client bug and **not** recoverable by telling the
    /// sender to re-sync — it has no cursor left to drop and nothing to move
    /// forward to, so it would restart at the same low `seq` forever
    /// (remote-control-arg).
    ///
    /// The caller must instead abandon the old epoch on the relay's side —
    /// [`SenderQueue::reset`], reached through
    /// [`RelayStore::reset_stream`][crate::store::RelayStore::reset_stream] —
    /// and re-append, then tell the *peer* to drop its now-stale inbound cursor
    /// so it does not dedup the restarted seqs away.
    Rewind {
        /// The `seq` the relay expected (`high_water + 1`).
        expected: u64,
        /// The `seq` that actually arrived — the restarted stream's first.
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
        // seq 0 is not a valid stream position — §6.1 streams start at 1. Reject
        // it up front: the adoption path below would otherwise underflow on
        // `env.seq - 1`. Reported as a gap (never a rewind) so a malformed frame
        // can never talk the relay into discarding a live stream's epoch.
        if env.seq == 0 {
            return Err(QueueError::Gap { expected, got: 0 });
        }
        // A stream we have never seen adopts the sender's cursor rather than
        // demanding it start at 1 (see the module docs): the relay's watermark
        // is a follower of the sender's, not an independent authority.
        let adopting = self.high_water == 0;
        if env.seq == expected || adopting {
            let overflow = self.buf.len() >= self.max_len;
            if overflow {
                // Drop the oldest un-acked envelope to make room. The dropped
                // envelope is not counted as acked; recovery is the sender's
                // responsibility (spec §6 amendment).
                self.buf.pop_front();
            }
            self.high_water = env.seq;
            if adopting {
                // Adopting mid-stream means everything below the adopted seq is
                // neither held nor owed: park the ack cursor just under it so
                // `resume` reads the buffer's front as contiguous rather than as
                // an overflow-shed hole. Only on adoption — doing this on every
                // accept would drag `ack_cursor` along with `high_water` and
                // make `ack` a no-op.
                self.ack_cursor = self.ack_cursor.max(env.seq - 1);
            }
            self.buf.push_back(env);
            Ok(AppendOutcome::Accepted { overflow })
        } else if env.seq == self.high_water {
            // Idempotent re-send of the current head; tolerate silently.
            Ok(AppendOutcome::Duplicate)
        } else if env.seq < self.high_water {
            Err(QueueError::Rewind {
                expected,
                got: env.seq,
            })
        } else {
            Err(QueueError::Gap {
                expected,
                got: env.seq,
            })
        }
    }

    /// Abandon this stream's current epoch and restart it at `next_seq`: discard
    /// every buffered envelope from the old epoch and park both cursors just
    /// below `next_seq`, so the very next [`Self::append`] of `next_seq` is
    /// accepted as the new epoch's first envelope.
    ///
    /// Used when a sender's `seq` [rewinds][QueueError::Rewind] — it lost its
    /// cursor and started over, so the envelopes we still hold belong to a
    /// stream that no longer exists. Dropping them is deliberate: the peer is
    /// told to re-sync and pulls a fresh snapshot, which supersedes anything the
    /// abandoned epoch was still carrying. `next_seq` of 0 is treated as 1 (a
    /// stream's first seq is 1); no envelope can carry seq 0.
    pub fn reset(&mut self, next_seq: u64) {
        let floor = next_seq.max(1) - 1;
        self.high_water = floor;
        self.ack_cursor = floor;
        self.buf.clear();
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
        // The receiver's cursor sits ABOVE everything this stream has ever
        // accepted. That means the sender restarted its stream under the
        // receiver (a [`Self::reset`] after a rewind) while the receiver kept
        // the old epoch's cursor, so every seq the new epoch will ever emit
        // looks like a duplicate to it and gets deduped away forever
        // (remote-control-arg). Tell it to resync instead of handing back an
        // empty replay that reads as "you're up to date".
        //
        // Guarded on a stream we actually know: `high_water == 0` is a stream
        // the relay has never seen, which adopts the sender's cursor on its
        // first envelope — a receiver resuming from 59 there is about to be
        // matched by a sender adopting 60, so nothing is stale.
        if self.high_water > 0 && from_seq > self.high_water {
            return ResumeOutcome::Resync;
        }
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

    /// The seqs a `Replay` carries; panics on a `Resync` so a test that expected
    /// a clean replay fails loudly rather than comparing against an empty vec.
    fn replayed_seqs(outcome: ResumeOutcome) -> Vec<u64> {
        match outcome {
            ResumeOutcome::Replay(v) => v.iter().map(|e| e.seq).collect(),
            ResumeOutcome::Resync => panic!("expected a clean replay, got Resync"),
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
            Err(QueueError::Gap {
                expected: 2,
                got: 3
            })
        );
        // High-water unchanged after a rejected gap.
        assert_eq!(q.high_water(), 1);
    }

    #[test]
    fn reports_a_regression_as_rewind_not_gap() {
        // remote-control-arg: a sender BEHIND the watermark lost its cursor and
        // restarted. That is a different failure from a sender ahead of us, and
        // the caller must be able to tell them apart — a gap asks the sender to
        // re-sync, a rewind asks the RELAY to abandon its epoch.
        let mut q = SenderQueue::new(100);
        assert!(q.append(env(1)).is_ok());
        assert!(q.append(env(2)).is_ok());
        assert!(q.append(env(3)).is_ok());
        assert_eq!(
            q.append(env(1)),
            Err(QueueError::Rewind {
                expected: 4,
                got: 1
            })
        );
        // Nothing was mutated by the rejection.
        assert_eq!(q.high_water(), 3);
        assert_eq!(q.len(), 3);
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
    fn unknown_stream_adopts_the_senders_cursor() {
        // remote-control-arg: a relay that came up with an empty store has no
        // watermark, but the sender and receiver still hold matching cursors
        // from before. Demanding seq 1 would reject every envelope the sender
        // will ever send. Adopt its cursor instead.
        let mut q = SenderQueue::new(100);
        assert_eq!(
            q.append(env(60)),
            Ok(AppendOutcome::Accepted { overflow: false })
        );
        assert_eq!(q.high_water(), 60);
        // The stream then continues gaplessly from the adopted point.
        assert_eq!(
            q.append(env(61)),
            Ok(AppendOutcome::Accepted { overflow: false })
        );
        assert_eq!(
            q.append(env(63)),
            Err(QueueError::Gap {
                expected: 62,
                got: 63
            })
        );
        // A receiver that resumes from the matching pre-existing cursor gets a
        // clean replay of the adopted epoch, NOT a spurious resync: adoption
        // parked `ack_cursor` at 59, so the front is contiguous.
        assert_eq!(
            replayed_seqs(q.resume(59)),
            vec![60, 61],
            "an adopted stream must replay cleanly to the matching cursor"
        );
    }

    #[test]
    fn seq_zero_is_rejected_as_a_gap_on_any_stream() {
        // seq 0 is not a valid position. On a FRESH stream it must not reach the
        // adoption path (which would underflow computing `seq - 1`), and on a
        // live stream it must not be mistaken for a rewind — a malformed frame
        // must never talk the relay into discarding a real epoch.
        let mut fresh = SenderQueue::new(100);
        assert_eq!(
            fresh.append(env(0)),
            Err(QueueError::Gap {
                expected: 1,
                got: 0
            })
        );
        assert_eq!(fresh.high_water(), 0, "a rejected frame changes nothing");

        let mut live = SenderQueue::new(100);
        for seq in 1..=5 {
            live.append(env(seq)).unwrap();
        }
        assert_eq!(
            live.append(env(0)),
            Err(QueueError::Gap {
                expected: 6,
                got: 0
            })
        );
        assert_eq!(live.high_water(), 5);
        assert_eq!(live.len(), 5);
    }

    #[test]
    fn reset_abandons_the_epoch_and_restarts_the_stream() {
        // remote-control-arg: the sender lost its cursor and restarted at 1. The
        // envelopes we hold belong to an epoch that no longer exists.
        let mut q = SenderQueue::new(100);
        for seq in 1..=60 {
            q.append(env(seq)).unwrap();
        }
        assert_eq!(q.high_water(), 60);

        q.reset(1);
        assert_eq!(q.high_water(), 0);
        assert_eq!(q.ack_cursor(), 0);
        assert!(
            q.is_empty(),
            "the abandoned epoch's envelopes are discarded"
        );

        // The restarted stream is now accepted from its first envelope.
        assert_eq!(
            q.append(env(1)),
            Ok(AppendOutcome::Accepted { overflow: false })
        );
        assert_eq!(
            q.append(env(2)),
            Ok(AppendOutcome::Accepted { overflow: false })
        );
        assert_eq!(q.high_water(), 2);
    }

    #[test]
    fn reset_to_a_mid_stream_seq_parks_the_cursors_just_below_it() {
        // A sender whose cursor came back at 4 restarts at 5, not 1.
        let mut q = SenderQueue::new(100);
        for seq in 1..=60 {
            q.append(env(seq)).unwrap();
        }
        q.reset(5);
        assert_eq!(q.high_water(), 4);
        assert_eq!(q.ack_cursor(), 4);
        assert_eq!(
            q.append(env(5)),
            Ok(AppendOutcome::Accepted { overflow: false })
        );
        // And a seq below the reset point is still a rewind, not silently taken.
        assert_eq!(
            q.append(env(2)),
            Err(QueueError::Rewind {
                expected: 6,
                got: 2
            })
        );
    }

    #[test]
    fn resume_above_high_water_signals_resync() {
        // remote-control-arg: after a reset the receiver still holds the OLD
        // epoch's cursor. Every seq the new epoch emits looks like a duplicate
        // to it, so an empty replay ("you're up to date") strands it forever.
        let mut q = SenderQueue::new(100);
        for seq in 1..=60 {
            q.append(env(seq)).unwrap();
        }
        q.ack(60);
        q.reset(1);
        q.append(env(1)).unwrap();

        // The peer's cursor (60) is above everything this stream now holds.
        assert_eq!(q.resume(60), ResumeOutcome::Resync);
        // A receiver in step with the new epoch is served normally.
        assert_eq!(replayed_seqs(q.resume(0)), vec![1]);
        assert_eq!(q.resume(1), ResumeOutcome::Replay(vec![]));
    }

    #[test]
    fn resume_above_high_water_on_an_unseen_stream_is_clean() {
        // The guard must not fire for a stream the relay has never seen: the
        // sender is about to ADOPT a cursor that matches this receiver's, so
        // there is nothing stale to resync.
        let q = SenderQueue::new(100);
        assert_eq!(q.resume(59), ResumeOutcome::Replay(vec![]));
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
