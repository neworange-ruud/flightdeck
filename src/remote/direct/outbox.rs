//! The Mac-held outbox for one direct-mode pairing (spec §5.10, §6).
//!
//! On a relay-mode pairing the relay's per-(pairing, sender) queue holds the
//! desktop's un-acked envelopes while the phone is away, replays them on
//! `resume`, and trims them on `ack`. A direct-mode pairing has no relay in its
//! data plane, so this module takes that job over on the Mac. It mirrors the
//! relay's `SenderQueue` (`remote/relay/src/queue.rs`) rule for rule, so a phone
//! sees the same delivery semantics on either transport:
//!
//! * **Bounded, drop-oldest** — at most [`MAX_ENVELOPES`] envelopes and
//!   [`MAX_BYTES`] of payload; the oldest is shed first so the newest state
//!   always wins (§6.1 "Queue bound & overflow").
//! * **Replay strictly above `resume.from_seq`** (§6.3), and `seq_violation`
//!   when that cannot be served without a hole: `from_seq` precedes the oldest
//!   retained envelope after an overflow, or sits above everything this stream
//!   ever emitted (§6.3, both cases).
//! * **Cumulative ack** trims everything at or below the cursor (§6.2).
//! * **A rewind restarts the epoch** (§6.1): a `seq` at or below the high-water
//!   mark means the sender lost its cursor; the old epoch's envelopes belong to
//!   a stream that no longer exists and are dropped.
//!
//! It also keeps the **inbound** dedup cursor for the phone's stream (§6.4),
//! because on a direct pairing that cursor belongs to the same per-pairing file.
//!
//! ## One deliberate difference from the relay: the overflow gap heals
//!
//! After an overflow, the relay answers every `resume` below the retained front
//! with `seq_violation` — including the `resume { from_seq: 0 }` the phone sends
//! as its recovery (§6.3), which would loop. Here the outbox advances its ack
//! cursor to just below the front when it reports the gap, so the shed seqs
//! count as given up on and the phone's follow-up `resume` from 0 replays what
//! is retained (the phone tolerates a forward jump; only a rewind is special).
//!
//! ## Persistence (atomic, debounced, seq-continuous)
//!
//! [`OutboxFile`] persists the outbox as one JSON document written through the
//! [`FileSystem`] seam, whose real implementation writes atomically (temp file +
//! rename) — a crash leaves either the old or the new file, never a torn one.
//! Writes are **debounced** by the caller (the direct link flushes dirty
//! outboxes every [`super::FLUSH_INTERVAL`]), because rewriting up to 8 MiB per
//! envelope would be absurd.
//!
//! Debouncing must not cost seq continuity, or a crash would let the bridge
//! re-number envelopes the phone already holds and its dedup would silently
//! drop the new ones (the failure mode in remote-control-arg). So every write
//! also records `seq_reserved`: a high-water mark [`SEQ_RESERVE`] ahead of the
//! real one, and a push that would pass it is written **immediately** with a
//! fresh reservation. On load the high-water mark is floored to the
//! reservation, so after a crash the next envelope is numbered past anything
//! the dead process could have sent. The jump is harmless — receivers only
//! reject seqs that go *backwards* — and a clean shutdown writes the real
//! high-water mark as its reservation, so an orderly restart does not jump at
//! all. What a crash can lose is the last flush interval of not-yet-written
//! envelopes, which the phone recovers by the snapshot it is sent on every
//! (re)attach.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use flightdeck_remote_protocol::{EncryptedEnvelope, Role};
use serde::{Deserialize, Serialize};

use crate::contracts::{FileSystem, FlightDeckError, Result};

/// Newest envelopes kept per pairing (spec §5.10; the relay's
/// `QUEUE_MAX_PER_PAIRING` default).
pub const MAX_ENVELOPES: usize = 1000;
/// Payload bytes kept per pairing (spec §5.10): 8 MiB.
pub const MAX_BYTES: usize = 8 * 1024 * 1024;
/// How far ahead of the real high-water mark each write reserves (see the
/// module docs). Big enough that a debounced flush interval never runs through
/// it under shell streaming, small enough that the post-crash jump is cosmetic.
pub const SEQ_RESERVE: u64 = 1024;
/// On-disk schema version of [`OutboxFile`].
const OUTBOX_FILE_VERSION: u32 = 1;
/// File name inside the pairing's directory.
const OUTBOX_FILE_NAME: &str = "outbox.json";

