//! FlightDeck Web wire protocol **v1** — versioned JSON over one WebSocket
//! (`specs/WEB_INTERFACE.md` D12).
//!
//! This module is **types only**: serde shapes, a version constant, and the
//! small pure helpers that keep the wire encoding defined in exactly one place.
//! No I/O, no runtime, no server. [`super::server`] owns the socket,
//! [`super::stream`] owns the byte pump, [`super::replay`] owns the ring buffer.
//!
//! # Why this is not the phone protocol
//!
//! [`flightdeck_remote_protocol`] exists to solve problems this surface does not
//! have: an untrusted relay in the middle (hence E2E envelopes), lossy mobile
//! links with queued delivery (hence per-pairing sequence numbers, resume and
//! acks), and a *curated* read-mostly phone view (hence cleaned transcripts
//! instead of PTY bytes). The browser sits on **one trusted socket on the local
//! network** and wants **full fidelity**. D12 therefore keeps the two protocols
//! separate, so web work can never destabilise the phone wire format or its iOS
//! Swift mirror.
//!
//! What it *does* borrow is the **vocabulary**, and it borrows it by reusing the
//! domain types rather than restating them: [`InterpretedStatus`],
//! [`ManualStatus`] and [`TabId`] travel on this wire as themselves, encoded
//! through the same `as_str`/`from_str_lossy` labels the TUI and `state.json`
//! already use. That is the concrete answer to D12's accepted cost ("`AgentStatus`
//! and git-detail semantics will exist in two places and must not drift"): for
//! status there is only one place, and `tests.rs` asserts every variant survives
//! the round trip so a new state cannot be added on one side only.
//!
//! # Message flow
//!
//! ```text
//!   browser                                          host (FlightDeck)
//!      │
//!      │  Attach { protocol_version, seat, cursors[] }
//!      ├────────────────────────────────────────────────────────────►
//!      │                                    version check ─┬─ mismatch
//!      │  Error { code: version_mismatch }                 │
//!      ◄───────────────────────────────────────────────────┘
//!      │
//!      │  Snapshot { protocol_version, seat, projects, geometry, activity, … }
//!      ◄────────────────────────────────────────────────────────────
//!      │
//!      │  Delta  × n        (status, git bar, activity, dialog, seats)
//!      │  TermBytes × n     (offset, data, truncated)
//!      ◄────────────────────────────────────────────────────────────
//!      │
//!      │  Input { seq, terminal_id, data }        Resize { viewport }
//!      ├────────────────────────────────────────────────────────────►
//!      │  Ack { seq, outcome }
//!      ◄────────────────────────────────────────────────────────────
//!      │                              input lock ──┬─ another writer is
//!      │  Ack { seq, rejected }                    │  mid-burst (D14)
//!      │  Error { code: seat_held, incumbent }     │
//!      ◄───────────────────────────────────────────┘
//!      │      … user picks `Take over` → Attach { seat: take_over }
//!      │      … or waits: the lock frees itself once they go quiet
//!      │
//!      ╳  link drops. The browser keeps typing into a local queue (§5.1);
//!      │  it remembers per-terminal `next_offset` and its own `ViewerId`.
//!      │
//!      │  Attach { cursors: [{terminal_id, next_offset}], resume_viewer }
//!      ├────────────────────────────────────────────────────────────►
//!      │  Snapshot { last_input_seq }   ← drop queued input ≤ this, replay rest
//!      │  TermBytes { offset: cursor, truncated: false }   resumed
//!      │  …  or  TermBytes { offset: replay_from, truncated: true }  aged out
//!      ◄────────────────────────────────────────────────────────────
//!      │
//!      │  Shutdown { reason, self_initiated }   ← terminal state, stop retrying
//!      ◄────────────────────────────────────────────────────────────
//! ```
//!
//! # Which decision requires which variant
//!
//! | Type / field | Required by |
//! | --- | --- |
//! | [`PROTOCOL_VERSION`], [`Snapshot::protocol_version`], [`ErrorCode::VersionMismatch`] | D9 bakes the SPA into the binary, so a host update leaves an open tab running old code (turn 2 §4, `remote-control-l7ya`) |
//! | [`TermBytes`] with `offset` + `truncated`, [`Attach::cursors`] | D2 raw PTY bytes, Q2 bounded ring, Q3 resume-from-cursor |
//! | [`Snapshot::geometry`], [`Delta::Geometry`] | D4 as revised by turn 2 — the browser letterboxes the host's grid, so it must be told the grid |
//! | [`Resize`] (viewport only, no target) | D4 — the desktop owns PTY geometry, unconditionally |
//! | [`SeatRequest`], [`Seat`], [`SeatInfo`], [`Delta::Seats`] | D14 as revised — N writers + N observers, the input lock, takeover, read-only watch |
//! | [`ShutdownReason`], `self_initiated` | Q5 — deliberate quit vs network failure, and "I asked for this" |
//! | [`Input::seq`], [`Ack`], [`Snapshot::last_input_seq`] | turn 2 §5.1 — input is queued, never dropped, never reordered, never doubled |
//! | [`Delta::Status`], [`Delta::Git`], [`Delta::Activity`] | D11 activity feed, and the live sidebar/git bar |
//! | [`Delta::DialogOpened`] + [`DialogOrigin`] | D13 — shared dialogs carry who opened them |
//! | [`Command`] (name + free-form args) | D13/D8 — the M2 door: palette, dialogs, git commands |
//! | [`SessionView::lifecycle_reporting`], [`StatusBucket::Unknown`] | turn 2 §5.1 "unknown stays unknown" |
//!
//! # Forward compatibility
//!
//! One policy, applied uniformly, so a peer built from a newer commit degrades
//! instead of disconnecting:
//!
//! 1. **Frame and delta kinds** ([`ServerMsg`], [`ClientMsg`], [`Delta`]) carry a
//!    `#[serde(other)]` catch-all. An unrecognised `type`/`change` parses to
//!    `Unrecognized` and is dropped by the receiver.
//! 2. **Open vocabularies** ([`ErrorCode`], [`ShutdownReason`]) go through a
//!    lossy string conversion, the same shape as
//!    [`InterpretedStatus::from_str_lossy`]. An unknown code becomes `Unknown`
//!    and keeps its human-readable `message`, which is the part the user needs.
//! 3. **Closed vocabularies** ([`Seat`], [`SeatRequest`], [`StatusBucket`],
//!    [`AckOutcome`], [`DialogOutcome`], [`ActivityTier`], [`TerminalRole`]) are
//!    exhaustive by construction. Extending one is a [`PROTOCOL_VERSION`] bump.
//! 4. **Unknown extra fields are ignored.** No type here uses
//!    `deny_unknown_fields`, and every field added after v1 must be
//!    `#[serde(default)]` so an older peer's frames still parse.
//!
//! What is deliberately *not* forward-compatible is a version the host cannot
//! speak at all: that is answered with [`ErrorCode::VersionMismatch`] and a
//! [`VersionMismatch`] payload, because silently half-speaking a protocol is
//! worse than saying "reload to update".

use serde::{Deserialize, Serialize};

use crate::agents::status::DisplayStatus;
use crate::contracts::{InterpretedStatus, ManualStatus, PtySize, TabId};
use crate::terminal::session::TerminalKind;

#[cfg(test)]
mod tests;

// ===========================================================================
// Version negotiation
// ===========================================================================

/// The protocol version this build speaks.
///
/// v1 was the M1 wire format: attach/snapshot/delta/term-bytes/input plus the
/// seat model and the byte cursor. **v2 is D14 as revised for multi-writer
/// input**: a seat is a writer or an observer rather than a controller or an
/// observer, several writers may be seated at once, and [`SeatInfo::holds_input`]
/// says which one holds the input lock right now. That is a closed vocabulary
/// growing a member the peer must understand ([`Seat`], [`SeatRequest`]), which
/// the forward-compatibility policy below makes a version bump by definition.
///
/// It is deliberately the whole range — there is no older web protocol to
/// interoperate with, because the browser SPA ships inside the same binary as
/// the server (D9), and a stale tab is answered with "reload to update" rather
/// than with a half-spoken v1.
///
/// That co-shipping is exactly why the constant matters. The SPA is baked in, so
/// `flightdeck update` on the host while a tab is open leaves that tab running
/// **last version's JavaScript against this version's server**. The browser
/// compares its own baked-in constant against [`Snapshot::protocol_version`] and
/// the host compares [`Attach::protocol_version`] against this one; either side
/// detecting a difference renders "reload to update" rather than mis-parsing
/// frames (turn 2 §4).
///
/// Bump this when a change is **not** covered by the forward-compatibility
/// policy in the module docs — i.e. when a field's meaning changes, a required
/// field appears, or a closed vocabulary grows a member the peer must understand.
pub const PROTOCOL_VERSION: u16 = 2;

/// Oldest version this build can serve. Equal to [`PROTOCOL_VERSION`]: server
/// and SPA ship in the same binary (D9), so there is no older peer to keep.
pub const MIN_SUPPORTED_VERSION: u16 = 2;

/// Newest version this build can serve. Equal to [`PROTOCOL_VERSION`].
pub const MAX_SUPPORTED_VERSION: u16 = 2;

// The version this build prefers must be inside the range it advertises, or
// `check_version` would refuse the very version we send in every `Snapshot`.
// Checked at compile time rather than in a test, because the failure mode is a
// protocol that cannot talk to itself.
const _: () = assert!(MIN_SUPPORTED_VERSION <= PROTOCOL_VERSION);
const _: () = assert!(PROTOCOL_VERSION <= MAX_SUPPORTED_VERSION);

/// A version the local build cannot speak, with everything the peer needs to
/// explain it to the user. Carried by [`WireError::version`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionMismatch {
    /// The version the side that detected the mismatch speaks.
    pub local: u16,
    /// The version the peer advertised.
    pub peer: u16,
    /// Oldest version the detecting side can serve.
    pub min_supported: u16,
    /// Newest version the detecting side can serve.
    pub max_supported: u16,
}

