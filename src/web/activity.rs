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
//! ## The reason string, and what the host may not say
//!
//! [`super::protocol::ActivityEvent::reason`] carries the one part of a feed row
//! a user actually reads: the status pair alone does not distinguish *why* an
//! agent moved from `in progress` to `error`. [`observe`] is the single place
//! that decides what goes in it, and it is deliberately stingy. The design's
//! rows show three kinds of reason (artboard 2e) and the host can only be honest
//! about some of them:
//!
//! | Artboard row | What the host really knows |
//! | --- | --- |
//! | `agent exited (code 1)` | [`ProcessState::Exited`] carries the code — supplied verbatim. |
//! | `set by hand on the desktop` | The manual override moved — a fact, supplied. |
//! | `Codex CLI reports no lifecycle` | [`crate::agents::setup::status_backend`] says so — supplied. |
//! | `asked a question` | **Not knowable.** The status hook writes `waiting`; *why* it is waiting never reaches the host. |
//! | `finished, 18 files touched` | **Not knowable by [`observe`].** The count comes from git, one tab at a time, at the finish edge — see the next section. |
//!
//! `asked a question` gets an **empty** reason, permanently. §5.1's "unknown
//! stays unknown" applies to the reason exactly as it applies to the statuses,
//! and a plausible number is worse than no number: a user who trusts "18 files
//! touched" and acts on it has been misled by us, not by their agent.
//!
//! ## The finish edge asks git itself
//!
//! The file count is the one unknowable row that *can* be made knowable, and
//! the reason it took a second mechanism is worth stating: the git-status cache
//! ([`crate::tui::render::GitStatusCache`]) is refreshed every
//! `GIT_REFRESH_EVERY` ticks **for the active project only**, so a number read
//! out of it at transition time would be stale for the project on screen and
//! simply absent for a project nobody is looking at. Widening that periodic
//! refresh to every open project would buy the count by running git for every
//! project every N ticks forever — a permanent cost for a row that appears
//! when a session finishes, which is rare.
//!
//! So the refresh is **scoped and one-shot**: exactly one
//! `git status --porcelain`, for exactly the tab whose row is being written, at
//! exactly the moment its agent finished. [`wants_file_count`] decides which
//! edges earn one (a `finished`-tier move whose reason is still empty — a
//! manual override or an exit code already outranks it, see [`observe`]);
//! [`file_count`] is the single git call, behind [`crate::contracts::GitExecutor`]
//! like every other side effect here; and [`PendingFinishes`] holds the row
//! while the call runs off the event loop (SPECS §21 — git status never blocks
//! the UI thread).
//!
//! **A parked row is never lost, and never padded.**
//! [`PendingFinishes::resolve`] records it with the count git reported;
//! `resolve` with `None` — git failed, the worktree is gone, the repo is locked
//! — and [`PendingFinishes::expire`] past [`FINISH_COUNT_DEADLINE_MS`] both
//! record it with the **empty** reason it would have had before any of this
//! existed. Slow and broken degrade to exactly the same honest row, which is
//! why the deadline can be short.
//!
//! Because the row is recorded when the count lands rather than when the edge
//! was seen, [`PendingFinishes`] carries the edge's own `at_ms` into
//! [`ActivityStore::record_at`]. The timestamp a user reads is when the agent
//! finished, not when git got round to answering.
//!
//! ## A second record, not the only one
//!
//! This is **not** the desktop's sole source of lifecycle awareness. The
//! desktop posts OS notifications from the very same interpreted-status
//! transitions, via [`crate::app::state::AppState::take_finish_notifications`]
//! (which watches `notify_phase(interpreted)` transitions and hands the result
//! to a [`crate::contracts::Notifier`]).
//!
//! Both records are driven from the same signal —
//! [`crate::app::state::AppState::take_status_transitions`] reads the identical
//! `RuntimeTab::display_status` value `notify_phase` classifies — but the feed
//! **tees the source rather than consuming the notifications**. It has to:
//! `take_finish_notifications` is destructive (it spends each tab's
//! `notify_armed` on every settled edge) and it drops everything the user
//! disabled in `[notifications]` or that the startup grace window suppressed, so
//! a feed built from its *output* would have holes in exactly the places D11
//! exists to cover. The two therefore keep separate per-tab edge memory
//! (`notify_armed` for the desktop, `activity_seen` for the feed) and neither can
//! starve the other, whatever order the event loop calls them in.
//!
//! The feed is also deliberately **wider** than the notifications: §5.1 gives it
//! a `quiet` tier for moves that would never earn an alert (a manual override,
//! `unknown → unknown`), because a browser tab has nothing else to learn them
//! from.