/// Per-outbox bounds. [`OutboxLimits::default`] is the spec's 1000 / 8 MiB;
/// tests shrink them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutboxLimits {
    /// Most envelopes retained.
    pub max_envelopes: usize,
    /// Most payload bytes retained (see [`envelope_cost`]).
    pub max_bytes: usize,
}

impl Default for OutboxLimits {
    fn default() -> Self {
        OutboxLimits {
            max_envelopes: MAX_ENVELOPES,
            max_bytes: MAX_BYTES,
        }
    }
}

/// What a byte bound charges for one envelope: its two base64 payload fields.
/// The fixed header fields are small and the same for every envelope, so
/// leaving them out keeps the bound meaningful ("8 MiB of sealed payload").
pub fn envelope_cost(env: &EncryptedEnvelope) -> usize {
    env.ciphertext.len() + env.nonce.len()
}

/// The answer to a phone's `resume` (spec §6.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumeOutcome {
    /// Everything retained strictly above `from_seq`, in order.
    Replay(Vec<EncryptedEnvelope>),
    /// The resume cannot be served without stranding the phone; answer
    /// `error { code: seq_violation }`.
    Resync,
}

/// What [`Outbox::push`] did beyond appending.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PushOutcome {
    /// The push rewound the stream, so the previous epoch was discarded.
    pub restarted: bool,
    /// How many of the oldest envelopes the bounds shed.
    pub dropped: usize,
}

/// The pure, in-memory outbox (see the module docs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outbox {
    limits: OutboxLimits,
    buf: VecDeque<EncryptedEnvelope>,
    bytes: usize,
    /// Highest `seq` this stream has accepted. Survives acks and drops; the
    /// startup floor for the bridge's outbound counter.
    high_water: u64,
    /// Highest cumulative ack from the phone.
    ack_cursor: u64,
    /// Highest phone→desktop `seq` handled (the §6.4 dedup cursor).
    last_received: u64,
}

impl Outbox {
    /// An empty outbox with the given bounds.
    pub fn new(limits: OutboxLimits) -> Self {
        Outbox {
            limits,
            buf: VecDeque::new(),
            bytes: 0,
            high_water: 0,
            ack_cursor: 0,
            last_received: 0,
        }
    }

    /// Highest outbound `seq` ever accepted.
    pub fn high_water(&self) -> u64 {
        self.high_water
    }

    /// Highest cumulative ack the phone has sent.
    pub fn ack_cursor(&self) -> u64 {
        self.ack_cursor
    }

    /// Highest inbound `seq` handled.
    pub fn last_received(&self) -> u64 {
        self.last_received
    }

    /// Envelopes currently retained.
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    /// Whether nothing is retained.
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Payload bytes currently retained.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// `seq` of the oldest retained envelope.
    pub fn front_seq(&self) -> Option<u64> {
        self.buf.front().map(|e| e.seq)
    }

    /// Append a desktop→phone envelope, shedding the oldest past the bounds.
    ///
    /// A `seq` at or below the high-water mark is a stream restart (§6.1): the
    /// sender lost its cursor, so the old epoch's envelopes — which belong to a
    /// stream that no longer exists — are discarded and the new epoch adopted.
    /// The newest envelope is always kept, even if it alone exceeds the byte
    /// bound: dropping it would lose the very state the bound exists to favour.
    pub fn push(&mut self, env: EncryptedEnvelope) -> PushOutcome {
        let mut outcome = PushOutcome::default();
        if self.high_water > 0 && env.seq <= self.high_water {
            self.buf.clear();
            self.bytes = 0;
            self.ack_cursor = env.seq.saturating_sub(1);
            outcome.restarted = true;
        }
        self.high_water = env.seq;
        self.bytes += envelope_cost(&env);
        self.buf.push_back(env);
        while self.buf.len() > 1
            && (self.buf.len() > self.limits.max_envelopes || self.bytes > self.limits.max_bytes)
        {
            if let Some(old) = self.buf.pop_front() {
                self.bytes -= envelope_cost(&old);
                outcome.dropped += 1;
            }
        }
        outcome
    }