/// Check a peer's advertised version against this build's range.
///
/// Unlike the relay protocol's `negotiate_version`, this does **not** fall back
/// to a lower shared version: server and client ship in the same binary (D9), so
/// a difference is not a negotiation, it is a stale tab. Returning an error is
/// the honest outcome — the fix is a reload, not a downgrade.
pub fn check_version(peer_version: u16) -> Result<u16, VersionMismatch> {
    if (MIN_SUPPORTED_VERSION..=MAX_SUPPORTED_VERSION).contains(&peer_version) {
        Ok(peer_version)
    } else {
        Err(VersionMismatch {
            local: PROTOCOL_VERSION,
            peer: peer_version,
            min_supported: MIN_SUPPORTED_VERSION,
            max_supported: MAX_SUPPORTED_VERSION,
        })
    }
}

// ===========================================================================
// Identifiers
// ===========================================================================

/// Transparent newtypes over `String`: one JSON string on the wire, distinct
/// types in Rust so the compiler stops a [`TerminalId`] reaching a slot that
/// wanted a [`ProjectId`]. Same convention as `flightdeck_remote_protocol::ids`.
macro_rules! web_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            /// Wrap an owned or borrowed string as this id.
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            /// Borrow the underlying string.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_owned())
            }
        }
    };
}

web_id! {
    /// A project folder open in FlightDeck (the project-tab row of the design).
    ProjectId
}
web_id! {
    /// One terminal inside a session: the primary agent, an extra agent, or a
    /// child shell.
    ///
    /// **The host mints this and it must be stable for the terminal's whole
    /// life**, because the replay ring buffer and every byte cursor are keyed by
    /// it (Q2, Q3). A positional id would be a correctness bug, not a cosmetic
    /// one: close child 1 and the old child 2 would inherit its id, and a
    /// resuming viewer would resume the wrong stream at a plausible-looking
    /// offset. `<tab-id>:primary` / `<tab-id>:child:<mint-counter>` satisfies
    /// this; a bare index does not.
    TerminalId
}
web_id! {
    /// One attached browser socket, for the whole life of that socket.
    ///
    /// Presented back in [`Attach::resume_viewer`] after a drop so the host can
    /// tell the browser which of its queued keystrokes already landed
    /// ([`Snapshot::last_input_seq`], turn 2 §5.1).
    ViewerId
}
web_id! {
    /// One entry in the activity feed (D11).
    EventId
}
web_id! {
    /// One open dialog (D13). M2 owns the dialog bodies; v1 carries the identity
    /// and the origin label so the browser can already render "the desktop is
    /// asking something".
    DialogId
}

// ===========================================================================
// Status vocabulary — reused, not restated (D12)
// ===========================================================================

/// Serde adaptor: [`InterpretedStatus`] as the label the rest of FlightDeck
/// already writes (`"working"`, `"needs attention"`, …).
///
/// The domain type has no `Serialize` derive and is main-agent-owned, so this is
/// how the web protocol reuses it instead of forking a parallel enum. It is also
/// the same spelling `state.json`'s `last_known_status` uses, so a status string
/// means one thing everywhere in the product. Unknown labels degrade to
/// [`InterpretedStatus::Unknown`] rather than failing the frame.
mod interpreted_label {
    use super::InterpretedStatus;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(status: &InterpretedStatus, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(status.as_str())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<InterpretedStatus, D::Error> {
        let label = String::deserialize(d)?;
        Ok(InterpretedStatus::from_str_lossy(&label))
    }
}

/// Serde adaptor for an optional [`ManualStatus`], as its label (`"in progress"`,
/// `"waiting"`, `"blocked"`, `"done"`). `null` means no override is set.
///
/// A label the peer does not know deserializes to `None` — a manual override we
/// cannot name is better shown as "no override" than as a guess, since a manual
/// override takes colour priority in the design and a wrong one would mislead.
mod manual_label {
    use super::ManualStatus;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        status: &Option<ManualStatus>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        match status {
            Some(m) => s.serialize_str(m.as_str()),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<ManualStatus>, D::Error> {
        let label = Option::<String>::deserialize(d)?;
        Ok(label.as_deref().and_then(ManualStatus::from_str_lossy))
    }
}

/// The four sidebar buckets the design paints (`in progress` / `idle` /
/// `waiting` / `error`), plus the fifth turn 2 insisted on.
///
/// Derived from [`InterpretedStatus`] by [`StatusBucket::from_interpreted`] and
/// sent *pre-derived* on the wire so the browser never re-implements the mapping
/// — that re-implementation is precisely the drift D12 warns about.
///
/// `Unknown` is not in the original brief's table (which folded Unknown into
/// `idle`/green). Turn 2 §5.1 overrides it: an agent with no lifecycle hooks
/// renders `○` and `unknown → unknown · Codex CLI reports no lifecycle`, because
/// claiming "idle" for a state we never observed is a guess dressed as a fact.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusBucket {
    /// Starting / Running / Working — red indicator, cyan label, spinner.
    InProgress,
    /// Idle / Completed / Stopped / Recovered — green.
    Idle,
    /// WaitingForInput / NeedsAttention — red; the state the product exists for.
    Waiting,
    /// Failed / SessionLost — red.
    Error,
    /// No lifecycle signal was ever received. Renders as unknown, never as idle.
    Unknown,
}

impl StatusBucket {
    /// Map an interpreted status onto its sidebar bucket (design §7, as revised
    /// by turn 2 §5.1 for `Unknown`).
    pub fn from_interpreted(status: InterpretedStatus) -> Self {
        match status {
            InterpretedStatus::Starting
            | InterpretedStatus::Running
            | InterpretedStatus::Working => StatusBucket::InProgress,
            InterpretedStatus::Idle
            | InterpretedStatus::Completed
            | InterpretedStatus::Stopped
            | InterpretedStatus::Recovered => StatusBucket::Idle,
            InterpretedStatus::WaitingForInput | InterpretedStatus::NeedsAttention => {
                StatusBucket::Waiting
            }
            InterpretedStatus::Failed | InterpretedStatus::SessionLost => StatusBucket::Error,
            InterpretedStatus::Unknown => StatusBucket::Unknown,
        }
    }

    /// Urgency rank, **lower is more urgent**, for the project dot and the
    /// activity feed's unread chip.
    ///
    /// The brief's rule is "attention beats busy": needs-input outranks working
    /// outranks manual outranks idle. `Error` sits beside `Waiting` because both
    /// are red and both mean a human is needed. `Unknown` ranks *last* — below
    /// `Idle` — because a status we could not determine must never outrank one we
    /// could; turn 2's "unknown stays unknown" is a rule about display honesty,
    /// not about urgency.
    pub fn rank(self) -> u8 {
        match self {
            StatusBucket::Waiting => 0,
            StatusBucket::Error => 1,
            StatusBucket::InProgress => 2,
            StatusBucket::Idle => 3,
            StatusBucket::Unknown => 4,
        }
    }

    /// The dominant bucket of a group, by [`StatusBucket::rank`]. `None` for an
    /// empty group (a project with no sessions has no dot).
    pub fn rollup(buckets: impl IntoIterator<Item = StatusBucket>) -> Option<StatusBucket> {
        buckets.into_iter().min_by_key(|b| b.rank())
    }
}

/// A session's display-ready status: the same three facts
/// [`DisplayStatus`] carries, plus the pre-derived bucket and the turn timer.
///
/// `interpreted` and `manual` are kept as separate fields on purpose — design §7
/// rule 1: "a manual override takes colour priority but never hides the real
/// lifecycle state. Both must remain readable." A single collapsed field could
/// not render both.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionStatus {
    /// The interpreted lifecycle state, as its FlightDeck label.
    #[serde(with = "interpreted_label")]
    pub interpreted: InterpretedStatus,
    /// The user's manual override, as its label, or `null` when cleared.
    #[serde(with = "manual_label", default)]
    pub manual: Option<ManualStatus>,
    /// Which sidebar bucket `interpreted` falls into. Derived by the host.
    pub bucket: StatusBucket,
    /// Wall-clock seconds in the current (or last) turn, for the row's timer.
    pub running_time_secs: u64,
}

impl SessionStatus {
    /// Build the wire status from the desktop's own combined status.
    ///
    /// This conversion is the anti-drift seam: it is an exhaustive match through
    /// [`StatusBucket::from_interpreted`], so adding a member to
    /// [`InterpretedStatus`] fails to compile here until the web bucket mapping
    /// is updated too.
    pub fn from_display(display: DisplayStatus, running_time_secs: u64) -> Self {
        SessionStatus {
            interpreted: display.interpreted,
            manual: display.manual,
            bucket: StatusBucket::from_interpreted(display.interpreted),
            running_time_secs,
        }
    }
}

// ===========================================================================
// Geometry (D4)
// ===========================================================================

/// A character grid, in cells.
///
/// **The host owns this, unconditionally (D4).** `src/lib.rs`'s
/// `sync_terminal_sizes` calls `resize_if_changed` on the selected tab's
/// terminals **every frame**, so any viewer-set geometry would be reverted
/// within one frame.
/// The browser is told the grid and letterboxes it: natural size, centred, dark
/// margin, with the `120×34 · host owns geometry` chip in the git bar explaining
/// the margin (D4 as revised by turn 2). xterm.js is constructed from these
/// numbers and must not use `FitAddon`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Geometry {
    /// Columns.
    pub cols: u16,
    /// Rows.
    pub rows: u16,
}

impl From<PtySize> for Geometry {
    fn from(size: PtySize) -> Self {
        Geometry {
            cols: size.cols,
            rows: size.rows,
        }
    }
}

/// How many cells the browser could show if it were allowed to choose — which
/// it is not.
///
/// Reported so the host can log a viewer that is clipping the grid, and so the
/// browser's own letterbox maths and the chip agree with what the host believes.
/// It is **never** an input to PTY sizing; see [`Resize`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Viewport {
    /// Columns the browser viewport can display.
    pub cols: u16,
    /// Rows the browser viewport can display.
    pub rows: u16,
}

// ===========================================================================
// Seats (D14)
// ===========================================================================

