//! Host-side activity-feed event store (`specs/WEB_INTERFACE.md` D11, §5.1).
//!
//! This is **the entire substitute for OS notifications in the browser**. Web
//! Push is structurally blocked under D1 (no publicly reachable sender behind
//! a loopback server), so a browser tab that is not currently open has no other
//! way to learn "the agent you were watching finished, or needs you" than to
//! ask this store when it opens. That single requirement drives every choice
//! below: retention is generous enough that a tab opened after lunch still
//! shows the morning's failures, and the store never discards an event for any
//! reason except the two documented bounds.
//!
//! ## Two bounds, both enforced, together
//!
//! Retain the last [`MAX_EVENTS`] events **or** [`MAX_AGE_MS`], **whichever is
//! smaller** (§5.1). "Whichever is smaller" is not a choice this module makes
//! once — it is what falls out of enforcing both bounds unconditionally on
//! every [`ActivityStore::evict`]: age-based pruning removes anything older
//! than the cutoff regardless of count, and count-based pruning then trims any
//! remainder down to [`MAX_EVENTS`] regardless of age. A young, bursty session
//! is capped by count; a quiet store with a handful of old events is capped by
//! age; both together get both bounds applied, which is exactly "whichever is
//! smaller" without either bound needing to know about the other.
//!
//! ## The clock is a seam, not a field
//!
//! Nothing here calls `SystemTime::now()`. Every method that needs "now" — a
//! new event's timestamp, or the age cutoff for eviction — takes `clock: &dyn
//! Clock` as a parameter, the same convention [`crate::app::state::AppState`]
//! already uses (`clock: &'a dyn Clock`) rather than boxing a clock into the
//! struct. That keeps [`ActivityStore`] itself trivially owned behind a
//! `Mutex` in server state with no lifetime parameter to thread through, while
//! keeping the 24-hour bound fully testable against
//! [`crate::testing::FakeClock`] with no sleeping — see `tests.rs`.
//!
//! Because eviction only happens when something calls [`ActivityStore::evict`]
//! (directly, or via [`ActivityStore::record`]), a store that receives no new
//! events for 24+ hours does not spontaneously empty itself. The caller that
//! builds a `Snapshot` or otherwise reads the feed for a human is expected to
//! call [`ActivityStore::evict`] first — see the empty-state row in artboard
//! 2e ("Nothing has changed in 24 hours"), which is a *read-time* fact, not
//! something a background timer needs to exist to produce.
//!
//! ## Unknown stays unknown, by construction
//!
//! [`ActivityStore::record`] takes `from: InterpretedStatus` and `to:
//! InterpretedStatus` as **required, caller-supplied** values. There is no
//! code path in this module that derives, defaults, or infers either one from
//! partial information — no `ProcessState` fallback, no "assume idle if we
//! haven't heard otherwise." An agent with no lifecycle hooks is recorded
//! exactly as the caller describes it: `from: Unknown, to: Unknown`. That
//! honesty is the caller's to preserve (this store cannot stop a caller from
//! passing a fabricated status), but it *can* guarantee it never manufactures
//! one itself, which is the half of "unknown stays unknown" that belongs to a
//! data store rather than to the code deciding what to observe.
//!
//! ## Reusing the real precedence, not reinventing it
//!
//! [`ActivityStore::record`] derives an event's [`ActivityTier`] via
//! [`StatusBucket::from_interpreted`] then [`ActivityTier::for_bucket`] —
//! **both already committed in [`super::protocol`]**, unchanged here. That is
//! the same derivation the sidebar's project dot uses ([`StatusBucket::rank`],
//! [`StatusBucket::rollup`]) applied to a single transition's `to` state,
//! so a feed row's tier can never drift from what the dot would show for that
//! same transition.
//!
//! [`ActivityStore::unread_summary`] is a different question — "which of
//! several *unread* events is most urgent" — and deliberately does **not**
//! reuse [`StatusBucket::rank`]/[`rollup`](StatusBucket::rollup) for it. That
//! rank orders five buckets for the dot's "is this session busy or done"
//! question, where `InProgress` (rank 2) outranks `Idle` (rank 3) because a
//! still-running session is the one you might want to interrupt. The feed
//! chip asks "is this unread event worth surfacing", where a *finished* run
//! outranks a run that is merely still going (§5.1: attention beats finished
//! beats quiet) — reusing the dot's rank would rank a `Quiet` "still working"
//! transition above a `Finished` completion, exactly backwards for the chip.
//! The three-tier order therefore gets its own small `tier_rank` below, an
//! exhaustive match with no wildcard arm over [`ActivityTier`] — restating
//! that enum's own doc comment as code, so a fourth tier added to
//! `ActivityTier` fails to compile here rather than silently sorting wrong.
//!
//! ## The reason string: a gap in the committed wire type
//!
//! [`super::protocol::ActivityEvent`] — finished and committed — carries
//! `from`, `to`, `manual`, `tier`, timestamps and identity, but **no reason
//! string**. The design's rows are not honest without one (`"asked a
//! question"`, `"agent exited (code 1)"`, `"finished, 18 files touched"`,
//! artboard 2e) — the status pair alone does not distinguish *why* an agent
//! moved from `in progress` to `error`. Since `protocol.rs` is out of scope for
//! this module, the reason travels on [`protocol::ActivityEvent::reason`]
//! itself, so a caller building a `Snapshot` or a `Delta::Activity` hands the
//! stored event over whole and cannot lose the one part of a row a user
//! actually reads (artboard 2e).
//! module owes the orchestrator: **`protocol::ActivityEvent` needs a `reason:
//! String` field before the browser can render the design's rows verbatim.**
//! This module does not add it, per instruction; it reports the gap instead.
//!
//! ## A second record, not the only one
//!
//! This is **not** the desktop's sole source of lifecycle awareness. The
//! desktop already posts OS notifications from the very same interpreted-
//! status transitions, via [`crate::app::state::AppState::take_finish_notifications`]
//! (which watches `notify_phase(interpreted)` transitions and hands the result
//! to a [`crate::contracts::Notifier`]). That call site and this store's
//! [`ActivityStore::record`] must eventually be driven by **the same**
//! transition detection, or the OS notification and the browser's feed row for
//! the same event could disagree — see the module doc's framing in `mod.rs`
//! ("Sources the same lifecycle signals the desktop's OS notifications use —
//! this is a second record, not a replacement"). Wiring `record` calls to that
//! exact detection is the job of whichever module drives the event loop, not
//! this one; this comment exists so that wiring is done once, deliberately,
//! against the same source, rather than re-derived and left to drift.

