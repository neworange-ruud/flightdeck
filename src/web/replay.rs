//! Per-terminal replay ring buffer (`specs/WEB_INTERFACE.md` D2, Q2, Q3).
//!
//! A joining or reconnecting browser viewer needs to paint history before it
//! starts receiving live [`stream`](crate::web::stream) frames. This module is
//! the bounded buffer that makes that possible: pure data structure, no I/O,
//! no runtime.
//!
//! ## Bytes, not lines
//!
//! The buffer sits **in front of** the VT parser (xterm.js, in the browser).
//! There is no concept of a "line" at this layer — only the raw PTY byte
//! stream, exactly as the desktop's own `vt100` parser sees it (D2). This
//! means a viewer that attaches mid-escape-sequence can get one visually
//! broken repaint (a half-written SGR sequence, say). This is accepted and
//! intentional (Q2): xterm.js's own parser recovers on the next full redraw,
//! and there is no reliable way to find an "escape-sequence boundary" in a
//! byte stream without tracking full VT parser state here too — which would
//! duplicate the parser this module is deliberately upstream of. **Do not
//! "fix" this by scanning for escape boundaries.**
//!
//! ## The monotonic offset
//!
//! Every terminal has one [`ByteOffset`] counter: the total number of bytes
//! ever written to it, starting at 0 and only ever increasing. It does
//! **not** reset when the ring wraps and old bytes are discarded — the ring
//! wrapping is a storage detail, not a stream-position detail. This is what
//! lets a reconnecting viewer say "I have everything up to offset N" and have
//! that mean something durable across however many times the ring has
//! wrapped underneath it.
//!
//! [`ByteOffset`] is a `u64`. Overflow is not a practical concern: even a
//! terminal sustaining a relentless 100 MB/s of output forever would take
//! upwards of 5000 years to wrap a `u64` counter. No terminal session lives
//! that long, so no wraparound-of-the-counter-itself handling exists here.
//!
//! ## Three resume outcomes
//!
//! [`ReplayBuffer::resume`] is the whole point of the monotonic offset (Q3).
//! It returns a [`Resume`] enum rather than an `Option` or a plain slice
//! specifically so a caller cannot collapse "you're current" and "you missed
//! data, here's everything we have" into the same code path by accident:
//!
//! * [`Resume::UpToDate`] — the caller's offset is exactly the current
//!   offset. Nothing to send.
//! * [`Resume::Tail`] — the caller's offset is still inside the retained
//!   window. The exact continuation is returned (no data re-sent, no data
//!   skipped).
//! * [`Resume::Truncated`] — the caller's offset has aged out of the ring,
//!   belongs to a session this buffer knows nothing about, or (see below) is
//!   from the future. A full replay of everything currently retained is
//!   returned, explicitly flagged, so the viewer can honestly tell its user
//!   it missed output rather than silently pretending continuity.
//!
//! ### A future offset
//!
//! An offset ahead of the buffer's current offset cannot come from a viewer
//! that has been honestly following this buffer — it means either a client
//! bug (corrupted or fabricated cursor) or a host restart (the host process
//! restarted, zeroing its counters, while the viewer kept an offset from the
//! previous process's numbering). Neither case is a "the data still exists,
//! just skip ahead" situation: in both cases the safe, honest answer is the
//! same as aging-out — hand back everything currently retained, flagged
//! [`Resume::Truncated`], and let the viewer resynchronize. This module does
//! not special-case it further (no error variant, no panic): the caller
//! cannot distinguish "aged out" from "impossible" without out-of-band
//! session identity anyway, and both need the same recovery.
//!
//! ## Zero and one byte capacities
//!
//! [`ReplayBuffer::new`] accepts any `capacity`, including 0 and 1, rather
//! than making construction fallible. The write/resume algorithm already
//! degrades gracefully at these sizes (capacity 0 keeps no bytes and is
//! effectively a pure offset counter; capacity 1 keeps only the single most
//! recent byte), so rejecting them would only add a `Result` that every
//! caller must handle for a case that is not actually unsafe. Whether a
//! *configured* capacity is a sane default is a concern for the `[web]
//! replay_bytes` validation (a separate task), not for this data structure.
//!
//! ## Borrowed reads
//!
//! A live terminal can call [`ReplayBuffer::resume`] often (every reconnect,
//! every attach). Reads borrow from the ring rather than allocating: the
//! underlying storage is a [`VecDeque<u8>`], and when the retained bytes wrap
//! past the end of its backing storage, the two contiguous pieces are
//! returned as a [`ReplayChunk`] rather than copied into one `Vec`. Callers
//! that genuinely need one contiguous buffer can use
//! [`ReplayChunk::to_vec`], but the honest default is "here are the (up to)
//! two slices, in order."

use std::collections::VecDeque;

/// A monotonically increasing count of every byte ever written to a
/// terminal's replay buffer. Never resets on ring wraparound. See the module
/// doc for why `u64` overflow is not a practical concern.
pub type ByteOffset = u64;

/// Default replay window per terminal (Q2): 256 KiB. The `[web] replay_bytes`
/// config knob (a separate backlog task) overrides this by constructing
/// [`ReplayBuffer::new`] with a different capacity; this constant is simply
/// the documented default for that knob and for callers that don't wire
/// configuration through yet.
pub const DEFAULT_REPLAY_BYTES: usize = 256 * 1024;

/// Two byte slices, contiguous in stream order, that together make up one
/// read from a [`ReplayBuffer`]. `second` is non-empty only when the read
/// spans the ring's wraparound seam; otherwise it is empty.
///
/// Exposed as two slices — rather than always copying into one `Vec` — so a
/// terminal streaming at speed does not pay an allocation on every read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplayChunk<'a> {
    pub first: &'a [u8],
    pub second: &'a [u8],
}