/// What an attaching browser is asking for.
///
/// The three intents are distinct frames rather than one boolean because the
/// last one is a *deliberate interruption*: artboard 2f requires that taking
/// input away from somebody who is mid-burst is something a human confirmed, so
/// it cannot be the same frame as "seat me, I would like to type".
///
/// This is courtesy, not authorisation: anyone holding the credential can
/// interrupt anyone else. The protocol must not imply an authority it does not
/// have.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeatRequest {
    /// Seat me as a **writer**: I intend to type.
    ///
    /// Always granted — D14 as revised allows several writers at once. It does
    /// not hand over the input lock, which is claimed by typing (see
    /// [`SeatInfo::holds_input`] and [`crate::web::arbiter`]). A writer that
    /// types while another writer's burst is live is refused *that keystroke*,
    /// with [`ErrorCode::SeatHeld`] naming the holder; it keeps its seat.
    Write,
    /// Seat me as a writer **and take the input lock now**, interrupting
    /// whoever holds it.
    ///
    /// Sent only after the user confirmed `Take over` (2f). This is the one
    /// explicit override in the model, and it is deliberately the vocabulary
    /// that already existed rather than a new privilege: no surface, the
    /// desktop included, can cut into a live burst any other way.
    TakeOver,
    /// Watch read-only. Never contends for input, so N observers cost nothing in
    /// arbitration — D14's read-only fan-out is untouched by the revision.
    /// Reachable from both the arriving browser (`Cancel` leaves a live
    /// read-only view) and a writer that would rather stop competing (2f).
    Observe,
}

/// What a viewer currently holds.
///
/// **A seat is a role, not a turn.** Several viewers may be [`Seat::Writing`] at
/// once; which one may type *at this instant* is [`SeatInfo::holds_input`], and
/// it moves between writers as they type and go quiet. Splitting the two is what
/// lets the seat list stay stable while the lock moves several times a minute.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Seat {
    /// This viewer may contend for the input lock. Its [`Input`] is delivered to
    /// the PTY when it holds the lock, and answered
    /// [`AckOutcome::Rejected`] + [`ErrorCode::SeatHeld`] when another writer is
    /// mid-burst — never interleaved into theirs.
    Writing,
    /// This viewer receives everything and sends no input. Its [`Input`] frames,
    /// if any, are answered [`AckOutcome::Ignored`] — never silently dropped,
    /// because §5.1 forbids a keystroke vanishing without a trace.
    Observing,
}

/// One occupant of the viewer chip (2f).
///
/// The identifying detail is what turn 2 asks for: enough for the humans to work
/// out who is who, and no more.
///
/// ## Two facts, not one
///
/// [`SeatInfo::seat`] is the **role** — may this surface type at all — and
/// [`SeatInfo::holds_input`] is the **turn** — is it typing right now. Exactly
/// one row across the whole list can hold input, and it may be free, in which
/// case no row does. Merging them into a single "controlling" flag is what
/// protocol v1 did, and it is the thing D14's revision had to undo: it could not
/// express "three writers, one of them mid-burst".
///
/// ## Why the facts are separate fields as well as one label
///
/// Two surfaces want the same information in two shapes. The viewer chip wants
/// **one line** (`desktop + this tab`), so [`SeatInfo::label`] stays. Artboard
/// 2f's arriving-viewer panel wants **three rows** — `address` / `browser` /
/// `connected` — so each of those is its own field.
///
/// The alternative was for the browser to split `label` on its ` · ` separator,
/// and that is not implementable honestly: the browser half of the label is a
/// user-agent string, which is attacker-supplied free text and can contain
/// anything, the separator included. Splitting untrusted display text is
/// parsing it, and a parse that can be steered by the string it parses gives
/// the wrong answer on demand. So the split belongs **on the wire**, where the
/// host still knows which fact is which, and never in a browser-side parser.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatInfo {
    /// This viewer's id, or `null` for the desktop, which is not a viewer.
    #[serde(default)]
    pub viewer_id: Option<ViewerId>,
    /// One-line label for the chip, e.g. `192.168.2.20 · Chrome on macOS`, or
    /// `desktop`. Rendered verbatim.
    pub label: String,
    /// The address the **host observed on the socket**, e.g. `192.168.2.20`, or
    /// `null` for the desktop row, which arrived over no socket at all.
    ///
    /// Host-observed, never client-supplied: the rule that already governs
    /// [`ClientInfo`] survives this split unchanged. A browser can tell the host
    /// what kind of browser it is; it cannot tell the host where it is.
    #[serde(default)]
    pub address: Option<String>,
    /// What the browser said it is (`Chrome on macOS`), or `null` when it said
    /// nothing and the host recognised nothing in its `User-Agent`.
    ///
    /// **A claim, not a fact.** It is displayed verbatim, sanitised and
    /// length-capped, and it is never parsed, matched on, or allowed to stand in
    /// for anything the host observed. `null` means we do not know — 2f's panel
    /// then drops the `browser` row rather than printing a guess.
    #[serde(default)]
    pub user_agent_label: Option<String>,
    /// Whether this seat may type at all — a writer or an observer.
    pub seat: Seat,
    /// Whether this seat holds the **input lock** right now: the one surface
    /// whose keystrokes are reaching the PTY this instant.
    ///
    /// At most one row in a list is `true`, and none is when the lock is free
    /// (nobody has typed for [`crate::web::arbiter::INPUT_LOCK_IDLE_MS`]). Both
    /// surfaces render it, because both surfaces are writers and both deserve
    /// the same answer to "why did my keys stop working".
    ///
    /// `false` is the [`serde`] default and is the honest reading for a host
    /// that does not send it: it does not claim the lock is free, it claims this
    /// row is not the one holding it, which is true of every row on such a host.
    #[serde(default)]
    pub holds_input: bool,
    /// Wall-clock (unix ms) the seat was taken — "how long it has been
    /// connected", which turn 2 names as fair identifying detail.
    pub since_ms: i64,
    /// True for the row describing the recipient of this frame, so the browser
    /// can say `this tab` instead of repeating its own address back at itself.
    #[serde(default)]
    pub is_you: bool,
}

/// What the browser tells the host about itself, for
/// [`SeatInfo::user_agent_label`] and the chip's [`SeatInfo::label`].
///
/// Optional and advisory: the host owns the address it observed and must not
/// trust a client-supplied one for anything but display. There is deliberately
/// no `address` field here, and [`SeatInfo::address`] is filled from the socket
/// rather than from anything in this struct.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientInfo {
    /// Browser/OS as the browser describes itself, e.g. `Chrome on macOS`.
    #[serde(default)]
    pub user_agent: Option<String>,
    /// A name the user gave this tab, if the UI ever offers one.
    #[serde(default)]
    pub label: Option<String>,
}

// ===========================================================================
// Byte cursors (D2, Q2, Q3)
// ===========================================================================

/// Where a returning viewer wants the stream to resume.
///
/// `next_offset` is the offset of the **next byte the viewer has not seen** —
/// i.e. the total number of bytes it has consumed for that terminal, which is
/// `offset + data.len()` of the last [`TermBytes`] it applied (see
/// [`TermBytes::next_offset`]). Half-open, like a slice bound, so `0` means
/// "everything you still have" and needs no special case.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TermCursor {
    /// The terminal this cursor is for.
    pub terminal_id: TerminalId,
    /// Offset of the first byte the viewer still needs.
    pub next_offset: u64,
}

/// A chunk of raw PTY output (D2), positioned in the terminal's lifetime stream.
///
/// `data` is the **actual PTY bytes**, base64 on the wire. Not a JSON string:
/// PTY output is arbitrary bytes, a chunk boundary can fall inside a UTF-8
/// codepoint or an escape sequence, and JSON strings must be valid UTF-8 — so
/// lossy conversion would corrupt the very escape sequences xterm.js exists to
/// parse. Base64 costs 33% and never lies.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TermBytes {
    /// The terminal that produced these bytes.
    pub terminal_id: TerminalId,
    /// Offset of the **first** byte of `data` within everything this terminal
    /// has ever written. Monotonic per terminal, on **every** frame (Q3) — not
    /// only on resume, because that is what makes the viewer's saved cursor
    /// trustworthy at any moment the link dies.
    pub offset: u64,
    /// The raw bytes. Base64 on the wire, `Vec<u8>` in Rust.
    #[serde(with = "b64")]
    pub data: Vec<u8>,
    /// True when output was discarded from the ring buffer before this viewer
    /// could receive it — its cursor had aged out (Q2/Q3), so `offset` is
    /// greater than the `next_offset` it asked for.
    ///
    /// The viewer must then say it missed output rather than pretending
    /// continuity. Only ever set on the first frame of a resume; a live stream
    /// is never truncated.
    #[serde(default)]
    pub truncated: bool,
}

impl TermBytes {
    /// A frame of live output, continuing the stream (never truncated).
    pub fn live(terminal_id: TerminalId, offset: u64, data: Vec<u8>) -> Self {
        TermBytes {
            terminal_id,
            offset,
            data,
            truncated: false,
        }
    }

    /// The offset a viewer should store after applying this frame, and send back
    /// as [`TermCursor::next_offset`] when it returns.
    pub fn next_offset(&self) -> u64 {
        self.offset + self.data.len() as u64
    }
}

/// Serde adaptor: `Vec<u8>` as standard-padded base64.
///
/// Defined here so [`super::server`] and [`super::stream`] cannot each invent
/// their own encoding — the browser has to agree with exactly one of them.
mod b64 {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine as _;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let encoded = String::deserialize(d)?;
        STANDARD.decode(&encoded).map_err(serde::de::Error::custom)
    }
}

// ===========================================================================
// The state the browser renders
// ===========================================================================

/// What a terminal hosts. Mirrors [`TerminalKind`] so the browser can label the
/// terminal tabs (`agent`, `shell 1`) without the desktop's enum needing serde.
///
/// The [`From`] impl is exhaustive, so a new kind in the terminal layer fails to
/// compile here rather than silently becoming the wrong tab label.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalRole {
    /// The session's primary agent.
    Primary,
    /// An additional agent process in the same worktree (SPECS §19).
    Agent,
    /// A child shell.
    Shell,
}

