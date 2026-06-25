//! Application commands and effects (T7, SPECS §22).
//!
//! [`Command`] covers every SPECS §22 command-palette action. The dispatcher
//! lives in [`crate::app::state`] (`AppState::dispatch`) so it has direct access
//! to the runtime tabs and live sessions; this module only defines the data the
//! UI passes in and the [`Effect`] it gets back.
//!
//! The app core never executes git/fs/pty directly — `dispatch` calls the
//! services through trait objects (SPECS §27).

use crate::contracts::ManualStatus;
use crate::git::remote::PushPlan;
use crate::git::status::WorktreeStatus;

/// A relative or index-based target for tab/child switching (SPECS §22).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selector {
    /// Select by zero-based index.
    Index(usize),
    /// Select the next item, wrapping.
    Next,
    /// Select the previous item, wrapping.
    Prev,
}

/// How to handle a tab's running processes when closing it (SPECS §25).
///
/// FlightDeck never escalates to force-kill automatically: the UI presents the
/// option set ([`CloseTabOptions`]) and the user picks one of these, which the
/// UI then dispatches as [`Command::CloseAgentTab`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseAction {
    /// Send Ctrl-C to the primary agent (the default suggested action).
    CtrlCPrimary,
    /// Send Ctrl-C to the primary and all child terminals.
    CtrlCAll,
    /// Force-terminate the whole process tree.
    ForceTerminate,
    /// Close only if all processes have already stopped (refuse otherwise).
    IfAllStopped,
}

impl CloseAction {
    /// The default suggested close action (SPECS §25).
    pub fn default_action() -> CloseAction {
        CloseAction::CtrlCPrimary
    }
}

/// The close-tab option set surfaced to the user (SPECS §25). The first entry is
/// the default suggested action and FlightDeck never auto-escalates to force.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloseTabOptions {
    /// The actions to offer, in display order; `actions[0]` is the default.
    pub actions: Vec<CloseAction>,
}

impl CloseTabOptions {
    /// The standard option set (SPECS §25), default-first, force never default.
    pub fn standard() -> CloseTabOptions {
        CloseTabOptions {
            actions: vec![
                CloseAction::CtrlCPrimary,
                CloseAction::CtrlCAll,
                CloseAction::ForceTerminate,
                CloseAction::IfAllStopped,
            ],
        }
    }

    /// The default suggested action (SPECS §25).
    pub fn default_action(&self) -> CloseAction {
        self.actions
            .first()
            .copied()
            .unwrap_or(CloseAction::CtrlCPrimary)
    }
}

/// How to handle uncommitted changes when pushing (SPECS §14).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushConfirm {
    /// Push committed changes only (the warned-about path).
    PushCommitted,
    /// Cancel the push (the user will commit manually first).
    Cancel,
}

/// Every command-palette action (SPECS §22). Payloads carry exactly the data the
/// dispatcher needs; everything else is read from [`AppState`](crate::app::state::AppState).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// New Agent Tab: create branch + worktree and spawn the agent (SPECS §4, §16, §17).
    NewAgentTab {
        /// Free-form tab/task name (label only — never passed to the agent).
        name: String,
        /// Agent registry key; `None` uses the configured default (SPECS §4).
        agent_key: Option<String>,
    },
    /// Rename the selected Agent Tab — name only, never branch/slug (SPECS §18).
    RenameAgentTab {
        /// The new display name.
        new_name: String,
    },
    /// Close the selected Agent Tab with the chosen process-handling action.
    /// When `action` is `None`, dispatch returns the option set without closing
    /// (SPECS §25 — the UI then re-dispatches with a chosen action).
    CloseAgentTab {
        /// How to handle running processes; `None` = ask (return options).
        action: Option<CloseAction>,
    },
    /// Push the selected tab's branch (SPECS §14). With `confirm` `None`, a dirty
    /// worktree returns the push warning instead of pushing.
    PushBranch {
        /// The user's choice once warned; `None` = not yet confirmed.
        confirm: Option<PushConfirm>,
    },
    /// Finish / local merge-back of the selected tab into base (SPECS §13, §15).
    FinishLocalMerge {
        /// Whether the user explicitly confirmed the merge (SPECS §15).
        confirm: bool,
    },
    /// Rebase the selected tab's worktree onto its base branch (SPECS §5
    /// carve-out). With `confirm` false the first dispatch checks preconditions
    /// and returns [`Effect::RebaseConfirm`]; the UI confirms and re-dispatches
    /// with `confirm: true`. Aborts (leaving the worktree untouched) on conflict.
    RebaseWorktree {
        /// Whether the user explicitly confirmed the rebase.
        confirm: bool,
    },
    /// Copy `.env.local` or `.env` from the base folder into the selected worktree.
    CopyEnvFile,
    /// Abandon (remove) the selected tab's worktree (SPECS §5/§15). With
    /// `confirm` false, a dirty worktree returns [`Effect::AbandonWarning`]
    /// instead of removing; with `confirm` true the worktree is force-removed
    /// even with uncommitted changes.
    AbandonWorktree {
        /// Whether the user confirmed discarding uncommitted changes.
        confirm: bool,
    },
    /// Open a new child shell terminal in the selected tab (SPECS §19).
    NewChildTerminal,
    /// Close the selected tab's currently-selected child terminal (SPECS §19).
    CloseChildTerminal,
    /// Switch the selected Agent Tab (SPECS §22).
    SwitchAgentTab(Selector),
    /// Switch the selected tab's child terminal (SPECS §22).
    SwitchChildTerminal(Selector),
    /// Set or clear the manual status override (SPECS §24). `None` clears it.
    SetManualStatus(Option<ManualStatus>),
    /// Restart the primary agent of the selected (recovered/stopped) tab (SPECS §10, §23).
    RestartAgent,
    /// Open a shell child terminal (alias of New Child Terminal for recovered
    /// tabs, SPECS §10/§22 "Open Shell").
    OpenShell,
    /// Show the git status panel for the selected tab (SPECS §21).
    ShowGitStatus,
    /// Show help / keybindings (SPECS §23).
    ShowHelp,
    /// Toggle split view: lay the selected tab's terminals (agent + shells) out
    /// side by side in equal-width columns instead of as horizontal tabs.
    ToggleSplitView,
    /// Quit FlightDeck (signals clean teardown to the wiring layer, SPECS §23).
    Quit,
}

