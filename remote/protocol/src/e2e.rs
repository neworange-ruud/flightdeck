//! The **E2E plane**: the application messages exchanged phone <-> desktop.
//!
//! These types are the *plaintext*. They are serialized to JSON, then sealed by
//! a crypto layer this crate does not implement, and the resulting bytes travel
//! inside a [`crate::relay::EncryptedEnvelope`]. The relay never sees any of
//! this. Two top-level types:
//!
//! * [`DesktopToPhone`] — feeds pushed from the Mac to the phone.
//! * [`PhoneCommand`] — commands issued by the phone; every one carries a
//!   client-generated [`CommandId`] and is acknowledged by [`CommandAck`].

use serde::{Deserialize, Serialize};

use crate::common::{AgentStatus, AgentType, GitIndicators, GitStatusDetail, RollupDot};
use crate::ids::{CommandId, EventId, ItemId, ProjectId, PromptId, SessionId, ShellId};

// ===========================================================================
// Shared value types
// ===========================================================================

/// The choice on a permission prompt. Matches FlightDeck's allow-once / deny.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionChoice {
    /// Allow the requested action a single time.
    AllowOnce,
    /// Deny the requested action.
    Deny,
}

/// Which kind of prompt a [`TranscriptItem::PermissionPrompt`] represents.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptKind {
    /// Binary allow/deny permission request.
    #[default]
    Permission,
    /// A multiple-choice question (Claude AskUserQuestion / OpenCode question.asked).
    Question,
}

/// A deep-link target so a notification tap lands on the exact agent/item.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeepLink {
    /// Project to open.
    pub project_id: ProjectId,
    /// Session to open within the project.
    pub session_id: SessionId,
    /// Optional transcript item to scroll to.
    pub item_id: Option<ItemId>,
}

// ===========================================================================
// State snapshot & status
// ===========================================================================

/// One agent session as shown on a session row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionState {
    /// Session id.
    pub session_id: SessionId,
    /// Owning project.
    pub project_id: ProjectId,
    /// Session name (== worktree == branch leaf), e.g. `fix-login`.
    pub name: String,
    /// Which agent CLI runs here.
    pub agent_type: AgentType,
    /// Current status.
    pub status: AgentStatus,
    /// Compact git indicators for the row.
    pub git: GitIndicators,
    /// Wall-clock running time of the current/last turn, in seconds.
    pub running_time_secs: u64,
    /// Preview of what a waiting agent is asking (present when needs-input).
    pub pending_question: Option<String>,
}

/// Aggregated status for a project (the single dot + plain-language summary).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusRollup {
    /// Which status dominates the dot (see [`RollupDot`] precedence).
    pub dot: RollupDot,
    /// Plain-language summary, e.g. `1 needs input · 1 working · 3 agents`.
    pub summary: String,
    /// Number of agents working.
    pub working: u32,
    /// Number of agents idle/finished.
    pub idle: u32,
    /// Number of agents needing input.
    pub needs_input: u32,
    /// Number of agents under manual override.
    pub manual: u32,
    /// Total agent count.
    pub agent_count: u32,
}

/// One project and all its sessions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectState {
    /// Project id.
    pub project_id: ProjectId,
    /// Display name.
    pub name: String,
    /// Rolled-up status for the project row.
    pub rollup: StatusRollup,
    /// The project's agent sessions.
    pub sessions: Vec<SessionState>,
}

/// A full state snapshot: everything the phone needs to render the projects and
/// sessions lists. Sent on connect and on `request_snapshot`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateSnapshot {
    /// Desktop wall-clock time (unix ms) the snapshot was taken.
    pub server_time_ms: i64,
    /// All open projects (each stays live in the background).
    pub projects: Vec<ProjectState>,
}

/// An incremental status change for one session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionStatusDelta {
    /// Session that changed.
    pub session_id: SessionId,
    /// Owning project.
    pub project_id: ProjectId,
    /// New status.
    pub status: AgentStatus,
    /// New running time, if it changed.
    pub running_time_secs: Option<u64>,
    /// New pending-question preview, if it changed (clears with `Some("")`
    /// is not used — use `null` to leave unchanged; senders resend full deltas).
    pub pending_question: Option<String>,
}

/// A batch of incremental status changes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusUpdate {
    /// The changed sessions.
    pub updates: Vec<SessionStatusDelta>,
}

