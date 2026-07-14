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
    ProjectId, PromptId, SessionId,
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
            Translation::PtyInput {
                project: s.project,
                tab: s.tab,
                bytes: permission_keystroke(backend, *choice).to_vec(),
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

        // --- Git actions: a separate task. The translations will dispatch the
        //     existing guarded commands (PullBase / FinishLocalMerge /
        //     AbandonWorktree with the confirm_name echo checked against the
        //     session name) — never GitExecutor directly. ---
        CommandBody::GitPullBase { .. } => reject("git pull-base is not implemented yet"),
        CommandBody::GitMergeBack { .. } => reject("git merge-back is not implemented yet"),
        CommandBody::GitAbandonWorktree { .. } => reject("abandon worktree is not implemented yet"),

        // --- Remote shell: a separate task (ShellOpen/Input/Interrupt/Close
        //     will manage a dedicated child terminal per session). ---
        CommandBody::ShellOpen { .. }
        | CommandBody::ShellInput { .. }
        | CommandBody::ShellInterrupt { .. }
        | CommandBody::ShellClose { .. } => reject("the remote shell is not implemented yet"),

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

/// A first task queued by a `new_agent` command, awaiting a ready agent.
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