/// What the UI should surface after a [`Command`] is dispatched.
///
/// The app core is headless: it never renders. It returns one of these so the
/// TUI can decide what to draw (a toast, a modal, a URL, a refusal, …).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Nothing to surface.
    None,
    /// FlightDeck should quit (wiring layer terminates sessions, SPECS §23).
    Quit,
    /// An informational message (e.g. "Pushed flightdeck/foo").
    Message(String),
    /// A persistent/important warning (e.g. dirty base → merge disabled, §13).
    Warning(String),
    /// A safety refusal with its reason (e.g. dirty worktree on abandon, §5/§15).
    Refused(String),
    /// A GitHub PR compare URL the user should open (SPECS §14).
    PrUrl(String),
    /// The push warning + the plan that triggered it; the UI offers the options
    /// and re-dispatches `PushBranch { confirm: Some(..) }` (SPECS §14).
    PushWarning(PushPlan),
    /// The selected tab's worktree has uncommitted changes; the UI must confirm
    /// before re-dispatching `AbandonWorktree { confirm: true }` (SPECS §5/§15).
    AbandonWarning,
    /// The merge is ready and awaits explicit confirmation; the UI confirms then
    /// re-dispatches `FinishLocalMerge { confirm: true }` (SPECS §15). On success
    /// the worktree is removed and the tab closed, so a running agent is stopped.
    MergeConfirm {
        /// The agent branch being merged.
        agent_branch: String,
        /// The base branch it merges into.
        base_branch: String,
        /// Whether the selected tab's primary agent is still running (it will be
        /// stopped as part of the post-merge cleanup).
        primary_running: bool,
    },
    /// A rebase is ready and awaits explicit confirmation; the UI confirms then
    /// re-dispatches `RebaseWorktree { confirm: true }` (SPECS §5 carve-out).
    /// Rewrites the worktree branch's history, so it is always confirmed first.
    RebaseConfirm {
        /// The agent branch being rebased.
        agent_branch: String,
        /// The base branch it is rebased onto.
        base_branch: String,
        /// How many commits the base has moved since this tab was created
        /// (SPECS §12 drift) — shown so the user knows what they are pulling in.
        drift: u32,
        /// Whether the primary agent is still running in the worktree. A rebase
        /// rewrites its HEAD underneath it, so the user is warned.
        primary_running: bool,
    },
    /// The branch already existed and was attached to (surfaced, never silent, §11).
    AttachedExisting {
        /// The attached branch name.
        branch: String,
    },
    /// The close-tab option set; the UI re-dispatches `CloseAgentTab` with a
    /// chosen [`CloseAction`] (SPECS §25).
    CloseTabOptions(CloseTabOptions),
    /// The git status panel data for the selected tab (SPECS §21).
    GitStatus(Box<WorktreeStatus>),
    /// The help screen should be shown (SPECS §23).
    ShowHelp,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_close_action_is_ctrl_c_primary() {
        // SPECS §25: default suggested action; never auto-escalate to force.
        assert_eq!(CloseAction::default_action(), CloseAction::CtrlCPrimary);
        let opts = CloseTabOptions::standard();
        assert_eq!(opts.default_action(), CloseAction::CtrlCPrimary);
        // Force is present but never first (never the default).
        assert_ne!(opts.actions[0], CloseAction::ForceTerminate);
        assert!(opts.actions.contains(&CloseAction::ForceTerminate));
    }

    #[test]
    fn standard_close_options_has_all_actions() {
        let opts = CloseTabOptions::standard();
        assert!(opts.actions.contains(&CloseAction::CtrlCPrimary));
        assert!(opts.actions.contains(&CloseAction::CtrlCAll));
        assert!(opts.actions.contains(&CloseAction::ForceTerminate));
        assert!(opts.actions.contains(&CloseAction::IfAllStopped));
    }
}
