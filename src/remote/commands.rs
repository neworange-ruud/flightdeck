//! Inbound phone-command bridge: idempotency ledger + pure translator.
//!
//! The relay/bridge layers deliver parsed [`PhoneCommand`]s (snapshot and
//! transcript requests are already serviced inside
//! [`crate::remote::bridge::RemoteBridge`]); this module decides what each
//! remaining command *means* on the desktop:
//!
//! * [`CommandLedger`] — remembers recently processed command ids so a
//!   retransmitted command is acked idempotently instead of applied twice
//!   (spec: the phone retries until it sees an ack for the command id).
//! * [`translate`] — a **pure** function from a [`CommandBody`] plus a
//!   [`SessionIndex`] (a per-tick read-model of the live workspace) to a
//!   [`Translation`]. It never touches git, PTYs, or state: execution happens
//!   in the event loop (`src/lib.rs`), on the main thread, exclusively through
//!   the existing safety-guarded paths — `AppState::dispatch(Command, ..)` or
//!   a write to a session's primary PTY. Remote commands therefore inherit
//!   every desktop guard (no history rewriting, confirm-gated closes, …).
//!
//! Git actions (pull base / merge back / abandon) and the remote shell are
//! separate tasks; their commands are matched explicitly below and rejected
//! with an honest "not implemented yet" so the seam is obvious.

use std::collections::VecDeque;

use crate::agents::setup::{status_backend, StatusBackend};
use crate::app::commands::{CloseAction, Command};
use crate::contracts::{ManualStatus, ProcessState};
use crate::remote::bridge::ProjectView;
use crate::remote::feed;

use flightdeck_remote_protocol::{
    AgentStatus, AgentType, CommandAck, CommandBody, CommandId, CommandOutcome, PermissionChoice,
    ProjectId, PromptId, SessionId, ShellId,
};

// ===========================================================================
// Idempotency ledger
// ===========================================================================

/// How many processed command ids the ledger remembers. Old entries are
/// evicted FIFO; a retransmit older than this window is (re)processed as new,
/// which is the documented protocol fallback.
pub const LEDGER_CAPACITY: usize = 256;

/// One remembered command outcome.
struct LedgerEntry {
    id: CommandId,
    outcome: CommandOutcome,
    message: Option<String>,
}

/// Remembers the outcome of recently processed commands so a duplicate
/// `command_id` re-emits an ack (outcome [`CommandOutcome::Duplicate`], with
/// the original result in the message) instead of being applied twice.
#[derive(Default)]
pub struct CommandLedger {
    seen: VecDeque<LedgerEntry>,
}

/// Human label for an outcome, used in duplicate-ack messages.
fn outcome_label(outcome: CommandOutcome) -> &'static str {
    match outcome {
        CommandOutcome::Accepted => "accepted",
        CommandOutcome::Applied => "applied",
        CommandOutcome::Rejected => "rejected",
        CommandOutcome::Failed => "failed",
        CommandOutcome::Duplicate => "duplicate",
    }
}

impl CommandLedger {
    /// An empty ledger.
    pub fn new() -> Self {
        Self::default()
    }

    /// If `id` was already processed, the duplicate ack to re-emit (a no-op
    /// otherwise). The message carries the original outcome so the phone can
    /// reconcile a retry with what actually happened.
    pub fn duplicate_ack(&self, id: &CommandId) -> Option<CommandAck> {
        self.seen.iter().find(|e| &e.id == id).map(|e| CommandAck {
            command_id: id.clone(),
            outcome: CommandOutcome::Duplicate,
            message: Some(match &e.message {
                Some(m) => format!("already processed ({}): {m}", outcome_label(e.outcome)),
                None => format!("already processed ({})", outcome_label(e.outcome)),
            }),
        })
    }

    /// Record the outcome of a freshly processed command.
    pub fn record(&mut self, id: CommandId, outcome: CommandOutcome, message: Option<String>) {
        self.seen.push_back(LedgerEntry {
            id,
            outcome,
            message,
        });
        while self.seen.len() > LEDGER_CAPACITY {
            self.seen.pop_front();
        }
    }
}

// ===========================================================================
// Session index (per-tick read-model)
// ===========================================================================

/// Everything the translator needs to know about one live session (tab).
/// Owned data only, so the index can outlive the workspace borrow it was
/// built from.
pub struct SessionEntry {
    /// Wire session id (== `tab.meta.id`).
    pub id: SessionId,
    /// Index of the owning project in the workspace's project list.
    pub project: usize,
    /// Index of the tab within the project's `AppState::tabs`.
    pub tab: usize,
    /// Display name (== worktree/branch leaf name for confirm echoes).
    pub name: String,
    /// The supported agent backend, if recognised. `None` = custom/unknown
    /// agent, for which permission keystrokes are refused rather than guessed.
    pub backend: Option<StatusBackend>,
    /// Phone-facing status at index-build time.
    pub status: AgentStatus,
    /// Whether the primary agent process is running.
    pub primary_running: bool,
    /// Whether the primary agent has never been launched this session
    /// (`ProcessState::NotStarted`). True for recovered tabs whose project was
    /// not the active one at startup — they show as Idle on the phone but must
    /// be resumed before a reply can be typed (remote-control-1l4).
    pub primary_not_started: bool,
    /// Whether every process in the tab (primary + children) has stopped.
    pub all_stopped: bool,
    /// Whether the primary terminal application has enabled bracketed paste
    /// (DECSET 2004), which decides how reply text is framed.
    pub bracketed_paste: bool,
    /// The currently pending permission prompt id, if any (the most recently
    /// minted one from the transcript builder).
    pub pending_prompt: Option<PromptId>,
}