impl<'a> ReplayChunk<'a> {
    /// Total bytes across both slices.
    pub fn len(&self) -> usize {
        self.first.len() + self.second.len()
    }

    /// True if this chunk carries no bytes at all.
    pub fn is_empty(&self) -> bool {
        self.first.is_empty() && self.second.is_empty()
    }

    /// Copies both slices into one contiguous buffer. Allocates — prefer
    /// `first`/`second` directly (e.g. two `write_all` calls) when the
    /// destination can accept two writes.
    pub fn to_vec(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.len());
        out.extend_from_slice(self.first);
        out.extend_from_slice(self.second);
        out
    }
}

/// One replay read: the bytes, plus the offset the caller should remember as
/// "current" and resume from next time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplaySlice<'a> {
    pub chunk: ReplayChunk<'a>,
    pub offset: ByteOffset,
}

/// The outcome of [`ReplayBuffer::resume`]. See the module doc's "Three
/// resume outcomes" section — this type exists specifically so the three
/// cases cannot be collapsed into one code path by accident.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resume<'a> {
    /// The caller's offset is exactly the buffer's current offset: nothing
    /// new has been written since.
    UpToDate,
    /// The caller's offset is still inside the retained window: an exact,
    /// gap-free continuation.
    Tail(ReplaySlice<'a>),
    /// The caller's offset has aged out of the ring, names a session this
    /// buffer has no record of, or is from the future (see the module doc).
    /// Carries a full replay of everything currently retained.
    Truncated(ReplaySlice<'a>),
}

/// A bounded, discard-oldest-first byte ring buffer for one live terminal.
///
/// See the module doc for the byte-vs-line, monotonic-offset, and
/// resume-outcome design decisions.
pub struct ReplayBuffer {
    capacity: usize,
    ring: VecDeque<u8>,
    total_written: ByteOffset,
}

impl std::fmt::Debug for ReplayBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReplayBuffer")
            .field("capacity", &self.capacity)
            .field("retained", &self.ring.len())
            .field("total_written", &self.total_written)
            .finish()
    }
}

impl ReplayBuffer {
    /// Creates an empty buffer holding at most `capacity` bytes. `capacity`
    /// of 0 or 1 is accepted; see the module doc for why.
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            ring: VecDeque::with_capacity(capacity),
            total_written: 0,
        }
    }

    /// The configured capacity in bytes.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Bytes currently retained (`<= capacity`).
    pub fn len(&self) -> usize {
        self.ring.len()
    }

    /// True if nothing is currently retained (either nothing has been
    /// written yet, or `capacity` is 0).
    pub fn is_empty(&self) -> bool {
        self.ring.is_empty()
    }

    /// Total bytes ever written to this terminal. Monotonic; never resets.
    pub fn current_offset(&self) -> ByteOffset {
        self.total_written
    }

    /// The oldest offset still retained. Equal to `current_offset()` when
    /// nothing is retained (empty or zero-capacity buffer).
    pub fn oldest_offset(&self) -> ByteOffset {
        self.total_written
            .saturating_sub(self.ring.len() as ByteOffset)
    }

    /// Appends bytes, discarding the oldest retained bytes first if this
    /// would exceed `capacity`. A write larger than the whole capacity keeps
    /// only its newest `capacity` bytes and does not panic. A zero-length
    /// write is a no-op: it does not advance the offset.
    pub fn write(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        self.total_written += bytes.len() as ByteOffset;

        if bytes.len() >= self.capacity {
            // Bigger than (or exactly) the whole buffer: it entirely
            // replaces the contents. Keep only the newest `capacity` bytes.
            self.ring.clear();
            self.ring.extend(&bytes[bytes.len() - self.capacity..]);
            return;
        }

        let overflow = (self.ring.len() + bytes.len()).saturating_sub(self.capacity);
        if overflow > 0 {
            self.ring.drain(..overflow);
        }
        self.ring.extend(bytes);
    }

    /// Replays the entire retained buffer. Used when a viewer attaches with
    /// no prior offset (Q2) — see the module doc for the accepted
    /// mid-escape-sequence imperfection this implies.
    pub fn attach(&self) -> ReplaySlice<'_> {
        ReplaySlice {
            chunk: self.full_chunk(),
            offset: self.total_written,
        }
    }

    /// Resumes a reconnecting viewer from `from`, its last known offset. See
    /// the module doc for the three possible outcomes.
    pub fn resume(&self, from: ByteOffset) -> Resume<'_> {
        let current = self.total_written;
        if from == current {
            return Resume::UpToDate;
        }

        let oldest = self.oldest_offset();
        if from < oldest || from > current {
            // Aged out, unknown/older session, or from the future: the
            // module doc explains why all three get the same honest answer.
            return Resume::Truncated(ReplaySlice {
                chunk: self.full_chunk(),
                offset: current,
            });
        }

        // `oldest <= from < current`, so `skip` is strictly less than the
        // number of retained bytes: safe to index into either ring slice.
        let skip = (from - oldest) as usize;
        Resume::Tail(ReplaySlice {
            chunk: self.skip_chunk(skip),
            offset: current,
        })
    }

    fn full_chunk(&self) -> ReplayChunk<'_> {
        let (first, second) = self.ring.as_slices();
        ReplayChunk { first, second }
    }

    fn skip_chunk(&self, skip: usize) -> ReplayChunk<'_> {
        let (first, second) = self.ring.as_slices();
        if skip <= first.len() {
            ReplayChunk {
                first: &first[skip..],
                second,
            }
        } else {
            let skip2 = (skip - first.len()).min(second.len());
            ReplayChunk {
                first: &second[skip2..],
                second: &[],
            }
        }
    }
}

#[cfg(test)]
mod tests;