impl From<TerminalKind> for TerminalRole {
    fn from(kind: TerminalKind) -> Self {
        match kind {
            TerminalKind::Primary => TerminalRole::Primary,
            TerminalKind::Agent => TerminalRole::Agent,
            TerminalKind::Child => TerminalRole::Shell,
        }
    }
}

/// One terminal the browser can attach xterm.js to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalView {
    /// Stable id — see [`TerminalId`] on why it must not be positional.
    pub terminal_id: TerminalId,
    /// The session that owns it. Repeated here (as well as being nested inside
    /// [`SessionView`]) so a [`Delta::TerminalUpsert`] is self-locating.
    pub session_id: TabId,
    /// What it hosts.
    pub role: TerminalRole,
    /// Tab title, e.g. `agent`, `shell 1`.
    pub title: String,
    /// The host's authoritative grid for this terminal (D4).
    pub geometry: Geometry,
    /// Total bytes this terminal has ever written — the end of the stream. A
    /// fresh viewer knows immediately how much history exists.
    pub byte_len: u64,
    /// Oldest offset still in the replay ring (Q2). A [`TermCursor`] below this
    /// has aged out, which is how the host decides [`TermBytes::truncated`], and
    /// how a browser knows it lost output *before* the first frame arrives.
    pub replay_from: u64,
    /// False once the process exited; the browser freezes the caret.
    pub alive: bool,
    /// Exit status, when it exited normally.
    #[serde(default)]
    pub exit_code: Option<i32>,
}

/// Compact git facts for a session row and the git bar
/// (`⎇ flightdeck/fix-login │ +3 ~2 -1 (6 files) │ ↑0 ↓0`).
///
/// The core field names are **deliberately identical** to
/// `flightdeck_remote_protocol::common::GitIndicators`, which is the other half
/// of D12's accepted cost ("git-detail semantics will exist in two places and
/// must not drift"). `tests.rs` asserts that every key the phone protocol emits
/// also appears here with the same value, so a rename on either side is caught
/// by a failing test rather than by a confused reader.
///
/// The two extra fields are what the *web* git bar needs and the phone row does
/// not: a file count, and the difference between "clean" and "not collected yet"
/// — the design renders the latter as a dim `git: ?`, and turn 2 §5.1 puts it in
/// the lifted `--fd-text-quiet` tier precisely because mistaking one for the
/// other would lose a fact.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitBar {
    /// Branch name, if a branch is checked out.
    pub branch: Option<String>,
    /// Added (new) files (`+`).
    pub added: u32,
    /// Modified files (`~`).
    pub modified: u32,
    /// Removed files (`-`).
    pub removed: u32,
    /// Commits ahead of upstream (`↑`).
    pub ahead: u32,
    /// Commits behind upstream (`↓`).
    pub behind: u32,
    /// Commits of drift from the base branch (`drift:n`).
    pub drift: u32,
    /// Whether the branch has an upstream. `false` renders `no-upstream`.
    pub has_upstream: bool,
    /// Number of changed files, for `(6 files)`.
    pub files_changed: u32,
    /// False until git status has been collected for this worktree. Renders
    /// `git: ?`, **not** `clean` — the two mean opposite things.
    pub collected: bool,
}

impl GitBar {
    /// True when there are no uncommitted file changes (renders `clean`). Only
    /// meaningful once [`GitBar::collected`] is true.
    pub fn is_clean(&self) -> bool {
        self.added == 0 && self.modified == 0 && self.removed == 0
    }
}

/// Where a session is in its lifecycle, for the sidebar's special states.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionPhase {
    /// The worktree is still being materialised: spinner plus
    /// `creating worktree…` instead of the agent/status line.
    Creating,
    /// Normal.
    Ready,
}

/// One agent session — one worktree, one branch, one primary agent.
///
/// Identified by [`TabId`], the desktop's own stable Agent Tab id, rather than a
/// web-local session id. There is exactly one identity for a session in this
/// product and the browser uses it, so a feed row, a selection change and a
/// `state.json` entry all name the same thing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionView {
    /// The Agent Tab id.
    pub session_id: TabId,
    /// Owning project.
    pub project_id: ProjectId,
    /// Session name (== worktree == branch leaf), e.g. `fix-login`.
    pub name: String,
    /// Configured agent key, e.g. `claude`.
    pub agent: String,
    /// The agent's display name, e.g. `Claude Code`.
    pub agent_display_name: String,
    /// Lifecycle phase.
    pub phase: SessionPhase,
    /// Display-ready status.
    pub status: SessionStatus,
    /// Git facts for the row and the bar.
    pub git: GitBar,
    /// The session's terminals, in tab order.
    pub terminals: Vec<TerminalView>,
    /// True when this agent reports lifecycle events at all.
    ///
    /// `false` is what makes turn 2 §5.1's honesty requirement renderable: the
    /// browser writes `unknown → unknown · Codex CLI reports no lifecycle` from
    /// this flag plus [`SessionView::agent_display_name`]. The host sends a fact,
    /// not a sentence, so the wording stays in the design's hands.
    pub lifecycle_reporting: bool,
    /// The magenta `[recovered]` chip: this tab was reconstructed after a
    /// restart.
    #[serde(default)]
    pub recovered: bool,
    /// The cyan `[existing]` chip: attached to a branch that already existed.
    #[serde(default)]
    pub attached_existing_branch: bool,
}

/// One open project (a tab in the project row).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectView {
    /// Project id.
    pub project_id: ProjectId,
    /// Folder name, as shown on the tab.
    pub name: String,
    /// Absolute repository root, for the `host only` actions' tooltips (D16).
    pub root: String,
    /// The project's base branch.
    pub base_branch: String,
    /// The precedence-ordered project dot ([`StatusBucket::rollup`]). `null`
    /// when the project has no sessions.
    #[serde(default)]
    pub dot: Option<StatusBucket>,
    /// The project's sessions, in display order.
    pub sessions: Vec<SessionView>,
}

/// The single selected project / session / terminal, shared with the desktop.
///
/// D3: there is one selection for the whole instance, so changing it from the
/// browser moves the desktop too. That is what "remote control" means, and the
/// activity feed's rows say so on hover (`jump · also moves the desktop`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selection {
    /// Selected project, if any.
    #[serde(default)]
    pub project_id: Option<ProjectId>,
    /// Selected session, if any.
    #[serde(default)]
    pub session_id: Option<TabId>,
    /// Selected terminal within the session, if any.
    #[serde(default)]
    pub terminal_id: Option<TerminalId>,
    /// Whether the desktop is in split view (SPECS §19). The browser mirrors it;
    /// toggling it from the browser is M2 (D8).
    #[serde(default)]
    pub split_view: bool,
}

// ===========================================================================
// Activity feed (D11)
// ===========================================================================

/// Urgency tier of a feed entry, driving the unread chip's colour.
///
/// Turn 2 §5.1: three tiers following the existing project-dot precedence —
/// attention beats finished beats quiet — and the chip takes the colour of the
/// most urgent unread event.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityTier {
    /// An agent is waiting or has failed. Someone must look.
    Attention,
    /// An agent finished a turn.
    Finished,
    /// Everything else, including a manual override and `unknown → unknown`.
    Quiet,
}

impl ActivityTier {
    /// The tier a transition *into* `to` belongs to.
    pub fn for_bucket(to: StatusBucket) -> Self {
        match to {
            StatusBucket::Waiting | StatusBucket::Error => ActivityTier::Attention,
            StatusBucket::Idle => ActivityTier::Finished,
            StatusBucket::InProgress | StatusBucket::Unknown => ActivityTier::Quiet,
        }
    }
}

/// One status transition in the activity feed (D11).
///
/// The feed is the **entire** substitute for OS notifications in the browser —
/// Web Push is structurally blocked under D1 — so an entry carries everything
/// needed to render a row and act on it without a second request: project
/// attribution (the session you were not looking at is usually in another
/// project) and both ends of the transition.
///
/// `from` and `to` are full [`InterpretedStatus`] values, not buckets, so
/// `unknown → unknown` is a legal and honest row rather than something the wire
/// format cannot say (turn 2 §5.1).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityEvent {
    /// Stable event id, for read-marking.
    pub event_id: EventId,
    /// Wall-clock time (unix ms) of the transition.
    pub at_ms: i64,
    /// Owning project.
    pub project_id: ProjectId,
    /// Project name, denormalised so a feed row needs no lookup.
    pub project_name: String,
    /// The session that changed.
    pub session_id: TabId,
    /// Session name, denormalised for the same reason.
    pub session_name: String,
    /// Status before the transition.
    #[serde(with = "interpreted_label")]
    pub from: InterpretedStatus,
    /// Status after the transition.
    #[serde(with = "interpreted_label")]
    pub to: InterpretedStatus,
    /// The manual override in force after the transition, if any — the feed
    /// carries manual overrides as well as lifecycle changes.
    #[serde(with = "manual_label", default)]
    pub manual: Option<ManualStatus>,
    /// Why the transition happened, in the design's own words: `"asked a
    /// question"`, `"agent exited (code 1)"`, `"finished, 18 files touched"`
    /// (artboard 2e). This is the part of a feed row a user actually reads, so
    /// it belongs on the wire rather than being reconstructed in the browser
    /// from `from`/`to` — which could not produce "18 files touched" at all.
    ///
    /// Empty when the host has nothing honest to say. It must never be padded
    /// with a guess: turn 2 §5.1's "unknown stays unknown" applies to the
    /// reason exactly as it applies to the statuses.
    #[serde(default)]
    pub reason: String,
    /// Urgency tier for the unread chip.
    pub tier: ActivityTier,
    /// Whether this viewer has seen it. Backfilled events are read or unread
    /// per the host's own record; a fresh tab opens on history, not silence.
    #[serde(default)]
    pub read: bool,
}

// ===========================================================================
// Dialogs (D13) — the M2 door
// ===========================================================================