/// One open project, for `new_agent` routing.
pub struct ProjectEntry {
    /// Wire project id (derived from the project name, matching the feed).
    pub id: ProjectId,
    /// Index in the workspace's project list.
    pub index: usize,
    /// The project's base branch (`new_agent` must target exactly this in v1).
    pub base_branch: String,
}

/// A read-model of the live workspace, rebuilt cheaply per command so an
/// earlier command in the same batch (e.g. a close) cannot leave stale
/// indices behind.
#[derive(Default)]
pub struct SessionIndex {
    /// Every session of every open project.
    pub sessions: Vec<SessionEntry>,
    /// Every open project.
    pub projects: Vec<ProjectEntry>,
}

impl SessionIndex {
    /// Look up a session by wire id.
    pub fn session(&self, id: &SessionId) -> Option<&SessionEntry> {
        self.sessions.iter().find(|s| &s.id == id)
    }

    /// Look up a project by wire id.
    pub fn project(&self, id: &ProjectId) -> Option<&ProjectEntry> {
        self.projects.iter().find(|p| &p.id == id)
    }
}

/// Build the [`SessionIndex`] from the same read-only [`ProjectView`]s the
/// outbound feed uses. `pending_prompt` resolves a session id to its current
/// permission-prompt id (the bridge's transcript builders own that state).
pub fn build_index(
    projects: &[ProjectView<'_>],
    now_ms: u64,
    pending_prompt: &dyn Fn(&str) -> Option<PromptId>,
) -> SessionIndex {
    let mut index = SessionIndex::default();
    for (pi, pv) in projects.iter().enumerate() {
        index.projects.push(ProjectEntry {
            id: pv.id.clone(),
            index: pi,
            base_branch: pv.state.base_branch.clone(),
        });
        for (ti, tab) in pv.state.tabs.iter().enumerate() {
            let backend = pv
                .state
                .registry
                .get(&tab.meta.agent)
                .and_then(status_backend);
            index.sessions.push(SessionEntry {
                id: SessionId::new(&tab.meta.id),
                project: pi,
                tab: ti,
                name: tab.meta.name.clone(),
                backend,
                status: feed::agent_status(tab.display_status(now_ms)),
                primary_running: tab.session.primary_state() == ProcessState::Running,
                primary_not_started: tab.session.primary_state() == ProcessState::NotStarted,
                all_stopped: tab.session.all_stopped(),
                bracketed_paste: tab
                    .session
                    .primary()
                    .map(|t| t.bracketed_paste())
                    .unwrap_or(false),
                pending_prompt: pending_prompt(&tab.meta.id),
            });
        }
    }
    index
}

// ===========================================================================
// Translation
// ===========================================================================

/// Work only the event loop can perform (it owns the background-worker
/// plumbing the two-phase tab creation needs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MainLoopAction {
    /// Launch a new agent session: `begin_new_agent_tab` + a background
    /// worktree job (`spawn_worktree_job`), exactly like the desktop palette
    /// flow. `first_task` is delivered to the agent once it is ready.
    NewAgent {
        /// Workspace project index.
        project: usize,
        /// Session name (names the worktree + branch).
        name: String,
        /// FlightDeck agent registry key.
        agent_key: String,
        /// The first task to send once the agent is up (may be empty).
        first_task: String,
    },
    /// Resume a not-started agent (recovered tab whose project was not the
    /// active one at startup) so the phone can message it, then deliver `text`
    /// once its terminal is ready — the same resume the desktop performs when
    /// you navigate to the agent (remote-control-1l4). The launch is I/O, so it
    /// runs in the executor, not the pure translator.
    ResumeAndReply {
        /// Workspace project index.
        project: usize,
        /// The tab (== session) to resume; located by id at execution time.
        tab_id: String,
        /// The reply to deliver once the resumed agent is ready.
        text: String,
    },
}