    /// Apply a cumulative ack (§6.2): trim every envelope at or below `cursor`.
    /// A cursor beyond the high-water mark is clamped; one at or below the
    /// current ack point changes nothing. Returns whether the cursor advanced.
    pub fn ack(&mut self, cursor: u64) -> bool {
        let cursor = cursor.min(self.high_water);
        if cursor <= self.ack_cursor {
            return false;
        }
        self.ack_cursor = cursor;
        while self.buf.front().is_some_and(|e| e.seq <= cursor) {
            if let Some(old) = self.buf.pop_front() {
                self.bytes -= envelope_cost(&old);
            }
        }
        true
    }

    /// Answer `resume { from_seq }` (§6.3; see the module docs for the healed
    /// overflow gap, which is why this takes `&mut self`).
    pub fn resume(&mut self, from_seq: u64) -> ResumeOutcome {
        // The phone holds a cursor from a stream epoch this outbox never
        // reached: replaying nothing would read as "up to date" while every
        // seq we emit next is deduped away on the phone (remote-control-arg).
        if self.high_water > 0 && from_seq > self.high_water {
            return ResumeOutcome::Resync;
        }
        if let Some(front) = self.front_seq() {
            // The front sits above the ack point's successor only when an
            // overflow shed un-acked envelopes (acks advance it contiguously).
            let overflow_gap = front > self.ack_cursor + 1;
            if overflow_gap && from_seq + 1 < front {
                // Heal the gap: the shed seqs are gone for good, so stop
                // counting them as owed and let the phone's recovery resume
                // (from 0) replay what is retained instead of looping here.
                self.ack_cursor = front - 1;
                return ResumeOutcome::Resync;
            }
        }
        ResumeOutcome::Replay(self.after(from_seq))
    }

    /// Every retained envelope with `seq > after`, in order — the replay set and
    /// the live-delivery set alike.
    pub fn after(&self, after: u64) -> Vec<EncryptedEnvelope> {
        self.buf.iter().filter(|e| e.seq > after).cloned().collect()
    }

    /// Phone→desktop dedup (§6.4), exactly the rule the relay client applies:
    /// accept a strictly newer `seq`, or `seq == 1` while holding a cursor (the
    /// phone restarted its stream — deduping that would drop the recovered
    /// stream forever). Advances the cursor and returns `true` when accepted.
    pub fn accept_inbound(&mut self, seq: u64) -> bool {
        let is_reset = seq == 1 && self.last_received >= 1;
        if seq > self.last_received || is_reset {
            self.last_received = seq;
            true
        } else {
            false
        }
    }
}

/// The JSON document [`OutboxFile`] writes.
#[derive(Debug, Serialize, Deserialize)]
struct OutboxDoc {
    version: u32,
    pairing_id: String,
    high_water: u64,
    /// See the module docs: the floor for `high_water` on the next load.
    seq_reserved: u64,
    ack_cursor: u64,
    last_received: u64,
    envelopes: Vec<EncryptedEnvelope>,
}

/// An [`Outbox`] bound to its file under the direct link's state directory.
pub struct OutboxFile {
    fs: Arc<dyn FileSystem + Send + Sync>,
    dir: PathBuf,
    pairing_id: String,
    outbox: Outbox,
    /// The reservation last written to disk.
    reserved_on_disk: u64,
    /// Changed in memory since the last write.
    dirty: bool,
}

impl std::fmt::Debug for OutboxFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutboxFile")
            .field("dir", &self.dir)
            .field("pairing_id", &self.pairing_id)
            .field("high_water", &self.outbox.high_water)
            .field("len", &self.outbox.len())
            .finish_non_exhaustive()
    }
}

/// The directory holding `pairing_id`'s outbox under `root`. The pairing id is
/// relay-assigned and opaque, so anything outside `[A-Za-z0-9_-]` is
/// hex-escaped: the id can never steer the path out of `root`, and two
/// distinct ids never share a directory.
pub fn pairing_dir(root: &Path, pairing_id: &str) -> PathBuf {
    let mut name = String::with_capacity(pairing_id.len());
    for b in pairing_id.bytes() {
        if b.is_ascii_alphanumeric() || b == b'-' {
            name.push(b as char);
        } else {
            // `_` is the escape character itself, so it is escaped too.
            name.push_str(&format!("_{b:02x}"));
        }
    }
    if name.is_empty() {
        name.push_str("_empty");
    }
    root.join(name)
}