/// Who opened a dialog.
///
/// D13's origin label is load-bearing, not decoration: the desktop user gets a
/// modal they did not ask for, and the only thing that makes that acceptable is
/// knowing where it came from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "origin", rename_all = "snake_case")]
pub enum DialogOrigin {
    /// Opened on the desktop.
    Desktop,
    /// Opened from a browser. `label` is what the desktop renders after
    /// `opened from browser · `, e.g. `192.168.2.20`.
    Browser {
        /// The viewer that opened it, when it is still attached.
        #[serde(default)]
        viewer_id: Option<ViewerId>,
        /// Display label, rendered verbatim.
        label: String,
    },
}

/// An open dialog, minimally described.
///
/// **M2 owns the dialog family** (D8 puts dialogs out of M1), so v1 carries
/// identity, a machine-readable `kind`, a title and the origin — enough for the
/// browser to render "the desktop is asking something" and for the desktop to
/// render the origin label. `kind` is a `String` rather than an enum precisely so
/// M2 can add `new_agent`, `confirm_abandon`, `config_manager` and the two-step
/// destructive confirmation without a version bump; an unknown kind renders the
/// generic form instead of failing to parse.
///
/// M2 added the typed [`DialogBody`] — fields, options, buttons, and artboard
/// 1g's typed-session-name confirmation as [`ConfirmGate`] — inside the same
/// free-form slot, with no version bump. No `dangerous` flag was needed: what a
/// browser must do about a dangerous answer is the gate on the button that takes
/// it, which is a fact rather than an adjective.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DialogView {
    /// Stable dialog id.
    pub dialog_id: DialogId,
    /// Machine-readable dialog kind, e.g. `new_agent`.
    pub kind: String,
    /// Human-readable title, rendered verbatim.
    pub title: String,
    /// Who opened it (D13).
    pub origin: DialogOrigin,
    /// Free-form payload. Absent in M1; the forward-compatible slot M2's typed
    /// bodies land in without changing this type's shape.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<serde_json::Value>,
}

/// How a dialog closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DialogOutcome {
    /// Confirmed, by either surface (D13).
    Confirmed,
    /// Cancelled, by either surface.
    Cancelled,
    /// Replaced or invalidated without a decision.
    Superseded,
}

/// The shape [`DialogView::body`] carries in this build (D13).
///
/// `body` is a free-form `serde_json::Value` on the wire *by design* — v1 chose
/// that so M2 could add dialog bodies without a version bump — so this type is
/// the documented thing that goes **into** that slot rather than a second
/// `DialogView`. It is deliberately the *shell* artboard 1d describes ("same
/// shell for every dialog: titled accent frame, keyed buttons") rather than one
/// struct per dialog kind: the desktop already renders every prompt from one
/// model (`crate::tui::render::Dialog`), and giving the browser the same model
/// is what keeps the two surfaces from drifting into two dialog systems.
///
/// Artboard 1e's new-agent form is this shell with a `list` (the agent radio),
/// an `input` (the branch) and three `buttons` (`Enter` / `Tab` / `Esc`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DialogBody {
    /// The text-entry field's current content, or absent when the dialog has
    /// none. `Some("")` is an empty field, which is not the same as no field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
    /// The choice rows (1e's agent radio, the folder browser's subdirectories).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub list: Vec<DialogChoice>,
    /// The action buttons, in display order. The first is the primary.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub buttons: Vec<DialogKey>,
    /// Whether this build accepts [`command::DIALOG_CONFIRM`] for this dialog.
    /// `false` with a `refusal` is how a dialog a browser may see but not answer
    /// is shown honestly instead of hidden — cancelling stays available either
    /// way. Note that a dialog behind [`ConfirmGate`] is `true`: a browser *can*
    /// confirm it, through the gate's second step.
    pub confirmable: bool,
    /// Why a browser may not confirm it, when `confirmable` is false.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<String>,
    /// Artboard 1g's second step, when one of this dialog's buttons has one
    /// (`specs/WEB_INTERFACE.md` §6.5 R13). Absent — the common case — means
    /// every button this dialog shows is one press away.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm_gate: Option<ConfirmGate>,
}

/// Artboard 1g's **step 2**: the typed-name gate a remote surface must pass
/// before one particular button lands (`specs/WEB_INTERFACE.md` §6.5 R13).
///
/// It is published rather than kept secret, because it is not a password: the
/// artboard draws the expected name as the field's own hint. What it buys is
/// deliberateness — the browser is not the machine the effect lands on, so the
/// person answering has to name the thing they are about to destroy.
///
/// Only [`ConfirmGate::key`]'s button is gated. Every other button on the same
/// dialog is one press away, because the gate stands in front of the *answer*
/// that destroys work, not in front of the question.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfirmGate {
    /// The [`DialogKey::key`] this gate guards (`y`, `a`). Any other button of
    /// the same dialog — and cancelling, always — is unaffected.
    pub key: String,
    /// The name that must be typed back **exactly**: no trimming, no case
    /// folding. The host compares against this same string, so what the browser
    /// shows and what the host checks cannot drift.
    pub expected: String,
    /// 1g step 2's sentence, worded by the host and rendered verbatim — it says
    /// *why* there is a second step ("this browser is remote") rather than
    /// leaving the browser to invent a reason.
    pub instruction: String,
}

/// One choice row of a [`DialogBody`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DialogChoice {
    /// Rendered verbatim.
    pub label: String,
    /// The row the dialog currently has highlighted.
    pub selected: bool,
}

/// One button of a [`DialogBody`]: the key that fires it and its label.
///
/// `key` is what [`command::DIALOG_CONFIRM`]'s `choice` argument names, so the
/// browser can only ask for a key the dialog is showing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DialogKey {
    /// `y`, `1`, `i`, `Enter`, `Tab`, `Esc` — the desktop's own accelerator.
    pub key: String,
    /// Rendered verbatim.
    pub label: String,
}

// ===========================================================================
// Server -> browser
// ===========================================================================

/// The full state a freshly attached (or resuming) browser needs to paint every
/// row of the design, in one frame.
///
/// It is one frame and not a handshake sequence because the browser has nothing
/// useful to render until it has all of it, and a partial paint would flash.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    /// The version the host speaks — always [`PROTOCOL_VERSION`] for this build.
    /// The browser compares it with its own baked-in constant and renders
    /// "reload to update" on a difference (D9, turn 2 §4).
    pub protocol_version: u16,
    /// The host's FlightDeck version string, so the reload prompt can name what
    /// changed and stay distinguishable from the update-available chip.
    pub host_version: String,
    /// Host wall-clock (unix ms) the snapshot was taken, for the latency readout
    /// and for the frozen-clock badge on a stale terminal.
    pub server_time_ms: i64,
    /// This viewer's id, to be presented back in [`Attach::resume_viewer`].
    pub viewer_id: ViewerId,
    /// What this viewer holds (D14).
    pub seat: Seat,
    /// Everyone attached, including the desktop, for the `desktop + this tab`
    /// chip.
    pub seats: Vec<SeatInfo>,
    /// The highest [`Input::seq`] the host has applied from this viewer.
    ///
    /// Zero for a new viewer. A resuming viewer drops queued keystrokes at or
    /// below this and replays the rest **in seq order**, which is how §5.1's
    /// "queued, never dropped, never delivered out of order" is satisfied without
    /// the relay's envelope machinery — and how it avoids the opposite bug of
    /// typing everything twice.
    pub last_input_seq: u64,
    /// All open projects and their sessions.
    pub projects: Vec<ProjectView>,
    /// The shared selection (D3).
    pub selection: Selection,
    /// The host's authoritative grid for the selected terminal (D4). Per-terminal
    /// geometry also rides on each [`TerminalView`]; this is the one the
    /// letterbox and the `120×34 · host owns geometry` chip use.
    pub geometry: Geometry,
    /// Ring-buffer capacity per terminal, in bytes (Q2's `[web] replay_bytes`).
    /// Lets the browser explain *why* it missed output when
    /// [`TermBytes::truncated`] arrives.
    pub replay_capacity_bytes: u64,
    /// Activity-feed backfill, oldest first (D11). Retention — 200 events or 24
    /// hours, whichever is smaller — belongs to [`super::activity`].
    pub activity: Vec<ActivityEvent>,
    /// The open dialog, if any (D13).
    #[serde(default)]
    pub dialog: Option<DialogView>,
    /// The host's command inventory: every name this build accepts, with the
    /// label, group and `host only` flag the palette needs (D16, artboard 1d).
    ///
    /// Sent with the snapshot rather than compiled into the SPA because the
    /// host is the only thing that knows what it implements. A browser built
    /// against a different FlightDeck therefore renders *that* host's surface,
    /// and a name the host does not have cannot appear in the palette at all.
    /// See [`CommandView`] for how a row becomes a frame.
    #[serde(default)]
    pub commands: Vec<CommandView>,
}

/// One row of the browser's command palette, as the host describes it.
///
/// The shape deliberately mirrors `webui/src/state/commands.ts`'s
/// `PaletteCommand` (`{ id, label, group, run, hostOnly?, annotation? }`) so the
/// SPA's palette can render a host-supplied row without a second model — only
/// the snake_case → camelCase rename the adapter already does for every other
/// wire type. Two fields are additions the browser did not have:
///
/// * [`CommandView::target`] — the row is a *template*: expand it into one row
///   per project / session / terminal / unread event and fill `run.args`.
/// * [`CommandView::refusal`] — the host will refuse this name today, for the
///   stated reason. The row still renders (D16: visible and honest beats
///   hidden), and sending it returns this same sentence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandView {
    /// Stable id for keyed rendering. Equal to `run.name` for a plain row; a
    /// template row's expansions append their target id (`select_session:t1`).
    pub id: String,
    /// The label the user reads and filters on, matching the TUI's palette.
    pub label: String,
    /// The palette group heading (`Worktree`, `Git`, `Terminals`, …).
    pub group: String,
    /// The frame to send when the row is chosen.
    pub run: CommandRun,
    /// D16: the effect lands on the host's machine. Rendered as the `host only`
    /// badge and never hidden.
    #[serde(default, skip_serializing_if = "is_false")]
    pub host_only: bool,
    /// D13: this row **answers** the open dialog rather than opening anything.
    ///
    /// The desktop answers a dialog with its keyboard and the browser answers it
    /// with the dialog panel's own buttons, so neither surface lists these as
    /// palette rows. They are in the inventory because everything the browser may
    /// send is in the inventory — which is what makes the server's "refuse any
    /// name not in the table" a complete check — and this flag is how a browser
    /// tells the two kinds apart without a list of its own.
    #[serde(default, skip_serializing_if = "is_false")]
    pub answers_dialog: bool,
    /// Artboard 1d's right-hand tag (`current`, `3 unread`, …), if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotation: Option<String>,
    /// Set when the row is a template needing a target id in `run.args`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<CommandTarget>,
    /// Set when this build will refuse the name, with the reason it refuses.
    /// Absent means the host runs it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<String>,
}