/// A remote-shell operation, already resolved to a concrete session/shell. The
/// event loop applies it against the session's child-terminal machinery and the
/// [`crate::remote::shell::ShellManager`] (the cap check and shell-id matching
/// need that live state, so they happen at execution, not here).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellAction {
    /// Open the one remote shell for the session with the given geometry.
    Open {
        /// Client-generated shell id.
        shell_id: ShellId,
        /// Initial columns.
        cols: u16,
        /// Initial rows.
        rows: u16,
    },
    /// Write bytes to the shell's child PTY.
    Input {
        /// Target shell.
        shell_id: ShellId,
        /// The exact bytes to write.
        bytes: Vec<u8>,
    },
    /// Send Ctrl-C to the shell's foreground process.
    Interrupt {
        /// Target shell.
        shell_id: ShellId,
    },
    /// Close the shell (terminate + remove the child terminal).
    Close {
        /// Target shell.
        shell_id: ShellId,
    },
}

/// What a phone command translates to on the desktop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Translation {
    /// Write `bytes` to the session's **primary** agent terminal.
    PtyInput {
        /// Workspace project index.
        project: usize,
        /// Tab index within the project.
        tab: usize,
        /// The exact bytes to write.
        bytes: Vec<u8>,
    },
    /// Write `immediate` to the session's primary terminal now, then `deferred`
    /// Write a SEQUENCE of keystroke chunks to the session's primary terminal,
    /// one chunk every `step_delay_ms` starting immediately. Used for Claude's
    /// multi-select form: its Ink (React) TUI re-renders asynchronously and its
    /// key handler closes over the current highlight index, so a burst of
    /// navigation/toggle/submit keys races the re-render — a `Down` then `Enter`
    /// toggles the *pre-move* option (always the first), and the submit `Enter`
    /// arrives before the Confirm tab mounts. Spacing each keystroke lets React
    /// re-render (and the highlight/selection settle) in between
    /// (remote-control-dc9). `session_id` re-resolves the tab for the later
    /// chunks, since tab indices may shift during the sequence.
    PtyInputSequence {
        /// Workspace project index (for the first chunk).
        project: usize,
        /// Tab index within the project (for the first chunk).
        tab: usize,
        /// Session whose primary PTY receives the later chunks.
        session_id: SessionId,
        /// Keystroke chunks in order; chunk 0 is written now, chunk `i` at
        /// `now + i * step_delay_ms`.
        chunks: Vec<Vec<u8>>,
        /// Delay between consecutive chunks, in ms.
        step_delay_ms: u64,
    },
    /// Dispatch an existing app [`Command`] against the tab (the executor
    /// temporarily selects it, preserving the user's on-screen selection).
    Dispatch {
        /// Workspace project index.
        project: usize,
        /// Tab index within the project.
        tab: usize,
        /// The command to dispatch — all safety guards live behind it.
        command: Command,
    },
    /// Deferred to the event loop (async two-phase creation).
    NeedsMainLoop(MainLoopAction),
    /// A remote-shell operation against a resolved session (see [`ShellAction`]).
    Shell {
        /// Workspace project index.
        project: usize,
        /// Tab index within the project.
        tab: usize,
        /// The session the shell belongs to.
        session_id: SessionId,
        /// The operation to apply.
        action: ShellAction,
    },
    /// Refused, with an honest reason for the ack.
    Reject {
        /// Why the command was refused.
        reason: String,
    },
}

fn reject(reason: impl Into<String>) -> Translation {
    Translation::Reject {
        reason: reason.into(),
    }
}

/// Resolve a shell command's session to a [`Translation::Shell`], rejecting an
/// unknown session honestly. The shell registry state (cap, shell-id match) is
/// checked at execution, not here.
fn shell_translation(
    index: &SessionIndex,
    session_id: &SessionId,
    action: ShellAction,
) -> Translation {
    match index.session(session_id) {
        Some(s) => Translation::Shell {
            project: s.project,
            tab: s.tab,
            session_id: session_id.clone(),
            action,
        },
        None => reject(format!("unknown session '{session_id}'")),
    }
}

/// Map a protocol [`AgentType`] to the built-in registry key it launches.
/// (Inverse of [`feed::agent_type_of`]'s heuristic for the built-ins.)
fn agent_key_for(agent_type: AgentType) -> &'static str {
    match agent_type {
        AgentType::ClaudeCode => "claude",
        AgentType::Codex => "codex",
        AgentType::Opencode => "opencode",
    }
}