impl OutboxFile {
    /// Load `pairing_id`'s outbox from under `root`, or start an empty one. An
    /// unreadable or corrupt file starts empty too (best-effort, like every
    /// other FlightDeck state file); the bridge's own persisted cursor still
    /// floors the outbound counter in that case.
    pub fn open(
        fs: Arc<dyn FileSystem + Send + Sync>,
        root: &Path,
        pairing_id: &str,
        limits: OutboxLimits,
    ) -> Self {
        let dir = pairing_dir(root, pairing_id);
        let mut outbox = Outbox::new(limits);
        let mut reserved_on_disk = 0;
        let doc = fs
            .read_to_string(&dir.join(OUTBOX_FILE_NAME))
            .ok()
            .and_then(|raw| serde_json::from_str::<OutboxDoc>(&raw).ok())
            .filter(|d| d.pairing_id == pairing_id);
        if let Some(doc) = doc {
            // Rebuild through `push` so the bounds (which may have changed
            // between builds) apply on load exactly as at runtime.
            for env in doc.envelopes {
                if env.sender == Role::Desktop {
                    outbox.push(env);
                }
            }
            outbox.high_water = doc.high_water.max(doc.seq_reserved);
            outbox.ack_cursor = doc.ack_cursor.min(outbox.high_water);
            outbox.last_received = doc.last_received;
            reserved_on_disk = outbox.high_water;
        }
        OutboxFile {
            fs,
            dir,
            pairing_id: pairing_id.to_string(),
            outbox,
            reserved_on_disk,
            dirty: false,
        }
    }

    /// The in-memory outbox.
    pub fn outbox(&self) -> &Outbox {
        &self.outbox
    }

    /// Whether there are changes not yet on disk.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Append an outbound envelope. Written through immediately when its `seq`
    /// passes the on-disk reservation (seq continuity across a crash), otherwise
    /// left for the debounced flush.
    pub fn push(&mut self, env: EncryptedEnvelope) -> PushOutcome {
        let outcome = self.outbox.push(env);
        self.dirty = true;
        if outcome.restarted || self.outbox.high_water > self.reserved_on_disk {
            let _ = self.write(false);
        }
        outcome
    }

    /// Apply a phone ack. Debounced.
    pub fn ack(&mut self, cursor: u64) -> bool {
        let advanced = self.outbox.ack(cursor);
        self.dirty |= advanced;
        advanced
    }

    /// Answer a resume. Debounced (a healed gap moves the ack cursor).
    pub fn resume(&mut self, from_seq: u64) -> ResumeOutcome {
        let before = self.outbox.ack_cursor;
        let outcome = self.outbox.resume(from_seq);
        self.dirty |= self.outbox.ack_cursor != before;
        outcome
    }

    /// Phone→desktop dedup; see [`Outbox::accept_inbound`]. Debounced.
    pub fn accept_inbound(&mut self, seq: u64) -> bool {
        let accepted = self.outbox.accept_inbound(seq);
        self.dirty |= accepted;
        accepted
    }

    /// Write if dirty (the debounced flush), reserving ahead.
    pub fn flush(&mut self) -> Result<()> {
        if self.dirty {
            self.write(false)
        } else {
            Ok(())
        }
    }

    /// Write unconditionally with the real high-water mark as the reservation,
    /// so an orderly restart continues the stream without a jump.
    pub fn flush_clean(&mut self) -> Result<()> {
        self.write(true)
    }

    /// Delete this pairing's directory (revoke / unpair).
    pub fn delete(&self) -> Result<()> {
        if self.fs.exists(&self.dir) {
            self.fs.remove_dir_all(&self.dir)?;
        }
        Ok(())
    }

    fn write(&mut self, clean: bool) -> Result<()> {
        let seq_reserved = if clean {
            self.outbox.high_water
        } else {
            self.outbox.high_water + SEQ_RESERVE
        };
        let doc = OutboxDoc {
            version: OUTBOX_FILE_VERSION,
            pairing_id: self.pairing_id.clone(),
            high_water: self.outbox.high_water,
            seq_reserved,
            ack_cursor: self.outbox.ack_cursor,
            last_received: self.outbox.last_received,
            envelopes: self.outbox.buf.iter().cloned().collect(),
        };
        let json = serde_json::to_string(&doc)
            .map_err(|e| FlightDeckError::State(format!("failed to serialize outbox: {e}")))?;
        if !self.fs.exists(&self.dir) {
            self.fs.create_dir_all(&self.dir)?;
        }
        self.fs.write(&self.dir.join(OUTBOX_FILE_NAME), &json)?;
        self.reserved_on_disk = seq_reserved;
        self.dirty = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