use std::collections::VecDeque;

use crate::contracts::{Clock, InterpretedStatus, ManualStatus, TabId};

use super::protocol::{ActivityEvent, ActivityTier, EventId, ProjectId, StatusBucket};

#[cfg(test)]
mod tests;

/// Retain at most this many events (§5.1). See the module doc's "two bounds"
/// section for how this interacts with [`MAX_AGE_MS`].
pub const MAX_EVENTS: usize = 200;

/// Retain at most this much history, in milliseconds (§5.1): 24 hours.
pub const MAX_AGE_MS: i64 = 24 * 60 * 60 * 1000;

/// One status transition as reported by the caller — never derived, never
/// defaulted (see the module doc's "unknown stays unknown" section).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transition {
    /// Owning project.
    pub project_id: ProjectId,
    /// Project name, denormalised the same way [`ActivityEvent`] denormalises
    /// it, so a feed row needs no lookup.
    pub project_name: String,
    /// The session that changed.
    pub session_id: TabId,
    /// Session name, denormalised for the same reason.
    pub session_name: String,
    /// Status before the transition. Required — see the module doc.
    pub from: InterpretedStatus,
    /// Status after the transition. Required — see the module doc.
    pub to: InterpretedStatus,
    /// The manual override in force after the transition, if any.
    pub manual: Option<ManualStatus>,
    /// Free-form human-readable reason, e.g. `"asked a question"`,
    /// `"agent exited (code 1)"`, `"finished, 18 files touched"`,
    /// `"set by hand on the desktop"`, or `"Codex CLI reports no lifecycle"`
    /// for an unknown-lifecycle agent. See the module doc's "reason string"
    /// section for why this cannot yet ride the wire as part of
    /// [`ActivityEvent`].
    pub reason: String,
}

/// The unread chip's content (§5.1): the most urgent tier among unread
/// events, how many unread events sit at that tier, and how many are unread
/// in total. `None` from [`ActivityStore::unread_summary`] means "hide the
/// chip" — nothing unread, whether because the store is empty or because
/// everything has been marked read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnreadSummary {
    /// The most urgent tier with at least one unread event.
    pub tier: ActivityTier,
    /// Unread events at `tier`.
    pub count_at_tier: usize,
    /// Unread events across all tiers.
    pub total_unread: usize,
}

/// Ranks the three feed tiers by urgency, **lower is more urgent** — the same
/// convention as [`StatusBucket::rank`], but **not a reuse of it**. See the
/// module doc's "Reusing the real precedence" section for why reusing the
/// dot's five-bucket rank here would be wrong (it would rank a still-working
/// `Quiet` transition above a `Finished` completion). This match is
/// deliberately exhaustive with no wildcard arm: adding a variant to
/// [`ActivityTier`] fails to compile here rather than silently sorting it
/// last (or first) by accident.
fn tier_rank(tier: ActivityTier) -> u8 {
    match tier {
        ActivityTier::Attention => 0,
        ActivityTier::Finished => 1,
        ActivityTier::Quiet => 2,
    }
}