/// The keystroke injected into a supported backend's permission prompt.
///
/// The pending prompt is the agent CLI's own TUI prompt on the primary PTY,
/// so a decision is delivered as the keystroke that CLI expects. The map is
/// deliberately conservative — one unambiguous key per (backend, choice):
///
/// * **Claude Code** numbers its permission options (`1. Yes` / `2. Yes, and
///   …` / `3. No, and tell Claude what to do differently (esc)`), and a digit
///   selects immediately, so allow-once = `1`. Esc rejects the request.
///   Enter is deliberately **not** used for allow: it activates whatever
///   option currently has focus, which is not guaranteed to be allow-once.
/// * **Codex CLI**'s approval popup accepts the `y` shortcut for "Yes"
///   (approve once); Esc declines the request without entering the
///   provide-feedback flow.
/// * **OpenCode**'s permission dialog binds Enter = accept once ("once"),
///   `a` = accept always, Esc = reject. Enter is the documented accept-once
///   key (a fixed binding, not a focus-dependent default), so allow-once is
///   a carriage return.
///
/// Custom/unknown backends never reach this function — [`translate`] rejects
/// them honestly instead of guessing bytes at a prompt we cannot classify.
pub fn permission_keystroke(backend: StatusBackend, choice: PermissionChoice) -> &'static [u8] {
    const ESC: &[u8] = b"\x1b";
    match (backend, choice) {
        (StatusBackend::Claude, PermissionChoice::AllowOnce) => b"1",
        (StatusBackend::Claude, PermissionChoice::Deny) => ESC,
        (StatusBackend::Codex, PermissionChoice::AllowOnce) => b"y",
        (StatusBackend::Codex, PermissionChoice::Deny) => ESC,
        (StatusBackend::OpenCode, PermissionChoice::AllowOnce) => b"\r",
        (StatusBackend::OpenCode, PermissionChoice::Deny) => ESC,
    }
}

/// The keystrokes injected to select the `option_index`-th option of a
/// multi-option (Question) prompt on a supported backend's TUI list.
///
/// * **Claude Code** (`AskUserQuestion`) lists its options numbered `1..N`, and
///   pressing the **number key selects AND submits** that option outright —
///   verified live: the binary "1" keystroke answered option 1. This is used in
///   preference to arrow navigation because it is robust to the initial cursor
///   position, the terminal's cursor-keys mode, and any extra trailing rows
///   ("Type something" / "Chat about this") the list renders. Only single-digit
///   options (`1..9`, i.e. `index < 9`) are reachable this way; a longer list
///   falls back to arrow navigation. Fixes the phone answering option 1 but the
///   agent registering option 3 (remote-control-qa1).
/// * **OpenCode** (`question.asked`) uses arrow navigation: from the first
///   option focused, `n` DOWN arrows then Enter select option `n`. DOWN is the
///   CSI form `ESC [ B` — the exact bytes the desktop's own keyboard handler
///   sends for the Down key (see `tui::input`), which both agents accept. (An
///   earlier SS3 `ESC O B` form, gated on the terminal's DECCKM state, was
///   WRONG: the desktop is not DECCKM-aware, so its physical arrows are always
///   CSI; injecting SS3 for a DECCKM app like Claude was silently ignored —
///   remote-control-dc9.)
/// * **Codex** has no multi-option Question prompt, so it returns `None`; the
///   translate arm turns that into an honest rejection.
pub fn option_keystroke(backend: StatusBackend, option_index: u32) -> Option<Vec<u8>> {
    match backend {
        // Claude: press the option's number (select + submit) when single-digit.
        StatusBackend::Claude if option_index < 9 => Some(vec![b'1' + option_index as u8]),
        StatusBackend::Claude | StatusBackend::OpenCode => {
            // Arrow navigation: `n` DOWN presses (CSI, matching `tui::input`).
            let mut bytes = Vec::new();
            for _ in 0..option_index {
                bytes.extend_from_slice(DOWN_ARROW);
            }
            bytes.push(b'\r');
            Some(bytes)
        }
        // Codex has no multi-option prompt; refuse rather than guess.
        StatusBackend::Codex => None,
    }
}

/// The Down-arrow bytes the desktop injects for list navigation — the CSI form
/// `ESC [ B`, identical to what `tui::input` sends for a physical Down key.
/// FlightDeck's input layer is not DECCKM-aware (it never emits the SS3 `ESC O
/// B` form), so both agents' TUIs are driven with CSI regardless of their
/// cursor-keys mode (remote-control-dc9).
const DOWN_ARROW: &[u8] = b"\x1b[B";

/// Delay between consecutive injected keystrokes when driving Claude's
/// multi-select form (see [`Translation::PtyInputSequence`]). Long enough for
/// the Ink TUI to re-render between keys so the highlight actually moves and the
/// toggle/submit see the settled state, short enough to feel responsive. The
/// 50ms main-loop poll means each step rounds up to the next tick.
pub const MULTI_SELECT_STEP_DELAY_MS: u64 = 150;