/// The [`Command`] frame a [`CommandView`] row sends.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandRun {
    /// The command name; see [`command`].
    pub name: String,
    /// Pre-filled arguments, if the row needs none from the browser.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<serde_json::Value>,
}

/// What a template [`CommandView`] expands over, and which `run.args` key the
/// browser fills with the chosen target's id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandTarget {
    /// One row per open project; `args: { project_id }`.
    Project,
    /// One row per session in the selected project; `args: { session_id }`.
    Session,
    /// One row per terminal in the selected session; `args: { terminal_id }`.
    Terminal,
    /// A single row carrying every unread event id; `args: { event_ids }`.
    UnreadActivity,
    /// A target kind this build does not know. The row is skipped rather than
    /// rendered without its argument.
    #[serde(other)]
    Unrecognized,
}

/// `skip_serializing_if` for a plain `bool` — a `false` flag stays off the wire.
fn is_false(value: &bool) -> bool {
    !*value
}

/// One state change, pushed as it happens.
///
/// Deliberately **one change per frame**: WebSocket frames are cheap on a LAN
/// socket, and a batch type would need its own ordering rules to buy something
/// no measurement has asked for yet. The variants are exactly the live changes
/// the design's rows can show.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "change", rename_all = "snake_case")]
pub enum Delta {
    /// A project appeared, or its name/dot/session list changed.
    ProjectUpsert(ProjectView),
    /// A project was closed.
    ProjectRemoved {
        /// The project that went away.
        project_id: ProjectId,
    },
    /// A session appeared, or a field the row shows changed wholesale.
    SessionUpsert(SessionView),
    /// A session was closed.
    SessionRemoved {
        /// The session that went away.
        session_id: TabId,
    },
    /// The cheap, frequent one: a status transition on one session.
    Status {
        /// The session that changed.
        session_id: TabId,
        /// Its new status.
        status: SessionStatus,
    },
    /// Refreshed git facts for one session's row and bar.
    Git {
        /// The session that changed.
        session_id: TabId,
        /// Its new git facts.
        git: GitBar,
    },
    /// A project's rolled-up dot changed without its sessions being resent.
    ProjectDot {
        /// The project that changed.
        project_id: ProjectId,
        /// Its new dot, or `null` when it has no sessions.
        #[serde(default)]
        dot: Option<StatusBucket>,
    },
    /// The shared selection moved — possibly because *this* browser moved it,
    /// possibly because the desktop did (D3).
    Selection(Selection),
    /// A terminal appeared, or its title/geometry/liveness changed.
    TerminalUpsert(TerminalView),
    /// A terminal's process exited.
    TerminalClosed {
        /// The terminal that exited.
        terminal_id: TerminalId,
        /// Its exit status, when it exited normally.
        #[serde(default)]
        exit_code: Option<i32>,
    },
    /// The host resized a PTY (D4 — the desktop's pane changed). The browser
    /// rebuilds xterm.js at the new grid and re-letterboxes.
    Geometry {
        /// The terminal that was resized.
        terminal_id: TerminalId,
        /// Its new host-owned grid.
        geometry: Geometry,
    },
    /// A new activity-feed entry (D11).
    Activity(ActivityEvent),
    /// A dialog opened on either surface (D13).
    DialogOpened(DialogView),
    /// A dialog closed on either surface.
    DialogClosed {
        /// The dialog that closed.
        dialog_id: DialogId,
        /// How it closed.
        outcome: DialogOutcome,
    },
    /// The seat map changed: someone attached, left, changed role, or the
    /// **input lock moved** (D14 as revised).
    ///
    /// The lock moves far more often than the roster does — every time one
    /// writer stops typing and another starts — so this frame is the one that
    /// keeps both surfaces' "who can type" honest, and it carries
    /// [`SeatInfo::holds_input`] on every row rather than a separate holder
    /// field: one list, one truth, and no way for the two to disagree.
    Seats {
        /// What the recipient now holds.
        you: Seat,
        /// Everyone attached, including the desktop.
        seats: Vec<SeatInfo>,
        /// The host's clock when this frame was built, paired with the rows'
        /// [`SeatInfo::since_ms`] the way [`Snapshot::server_time_ms`] is paired
        /// with everything it dates.
        ///
        /// Without it a `since_ms` is a number the browser cannot honestly use:
        /// dating it against `Date.now()` measures a host instant with a local
        /// clock that may be wrong, and artboard 2f's `connected` row would be
        /// a confident guess. So this frame carries its own reference clock, and
        /// the seat rows are as complete here as they are inside a snapshot.
        ///
        /// `0` is the [`serde`] default and means *an older host said nothing*:
        /// the row is then drawn without its `connected` line rather than with a
        /// fabricated or negative duration.
        #[serde(default)]
        server_time_ms: i64,
        /// Whether **the recipient of this frame** is the writer that was just
        /// interrupted by a *confirmed* preemption.
        ///
        /// ## Why the frame has to say it, and cannot be re-derived
        ///
        /// The input lock moves on every ordinary hand-off — one writer stops
        /// typing, another starts, several times a minute — and every one of
        /// those movements is a `Delta::Seats` too. A browser watching only the
        /// rows therefore cannot tell the two apart: "the lock left me" is true
        /// of both, and 2f's *evicted* panel in front of somebody every time
        /// their colleague starts a sentence is not a notice, it is an
        /// obstruction. The distinguishing fact is **intent**, and intent is
        /// known only at the host, at the moment the act happens
        /// ([`SeatRequest::TakeOver`], or `Take Input Lock` from either
        /// surface's palette). So it is carried, not inferred.
        ///
        /// ## Per-recipient, like `you`
        ///
        /// One preemption interrupts exactly one writer, and only that writer
        /// has anything to be told. This sits beside `you` for that reason: the
        /// registry already builds one frame per viewer so each can be told what
        /// *it* holds, and this is the second thing that differs between them.
        /// A list-shaped `preempted: Option<ViewerId>` would be the same fact
        /// broadcast to everybody, which invites a browser to compare it against
        /// its own id — and a browser that gets that comparison wrong shows a
        /// modal to the wrong person.
        ///
        /// It names no interrupter. The rows already do: the one with
        /// [`SeatInfo::holds_input`] is the surface that took it, and a second
        /// field naming the same surface is a second thing that can disagree.
        ///
        /// **`false` is the honest default**, and the reason this field is not a
        /// [`PROTOCOL_VERSION`] bump: it is additive and optional under the
        /// forward-compatibility policy, and a host that never sends it is a
        /// host that reports no preemptions — which leaves the browser exactly
        /// where it was before the field existed, with the lock moving silently.
        /// That is a lesser panel, not a wrong one.
        #[serde(default)]
        you_were_preempted: bool,
    },
    /// A change this build does not know. Ignored by the receiver; see the
    /// forward-compatibility policy in the module docs.
    #[serde(other)]
    Unrecognized,
}

/// Whether a client frame was applied.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AckOutcome {
    /// Applied. The browser may release it from its queue.
    Applied,
    /// Refused for a stated reason; see [`Ack::detail`]. **This frame was not
    /// applied and will not be**: the browser releases it from its queue.
    ///
    /// The input lock's refusal is one of these, paired with an
    /// [`ErrorCode::SeatHeld`] error naming the holder. Refusing rather than
    /// holding it back is deliberate: a keystroke replayed once the other writer
    /// stopped would arrive in the middle of whatever they had typed, which is
    /// the corruption the lock exists to prevent. The cost — the keystroke is
    /// gone — is paid visibly, which is what §5.1 requires.
    Rejected,
    /// Received but deliberately not applied — most often input from an
    /// [`Seat::Observing`] viewer. Acked rather than dropped so a keystroke never
    /// disappears without a trace (§5.1).
    Ignored,
}

/// Acknowledgement of one [`Input`] or [`Command`], by its `seq`.
///
/// This is **not** the relay protocol's ack machinery, which exists to survive a
/// queueing intermediary. It exists for exactly one requirement: turn 2 §5.1's
/// held-keystroke queue needs to know what landed so it can release it, and
/// needs to not re-send what already landed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ack {
    /// The `seq` of the frame being acknowledged.
    pub seq: u64,
    /// Whether it was applied.
    pub outcome: AckOutcome,
    /// Human-readable detail for a rejection or an ignore.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Why the host refused something. An open vocabulary — see the module's