/// The host-side, global-across-projects (D11) activity feed.
///
/// See the module doc for retention, the clock seam, the reason-string gap,
/// and how tier precedence is derived and ranked.
#[derive(Debug, Default)]
pub struct ActivityStore {
    /// Arrival order: index 0 (front) is the oldest arrival, back is the
    /// newest. Not re-sorted by timestamp — see [`ActivityStore::record`]'s
    /// doc for why arrival order, not `at_ms` order, is authoritative here.
    events: VecDeque<ActivityEvent>,
    /// Monotonic counter used to mint unique [`EventId`]s. Never reused, even
    /// across eviction, so a stale id a viewer still holds after eviction is
    /// simply not found rather than accidentally matching a newer event.
    next_seq: u64,
}

impl ActivityStore {
    /// An empty feed.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of events currently retained.
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// True if nothing is currently retained.
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// All retained events, **oldest first** — matching
    /// [`crate::web::protocol::Snapshot::activity`]'s documented backfill
    /// order.
    ///
    /// This is arrival order, not a re-sort by `at_ms`. See
    /// [`ActivityStore::record`]'s doc for why the two can differ and why
    /// arrival order is what this store treats as authoritative.
    pub fn events(&self) -> impl Iterator<Item = &ActivityEvent> + '_ {
        self.events.iter()
    }

    /// Records one transition, then enforces both retention bounds
    /// (`evict`). Returns the new event's id.
    ///
    /// **Arrival order, not timestamp order, is what this store preserves.**
    /// `clock.now_millis()` stamps `at_ms` at the moment of the call, but the
    /// event is always appended at the back — it is never inserted to keep
    /// the list sorted by `at_ms`. In production a real clock does not run
    /// backward, so the two orders coincide. Under a clock that *does* jump
    /// backward (a fake clock in a test, or — in principle — a host whose
    /// wall clock was stepped by NTP), this store keeps the call order and
    /// leaves the resulting `at_ms` values honestly out of sequence, rather
    /// than silently re-sorting the whole feed around one glitchy timestamp.
    /// A single-writer event loop calling this in the order things actually
    /// happened is what keeps arrival order meaningful; see `tests.rs` for a
    /// test that pins this behaviour down explicitly.
    pub fn record(&mut self, clock: &dyn Clock, transition: Transition) -> EventId {
        self.next_seq += 1;
        let event_id = EventId::new(format!("evt-{}", self.next_seq));

        let bucket = StatusBucket::from_interpreted(transition.to);
        let tier = ActivityTier::for_bucket(bucket);

        let event = ActivityEvent {
            event_id: event_id.clone(),
            at_ms: clock.now_millis() as i64,
            project_id: transition.project_id,
            project_name: transition.project_name,
            session_id: transition.session_id,
            session_name: transition.session_name,
            from: transition.from,
            to: transition.to,
            manual: transition.manual,
            reason: transition.reason,
            tier,
            read: false,
        };

        self.events.push_back(event);

        self.evict(clock);
        event_id
    }

    /// Enforces both retention bounds against `clock`'s current time (§5.1):
    /// drops anything older than [`MAX_AGE_MS`], then trims any remainder
    /// down to [`MAX_EVENTS`] oldest-first. Called automatically by
    /// [`ActivityStore::record`]; callers building a read view after a
    /// stretch of silence (e.g. before taking a `Snapshot`) should call this
    /// first so an idle feed can honestly age itself down to empty — see the
    /// module doc.
    pub fn evict(&mut self, clock: &dyn Clock) {
        let now = clock.now_millis() as i64;
        let cutoff = now - MAX_AGE_MS;
        self.events.retain(|event| event.at_ms >= cutoff);

        while self.events.len() > MAX_EVENTS {
            self.events.pop_front();
        }
    }

    /// Marks one event read by id. Returns `false` if no retained event has
    /// that id (already evicted, or never existed) — the caller can treat
    /// that as a no-op rather than an error.
    pub fn mark_read(&mut self, event_id: &EventId) -> bool {
        match self
            .events
            .iter_mut()
            .find(|event| &event.event_id == event_id)
        {
            Some(event) => {
                event.read = true;
                true
            }
            None => false,
        }
    }

    /// Marks every currently retained event read.
    pub fn mark_all_read(&mut self) {
        for event in self.events.iter_mut() {
            event.read = true;
        }
    }

    /// The unread chip's content (§5.1), or `None` when nothing is unread
    /// (empty store, or everything already read). See [`UnreadSummary`] and
    /// the module doc's "Reusing the real precedence" section for how the
    /// tier is chosen.
    pub fn unread_summary(&self) -> Option<UnreadSummary> {
        // Indexed by `tier_rank`: [attention, finished, quiet].
        let mut counts = [0usize; 3];
        for event in self.events.iter().filter(|event| !event.read) {
            counts[tier_rank(event.tier) as usize] += 1;
        }
        let total_unread: usize = counts.iter().sum();
        let rank = counts.iter().position(|&c| c > 0)?;
        let tier = match rank {
            0 => ActivityTier::Attention,
            1 => ActivityTier::Finished,
            _ => ActivityTier::Quiet,
        };
        Some(UnreadSummary {
            tier,
            count_at_tier: counts[rank],
            total_unread,
        })
    }
}
