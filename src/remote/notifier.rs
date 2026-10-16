//! Typed agent events for the phone, the push decision, plus a notifier
//! decorator.
//!
//! The desktop already posts OS notifications on the active→settled edge
//! ([`crate::app::state::AppState::take_finish_notifications`]). Those
//! [`Notification`]s carry no session identity, so the phone feed cannot be
//! driven from them directly. Instead the [`crate::remote::bridge`] derives
//! **typed** [`AgentEvent`]s from the very same status edge, using the arming
//! model here — which mirrors the TUI's notification semantics exactly:
//!
//! * an *active* status (starting/running/working) arms the session;
//! * the first *settled* status after that (idle/completed → finished,
//!   waiting/needs-attention → needs-input, failed → error) fires one event and
//!   disarms, so a quiet agent never re-fires until it works again.
//!
//! # The push decision (remote-control-fglt)
//!
//! The Mac, not the relay, decides when the phone gets a push notification.
//! [`PushDecider`] layers that decision on top of the same arming edge:
//!
//! * Only three classes ever push ([`PushClass`]): **needs input**
//!   (waiting-for-input / needs-attention, which includes permission prompts),
//!   **error** (failed) and **done** (settled idle/completed). No other status,
//!   transcript item, activity or envelope ever produces a push.
//! * Needs-input and error push **immediately**, once per active→settled edge
//!   (the arming model above). The one exception is in the bridge: a Claude
//!   needs-input whose question preview is still being settled is held up to
//!   `PROMPT_SETTLE_MS` so the push carries the question, not an empty body.
//! * Sealing and transport live in [`crate::remote::push`]; the bridge seals
//!   each decided [`PushIntent`] for the active pairing.
//! * Done is **debounced**: it pushes only once the agent has stayed settled
//!   idle for the configured window (`[remote].push_done_debounce_secs`,
//!   default 5 s). A return to an active status inside the window cancels it,
//!   and the next settle restarts the window, so a mid-turn
//!   working→idle→working flicker never notifies. Leaving for a neutral status
//!   (stopped, session lost, …) also cancels it. If the agent moves from idle
//!   to needs-input or error *inside* the window, that is the turn's real
//!   ending: that class pushes immediately and the pending done is dropped.
//!   Outside a window the arming model holds: an idle agent that later shows
//!   needs-input without working in between does not push.
//! * Time is the injected clock: the bridge passes the `now_ms` it already
//!   receives from [`crate::contracts::Clock::now_millis`] on every render
//!   tick, and a pending done matures on whichever tick first observes the
//!   window elapsed. Nothing sleeps, so tests drive it with plain numbers.
//! * The startup grace window gates pushes exactly like feed events: the
//!   bridge never reports a settle edge from inside it, so a resumed agent
//!   settling at launch cannot open a done window or push.
//! * A session that disappears while a done is pending never pushes: the
//!   decider only fires for a session the bridge observes on that tick, and
//!   [`PushDecider::retain_sessions`] drops state for sessions that are gone.
//!
//! **The in-app feed is deliberately NOT debounced.** The typed
//! [`AgentEvent`] stream keeps its immediate, one-per-edge semantics
//! (including a `Finished` per flicker): it is cheap, it lands in the phone's
//! activity feed rather than on the lock screen, and the phone dedups by
//! `event_id`. Only the push, the thing that interrupts a person, waits. The
//! done push reuses the `event_id` of the feed `Finished` event that opened
//! its window, so the phone can match a push to the feed entry it already has.
//!
//! **Phone notification settings are not consulted here.** The phone's
//! toggles (needs input / finished / chime) and per-project mute live only on
//! the phone (`NotificationPreferences` in the iOS app); no protocol message
//! syncs them to the desktop. The phone filters what it presents.
//!
//! [`CompositeNotifier`] is the decorator seam named in the integration plan: it
//! wraps the real [`crate::notify::SystemNotifier`] so OS notifications keep
//! flowing unchanged while the remote layer taps the same stream if it wants to.

use std::collections::HashMap;

use crate::contracts::{InterpretedStatus, Notification, Notifier};

use flightdeck_remote_protocol::e2e::{
    truncate_push_text, MAX_PUSH_BODY_BYTES, MAX_PUSH_TITLE_BYTES,
};
use flightdeck_remote_protocol::{
    AgentEvent, DeepLink, EventId, EventKind, PushClass, PushPayload, SessionId,
};

// ---------------------------------------------------------------------------
// Notifier decorator
// ---------------------------------------------------------------------------