/// forward-compatibility policy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ErrorCode {
    /// The peer speaks a version this build cannot serve. Accompanied by
    /// [`WireError::version`]. The browser's answer is "reload to update", not a
    /// retry (D9, turn 2 §4).
    VersionMismatch,
    /// Missing, expired or revoked credential (D5, Q4). The browser sends the
    /// user back through the access screens.
    Unauthorized,
    /// Too many bootstrap-code attempts from this address (Q1's per-address
    /// rate limit). See [`WireError::retry_after_ms`].
    RateLimited,
    /// Another writer holds the **input lock** and is mid-burst, so this
    /// keystroke was refused rather than mixed into theirs (D14 as revised).
    /// Accompanied by [`WireError::incumbent`] — who is typing — and by an
    /// [`Ack`] with [`AckOutcome::Rejected`] for the same `seq`, so §5.1's
    /// held-keystroke queue can release it.
    ///
    /// **It does not cost the recipient its seat.** The browser stays a writer,
    /// renders 2f's panel, and may either wait (the lock frees itself once the
    /// holder goes quiet) or re-attach with [`SeatRequest::TakeOver`] to
    /// interrupt.
    SeatHeld,
    /// This viewer is read-only, so the frame was refused rather than applied.
    ReadOnly,
    /// The frame named a session or terminal the host does not have — usually a
    /// stale id after a reconnect.
    UnknownTarget,
    /// A well-formed [`Command`] this build does not implement. The M2 door's
    /// failure mode: a newer browser asking for a palette command is told no,
    /// clearly, rather than being disconnected.
    NotSupported,
    /// Something broke on the host.
    Internal,
    /// A code this build does not know. The `message` still renders.
    #[default]
    Unknown,
}

impl ErrorCode {
    /// The wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::VersionMismatch => "version_mismatch",
            ErrorCode::Unauthorized => "unauthorized",
            ErrorCode::RateLimited => "rate_limited",
            ErrorCode::SeatHeld => "seat_held",
            ErrorCode::ReadOnly => "read_only",
            ErrorCode::UnknownTarget => "unknown_target",
            ErrorCode::NotSupported => "not_supported",
            ErrorCode::Internal => "internal",
            ErrorCode::Unknown => "unknown",
        }
    }

    /// Parse a wire spelling; anything unrecognised is [`ErrorCode::Unknown`].
    ///
    /// Mirrors [`InterpretedStatus::from_str_lossy`] rather than using
    /// `#[serde(other)]`, which serde only supports on tagged enums — and going
    /// through a string keeps the accompanying `message` intact, which is the
    /// half the user actually reads.
    pub fn from_str_lossy(s: &str) -> Self {
        match s {
            "version_mismatch" => ErrorCode::VersionMismatch,
            "unauthorized" => ErrorCode::Unauthorized,
            "rate_limited" => ErrorCode::RateLimited,
            "seat_held" => ErrorCode::SeatHeld,
            "read_only" => ErrorCode::ReadOnly,
            "unknown_target" => ErrorCode::UnknownTarget,
            "not_supported" => ErrorCode::NotSupported,
            "internal" => ErrorCode::Internal,
            _ => ErrorCode::Unknown,
        }
    }
}

impl Serialize for ErrorCode {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ErrorCode {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Ok(ErrorCode::from_str_lossy(&s))
    }
}

/// A refusal, with the code the browser branches on and the sentence it shows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireError {
    /// What went wrong.
    pub code: ErrorCode,
    /// Human-readable, rendered verbatim. Always present, even for an
    /// [`ErrorCode::Unknown`] from a newer host.
    pub message: String,
    /// The frame this answers, when it answers one, so the browser can fail the
    /// right queued item instead of the whole queue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    /// Set with [`ErrorCode::VersionMismatch`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<VersionMismatch>,
    /// Set with [`ErrorCode::SeatHeld`]: who holds the input lock (D14, 2f).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incumbent: Option<SeatInfo>,
    /// Set with [`ErrorCode::RateLimited`]: how long to wait.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<u64>,
}

impl WireError {
    /// A refusal with just a code and a message.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        WireError {
            code,
            message: message.into(),
            seq: None,
            version: None,
            incumbent: None,
            retry_after_ms: None,
        }
    }

    /// The version-mismatch refusal, carrying the numbers the browser needs to
    /// explain the reload.
    pub fn version_mismatch(mismatch: VersionMismatch) -> Self {
        WireError {
            version: Some(mismatch),
            ..WireError::new(
                ErrorCode::VersionMismatch,
                format!(
                    "this FlightDeck speaks web protocol v{}-v{}; the page asked for v{}. Reload to update.",
                    mismatch.min_supported, mismatch.max_supported, mismatch.peer
                ),
            )
        }
    }

    /// The input-lock refusal, carrying the holder so the refused browser can
    /// say *who* is typing and offer `Take over` / `Watch read-only` (2f).
    ///
    /// The sentence names the holder rather than the failure, because "your
    /// keystroke was refused" on its own is indistinguishable from a broken
    /// host, and 2f's whole reason to exist is that neither person should have
    /// to wonder why the keys stopped working.
    pub fn seat_held(incumbent: SeatInfo) -> Self {
        let message = format!("{} is typing right now.", incumbent.label);
        WireError {
            incumbent: Some(incumbent),
            ..WireError::new(ErrorCode::SeatHeld, message)
        }
    }
}

/// Why the socket is closing for good (Q5).
///
/// The whole point is that `reconnecting…` against a host that is gone is a lie
/// that wastes the user's time. Every reason except [`ShutdownReason::Restarting`]
/// puts the browser in a **terminal state where it stops retrying** and names the
/// reason.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ShutdownReason {
    /// FlightDeck itself is quitting — `Ctrl-q`, which kills every agent. The
    /// browser stops retrying and says the host is gone.
    HostQuit,
    /// The web interface was stopped (`Stop Web Interface`, D10) while
    /// FlightDeck keeps running. Distinguished from [`ShutdownReason::HostQuit`]
    /// because the agents are still alive and the desktop is still usable.
    ServerStopped,
    /// The credential this browser used was revoked or rotated (D5, D10). The
    /// browser goes back to the access screens, not to a retry loop.
    TokenRevoked,
    /// The host is coming back — a restart or an update. The one reason a retry
    /// is the right behaviour.
    Restarting,
    /// A reason this build does not know. Treated like the others: stop
    /// retrying, and show [`ServerMsg::Shutdown`]'s `detail`.
    #[default]
    Unknown,
}

impl ShutdownReason {
    /// The wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            ShutdownReason::HostQuit => "host_quit",
            ShutdownReason::ServerStopped => "server_stopped",
            ShutdownReason::TokenRevoked => "token_revoked",
            ShutdownReason::Restarting => "restarting",
            ShutdownReason::Unknown => "unknown",
        }
    }

    /// Parse a wire spelling; anything unrecognised is
    /// [`ShutdownReason::Unknown`].
    pub fn from_str_lossy(s: &str) -> Self {
        match s {
            "host_quit" => ShutdownReason::HostQuit,
            "server_stopped" => ShutdownReason::ServerStopped,
            "token_revoked" => ShutdownReason::TokenRevoked,
            "restarting" => ShutdownReason::Restarting,
            _ => ShutdownReason::Unknown,
        }
    }

    /// Whether a browser seeing this reason should keep trying to reconnect.
    ///
    /// Only a restart says yes. An unknown reason says **no**: the honest default
    /// for "the host said something final that we do not understand" is to stop
    /// and say so, not to spin.
    pub fn should_retry(self) -> bool {
        matches!(self, ShutdownReason::Restarting)
    }
}

impl Serialize for ShutdownReason {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ShutdownReason {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Ok(ShutdownReason::from_str_lossy(&s))
    }
}

/// A frame from the host to a browser. Internally tagged by `type`; struct
/// payloads are flattened next to the tag, as in the phone protocol.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    /// Everything the browser needs to paint, in one frame. Sent in reply to a
    /// successful [`Attach`].
    Snapshot(Snapshot),
    /// One live state change.
    Delta(Delta),
    /// Raw PTY output with its byte offset (D2, Q3).
    TermBytes(TermBytes),
    /// Acknowledgement of an [`Input`] or [`Command`].
    Ack(Ack),
    /// A refusal. Not fatal on its own — the socket stays open unless the host
    /// also sends [`ServerMsg::Shutdown`].
    Error(WireError),
    /// The socket is closing for good (Q5). Sent **before** the listener closes,
    /// so the browser can enter a terminal state instead of a retry loop.
    Shutdown {
        /// Why.
        reason: ShutdownReason,
        /// True when this shutdown was caused by a [`Command`] from **this**
        /// viewer.
        ///
        /// Q5's real requirement: a browser that just pressed `Ctrl-q` must show
        /// an acknowledgement of its own action, and a browser that was merely
        /// attached must show a failure. Same frame, two different screens, and
        /// the difference is not derivable from the reason alone.
        #[serde(default)]
        self_initiated: bool,
        /// Extra human-readable detail, rendered verbatim.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// A frame this build does not know. Ignored; see the forward-compatibility
    /// policy in the module docs.
    #[serde(other)]
    Unrecognized,
}

// ===========================================================================
// Browser -> server
// ===========================================================================

/// The opening frame: what version the browser speaks, what seat it wants, and
/// where it left off.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attach {
    /// The version the browser's baked-in SPA speaks. Checked with
    /// [`check_version`].
    pub protocol_version: u16,
    /// Controlling, taking over, or read-only (D14).
    pub seat: SeatRequest,
    /// Per-terminal byte cursors from the previous connection (Q3). Empty on a
    /// first attach; the host then replays whatever the ring holds.
    #[serde(default)]
    pub cursors: Vec<TermCursor>,
    /// The [`ViewerId`] from the connection this one is resuming, so the host can
    /// answer with [`Snapshot::last_input_seq`] and the browser can release
    /// already-applied keystrokes from its queue (§5.1).
    #[serde(default)]
    pub resume_viewer: Option<ViewerId>,
    /// The browser's viewport, so the first snapshot's letterbox is right without
    /// a follow-up [`Resize`]. Never affects PTY geometry (D4).
    #[serde(default)]
    pub viewport: Option<Viewport>,
    /// Advisory self-description for the seat chip.
    #[serde(default)]
    pub client: Option<ClientInfo>,
}