/// A project's refreshed roll-up (without resending its sessions).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectRollup {
    /// Project id.
    pub project_id: ProjectId,
    /// Refreshed roll-up.
    pub rollup: StatusRollup,
}

/// A batch of project roll-up refreshes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RollupUpdate {
    /// The refreshed projects.
    pub projects: Vec<ProjectRollup>,
}

// ===========================================================================
// Transcript feed
// ===========================================================================

/// Kind of activity a collapsed pill represents (drives the icon).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityKind {
    /// A file edit, e.g. `Edited auth.ts +18 −4`.
    Edit,
    /// A shell command the agent ran, e.g. `Ran npm test · 42 passed`.
    Command,
    /// A test run.
    Test,
    /// A search/grep.
    Search,
    /// Anything else.
    Other,
}

/// One item in the cleaned transcript. Internally tagged by `type`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TranscriptItem {
    /// Prose the user sent to the agent.
    UserMessage {
        /// Stable item id.
        item_id: ItemId,
        /// The message text.
        text: String,
        /// Wall-clock time (unix ms).
        at_ms: i64,
    },
    /// Prose the agent produced (decisions, explanations).
    AgentMessage {
        /// Stable item id.
        item_id: ItemId,
        /// The message text.
        text: String,
        /// Wall-clock time (unix ms).
        at_ms: i64,
    },
    /// A collapsible activity pill (noisy tool call, summarized).
    Activity {
        /// Stable item id.
        item_id: ItemId,
        /// One-line summary shown collapsed, e.g. `Edited auth.ts +18 −4`.
        summary: String,
        /// Optional secondary detail, e.g. `42 passed`.
        detail: Option<String>,
        /// Optional expanded body (raw output) revealed on tap.
        body: Option<String>,
        /// What kind of activity this is.
        kind: ActivityKind,
        /// Wall-clock time (unix ms).
        at_ms: i64,
    },
    /// An inline permission prompt awaiting a decision.
    PermissionPrompt {
        /// Stable item id.
        item_id: ItemId,
        /// The prompt id echoed back in a `permission_decision` command.
        prompt_id: PromptId,
        /// Permission (binary) vs Question (N-option / free-text). Defaults to
        /// Permission so v1 JSON without this field still parses.
        #[serde(default)]
        kind: PromptKind,
        /// Command/action text (Permission) or question text (Question), shown
        /// verbatim.
        command: String,
        /// The offered options (>=1). Each carries a stable 0-based index.
        options: Vec<PermissionOption>,
        /// Whether the phone may submit a free-text answer ("Type your own
        /// answer").
        #[serde(default)]
        allow_free_text: bool,
        /// Wall-clock time (unix ms).
        at_ms: i64,
    },
}

/// A selectable option on a permission prompt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionOption {
    /// Stable 0-based index within the prompt's option list.
    #[serde(default)]
    pub index: u32,
    /// The binary choice this option maps to (permission prompts only). None for
    /// arbitrary Question options.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub choice: Option<PermissionChoice>,
    /// Human-readable button label, e.g. `Allow once` or an AskUserQuestion label.
    pub label: String,
    /// Optional longer description (AskUserQuestion option descriptions).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// A slice of a session's cleaned transcript. Used both for a full load
/// (`replace = true`) and for incremental appends (`replace = false`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptFeed {
    /// Session the transcript belongs to.
    pub session_id: SessionId,
    /// Ordinal of the first item in `items` within the session's transcript.
    pub from_index: u64,
    /// If true, replace any existing items from `from_index` onward.
    pub replace: bool,
    /// The transcript items, in order.
    pub items: Vec<TranscriptItem>,
}

// ===========================================================================
// Typed events (drive notifications + activity feed)
// ===========================================================================

/// The typed payload of an agent event.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventKind {
    /// The agent stopped and needs the human (most urgent).
    NeedsInput {
        /// Preview of the question/permission, for the notification body.
        preview: String,
    },
    /// The agent finished its turn.
    Finished {
        /// Short summary of what happened.
        summary: String,
        /// Number of files changed this turn.
        files_changed: u32,
        /// Whether the branch is in a ready-to-push state (informational only;
        /// the remote never pushes — the agent does).
        ready_to_push: bool,
    },
    /// The agent hit an error.
    Error {
        /// Error detail for the notification body.
        message: String,
    },
}