/// Wraps an inner [`Notifier`] (production: `SystemNotifier`), delegating every
/// notification unchanged. It exists as the stable seam for the remote layer;
/// the typed-event feed is derived independently by the bridge (a
/// [`Notification`] lacks the session identity a [`DeepLink`] needs), so this
/// decorator deliberately changes nothing about OS-notification behaviour.
pub struct CompositeNotifier<'a> {
    inner: &'a dyn Notifier,
}

impl<'a> CompositeNotifier<'a> {
    /// Wrap an inner notifier.
    pub fn new(inner: &'a dyn Notifier) -> Self {
        CompositeNotifier { inner }
    }
}

impl Notifier for CompositeNotifier<'_> {
    fn notify(&self, notification: &Notification) {
        self.inner.notify(notification);
    }
}

// ---------------------------------------------------------------------------
// Phase + arming (mirrors app::state::notify_phase, on the public status enum)
// ---------------------------------------------------------------------------

/// The notification phase of an interpreted status. Mirrors the private
/// `notify_phase` in `app::state` but is expressed over the public
/// [`InterpretedStatus`] so the remote layer can reuse the exact semantics
/// without touching the core.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// The agent is actively working (arms the session).
    Active,
    /// Settled into a notifiable category.
    Settled(EventClass),
    /// Neither arms nor fires.
    Neutral,
}

/// Which typed event a settled edge produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventClass {
    /// The agent finished its turn.
    Finished,
    /// The agent stopped and needs the human.
    NeedsInput,
    /// The agent errored out.
    Error,
}

fn phase(status: InterpretedStatus) -> Phase {
    match status {
        InterpretedStatus::Starting | InterpretedStatus::Running | InterpretedStatus::Working => {
            Phase::Active
        }
        InterpretedStatus::Idle | InterpretedStatus::Completed => {
            Phase::Settled(EventClass::Finished)
        }
        InterpretedStatus::WaitingForInput | InterpretedStatus::NeedsAttention => {
            Phase::Settled(EventClass::NeedsInput)
        }
        InterpretedStatus::Failed => Phase::Settled(EventClass::Error),
        InterpretedStatus::Stopped
        | InterpretedStatus::SessionLost
        | InterpretedStatus::Recovered
        | InterpretedStatus::Unknown => Phase::Neutral,
    }
}

/// Per-session arming state for edge-detected events.
#[derive(Debug, Clone, Copy, Default)]
pub struct EventArming {
    armed: bool,
}