/// The spaced keystroke chunks that drive a MULTI-question tabbed form (a Claude
/// `AskUserQuestion` carrying several questions) to answer each tab and submit.
/// `selections[i]` is the chosen 0-based option indices for question tab `i`
/// (each already sorted + deduplicated by the caller), in tab order.
///
/// Each chunk is one keystroke, delivered one [`MULTI_SELECT_STEP_DELAY_MS`]
/// apart so an async (Ink/React) TUI re-render settles between them
/// (remote-control-dc9). Per the verified model (bd memory
/// `askuserquestion-real-tui-model`): each question is a tab, plus a final
/// **Confirm** tab that submits. For each question tab, from its first option:
/// one `Down` per row to reach each chosen option and `Enter` to toggle/select
/// it; then `Tab` to advance to the next tab. After the LAST question that
/// final `Tab` lands on the Confirm tab, and a closing `Enter` submits the whole
/// form. A question with no selected options contributes only its `Tab` (the tab
/// is skipped, leaving it unanswered).
fn multi_question_chunks(selections: &[Vec<u32>]) -> Vec<Vec<u8>> {
    let mut chunks: Vec<Vec<u8>> = Vec::new();
    for indices in selections {
        let mut cursor = 0u32;
        for &target in indices {
            // One Down per row: consecutive Downs also race the re-render (each
            // handler would read the same stale index), so they are separate
            // chunks.
            for _ in cursor..target {
                chunks.push(DOWN_ARROW.to_vec());
            }
            chunks.push(b"\r".to_vec()); // Enter toggles/selects the highlighted option.
            cursor = target;
        }
        // Tab to the next question tab — or, after the last question, to Confirm.
        chunks.push(b"\t".to_vec());
    }
    chunks.push(b"\r".to_vec()); // Enter on the Confirm tab submits the whole form.
    chunks
}

/// The spaced keystroke chunks that drive Claude's SINGLE-question multi-select
/// form to toggle exactly `indices` and submit. `indices` must be sorted +
/// deduplicated by the caller. A single-question form is just the one-tab case
/// of [`multi_question_chunks`]: navigate + toggle within the sole question,
/// then `Tab` to Confirm and `Enter` to submit.
fn claude_multi_select_chunks(indices: &[u32]) -> Vec<Vec<u8>> {
    multi_question_chunks(std::slice::from_ref(&indices.to_vec()))
}

/// The keystrokes injected to select SEVERAL options of a multi-select
/// (checklist / `multiSelect`) Question prompt, then submit them together.
/// `option_indices` are the phone's chosen 0-based indices (deduplicated and
/// sorted here so navigation is monotonic and each option is toggled once).
///
/// Claude Code and OpenCode render the SAME multi-select form (verified live —
/// remote-control-dc9, see bd memory `askuserquestion-real-tui-model`):
///
/// * **Up/Down** move the highlight within the current question's options.
/// * **Enter** *toggles* the highlighted option's checkbox — it does NOT submit
///   (this is the key difference from single-select, where Enter/the number key
///   selects AND submits).
/// * **Tab** (and Left/Right) switch between question tabs; the far-right
///   **Confirm** tab submits the whole form.
///
/// So, from the initial highlight on the first option, this walks Down to each
/// target in ascending order and presses Enter to toggle it, then presses Tab
/// to reach the Confirm tab and Enter to submit. Down is the CSI form `ESC [ B`
/// ([`DOWN_ARROW`], matching the desktop's own key encoding); Tab (`\t`) and
/// Enter (`\r`) are mode-independent.
///
/// SCOPE: a single question's options. Multi-QUESTION forms are answered via
/// [`multi_question_keystroke`] / [`multi_question_chunks`], which walk the
/// question tabs before reaching Confirm.
///
/// **Codex** has no such prompt → `None` (an honest rejection upstream).
pub fn multi_option_keystroke(backend: StatusBackend, option_indices: &[u32]) -> Option<Vec<u8>> {
    let mut indices: Vec<u32> = option_indices.to_vec();
    indices.sort_unstable();
    indices.dedup();
    if indices.is_empty() {
        return None;
    }
    match backend {
        // Both render the same one-question form; emit its chunks as one burst
        // (OpenCode processes input synchronously, so it needs no spacing).
        StatusBackend::Claude | StatusBackend::OpenCode => {
            Some(multi_question_chunks(std::slice::from_ref(&indices)).concat())
        }
        // Codex has no multi-option prompt; refuse rather than guess.
        StatusBackend::Codex => None,
    }
}

/// The flattened keystrokes that answer a MULTI-question tabbed form in one
/// burst (used for OpenCode, which processes input synchronously). `selections`
/// is the per-question chosen indices in tab order; each inner slice is sorted +
/// deduplicated here. See [`multi_question_chunks`] for the navigation model.
/// **Codex** has no such prompt → `None`.
pub fn multi_question_keystroke(
    backend: StatusBackend,
    selections: &[Vec<u32>],
) -> Option<Vec<u8>> {
    match backend {
        StatusBackend::Claude | StatusBackend::OpenCode => {
            Some(multi_question_chunks(&normalize_selections(selections)).concat())
        }
        StatusBackend::Codex => None,
    }
}

/// Sort + deduplicate each question's chosen option indices so navigation is
/// monotonic and each option is toggled at most once per question.
fn normalize_selections(selections: &[Vec<u32>]) -> Vec<Vec<u32>> {
    selections
        .iter()
        .map(|indices| {
            let mut v = indices.clone();
            v.sort_unstable();
            v.dedup();
            v
        })
        .collect()
}

