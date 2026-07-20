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
/// EMPIRICAL ASSUMPTION — **UNVERIFIED, MUST BE VALIDATED ON A LIVE BUILD.**
/// Unlike [`permission_keystroke`] (whose per-key bindings are documented CLI
/// shortcuts), the arrow-navigation model below is inferred from how these
/// TUIs render selectable lists, not confirmed against a running agent. It is
/// flagged in the s81 handoff and cannot be checked unattended.
///
/// The model: a Question prompt renders its options as a vertical list with
/// the first option (index 0) focused. Moving the selection down one option is
/// a DOWN arrow (ESC `[` `B` = `b"\x1b[B"`); Enter (`b"\r"`) activates the
/// focused option. So selecting option `n` is `n` DOWN arrows followed by
/// Enter; index 0 is just Enter.
///
/// * **Claude Code** (`AskUserQuestion`) and **OpenCode** (`question.asked`)
///   both use this arrow-nav list model.
/// * **Codex** has no multi-option Question prompt, so it returns `None`; the
///   translate arm turns that into an honest rejection.
pub fn option_keystroke(backend: StatusBackend, option_index: u32) -> Option<Vec<u8>> {
    const DOWN: &[u8] = b"\x1b[B";
    match backend {
        StatusBackend::Claude | StatusBackend::OpenCode => {
            let mut bytes = Vec::new();
            for _ in 0..option_index {
                bytes.extend_from_slice(DOWN);
            }
            bytes.push(b'\r');
            Some(bytes)
        }
        // Codex has no multi-option prompt; refuse rather than guess.
        StatusBackend::Codex => None,
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
            option_index,
            free_text,
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
            //   1. `choice`      — the binary fast-path (byte-identical to the
            //                      original permission-only behaviour).
            //   2. `free_text`   — a typed answer, delivered like any reply.
            //                      Deliberately ranked ABOVE `option_index`: if
            //                      a phone somehow sends both, the explicit
            //                      free-text answer wins over a list position.
            //   3. `option_index`— arrow-nav selection of a Question option.
            //   4. otherwise     — nothing actionable; reject honestly.
            let free_text = free_text.as_deref().filter(|t| !t.is_empty());
            let bytes = if let Some(choice) = choice {
                permission_keystroke(backend, *choice).to_vec()
            } else if let Some(text) = free_text {
                encode_reply(text, s.bracketed_paste)
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