use std::collections::VecDeque;
use std::path::Path;

use crate::agents::status::DisplayStatus;
use crate::contracts::{Clock, GitExecutor, InterpretedStatus, ManualStatus, ProcessState, TabId};
use crate::git::status::parse_porcelain_changes;

use super::protocol::{
    Ack, AckOutcome, ActivityEvent, ActivityTier, Command, EventId, ProjectId, StatusBucket,
};

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
        self.record_at(clock, clock.now_millis() as i64, transition)
    }

    /// [`ActivityStore::record`] for a transition that was **observed earlier
    /// than it is being recorded**: `at_ms` is the moment the status actually
    /// moved, `clock` is still the current time and is used only to enforce
    /// retention.
    ///
    /// The one caller that needs the two to differ is [`PendingFinishes`]: a
    /// finished row waits for git to count its files, so it arrives a moment
    /// late. Stamping it with the edge's own time keeps `at_ms` telling the
    /// truth about when the agent finished, and leaves the small arrival-order
    /// wrinkle where this store already documents it (see
    /// [`ActivityStore::record`]'s doc).
    pub fn record_at(&mut self, clock: &dyn Clock, at_ms: i64, transition: Transition) -> EventId {
        self.next_seq += 1;
        let event_id = EventId::new(format!("evt-{}", self.next_seq));

        let bucket = StatusBucket::from_interpreted(transition.to);
        let tier = ActivityTier::for_bucket(bucket);

        let event = ActivityEvent {
            event_id: event_id.clone(),
            at_ms,
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

// ===========================================================================
// From a desktop status change to an honest feed row (§5.1)
// ===========================================================================

/// Reason for a transition the user caused themselves (artboard 2e).
const REASON_MANUAL: &str = "set by hand on the desktop";

/// Reason for an agent that never came up at all — distinct from a non-zero
/// exit, and not something `to: Failed` alone can tell you.
const REASON_SPAWN_FAILED: &str = "agent failed to start";

/// The honest wire values for one observed desktop status change: what
/// [`Transition`]'s `from`, `to`, `manual` and `reason` may say about it.
///
/// Separated from [`Transition`] because the caller supplies the project and
/// session identity (which this module cannot know) while this module owns the
/// honesty policy (which the event loop should not have to re-derive).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observed {
    /// Status before the change, as the feed may report it.
    pub from: InterpretedStatus,
    /// Status after it, as the feed may report it.
    pub to: InterpretedStatus,
    /// The manual override in force afterwards, if any.
    pub manual: Option<ManualStatus>,
    /// Why, in the design's words — **empty when the host has nothing honest to
    /// say**. See the module doc's reason-string table.
    pub reason: String,
}

/// Turn one observed change in a tab's [`DisplayStatus`] into the values a feed
/// row may honestly carry (§5.1).
///
/// `lifecycle_reporting` is the fact
/// [`crate::web::stream::lifecycle_reporting`] computes from
/// [`crate::agents::setup::status_backend`] — *not* a guess about whether the
/// agent happens to have said anything lately. When it is `false` the returned
/// pair is **always** `unknown → unknown`, whatever the process state implies:
/// §5.1 requires a credible "we don't know" for an agent with no lifecycle
/// hooks, and `Exited(0)` → "completed" is precisely the inference it forbids.
/// The process state still gets to explain the *reason*, because "the agent
/// exited with code 1" is an observation rather than an interpretation of an
/// agent's internal state.
///
/// Reason precedence, most specific first:
///
/// 1. **The manual override moved** → [`REASON_MANUAL`]. The user did this, so
///    nothing else is a better explanation — including a process event that
///    landed on the same tick, which is rare and is why this is documented
///    rather than merged.
/// 2. **The agent reports no lifecycle** → `"<Agent> reports no lifecycle"`,
///    matching the browser's own `lifecycleNote` wording so a feed row and a
///    sidebar row cannot disagree about the same agent.
/// 3. **The process ended** → `"agent exited (code N)"` from
///    [`ProcessState::Exited`], or [`REASON_SPAWN_FAILED`] for
///    [`ProcessState::Failed`].
/// 4. **Otherwise** → empty. Notably `to: WaitingForInput` gets *no* reason: the
///    design's `asked a question` is not a fact any hook reports.
pub fn observe(
    was: DisplayStatus,
    now: DisplayStatus,
    agent_display_name: &str,
    lifecycle_reporting: bool,
) -> Observed {
    let reason = if was.manual != now.manual {
        REASON_MANUAL.to_string()
    } else if !lifecycle_reporting {
        format!("{agent_display_name} reports no lifecycle")
    } else {
        match now.process {
            ProcessState::Exited(code) => format!("agent exited (code {code})"),
            ProcessState::Failed => REASON_SPAWN_FAILED.to_string(),
            // Everything else is already said by `to`. Restating it would be
            // noise, and inventing anything more would be a guess.
            ProcessState::NotStarted
            | ProcessState::Starting
            | ProcessState::Running
            | ProcessState::Stopped
            | ProcessState::Lost => String::new(),
        }
    };

    let (from, to) = if lifecycle_reporting {
        (was.interpreted, now.interpreted)
    } else {
        (InterpretedStatus::Unknown, InterpretedStatus::Unknown)
    };

    Observed {
        from,
        to,
        manual: now.manual,
        reason,
    }
}

// ===========================================================================
// `finished, N files touched`: the one-shot git refresh at the finish edge
// ===========================================================================

/// How long a finished row waits for its file count before it is recorded
/// without the clause (see the module doc's "the finish edge asks git itself").
///
/// Short on purpose. The call it waits for is a single `git status --porcelain`
/// on one worktree, so anything past this is a repo that is locked, huge, or on
/// a network filesystem — and in every one of those cases the row a user wants
/// is the one that arrives, without a number, rather than the one that never
/// arrives at all.
pub const FINISH_COUNT_DEADLINE_MS: i64 = 2_000;

/// The design's `finished, 18 files touched` (artboard 2e), for the count git
/// actually reported. `0` is a real answer and is said plainly: an agent that
/// finished a turn without changing anything is a fact worth reading.
fn reason_finished(files: u32) -> String {
    match files {
        1 => "finished, 1 file touched".to_string(),
        n => format!("finished, {n} files touched"),
    }
}

/// Whether this observed change is the finish edge whose row wants a file
/// count.
///
/// Two conditions, both load-bearing. The tier must be
/// [`ActivityTier::Finished`] — derived through the same
/// [`StatusBucket::from_interpreted`]/[`ActivityTier::for_bucket`] pair the
/// recorded event's own tier comes from, so "the row that says finished" and
/// "the row that gets a count" cannot drift apart. And the reason must still be
/// empty: [`observe`]'s precedence already gave a better explanation to a
/// manual override, an exit code, and a no-lifecycle agent, and none of those
/// should be replaced by a file count.
pub fn wants_file_count(observed: &Observed) -> bool {
    observed.reason.is_empty()
        && ActivityTier::for_bucket(StatusBucket::from_interpreted(observed.to))
            == ActivityTier::Finished
}

/// Ask git how many files one worktree has touched, **right now**.
///
/// One `git status --porcelain` through the [`GitExecutor`] seam, counted by
/// [`parse_porcelain_changes`] — the same parse the git bar's own counts come
/// from, so a feed row and the status panel can never disagree about the same
/// worktree.
///
/// `None`, never a zero and never a guess, when git could not answer: the
/// worktree is gone, the repo is locked, git is not on `PATH`. The caller
/// records the row without the clause.
pub fn file_count(git: &dyn GitExecutor, worktree: &Path) -> Option<u32> {
    let porcelain = git.status_porcelain(worktree).ok()?;
    Some(parse_porcelain_changes(&porcelain).total())
}

/// Identifies one outstanding file-count request, so the answer that comes back
/// off a worker thread lands on the row that asked for it. Never reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FinishCountId(u64);

/// One feed row held back while git counts its files.
#[derive(Debug)]
struct PendingFinish {
    id: FinishCountId,
    /// When the status actually moved — carried into
    /// [`ActivityStore::record_at`] so the row is stamped with the finish, not
    /// with git's reply.
    at_ms: i64,
    transition: Transition,
}

/// The finished rows waiting on their one-shot git refresh (see the module
/// doc's "the finish edge asks git itself").
///
/// Deliberately holds the *whole* [`Transition`] rather than amending an
/// already-recorded event: `crate::web::stream::deltas` matches feed entries by
/// id and emits one only for an id the browser has not seen, so a row published
/// empty and corrected later would reach an open tab as nothing at all. A row
/// therefore enters the store exactly once, complete.
#[derive(Debug, Default)]
pub struct PendingFinishes {
    pending: Vec<PendingFinish>,
    next_seq: u64,
}

impl PendingFinishes {
    /// Nothing waiting.
    pub fn new() -> Self {
        Self::default()
    }

    /// How many rows are currently held.
    pub fn len(&self) -> usize {
        self.pending.len()
    }

    /// True when nothing is held.
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Hold `transition` — observed at `at_ms` — until its count arrives.
    /// Returns the id the answer must carry back.
    pub fn park(&mut self, at_ms: i64, transition: Transition) -> FinishCountId {
        self.next_seq += 1;
        let id = FinishCountId(self.next_seq);
        self.pending.push(PendingFinish {
            id,
            at_ms,
            transition,
        });
        id
    }

    /// Record the row `id` was holding, with `files` if git answered and
    /// without the clause if it did not.
    ///
    /// `None` for an id that is no longer held — it already expired, or the
    /// answer arrived twice — which is a no-op rather than an error.
    pub fn resolve(
        &mut self,
        store: &mut ActivityStore,
        clock: &dyn Clock,
        id: FinishCountId,
        files: Option<u32>,
    ) -> Option<EventId> {
        let index = self.pending.iter().position(|p| p.id == id)?;
        let mut held = self.pending.remove(index);
        if let Some(files) = files {
            held.transition.reason = reason_finished(files);
        }
        Some(store.record_at(clock, held.at_ms, held.transition))
    }

    /// Give up on every row that has waited [`FINISH_COUNT_DEADLINE_MS`] or
    /// longer, recording each without the clause. Returns how many were let go.
    ///
    /// This is what makes the wait safe: a worker that never reports back —
    /// killed, panicking, blocked on a repo lock nobody releases — costs a
    /// missing clause, never a missing row.
    pub fn expire(&mut self, store: &mut ActivityStore, clock: &dyn Clock, now_ms: i64) -> usize {
        let deadline = now_ms - FINISH_COUNT_DEADLINE_MS;
        // Drained in park order, so the rows that waited longest are recorded
        // first — the same arrival order the event loop would have produced.
        let (expired, held): (Vec<_>, Vec<_>) = std::mem::take(&mut self.pending)
            .into_iter()
            .partition(|p| p.at_ms <= deadline);
        self.pending = held;
        let count = expired.len();
        for p in expired {
            store.record_at(clock, p.at_ms, p.transition);
        }
        count
    }
}

// ===========================================================================
// Read-marking, from the browser's command frame (D11)
// ===========================================================================

/// The `event_ids` argument of
/// [`crate::web::protocol::command::MARK_ACTIVITY_READ`].
const ARG_EVENT_IDS: &str = "event_ids";

/// Apply one [`crate::web::protocol::command::MARK_ACTIVITY_READ`] frame and
/// return the [`Ack`] the event loop owes the browser that sent it.
///
/// The read flag lives on the **host**, not in the tab, and that is the whole
/// point: D11 makes the feed the browser's only notification channel, so two
/// tabs (or the same tab reopened tomorrow) must agree about what has already
/// been seen. A tab that marks the feed read here changes what every later
/// [`crate::web::protocol::Snapshot`] backfills.
///
/// Argument shapes, and what each honestly deserves:
///
/// - **No `args` at all** → mark everything retained read. "Mark all read" has
///   no ids to name, so its absence is the request rather than a malformed one.
/// - **`event_ids: [..]`** → mark those. Ids the store no longer retains are a
///   no-op, not an error ([`ActivityStore::mark_read`]) — they were evicted
///   under §5.1's bounds while the tab still held them, which is expected. The
///   ack says how many missed so a browser is not silently misled.
/// - **`event_ids: []`** → [`AckOutcome::Ignored`]. Nothing was asked for and
///   nothing was done; claiming `Applied` would be a small lie.
/// - **A list where nothing matched** → [`AckOutcome::Ignored`] too, for the
///   same reason.
/// - **`event_ids` present but not a list of strings** → [`AckOutcome::Rejected`]
///   with the detail. A malformed frame is the browser's bug and it deserves to
///   hear about it rather than to see a silent success.
pub fn apply_mark_read(store: &mut ActivityStore, command: &Command) -> Ack {
    let rejected = |detail: String| Ack {
        seq: command.seq,
        outcome: AckOutcome::Rejected,
        detail: Some(detail),
    };
    let ignored = |detail: &str| Ack {
        seq: command.seq,
        outcome: AckOutcome::Ignored,
        detail: Some(detail.to_string()),
    };

    let ids: Vec<EventId> = match command.args.as_ref() {
        None => {
            store.mark_all_read();
            return Ack {
                seq: command.seq,
                outcome: AckOutcome::Applied,
                detail: None,
            };
        }
        Some(args) => match args.get(ARG_EVENT_IDS) {
            None => {
                store.mark_all_read();
                return Ack {
                    seq: command.seq,
                    outcome: AckOutcome::Applied,
                    detail: None,
                };
            }
            Some(serde_json::Value::Array(values)) => {
                let mut ids = Vec::with_capacity(values.len());
                for value in values {
                    match value.as_str() {
                        Some(id) => ids.push(EventId::new(id)),
                        None => {
                            return rejected(format!(
                                "{ARG_EVENT_IDS} must be a list of event ids, but one entry was {value}"
                            ))
                        }
                    }
                }
                ids
            }
            Some(other) => {
                return rejected(format!(
                    "{ARG_EVENT_IDS} must be a list of event ids, not {other}"
                ))
            }
        },
    };

    if ids.is_empty() {
        return ignored("no event ids given");
    }
    let marked = ids.iter().filter(|id| store.mark_read(id)).count();
    if marked == 0 {
        return ignored("no retained event matched");
    }
    Ack {
        seq: command.seq,
        outcome: AckOutcome::Applied,
        detail: (marked < ids.len()).then(|| {
            format!(
                "{} of {} events are no longer retained",
                ids.len() - marked,
                ids.len()
            )
        }),
    }
}