impl EventArming {
    /// Observe the current interpreted status and return a typed event class on
    /// the arming edge (active → settled). Arming is always updated, so a caller
    /// that suppresses events during a startup grace window still tracks state
    /// correctly and only *new* finishes after the window fire.
    pub fn observe(&mut self, status: InterpretedStatus) -> Option<EventClass> {
        match phase(status) {
            Phase::Active => {
                self.armed = true;
                None
            }
            Phase::Neutral => {
                self.armed = false;
                None
            }
            Phase::Settled(class) => {
                let was_armed = self.armed;
                self.armed = false;
                was_armed.then_some(class)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Event construction
// ---------------------------------------------------------------------------

/// Context needed to turn an [`EventClass`] into a wire [`AgentEvent`].
pub struct EventContext {
    /// Stable event id.
    pub event_id: EventId,
    /// Where a tap lands.
    pub deep_link: DeepLink,
    /// Wall-clock time of the event.
    pub occurred_at_ms: i64,
    /// Session name, for the title.
    pub session_name: String,
    /// Preview text (the pending question, for needs-input).
    pub preview: Option<String>,
    /// Files changed this turn (for finished).
    pub files_changed: u32,
    /// Whether the branch looks ready to push (informational).
    pub ready_to_push: bool,
    /// Error detail (for error events).
    pub error_message: Option<String>,
}

/// The title shared by the feed event and the push for a settled class:
/// "`<session>` finished its turn" / "needs input" / "hit an error".
fn event_title(class: EventClass, session_name: &str) -> String {
    match class {
        EventClass::Finished => format!("{session_name} finished its turn"),
        EventClass::NeedsInput => format!("{session_name} needs input"),
        EventClass::Error => format!("{session_name} hit an error"),
    }
}

/// The error detail shown when the agent gave none.
const DEFAULT_ERROR_MESSAGE: &str = "the agent hit an error";

/// Build the wire [`AgentEvent`] for a settled edge.
pub fn build_event(class: EventClass, ctx: EventContext) -> AgentEvent {
    let title = event_title(class, &ctx.session_name);
    let kind = match class {
        EventClass::Finished => EventKind::Finished {
            summary: title.clone(),
            files_changed: ctx.files_changed,
            ready_to_push: ctx.ready_to_push,
        },
        EventClass::NeedsInput => EventKind::NeedsInput {
            preview: ctx.preview.clone().unwrap_or_default(),
        },
        EventClass::Error => EventKind::Error {
            message: ctx
                .error_message
                .clone()
                .unwrap_or_else(|| DEFAULT_ERROR_MESSAGE.to_string()),
        },
    };
    AgentEvent {
        event_id: ctx.event_id,
        kind,
        deep_link: ctx.deep_link,
        occurred_at_ms: ctx.occurred_at_ms,
        title,
    }
}

/// A fresh, globally unique event id: a random (version 4) UUID in its
/// canonical hyphenated form.
///
/// Event ids must stay unique across desktop restarts, not just within one
/// run: the phone dedups its feed by `event_id`, and a push uses it as the
/// APNs collapse id and in its AEAD associated data, so a counter that
/// restarts at 1 would let a new event collide with, and silently replace, an
/// old notification still on the lock screen.
pub fn new_event_id() -> EventId {
    use rand_core::RngCore;
    let mut b = [0u8; 16];
    rand_core::OsRng.fill_bytes(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40; // version 4
    b[8] = (b[8] & 0x3f) | 0x80; // RFC 4122 variant
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    EventId::new(format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    ))
}

// ---------------------------------------------------------------------------
// Push decision (remote-control-fglt); see the module docs
// ---------------------------------------------------------------------------

/// The push class a settled class produces.
fn push_class_of(class: EventClass) -> PushClass {
    match class {
        EventClass::Finished => PushClass::Done,
        EventClass::NeedsInput => PushClass::NeedsInput,
        EventClass::Error => PushClass::Error,
    }
}

/// The feed class whose title/body conventions a push reuses.
fn feed_class_of(class: PushClass) -> EventClass {
    match class {
        PushClass::Done => EventClass::Finished,
        PushClass::NeedsInput => EventClass::NeedsInput,
        PushClass::Error => EventClass::Error,
    }
}

/// An active→settled edge as the bridge reports it to [`PushDecider::observe`]:
/// what [`EventArming::observe`] returned, plus the id the bridge gave the feed
/// event for it. The bridge only reports edges it would send, never one inside
/// the startup grace window, so a suppressed settle can never push later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettleEdge {
    /// The settled class.
    pub class: EventClass,
    /// The feed event's id for this edge.
    pub event_id: EventId,
}

/// The decider's verdict for one session on one tick: push this class now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushFire {
    /// What to push.
    pub class: PushClass,
    /// The id to push under: the feed event's id when the push answers that
    /// edge (needs-input/error now, or the `Finished` edge that opened a done
    /// window). `None` when no feed event corresponds (idle turning into
    /// needs-input/error inside a done window); the caller mints a fresh one.
    pub event_id: Option<EventId>,
    /// Clock-ms of the moment the agent settled into this state (for done, the
    /// start of its window, not the tick it matured on).
    pub occurred_at_ms: u64,
}

/// A done push waiting out its debounce window.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingDone {
    since_ms: u64,
    event_id: EventId,
}

/// Decides, per session and per tick, whether the phone gets a push. A pure
/// state machine over the bridge's arming edges and the injected clock; see
/// the module docs for the rules.
#[derive(Debug, Default)]
pub struct PushDecider {
    pending: HashMap<SessionId, PendingDone>,
}

impl PushDecider {
    /// An empty decider (nothing pending).
    pub fn new() -> Self {
        Self::default()
    }

    /// Observe one session on one tick and return a push to send, if any.
    ///
    /// `status` is the session's current interpreted status, `edge` what
    /// [`EventArming::observe`] produced for it this tick (only when the bridge
    /// would send it, i.e. outside the startup grace window), `now_ms` the
    /// injected clock and `done_debounce_ms` the effective
    /// `[remote].push_done_debounce_secs` for the session's project, in ms.
    /// Call it for every live session on every tick: a pending done only
    /// matures on a tick that observes its session.
    pub fn observe(
        &mut self,
        session: &SessionId,
        status: InterpretedStatus,
        edge: Option<SettleEdge>,
        now_ms: u64,
        done_debounce_ms: u64,
    ) -> Option<PushFire> {
        if let Some(edge) = edge {
            match edge.class {
                EventClass::NeedsInput | EventClass::Error => {
                    self.pending.remove(session);
                    return Some(PushFire {
                        class: push_class_of(edge.class),
                        event_id: Some(edge.event_id),
                        occurred_at_ms: now_ms,
                    });
                }
                EventClass::Finished => {
                    // (Re)start the window; a flicker's second settle lands here.
                    self.pending.insert(
                        session.clone(),
                        PendingDone {
                            since_ms: now_ms,
                            event_id: edge.event_id,
                        },
                    );
                }
            }
        }
        match phase(status) {
            Phase::Settled(EventClass::Finished) => {
                let due = self
                    .pending
                    .get(session)
                    .is_some_and(|p| now_ms.saturating_sub(p.since_ms) >= done_debounce_ms);
                if !due {
                    return None;
                }
                let p = self.pending.remove(session)?;
                Some(PushFire {
                    class: PushClass::Done,
                    event_id: Some(p.event_id),
                    occurred_at_ms: p.since_ms,
                })
            }
            // Idle turned into a question or an error inside the done window:
            // that is how this turn really ended. No arming edge fires here (the
            // idle settle disarmed), so without a pending done this is a quiet
            // agent changing state and stays silent.
            Phase::Settled(class) => self.pending.remove(session).map(|_| PushFire {
                class: push_class_of(class),
                event_id: None,
                occurred_at_ms: now_ms,
            }),
            // Back to work (cancels; the next settle restarts the window), or
            // stopped / lost / unknown (cancels for good).
            Phase::Active | Phase::Neutral => {
                self.pending.remove(session);
                None
            }
        }
    }