/// A typed status event, mirrored into the Activity feed and driving pushes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentEvent {
    /// Stable event id (used for `mark_read` and dedup in the feed).
    pub event_id: EventId,
    /// The typed payload.
    pub kind: EventKind,
    /// Where a tap should land.
    pub deep_link: DeepLink,
    /// Wall-clock time (unix ms) the event occurred.
    pub occurred_at_ms: i64,
    /// Short title, e.g. `add-tests finished its turn`.
    pub title: String,
}

// ===========================================================================
// Shell
// ===========================================================================

/// Which stream a shell output chunk came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShellStream {
    /// Standard output.
    Stdout,
    /// Standard error.
    Stderr,
}

/// A chunk of shell output. `data` is a UTF-8 string that may contain ANSI
/// escape sequences (basic colors); the phone renders it in the minimal
/// terminal. Chunks are ordered per shell by `seq`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShellOutput {
    /// Owning session.
    pub session_id: SessionId,
    /// The shell within the session.
    pub shell_id: ShellId,
    /// Which stream produced this chunk.
    pub stream: ShellStream,
    /// Monotonic per-shell chunk sequence (starts at 1).
    pub seq: u64,
    /// The output text (may contain ANSI escapes).
    pub data: String,
}

/// A shell lifecycle transition.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ShellEventKind {
    /// The shell opened with the given geometry.
    Opened {
        /// Terminal columns.
        cols: u16,
        /// Terminal rows.
        rows: u16,
    },
    /// The shell process exited.
    Exited {
        /// Exit code, if known.
        code: Option<i32>,
    },
    /// The shell was closed (by either side).
    Closed,
}

/// A shell lifecycle event for a session's terminal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShellEvent {
    /// Owning session.
    pub session_id: SessionId,
    /// The shell within the session.
    pub shell_id: ShellId,
    /// What happened.
    pub kind: ShellEventKind,
}

// ===========================================================================
// Command acknowledgement
// ===========================================================================

/// Outcome of a phone command, from the desktop's point of view.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandOutcome {
    /// Received and validated; will be applied.
    Accepted,
    /// Applied successfully.
    Applied,
    /// Refused for a stated reason (e.g. failed type-to-confirm).
    Rejected,
    /// Attempted but failed (e.g. git merge conflict).
    Failed,
    /// A command with this id was already processed; ignored idempotently.
    Duplicate,
}

/// The desktop's acknowledgement of a phone command. Delivery honesty: the phone
/// shows "not delivered — retry" until it sees an ack for the command id.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandAck {
    /// The command being acknowledged.
    pub command_id: CommandId,
    /// The outcome.
    pub outcome: CommandOutcome,
    /// Human-readable detail (reason for reject/fail, or a result note).
    pub message: Option<String>,
}

// ===========================================================================
// Desktop -> phone feed
// ===========================================================================

/// A message pushed from the desktop to the phone. Internally tagged by `type`;
/// rich payloads are flattened alongside the tag.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DesktopToPhone {
    /// Full state snapshot (projects -> sessions).
    Snapshot(StateSnapshot),
    /// Incremental per-session status changes.
    StatusUpdate(StatusUpdate),
    /// Project roll-up refreshes.
    Rollup(RollupUpdate),
    /// A full (or from-cursor) transcript load for a session.
    Transcript(TranscriptFeed),
    /// Incremental transcript items appended to a session.
    TranscriptAppend(TranscriptFeed),
    /// A typed status event (needs-input / finished / error) with deep link.
    Event(AgentEvent),
    /// Full git status detail for a session.
    GitStatus(GitStatusDetail),
    /// A chunk of shell output.
    ShellOutput(ShellOutput),
    /// A shell lifecycle event.
    ShellEvent(ShellEvent),
    /// Acknowledgement of a phone command.
    CommandAck(CommandAck),
}

// ===========================================================================
// Phone -> desktop commands
// ===========================================================================