/// Keystrokes for a terminal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Input {
    /// Monotonic per-viewer sequence number, continuing **across reconnects**.
    ///
    /// The browser's held-keystroke queue is keyed by it: it replays in seq
    /// order, and it drops everything at or below [`Snapshot::last_input_seq`].
    /// That is how §5.1's three requirements — never dropped, never reordered,
    /// and (the unstated fourth) never doubled — are all satisfiable.
    pub seq: u64,
    /// The terminal to write to.
    pub terminal_id: TerminalId,
    /// The bytes to write. Base64 on the wire, for the same reason as
    /// [`TermBytes::data`]: a keystroke is not necessarily a character.
    ///
    /// Delivered only while this viewer holds the input lock. A writer that is
    /// not holding it is answered [`AckOutcome::Rejected`] plus
    /// [`ErrorCode::SeatHeld`], and an observer [`AckOutcome::Ignored`]; in
    /// neither case do these bytes reach a PTY, and in neither case do they
    /// vanish unremarked.
    #[serde(with = "b64")]
    pub data: Vec<u8>,
}

/// The browser's viewport, reported for letterboxing.
///
/// **This never reaches `portable_pty`.** D4 is unconditional: the desktop owns
/// PTY geometry, because `sync_terminal_sizes` re-asserts the pane's size every
/// frame and would revert any viewer-set geometry within one frame anyway.
///
/// The type enforces that structurally: **it carries no terminal id and no
/// session id**, so there is no PTY it could name. A `Resize` is not a resize
/// request that the host politely declines — it is a frame that cannot express
/// one. What the host does with it is bounded to display concerns: the seat
/// chip, and knowing whether a viewer is clipping the grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resize {
    /// Cells the browser can currently show.
    pub viewport: Viewport,
}

/// Every command name the host answers. String constants rather than an enum so
/// [`Command::name`] stays open (see [`Command`]), while the host and the SPA
/// still share one spelling.
///
/// The spellings live here; which palette action each one *runs*, and whether it
/// runs at all, lives in [`super::commands`] — one table, so a palette action
/// cannot become silently unreachable from a browser.
pub mod command {
    /// Select a project (D3 — moves the desktop too).
    pub const SELECT_PROJECT: &str = "select_project";
    /// Select a session.
    pub const SELECT_SESSION: &str = "select_session";
    /// Select a terminal within the selected session.
    pub const SELECT_TERMINAL: &str = "select_terminal";
    /// Mark activity-feed events read (D11).
    pub const MARK_ACTIVITY_READ: &str = "mark_activity_read";
    /// Ask for a fresh [`super::Snapshot`] — the browser's recovery path when it
    /// believes it has drifted.
    pub const REQUEST_SNAPSHOT: &str = "request_snapshot";
    /// Stop competing for input voluntarily and become an observer (D14).
    pub const RELEASE_SEAT: &str = "release_seat";
    /// Take the input lock now, interrupting whoever holds it (D14 as revised).
    ///
    /// The explicit override, and the *only* way past another writer's live
    /// burst. Named as a command as well as a [`super::SeatRequest`] so a
    /// surface that is already seated as a writer — the desktop's palette, or a
    /// browser that does not want to re-attach — has the same door rather than a
    /// privilege of its own.
    pub const TAKE_INPUT_LOCK: &str = "take_input_lock";

    // -- the shared dialog (D13) -------------------------------------------

    /// Confirm the open dialog, whichever surface opened it (D13).
    ///
    /// `args` names the dialog and, when it has one, what the browser filled in:
    /// `{ dialog_id, choice?, text?, toggle?, confirm_name? }`. `choice` is the
    /// *key label* of a button the dialog is currently showing (`y`, `1`, `i`,
    /// `Enter`) and `text` is the input field's content, so a browser can only
    /// ever press a key the dialog is offering — the same power the desktop's
    /// keyboard has and no more.
    ///
    /// `confirm_name` is artboard 1g's second step (see [`ConfirmGate`]): a
    /// frame answering a gated button without it — or with a name that is not
    /// exactly [`ConfirmGate::expected`] — is refused before a single key is
    /// fed into the prompt, so the effect provably does not happen.
    pub const DIALOG_CONFIRM: &str = "dialog_confirm";
    /// Cancel the open dialog, whichever surface opened it (D13). `args` is
    /// `{ dialog_id }`; cancelling is the one dialog decision that is never
    /// destructive, so it is accepted for every dialog kind.
    pub const DIALOG_CANCEL: &str = "dialog_cancel";

    // -- the palette surface (§22's actions, by wire name) -----------------

    /// Open a project folder picker (SPECS §22 "Open Project").
    pub const OPEN_PROJECT: &str = "open_project";
    /// Close the active project.
    pub const CLOSE_PROJECT: &str = "close_project";
    /// Switch to the next open project.
    pub const NEXT_PROJECT: &str = "next_project";
    /// Switch to the previous open project.
    pub const PREVIOUS_PROJECT: &str = "previous_project";
    /// Create a branch + worktree and spawn an agent (SPECS §4, §16, §17).
    pub const NEW_AGENT_SESSION_TAB: &str = "new_agent_session_tab";
    /// Rename the selected session tab (SPECS §18).
    pub const RENAME_AGENT_SESSION_TAB: &str = "rename_agent_session_tab";
    /// Close the selected session tab (SPECS §25's option set).
    pub const CLOSE_AGENT_SESSION_TAB: &str = "close_agent_session_tab";
    /// Move to the next session tab.
    pub const SWITCH_AGENT_SESSION_TAB: &str = "switch_agent_session_tab";
    /// Restart the selected session's primary agent (SPECS §10, §23).
    pub const RESTART_AGENT: &str = "restart_agent";
    /// Rebase the selected worktree onto its base branch (SPECS §5.1).
    pub const REBASE_WORKTREE: &str = "rebase_worktree";
    /// Remove the selected worktree (SPECS §5/§15).
    pub const ABANDON_WORKTREE: &str = "abandon_worktree";
    /// Reveal the selected worktree in the OS file manager (D16: host only).
    pub const OPEN_WORKTREE_IN_FILE_MANAGER: &str = "open_worktree_in_file_manager";
    /// Open a config file in the host's `$EDITOR` (D16: host only).
    pub const EDIT_IN_EDITOR: &str = "edit_in_editor";
    /// Push the selected session's branch (SPECS §14).
    pub const PUSH_BRANCH: &str = "push_branch";
    /// Local merge-back into the base branch (SPECS §13, §15).
    pub const FINISH_LOCAL_MERGE: &str = "finish_local_merge";
    /// Pull the base branch in the base folder (SPECS §5.2).
    pub const PULL_BASE: &str = "pull_base";
    /// Show the git status panel (SPECS §21).
    pub const SHOW_GIT_STATUS: &str = "show_git_status";
    /// Open a child shell terminal (SPECS §19).
    pub const NEW_CHILD_TERMINAL: &str = "new_child_terminal";
    /// Close the selected child shell terminal.
    pub const CLOSE_CHILD_TERMINAL: &str = "close_child_terminal";
    /// Spawn an additional agent in the selected session's worktree.
    pub const NEW_AGENT: &str = "new_agent";
    /// Close the selected additional agent terminal.
    pub const CLOSE_AGENT: &str = "close_agent";
    /// Move to the next child terminal.
    pub const SWITCH_CHILD_TERMINAL: &str = "switch_child_terminal";
    /// Open a shell in the selected session's worktree (SPECS §10/§22).
    pub const OPEN_SHELL: &str = "open_shell";
    /// Set or clear the manual status override (SPECS §24).
    pub const SET_MANUAL_STATUS: &str = "set_manual_status";
    /// Open the configuration manager (SPECS §8).
    pub const OPEN_CONFIGURATION: &str = "open_configuration";
    /// Begin pairing a phone (FlightDeck Remote).
    pub const PAIR_PHONE: &str = "pair_phone";
    /// Forget the paired phone.
    pub const UNPAIR_PHONE: &str = "unpair_phone";
    /// Start the embedded web interface (D10).
    pub const START_WEB_INTERFACE: &str = "start_web_interface";
    /// Stop the embedded web interface (Q5).
    pub const STOP_WEB_INTERFACE: &str = "stop_web_interface";
    /// Lay the selected session's terminals out side by side.
    pub const TOGGLE_SPLIT_VIEW: &str = "toggle_split_view";
    /// Show help / keybindings (SPECS §23).
    pub const SHOW_HELP: &str = "show_help";
    /// Show the About dialog.
    pub const ABOUT_FLIGHTDECK: &str = "about_flightdeck";
    /// Quit FlightDeck and every agent with it (D16: never from a bare frame).
    pub const QUIT: &str = "quit";
}

/// A named command with free-form arguments — **the M2 door** (D13).
///
/// M1 implements only the handful in [`command`]; D8 puts the palette, the dialog
/// family, git commands, the configuration manager and destructive operations in
/// M2. Keeping `name` a `String` and `args` a `serde_json::Value` means every one
/// of those arrives without a protocol version bump, and an M1 host answers an
/// unknown name with [`ErrorCode::NotSupported`] instead of failing to parse the
/// frame and dropping the socket.
///
/// **What M2 adds here:** `git_pull_base`, `git_merge_back`,
/// `git_abandon_worktree` (with the typed-name confirmation of artboard 1g),
/// `new_agent`, `restart_agent`, `close_session`, `set_manual_status`,
/// `clear_manual_status`, `dialog_confirm` / `dialog_cancel`, `toggle_split_view`,
/// `open_palette`, and the configuration-manager writes. Each is a `name` plus an
/// `args` object; none of them changes this type.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Command {
    /// Monotonic per-viewer sequence number, shared with [`Input`] so an [`Ack`]
    /// needs only one field to identify what it answers.
    pub seq: u64,
    /// The command name; see [`command`] for M1's set.
    pub name: String,
    /// Arguments, shaped per command. Absent for commands that take none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<serde_json::Value>,
}

/// A frame from a browser to the host. Internally tagged by `type`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    /// Open (or resume) the session and ask for a seat.
    Attach(Attach),
    /// Keystrokes for a terminal.
    Input(Input),
    /// The browser's viewport — never a PTY resize (D4).
    Resize(Resize),
    /// A named command; the M2 door.
    Command(Command),
    /// A frame this build does not know. Answered with
    /// [`ErrorCode::NotSupported`] where a reply is possible, otherwise ignored.
    #[serde(other)]
    Unrecognized,
}