    /// Forget every session `keep` rejects. Called once per tick with the
    /// sessions that still exist, so a session removed while a done is pending
    /// can never push (and a recycled id never inherits stale state).
    pub fn retain_sessions(&mut self, mut keep: impl FnMut(&SessionId) -> bool) {
        self.pending.retain(|sid, _| keep(sid));
    }

    /// Whether `session` has a done push waiting out its window.
    pub fn has_pending_done(&self, session: &SessionId) -> bool {
        self.pending.contains_key(session)
    }
}

/// A decided push, ready to seal into a [`PushPayload`] (spec §7.2,
/// [`Self::to_payload`]) and send to the relay as `push_notify`. Title and body
/// are already truncated to the protocol's byte limits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushIntent {
    /// What kind of push this is (the frame's only plaintext hint).
    pub class: PushClass,
    /// Unique id: AEAD associated data and APNs collapse id.
    pub event_id: EventId,
    /// e.g. "fd-api needs input"; at most [`MAX_PUSH_TITLE_BYTES`].
    pub title: String,
    /// The pending question, the error detail, or what the turn left behind;
    /// may be empty; at most [`MAX_PUSH_BODY_BYTES`].
    pub body: String,
    /// Where a tap lands (project + session identity).
    pub deep_link: DeepLink,
    /// The project's display name, when known.
    pub project_name: Option<String>,
    /// When the agent settled (ms from the injected clock).
    pub occurred_at_ms: i64,
}

/// Build the [`PushIntent`] for a decided push, reusing [`build_event`]'s title
/// and preview conventions. `ctx.event_id`/`occurred_at_ms` are taken as given.
pub fn build_push_intent(
    class: PushClass,
    ctx: EventContext,
    project_name: Option<String>,
) -> PushIntent {
    let title = event_title(feed_class_of(class), &ctx.session_name);
    let body = match class {
        PushClass::NeedsInput => ctx.preview.unwrap_or_default(),
        PushClass::Error => ctx
            .error_message
            .unwrap_or_else(|| DEFAULT_ERROR_MESSAGE.to_string()),
        PushClass::Done => done_body(ctx.files_changed, ctx.ready_to_push),
    };
    PushIntent {
        class,
        event_id: ctx.event_id,
        title: truncate_push_text(&title, MAX_PUSH_TITLE_BYTES),
        body: truncate_push_text(&body, MAX_PUSH_BODY_BYTES),
        deep_link: ctx.deep_link,
        project_name,
        occurred_at_ms: ctx.occurred_at_ms,
    }
}

impl PushIntent {
    /// The sealed plaintext for this push (spec §7.2): the intent's fields
    /// verbatim. Title and body are already within their byte limits.
    pub fn to_payload(&self) -> PushPayload {
        PushPayload {
            event_id: self.event_id.clone(),
            class: self.class,
            title: self.title.clone(),
            body: self.body.clone(),
            deep_link: self.deep_link.clone(),
            project_name: self.project_name.clone(),
            occurred_at_ms: self.occurred_at_ms,
        }
    }
}

/// The done push's body: what the turn left behind, or nothing.
fn done_body(files_changed: u32, ready_to_push: bool) -> String {
    match (files_changed, ready_to_push) {
        (0, true) => "Ready to push".to_string(),
        (0, false) => String::new(),
        (1, _) => "1 file changed".to_string(),
        (n, _) => format!("{n} files changed"),
    }
}

#[cfg(test)]
mod tests;