/// The action a phone command performs. Internally tagged by `type`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CommandBody {
    /// Reply / follow-up prose to an agent.
    Reply {
        /// Target session.
        session_id: SessionId,
        /// The reply text.
        text: String,
    },
    /// Resolve a permission prompt.
    PermissionDecision {
        /// Target session.
        session_id: SessionId,
        /// The prompt being decided (from the transcript item).
        prompt_id: PromptId,
        /// Binary fast-path choice (permission prompts). None when answering a
        /// Question by option index or free text.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        choice: Option<PermissionChoice>,
        /// Selected option index (Question prompts).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        option_index: Option<u32>,
        /// Free-text answer ("Type your own answer").
        #[serde(default, skip_serializing_if = "Option::is_none")]
        free_text: Option<String>,
    },
    /// Launch a new agent session (v1 fields only; model/effort inherit desktop
    /// defaults).
    NewAgent {
        /// Project to create the session in.
        project_id: ProjectId,
        /// Which agent CLI to run.
        agent_type: AgentType,
        /// Session name (names the worktree + branch).
        name: String,
        /// Base branch to create the worktree from, e.g. `main`.
        base_branch: String,
        /// The first task, typed or dictated.
        first_task: String,
    },
    /// Restart the agent in place (fresh process, same worktree/branch,
    /// transcript preserved).
    RestartAgent {
        /// Target session.
        session_id: SessionId,
    },
    /// Close a session.
    CloseSession {
        /// Target session.
        session_id: SessionId,
    },
    /// Set the cyan manual-override status with a label.
    SetManualStatus {
        /// Target session.
        session_id: SessionId,
        /// The label to display.
        label: String,
    },
    /// Clear a manual-override status.
    ClearManualStatus {
        /// Target session.
        session_id: SessionId,
    },
    /// Pull the base branch into the worktree (guarded).
    GitPullBase {
        /// Target session.
        session_id: SessionId,
    },
    /// Merge the session branch back into its base (guarded).
    GitMergeBack {
        /// Target session.
        session_id: SessionId,
    },
    /// Abandon the worktree. Destructive; requires the typed confirmation name.
    GitAbandonWorktree {
        /// Target session.
        session_id: SessionId,
        /// The session name the user typed to confirm; the desktop rejects the
        /// command unless it matches the session's name exactly.
        confirm_name: String,
    },
    /// Open a shell in the session's worktree (one shell at a time per session).
    ShellOpen {
        /// Target session.
        session_id: SessionId,
        /// Client-generated shell id.
        shell_id: ShellId,
        /// Initial columns.
        cols: u16,
        /// Initial rows.
        rows: u16,
    },
    /// Send input (keystrokes/text) to a shell.
    ShellInput {
        /// Target session.
        session_id: SessionId,
        /// Target shell.
        shell_id: ShellId,
        /// The bytes to write, as a UTF-8 string.
        data: String,
    },
    /// Interrupt the foreground process (Ctrl-C).
    ShellInterrupt {
        /// Target session.
        session_id: SessionId,
        /// Target shell.
        shell_id: ShellId,
    },
    /// Close a shell.
    ShellClose {
        /// Target session.
        session_id: SessionId,
        /// Target shell.
        shell_id: ShellId,
    },
    /// Ask for a fresh snapshot (all projects, or one).
    RequestSnapshot {
        /// Limit to one project, or `null` for everything.
        project_id: Option<ProjectId>,
    },
    /// Ask for a session's transcript (from an item index, or the whole thing).
    RequestTranscript {
        /// Target session.
        session_id: SessionId,
        /// Start index, or `null` for the full transcript.
        from_index: Option<u64>,
    },
    /// Mark activity-feed events as read.
    MarkRead {
        /// Events to mark read.
        event_ids: Vec<EventId>,
    },
}

/// A command issued by the phone. Every command carries a client-generated
/// [`CommandId`]; the desktop must apply commands **idempotently** — a repeat of
/// an already-seen `command_id` is a no-op that re-emits the original
/// [`CommandAck`] (with outcome `duplicate` if the original result is gone).
///
/// On the wire the [`CommandBody`] is flattened, so a command looks like:
/// `{"command_id":"…","issued_at_ms":…,"type":"reply","session_id":"…","text":"…"}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhoneCommand {
    /// Client-generated, unique per logical command; the idempotency key.
    pub command_id: CommandId,
    /// Phone wall-clock time (unix ms) at issue, for latency/ordering.
    pub issued_at_ms: i64,
    /// What the command does.
    #[serde(flatten)]
    pub body: CommandBody,
}
