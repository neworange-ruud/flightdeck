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
}