/// Encode a phone reply exactly as the desktop TUI delivers typed/pasted
/// input: the text as one paste (bracketed iff the agent enabled DECSET 2004,
/// newlines normalised to carriage returns — see `encode_paste`), followed by
/// the `\r` a terminal sends for Enter to submit it.
pub fn encode_reply(text: &str, bracketed: bool) -> Vec<u8> {
    let mut bytes = crate::encode_paste(text, bracketed);
    bytes.push(b'\r');
    bytes
}

/// Translate one phone command against the current [`SessionIndex`].
/// Pure: no I/O, no state mutation.
pub fn translate(body: &CommandBody, index: &SessionIndex) -> Translation {
    match body {
        CommandBody::Reply { session_id, text } => {
            let Some(s) = index.session(session_id) else {
                return reject(format!("unknown session '{session_id}'"));
            };
            if text.trim().is_empty() {
                return reject("empty reply");
            }
            if !s.primary_running {
                // A recovered tab whose project was not the active one at
                // startup is NotStarted but shows as Idle on the phone. Resume
                // it (the same continuation the desktop runs on navigation) and
                // deliver the reply once its terminal is ready, rather than
                // rejecting (remote-control-1l4). A genuinely stopped/exited
                // agent still asks the user to restart it explicitly.
                if s.primary_not_started {
                    return Translation::NeedsMainLoop(MainLoopAction::ResumeAndReply {
                        project: s.project,
                        tab_id: s.id.as_str().to_string(),
                        text: text.clone(),
                    });
                }
                return reject("the agent is not running; restart it first");
            }
            Translation::PtyInput {
                project: s.project,
                tab: s.tab,
                bytes: encode_reply(text, s.bracketed_paste),
            }
        }

        CommandBody::PermissionDecision {
            session_id,
            prompt_id,
            choice,
            option_index,
            option_indices,
            free_text,
            answers,
        } => {
            let Some(s) = index.session(session_id) else {
                return reject(format!("unknown session '{session_id}'"));
            };
            if !matches!(s.status, AgentStatus::NeedsInput) {
                return reject("no pending permission prompt for this session");
            }
            if s.pending_prompt.as_ref() != Some(prompt_id) {
                return reject("this prompt is no longer pending (answered or superseded)");
            }
            let Some(backend) = s.backend else {
                return reject(
                    "this session runs a custom agent; permission prompts \
                     can only be answered from the desktop",
                );
            };
            if !s.primary_running {
                return reject("the agent is not running");
            }
            // Decision-field precedence (commands.rs never sees the prompt
            // kind, so bytes are chosen purely from which decision field is
            // set):
            //   1. `choice`         — the binary fast-path (byte-identical to
            //                         the original permission-only behaviour).
            //   2. `free_text`      — a typed answer, delivered like any reply.
            //                         Deliberately ranked ABOVE the option
            //                         fields: if a phone somehow sends both, the
            //                         explicit free-text answer wins over a list
            //                         position.
            //   3. `answers`        — a MULTI-question tabbed form: per-question
            //                         selections walked across the question tabs
            //                         to Confirm. Ranked above the single-question
            //                         option fields because a phone only populates
            //                         it for a multi-question prompt.
            //   4. `option_indices` — single-question multi-select (checklist)
            //                         toggle + submit. Ranked above `option_index`
            //                         because it is the more specific answer; a
            //                         phone only populates it for a `multi_select`
            //                         prompt.
            //   5. `option_index`   — single-question single-option selection
            //                         (the untouched number-key path).
            //   6. otherwise        — nothing actionable; reject honestly.
            let free_text = free_text.as_deref().filter(|t| !t.is_empty());
            let answers = answers.as_deref().filter(|a| !a.is_empty());
            let option_indices = option_indices.as_deref().filter(|v| !v.is_empty());
            let bytes = if let Some(choice) = choice {
                permission_keystroke(backend, *choice).to_vec()
            } else if let Some(text) = free_text {
                encode_reply(text, s.bracketed_paste)
            } else if let Some(answers) = answers {
                // Multi-question tabbed form. For each question tab, navigate to
                // and toggle the chosen options, Tab to the next tab, then Enter
                // on the final Confirm tab. Claude's Ink TUI re-renders
                // asynchronously, so its keystrokes must be spaced out as a timed
                // sequence (remote-control-dc9); OpenCode processes input
                // synchronously and takes the whole burst at once.
                let selections: Vec<Vec<u32>> =
                    answers.iter().map(|a| a.option_indices.clone()).collect();
                let selections = normalize_selections(&selections);
                if backend == StatusBackend::Claude {
                    return Translation::PtyInputSequence {
                        project: s.project,
                        tab: s.tab,
                        session_id: session_id.clone(),
                        chunks: multi_question_chunks(&selections),
                        step_delay_ms: MULTI_SELECT_STEP_DELAY_MS,
                    };
                }
                match multi_question_keystroke(backend, &selections) {
                    Some(b) => b,
                    None => {
                        return reject("this agent does not support multi-question prompts");
                    }
                }
            } else if let Some(indices) = option_indices {
                // Claude's Ink TUI races injected keystrokes against its async
                // re-render, so navigation + toggle + submit must be spaced out —
                // a burst toggles the wrong (pre-move) option and never submits
                // (remote-control-dc9). Drive it as a timed keystroke sequence.
                // OpenCode processes input synchronously and submits in one burst.
                if backend == StatusBackend::Claude {
                    let mut sorted: Vec<u32> = indices.to_vec();
                    sorted.sort_unstable();
                    sorted.dedup();
                    return Translation::PtyInputSequence {
                        project: s.project,
                        tab: s.tab,
                        session_id: session_id.clone(),
                        chunks: claude_multi_select_chunks(&sorted),
                        step_delay_ms: MULTI_SELECT_STEP_DELAY_MS,
                    };
                }
                match multi_option_keystroke(backend, indices) {
                    Some(b) => b,
                    None => {
                        return reject("this agent does not support multi-select prompts");
                    }
                }
            } else if let Some(n) = option_index {
                match option_keystroke(backend, *n) {
                    Some(b) => b,
                    None => {
                        return reject("this agent does not support multi-option prompts");
                    }
                }
            } else {
                return reject("empty decision");
            };
            Translation::PtyInput {
                project: s.project,
                tab: s.tab,
                bytes,
            }
        }

        CommandBody::NewAgent {
            project_id,
            agent_type,
            name,
            base_branch,
            first_task,
        } => {
            let Some(p) = index.project(project_id) else {
                return reject(format!("unknown project '{project_id}'"));
            };
            if base_branch != &p.base_branch {
                return reject(format!(
                    "base branch must be '{}' (per-tab base branches are not \
                     supported from the phone)",
                    p.base_branch
                ));
            }
            Translation::NeedsMainLoop(MainLoopAction::NewAgent {
                project: p.index,
                name: name.clone(),
                agent_key: agent_key_for(*agent_type).to_string(),
                first_task: first_task.clone(),
            })
        }

        CommandBody::RestartAgent { session_id } => match index.session(session_id) {
            // Fresh process, same worktree/branch (PRD §5.8); the transcript
            // builder keyed by session id is untouched, so history persists.
            Some(s) => Translation::Dispatch {
                project: s.project,
                tab: s.tab,
                command: Command::RestartAgent,
            },
            None => reject(format!("unknown session '{session_id}'")),
        },

        CommandBody::CloseSession { session_id } => match index.session(session_id) {
            // Close semantics (documented choice): never force-kill from the
            // phone. If everything already stopped, close the tab
            // (`IfAllStopped` — removes it, or refuses if something raced back
            // to life). Otherwise send Ctrl-C to the primary (the desktop's
            // default suggested action, SPECS §25) and leave the tab open; the
            // ack says so and the phone can retry close once it settles.
            Some(s) => Translation::Dispatch {
                project: s.project,
                tab: s.tab,
                command: Command::CloseAgentTab {
                    action: Some(if s.all_stopped {
                        CloseAction::IfAllStopped
                    } else {
                        CloseAction::CtrlCPrimary
                    }),
                },
            },
            None => reject(format!("unknown session '{session_id}'")),
        },

        CommandBody::SetManualStatus { session_id, label } => {
            let Some(s) = index.session(session_id) else {
                return reject(format!("unknown session '{session_id}'"));
            };
            match ManualStatus::from_str_lossy(label) {
                Some(status) => Translation::Dispatch {
                    project: s.project,
                    tab: s.tab,
                    command: Command::SetManualStatus(Some(status)),
                },
                None => reject(format!(
                    "unknown manual status '{label}' (valid: in progress, \
                     waiting, blocked, done)"
                )),
            }
        }

        CommandBody::ClearManualStatus { session_id } => match index.session(session_id) {
            Some(s) => Translation::Dispatch {
                project: s.project,
                tab: s.tab,
                command: Command::SetManualStatus(None),
            },
            None => reject(format!("unknown session '{session_id}'")),
        },

        // --- Git actions: dispatch the existing guarded commands so every
        //     safety guard (no history rewrite, precondition checks, confirm
        //     gates) is inherited — never GitExecutor directly. The phone has
        //     already confirmed destructive actions (PRD §8), so the confirmed
        //     path is dispatched (mirroring the TUI's post-confirm dispatch). ---
        CommandBody::GitPullBase { session_id } => match index.session(session_id) {
            // Pull base is a global (repo-root) action; it ignores the selected
            // tab, but we route through the session so the ack targets a real
            // project and unknown sessions are refused honestly.
            Some(s) => Translation::Dispatch {
                project: s.project,
                tab: s.tab,
                command: Command::PullBase,
            },
            None => reject(format!("unknown session '{session_id}'")),
        },

        CommandBody::GitMergeBack { session_id } => match index.session(session_id) {
            // The phone confirmed already (PRD §8): dispatch the confirmed
            // FinishLocalMerge, exactly as the TUI does after its MergeConfirm.
            // Preconditions (dirty base/worktree, wrong branch, conflicts) are
            // enforced inside dispatch and surface as honest ack outcomes.
            Some(s) => Translation::Dispatch {
                project: s.project,
                tab: s.tab,
                command: Command::FinishLocalMerge { confirm: true },
            },
            None => reject(format!("unknown session '{session_id}'")),
        },

        CommandBody::GitAbandonWorktree {
            session_id,
            confirm_name,
        } => {
            let Some(s) = index.session(session_id) else {
                return reject(format!("unknown session '{session_id}'"));
            };
            // Destructive: the typed confirmation must match the session name
            // exactly (the same type-to-confirm guard the desktop enforces).
            if confirm_name != &s.name {
                return reject(format!(
                    "confirmation name '{confirm_name}' does not match the session name '{}'",
                    s.name
                ));
            }
            Translation::Dispatch {
                project: s.project,
                tab: s.tab,
                command: Command::AbandonWorktree { confirm: true },
            }
        }

        // --- Remote shell: resolve the session here (unknown → honest reject);
        //     the cap check and shell-id matching need the live shell registry,
        //     so they run at execution time. ---
        CommandBody::ShellOpen {
            session_id,
            shell_id,
            cols,
            rows,
        } => shell_translation(
            index,
            session_id,
            ShellAction::Open {
                shell_id: shell_id.clone(),
                cols: *cols,
                rows: *rows,
            },
        ),
        CommandBody::ShellInput {
            session_id,
            shell_id,
            data,
        } => shell_translation(
            index,
            session_id,
            ShellAction::Input {
                shell_id: shell_id.clone(),
                bytes: data.clone().into_bytes(),
            },
        ),
        CommandBody::ShellInterrupt {
            session_id,
            shell_id,
        } => shell_translation(
            index,
            session_id,
            ShellAction::Interrupt {
                shell_id: shell_id.clone(),
            },
        ),
        CommandBody::ShellClose {
            session_id,
            shell_id,
        } => shell_translation(
            index,
            session_id,
            ShellAction::Close {
                shell_id: shell_id.clone(),
            },
        ),

        // --- Activity feed read-state: a separate task. ---
        CommandBody::MarkRead { .. } => reject("mark_read is not implemented yet"),

        // Serviced inside RemoteBridge::route_command; they only reach the
        // translator if that routing changes — refuse rather than guess.
        CommandBody::RequestSnapshot { .. } | CommandBody::RequestTranscript { .. } => {
            reject("data requests are serviced by the feed bridge")
        }
    }
}

