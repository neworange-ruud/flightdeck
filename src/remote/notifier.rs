//! Typed agent events for the phone, plus a notifier decorator.
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
//! [`CompositeNotifier`] is the decorator seam named in the integration plan: it
//! wraps the real [`crate::notify::SystemNotifier`] so OS notifications keep
//! flowing unchanged while the remote layer taps the same stream if it wants to.

use crate::contracts::{InterpretedStatus, Notification, Notifier};

use flightdeck_remote_protocol::{AgentEvent, DeepLink, EventId, EventKind};

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

/// Build the wire [`AgentEvent`] for a settled edge.
pub fn build_event(class: EventClass, ctx: EventContext) -> AgentEvent {
    let (kind, title) = match class {
        EventClass::Finished => (
            EventKind::Finished {
                summary: format!("{} finished its turn", ctx.session_name),
                files_changed: ctx.files_changed,
                ready_to_push: ctx.ready_to_push,
            },
            format!("{} finished its turn", ctx.session_name),
        ),
        EventClass::NeedsInput => (
            EventKind::NeedsInput {
                preview: ctx.preview.clone().unwrap_or_default(),
            },
            format!("{} needs input", ctx.session_name),
        ),
        EventClass::Error => (
            EventKind::Error {
                message: ctx
                    .error_message
                    .clone()
                    .unwrap_or_else(|| "the agent hit an error".to_string()),
            },
            format!("{} hit an error", ctx.session_name),
        ),
    };
    AgentEvent {
        event_id: ctx.event_id,
        kind,
        deep_link: ctx.deep_link,
        occurred_at_ms: ctx.occurred_at_ms,
        title,
    }
}

#[cfg(test)]
mod tests;