// ===========================================================================
// First-task delivery (for phone-initiated sessions)
// ===========================================================================

/// How long to wait for a fresh agent to enable bracketed paste before
/// falling back to raw text (covers plain-prompt/custom agents).
pub const FIRST_TASK_BRACKETED_WAIT_MS: u64 = 10_000;

/// After this, an undeliverable first task is dropped (the session's snapshot
/// state tells the phone what actually happened to the tab).
pub const FIRST_TASK_EXPIRY_MS: u64 = 120_000;

/// A prompt queued for an agent that is not ready to receive it yet, awaiting a
/// ready terminal. Two sources: a `new_agent` command's first task, and a reply
/// sent to a not-started agent that is being resumed (remote-control-1l4). Both
/// share the same readiness gate ([`first_task_decision`]) and expiry.
pub struct PendingFirstTask {
    /// The tab (== session) the task belongs to.
    pub tab_id: String,
    /// The task text.
    pub text: String,
    /// When it was queued (clock millis), for the fallback/expiry windows.
    pub queued_at_ms: u64,
}

/// Whether a pending first task should be delivered now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirstTaskDecision {
    /// Not ready yet; keep it queued.
    Wait,
    /// Deliver now, framed per `bracketed`.
    Send {
        /// Whether to wrap the text in bracketed-paste guards.
        bracketed: bool,
    },
    /// Give up (expired); drop it.
    Expire,
}

/// Decide what to do with a queued first task, given the tab's current state.
/// Prefers a bracketed paste (what interactive agents expect) as soon as the
/// agent enables the mode; falls back to raw text after
/// [`FIRST_TASK_BRACKETED_WAIT_MS`] so plain-prompt agents still get the task.
pub fn first_task_decision(
    primary_running: bool,
    bracketed_paste: bool,
    age_ms: u64,
) -> FirstTaskDecision {
    if age_ms >= FIRST_TASK_EXPIRY_MS {
        return FirstTaskDecision::Expire;
    }
    if !primary_running {
        return FirstTaskDecision::Wait;
    }
    if bracketed_paste {
        return FirstTaskDecision::Send { bracketed: true };
    }
    if age_ms >= FIRST_TASK_BRACKETED_WAIT_MS {
        return FirstTaskDecision::Send { bracketed: false };
    }
    FirstTaskDecision::Wait
}

#[cfg(test)]
mod tests;
