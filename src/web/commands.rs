//! The browser's command surface: one table, from wire name to palette action
//! (`specs/WEB_INTERFACE.md` §1, D3, D13, D16; SPECS §5, §22).
//!
//! ## Why this module exists at all
//!
//! §1 says the browser "drives the same code paths". The failure mode that
//! sentence guards against is a second implementation: a host-side
//! `"abandon_worktree" => remove_the_worktree()` arm that starts out identical
//! to the TUI's and drifts from it on the first bug fix. So nothing here
//! *performs* a command. [`Route::Palette`] carries a
//! [`PaletteAction`] — the very value the TUI's own palette hands to
//! `run_palette_action` when the user presses Enter on a row — and `src/lib.rs`
//! passes it into that same function. The browser is a second way to choose a
//! row, not a second way to run one.
//!
//! ## Why the mapping is a table and not a `match` on the name
//!
//! Two requirements meet here:
//!
//! 1. **Nothing may be silently unreachable.** A palette action added to
//!    `crate::tui::palette` must either get a wire name or say out loud why it
//!    has none. [`exposure_of`] is an exhaustive `match` over [`PaletteAction`]
//!    (and, through it, over [`Command`]) with **no wildcard arm**: a new
//!    variant in either enum fails to compile until it is classified. The
//!    accompanying test then checks the other direction — that the name it
//!    claims really is in [`INVENTORY`].
//! 2. **The browser must not have to guess.** The same table is serialised into
//!    [`Snapshot::commands`](crate::web::protocol::Snapshot::commands), so the
//!    palette the browser draws is the palette this build actually has, labels,
//!    groups, `host only` badges and refusals included.
//!
//! ## What "reachable" does not mean
//!
//! Reachable by name is not the same as runnable today, and the difference is
//! stated per row rather than hidden:
//!
//! * [`Route::Rejected`] — the host knows the command and refuses it, with the
//!   reason. D16's two desktop-only actions land here (their effect is a window
//!   on the host's screen, so a fake `Applied` would be a lie), and so does
//!   `quit`: a bare frame naming it **cannot** kill the process, it is refused
//!   pending artboard 1g's two-step confirmation (`remote-control-ll5.4`).
//! * [`Route::NotSupported`] — the action opens a desktop dialog the browser
//!   cannot see. Acking `Applied` while a modal appears on the host's screen
//!   would be the worst of both worlds, so the whole dialog family waits for
//!   `remote-control-ll5.3` and the git family for `.5`, and says so.
//!
//! ## The git-ownership boundary holds by construction (SPECS §5)
//!
//! A `Command` frame carries `args`, and a `Route::Palette` row **ignores them
//! entirely**: the action, including every `confirm` flag inside it, comes from
//! this table. So no browser frame can smuggle `confirm: true` into
//! `AbandonWorktree`, and the two history-rewriting commands
//! ([`rewrites_history`]) are not on a forwarding route at all. Both facts are
//! asserted in this module's tests.

use crate::app::commands::{Command, Selector};
use crate::tui::palette::PaletteAction;
use crate::web::protocol::{command as names, CommandRun, CommandTarget, CommandView};

#[cfg(test)]
mod tests;

// ===========================================================================
// The table
// ===========================================================================

/// How the host answers one command name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Route {
    /// Answered by [`crate::web::server`] from published state, for any seat.
    /// Never travels to the TUI.
    Server,
    /// The shared selection (D3). Applied against the workspace by the TUI,
    /// which then moves the desktop with it.
    Selection(SelectionTarget),
    /// D11's read-marking, applied against the host's activity store.
    ActivityRead,
    /// Dispatched through the TUI's own palette path with this action.
    Palette(PaletteAction),
    /// Refused with [`crate::web::protocol::AckOutcome::Rejected`] and this
    /// reason: the host implements it, but its effect must not — or cannot —
    /// land for a browser.
    Rejected(&'static str),
    /// Refused with [`crate::web::protocol::ErrorCode::NotSupported`] and this
    /// reason: this build has no browser-side surface for it yet.
    NotSupported(&'static str),
}

/// Which part of the shared selection a [`Route::Selection`] moves (D3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionTarget {
    /// `args: { project_id }`.
    Project,
    /// `args: { session_id }`.
    Session,
    /// `args: { terminal_id }`.
    Terminal,
}

/// One command the host accepts: its wire name, how the palette renders it, and
/// what it routes to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandSpec {
    /// The wire name, from [`crate::web::protocol::command`].
    pub name: &'static str,
    /// The label the user reads, matching the TUI's palette row.
    pub label: &'static str,
    /// The palette group heading, matching the TUI's palette.
    pub group: &'static str,
    /// D16: the effect lands on the host's machine, so the row carries the
    /// `host only` badge and is never hidden.
    pub host_only: bool,
    /// Artboard 1d's right-hand tag, when the host can word it statically.
    pub annotation: Option<&'static str>,
    /// How the host answers it.
    pub route: Route,
}

/// The reason `quit` is refused. Named because both the refusal and the test
/// that proves a bare frame cannot kill the process read it.
pub const QUIT_REFUSAL: &str = "Quit stops FlightDeck and every agent running in it. \
     From a browser that needs the two-step confirmation, which this build does \
     not have yet — quit from the desktop.";

/// Why a desktop-only action cannot be run from a browser (D16). One sentence,
/// so `Open Worktree in File Manager` and `edit in $EDITOR` refuse identically.
pub const HOST_ONLY_REFUSAL: &str =
    "This opens a window on the machine running FlightDeck, which is not the \
     machine this browser is on. Run it from the desktop.";

/// Why the dialog family is refused today (D13, `remote-control-ll5.3`).
const DIALOG_REFUSAL: &str =
    "This opens a dialog, and the browser's dialog surface is not implemented in \
     this build. Refused rather than half-opened: a modal on the desktop that \
     this browser cannot see or answer is worse than a clear no.";

/// Why the git family is refused today (`remote-control-ll5.5`, SPECS §5).
const GIT_REFUSAL: &str =
    "Git commands from the browser are not implemented in this build. Refused \
     rather than dispatched: SPECS §5 gates every history-touching operation \
     behind an explicit confirmation the browser cannot yet show.";

/// **Every command name this build accepts**, in palette display order.
///
/// The single source of truth: [`crate::web::server`] refuses anything not
/// listed here, `src/lib.rs` routes what is, and
/// [`Snapshot::commands`](crate::web::protocol::Snapshot::commands) is built
/// from it. See the module doc for why the refusing rows are listed rather than
/// omitted.
pub static INVENTORY: &[CommandSpec] = &[
    // -- the shared selection (D3): templates the browser expands ----------
    CommandSpec {
        name: names::SELECT_SESSION,
        label: "Select Session",
        group: "Sessions",
        host_only: false,
        annotation: None,
        route: Route::Selection(SelectionTarget::Session),
    },
    CommandSpec {
        name: names::SELECT_PROJECT,
        label: "Switch to Project",
        group: "Projects",
        host_only: false,
        annotation: None,
        route: Route::Selection(SelectionTarget::Project),
    },
    CommandSpec {
        name: names::SELECT_TERMINAL,
        label: "Select Terminal",
        group: "Terminals",
        host_only: false,
        annotation: None,
        route: Route::Selection(SelectionTarget::Terminal),
    },
    // -- projects (SPECS §22) ----------------------------------------------
    CommandSpec {
        name: names::OPEN_PROJECT,
        label: "Open Project",
        group: "Projects",
        host_only: false,
        annotation: None,
        route: Route::NotSupported(DIALOG_REFUSAL),
    },
    CommandSpec {
        name: names::CLOSE_PROJECT,
        label: "Close Project",
        group: "Projects",
        host_only: false,
        annotation: None,
        route: Route::NotSupported(DIALOG_REFUSAL),
    },
    CommandSpec {
        name: names::NEXT_PROJECT,
        label: "Next Project",
        group: "Projects",
        host_only: false,
        annotation: None,
        route: Route::Palette(PaletteAction::SwitchProjectNext),
    },
    CommandSpec {
        name: names::PREVIOUS_PROJECT,
        label: "Previous Project",
        group: "Projects",
        host_only: false,
        annotation: None,
        route: Route::Palette(PaletteAction::SwitchProjectPrev),
    },
    // -- session tabs ------------------------------------------------------
    CommandSpec {
        name: names::NEW_AGENT_SESSION_TAB,
        label: "New Agent Session Tab",
        group: "Agent Session Tabs",
        host_only: false,
        annotation: None,
        route: Route::NotSupported(DIALOG_REFUSAL),
    },
    CommandSpec {
        name: names::RENAME_AGENT_SESSION_TAB,
        label: "Rename Agent Session Tab",
        group: "Agent Session Tabs",
        host_only: false,
        annotation: None,
        route: Route::NotSupported(DIALOG_REFUSAL),
    },
    CommandSpec {
        name: names::CLOSE_AGENT_SESSION_TAB,
        label: "Close Agent Session Tab",
        group: "Agent Session Tabs",
        host_only: false,
        annotation: None,
        route: Route::NotSupported(DIALOG_REFUSAL),
    },
    CommandSpec {
        name: names::SWITCH_AGENT_SESSION_TAB,
        label: "Switch Agent Session Tab",
        group: "Agent Session Tabs",
        host_only: false,
        annotation: Some("next"),
        route: Route::Palette(PaletteAction::Dispatch(Command::SwitchAgentTab(
            Selector::Next,
        ))),
    },
    CommandSpec {
        name: names::RESTART_AGENT,
        label: "Restart Agent",
        group: "Agent Session Tabs",
        host_only: false,
        annotation: None,
        route: Route::Palette(PaletteAction::Dispatch(Command::RestartAgent)),
    },
    // -- worktree ----------------------------------------------------------
    CommandSpec {
        name: names::REBASE_WORKTREE,
        label: "Rebase Worktree",
        group: "Worktree",
        host_only: false,
        annotation: None,
        route: Route::NotSupported(GIT_REFUSAL),
    },
    CommandSpec {
        name: names::ABANDON_WORKTREE,
        label: "Abandon Worktree",
        group: "Worktree",
        host_only: false,
        annotation: Some("destructive"),
        route: Route::NotSupported(DIALOG_REFUSAL),
    },
    CommandSpec {
        name: names::OPEN_WORKTREE_IN_FILE_MANAGER,
        label: "Open Worktree in File Manager",
        group: "Worktree",
        host_only: true,
        annotation: None,
        route: Route::Rejected(HOST_ONLY_REFUSAL),
    },
    CommandSpec {
        name: names::EDIT_IN_EDITOR,
        label: "Edit in $EDITOR",
        group: "Worktree",
        host_only: true,
        annotation: None,
        route: Route::Rejected(HOST_ONLY_REFUSAL),
    },
    // -- git ---------------------------------------------------------------
    CommandSpec {
        name: names::PUSH_BRANCH,
        label: "Push Branch",
        group: "Git",
        host_only: false,
        annotation: None,
        route: Route::NotSupported(GIT_REFUSAL),
    },
    CommandSpec {
        name: names::FINISH_LOCAL_MERGE,
        label: "Finish / Local Merge",
        group: "Git",
        host_only: false,
        annotation: Some("destructive"),
        route: Route::NotSupported(GIT_REFUSAL),
    },
    CommandSpec {
        name: names::PULL_BASE,
        label: "Pull Base",
        group: "Git",
        host_only: false,
        annotation: None,
        route: Route::NotSupported(GIT_REFUSAL),
    },
    CommandSpec {
        name: names::SHOW_GIT_STATUS,
        label: "Show Git Status",
        group: "Git",
        host_only: false,
        annotation: None,
        route: Route::NotSupported(DIALOG_REFUSAL),
    },
    // -- terminals ---------------------------------------------------------
    CommandSpec {
        name: names::NEW_CHILD_TERMINAL,
        label: "New Child Terminal",
        group: "Terminals",
        host_only: false,
        annotation: None,
        route: Route::Palette(PaletteAction::Dispatch(Command::NewChildTerminal)),
    },
    CommandSpec {
        name: names::CLOSE_CHILD_TERMINAL,
        label: "Close Child Terminal",
        group: "Terminals",
        host_only: false,
        annotation: None,
        route: Route::NotSupported(DIALOG_REFUSAL),
    },
    CommandSpec {
        name: names::NEW_AGENT,
        label: "New Agent",
        group: "Terminals",
        host_only: false,
        annotation: None,
        route: Route::NotSupported(DIALOG_REFUSAL),
    },
    CommandSpec {
        name: names::CLOSE_AGENT,
        label: "Close Agent",
        group: "Terminals",
        host_only: false,
        annotation: None,
        route: Route::NotSupported(DIALOG_REFUSAL),
    },
    CommandSpec {
        name: names::SWITCH_CHILD_TERMINAL,
        label: "Switch Child Terminal",
        group: "Terminals",
        host_only: false,
        annotation: Some("next"),
        route: Route::Palette(PaletteAction::Dispatch(Command::SwitchChildTerminal(
            Selector::Next,
        ))),
    },
    CommandSpec {
        name: names::OPEN_SHELL,
        label: "Open Shell",
        group: "Terminals",
        host_only: false,
        annotation: None,
        route: Route::Palette(PaletteAction::Dispatch(Command::OpenShell)),
    },
    // -- status, configuration, remote -------------------------------------
    CommandSpec {
        name: names::SET_MANUAL_STATUS,
        label: "Set Manual Status",
        group: "Status",
        host_only: false,
        annotation: None,
        route: Route::NotSupported(DIALOG_REFUSAL),
    },
    CommandSpec {
        name: names::OPEN_CONFIGURATION,
        label: "Open Configuration",
        group: "Configuration",
        host_only: false,
        annotation: None,
        route: Route::NotSupported(
            "The configuration manager is a browser surface of its own \
             (remote-control-ll5.6); opening the desktop's overlay from here \
             would put a modal on a screen this browser cannot see.",
        ),
    },
    CommandSpec {
        name: names::PAIR_PHONE,
        label: "Pair Phone",
        group: "Remote",
        host_only: false,
        annotation: None,
        route: Route::NotSupported(
            "Pairing shows a QR code and a 4-digit code on the desktop's screen, \
             which this build cannot render in a browser.",
        ),
    },
    CommandSpec {
        name: names::UNPAIR_PHONE,
        label: "Unpair Phone",
        group: "Remote",
        host_only: false,
        annotation: None,
        route: Route::NotSupported(DIALOG_REFUSAL),
    },
    CommandSpec {
        name: names::START_WEB_INTERFACE,
        label: "Start Web Interface",
        group: "Remote",
        host_only: false,
        annotation: Some("already running"),
        route: Route::Rejected(
            "The web interface is already running — this browser is connected to it.",
        ),
    },
    CommandSpec {
        name: names::STOP_WEB_INTERFACE,
        label: "Stop Web Interface",
        group: "Remote",
        host_only: false,
        annotation: None,
        route: Route::Rejected(
            "Stopping the web interface would disconnect every browser, including \
             this one. Like quit, that needs the two-step confirmation this build \
             does not have yet — stop it from the desktop.",
        ),
    },
    // -- view --------------------------------------------------------------
    CommandSpec {
        name: names::TOGGLE_SPLIT_VIEW,
        label: "Toggle Split View",
        group: "View",
        host_only: false,
        annotation: None,
        route: Route::Palette(PaletteAction::Dispatch(Command::ToggleSplitView)),
    },
    CommandSpec {
        name: names::SHOW_HELP,
        label: "Show Help",
        group: "View",
        host_only: false,
        annotation: None,
        route: Route::NotSupported(
            "The help overlay is a browser surface of its own \
             (remote-control-ll5.8); this build would only open it on the desktop.",
        ),
    },
    CommandSpec {
        name: names::ABOUT_FLIGHTDECK,
        label: "About FlightDeck",
        group: "View",
        host_only: false,
        annotation: None,
        route: Route::NotSupported(
            "The About dialog is a browser surface of its own \
             (remote-control-ll5.8); this build would only open it on the desktop.",
        ),
    },
    // -- global ------------------------------------------------------------
    CommandSpec {
        name: names::QUIT,
        label: "Quit",
        group: "Global",
        host_only: false,
        annotation: Some("destructive"),
        route: Route::Rejected(QUIT_REFUSAL),
    },
    // -- the browser's own plumbing ----------------------------------------
    CommandSpec {
        name: names::REQUEST_SNAPSHOT,
        label: "Request Snapshot",
        group: "Session",
        host_only: false,
        annotation: Some("resync from host"),
        route: Route::Server,
    },
    CommandSpec {
        name: names::RELEASE_SEAT,
        label: "Release Seat",
        group: "Session",
        host_only: false,
        annotation: Some("give up control"),
        route: Route::Server,
    },
    CommandSpec {
        name: names::MARK_ACTIVITY_READ,
        label: "Mark All Activity Read",
        group: "Session",
        host_only: false,
        annotation: None,
        route: Route::ActivityRead,
    },
];

/// The spec for one wire name, or `None` if this build has no such command.
///
/// `None` is the whole of the M2 door's failure mode: the server answers it with
/// [`crate::web::protocol::ErrorCode::NotSupported`] and keeps the socket.
pub fn lookup(name: &str) -> Option<&'static CommandSpec> {
    INVENTORY.iter().find(|spec| spec.name == name)
}

impl CommandSpec {
    /// Whether a frame naming this command must come from the controlling seat
    /// (D14). Only the two rows the server answers from published state are
    /// open to an observer; everything else — including a refusal — is a
    /// controller's frame, so a read-only tab is told `read_only` rather than
    /// being told *why* a command it may not send would have been refused.
    pub fn requires_control(&self) -> bool {
        !matches!(self.route, Route::Server)
    }

    /// The reason this build refuses the command, if it does.
    pub fn refusal(&self) -> Option<&'static str> {
        match self.route {
            Route::Rejected(reason) | Route::NotSupported(reason) => Some(reason),
            Route::Server | Route::Selection(_) | Route::ActivityRead | Route::Palette(_) => None,
        }
    }

    /// What the browser must expand this row over, filling `run.args`.
    pub fn target(&self) -> Option<CommandTarget> {
        match self.route {
            Route::Selection(SelectionTarget::Project) => Some(CommandTarget::Project),
            Route::Selection(SelectionTarget::Session) => Some(CommandTarget::Session),
            Route::Selection(SelectionTarget::Terminal) => Some(CommandTarget::Terminal),
            Route::ActivityRead => Some(CommandTarget::UnreadActivity),
            Route::Server | Route::Palette(_) | Route::Rejected(_) | Route::NotSupported(_) => None,
        }
    }

    /// This row as the browser receives it.
    pub fn view(&self) -> CommandView {
        CommandView {
            id: self.name.to_string(),
            label: self.label.to_string(),
            group: self.group.to_string(),
            run: CommandRun {
                name: self.name.to_string(),
                args: None,
            },
            host_only: self.host_only,
            annotation: self.annotation.map(str::to_string),
            target: self.target(),
            refusal: self.refusal().map(str::to_string),
        }
    }
}

/// The whole inventory as the browser receives it, in palette display order.
pub fn inventory() -> Vec<CommandView> {
    INVENTORY.iter().map(CommandSpec::view).collect()
}

// ===========================================================================
// Coverage: a palette action cannot become silently unreachable
// ===========================================================================

/// Whether a palette action has a wire name, or why it deliberately has none.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exposure {
    /// Reachable from a browser under this wire name.
    Wire(&'static str),
    /// Deliberately not on the wire, for this reason.
    NotExposed(&'static str),
}

/// The wire name for one palette action, or the reason it has none.
///
/// **Exhaustive on purpose, with no wildcard arm.** Adding a variant to
/// [`PaletteAction`] — or, through the [`PaletteAction::Dispatch`] arm, to
/// [`Command`] — stops this crate compiling until the new action says whether a
/// browser may run it. That is the guarantee requirement "no palette action is
/// silently unreachable" rests on; the test module checks the other half, that
/// the name claimed here is really in [`INVENTORY`].
pub fn exposure_of(action: &PaletteAction) -> Exposure {
    match action {
        PaletteAction::Dispatch(cmd) => exposure_of_command(cmd),
        PaletteAction::NewAgentTab => Exposure::Wire(names::NEW_AGENT_SESSION_TAB),
        PaletteAction::NewAgentChild => Exposure::Wire(names::NEW_AGENT),
        PaletteAction::RenameAgentTab => Exposure::Wire(names::RENAME_AGENT_SESSION_TAB),
        PaletteAction::CloseAgentTab => Exposure::Wire(names::CLOSE_AGENT_SESSION_TAB),
        PaletteAction::SetManualStatus => Exposure::Wire(names::SET_MANUAL_STATUS),
        PaletteAction::OpenProject => Exposure::Wire(names::OPEN_PROJECT),
        PaletteAction::CloseProject => Exposure::Wire(names::CLOSE_PROJECT),
        PaletteAction::SwitchProjectNext => Exposure::Wire(names::NEXT_PROJECT),
        PaletteAction::SwitchProjectPrev => Exposure::Wire(names::PREVIOUS_PROJECT),
        PaletteAction::OpenConfig => Exposure::Wire(names::OPEN_CONFIGURATION),
        PaletteAction::PairPhone => Exposure::Wire(names::PAIR_PHONE),
        PaletteAction::UnpairPhone => Exposure::Wire(names::UNPAIR_PHONE),
        PaletteAction::StartWebInterface => Exposure::Wire(names::START_WEB_INTERFACE),
        PaletteAction::StopWebInterface => Exposure::Wire(names::STOP_WEB_INTERFACE),
    }
}

/// The wire name for one app [`Command`], or the reason it has none.
///
/// Exhaustive for the same reason as [`exposure_of`]. The commands with no wire
/// name are the ones the palette itself does not offer: they are the payload-
/// carrying second half of a two-phase flow whose first phase is the palette
/// row, or they are hidden from the palette outright.
fn exposure_of_command(cmd: &Command) -> Exposure {
    match cmd {
        Command::SwitchAgentTab(_) => Exposure::Wire(names::SWITCH_AGENT_SESSION_TAB),
        Command::RestartAgent => Exposure::Wire(names::RESTART_AGENT),
        Command::RebaseWorktree { .. } => Exposure::Wire(names::REBASE_WORKTREE),
        Command::AbandonWorktree { .. } => Exposure::Wire(names::ABANDON_WORKTREE),
        Command::OpenWorktreeInFileManager => Exposure::Wire(names::OPEN_WORKTREE_IN_FILE_MANAGER),
        Command::PushBranch { .. } => Exposure::Wire(names::PUSH_BRANCH),
        Command::FinishLocalMerge { .. } => Exposure::Wire(names::FINISH_LOCAL_MERGE),
        Command::PullBase => Exposure::Wire(names::PULL_BASE),
        Command::ShowGitStatus => Exposure::Wire(names::SHOW_GIT_STATUS),
        Command::NewChildTerminal => Exposure::Wire(names::NEW_CHILD_TERMINAL),
        Command::CloseChildTerminal => Exposure::Wire(names::CLOSE_CHILD_TERMINAL),
        Command::CloseAgentTerminal => Exposure::Wire(names::CLOSE_AGENT),
        Command::SwitchChildTerminal(_) => Exposure::Wire(names::SWITCH_CHILD_TERMINAL),
        Command::OpenShell => Exposure::Wire(names::OPEN_SHELL),
        Command::ToggleSplitView => Exposure::Wire(names::TOGGLE_SPLIT_VIEW),
        Command::ShowHelp => Exposure::Wire(names::SHOW_HELP),
        Command::ShowAbout => Exposure::Wire(names::ABOUT_FLIGHTDECK),
        Command::Quit => Exposure::Wire(names::QUIT),

        // Not palette rows, so not wire names of their own.
        Command::NewAgentTab { .. } => Exposure::NotExposed(
            "the palette row is `new_agent_session_tab`, which prompts for the \
             name this payload carries",
        ),
        Command::RenameAgentTab { .. } => Exposure::NotExposed(
            "the palette row is `rename_agent_session_tab`, which prompts for the \
             new name this payload carries",
        ),
        Command::CloseAgentTab { .. } => Exposure::NotExposed(
            "the palette row is `close_agent_session_tab`, which presents SPECS \
             §25's option set this payload chooses from",
        ),
        Command::NewAgentTerminal { .. } => Exposure::NotExposed(
            "the palette row is `new_agent`, which asks which backend this \
             payload names",
        ),
        Command::SetManualStatus(_) => Exposure::NotExposed(
            "the palette row is `set_manual_status`, which prompts for the status \
             this payload carries",
        ),
        Command::CopyEnvFile => Exposure::NotExposed(
            "hidden from the palette: `.env` files are symlinked into new \
             worktrees automatically, so the command has no row to expose",
        ),
    }
}

// ===========================================================================
// The git-ownership boundary (SPECS §5)
// ===========================================================================

/// Whether an app command rewrites git history (SPECS §5, §5.1, §5.2).
///
/// Exhaustive, so a new [`Command`] must classify itself here too. The two
/// history-touching commands are the §5.1 worktree rebase carve-out and §5.2's
/// `git pull --rebase` on the base folder; both go through
/// [`crate::contracts::traits::GitExecutor`]'s single guarded op.
pub fn rewrites_history(cmd: &Command) -> bool {
    match cmd {
        Command::RebaseWorktree { .. } | Command::PullBase => true,
        Command::NewAgentTab { .. }
        | Command::RenameAgentTab { .. }
        | Command::CloseAgentTab { .. }
        | Command::PushBranch { .. }
        | Command::FinishLocalMerge { .. }
        | Command::CopyEnvFile
        | Command::AbandonWorktree { .. }
        | Command::NewChildTerminal
        | Command::NewAgentTerminal { .. }
        | Command::CloseAgentTerminal
        | Command::CloseChildTerminal
        | Command::SwitchAgentTab(_)
        | Command::SwitchChildTerminal(_)
        | Command::SetManualStatus(_)
        | Command::RestartAgent
        | Command::OpenShell
        | Command::ShowGitStatus
        | Command::ShowHelp
        | Command::ShowAbout
        | Command::ToggleSplitView
        | Command::OpenWorktreeInFileManager
        | Command::Quit => false,
    }
}

/// Whether an app command creates a pull request (SPECS §5: FlightDeck never
/// does). Exhaustive, so a command that ever did would have to say so here and
/// fail this module's boundary test.
///
/// `PushBranch` is deliberately `false`: §14 pushes the branch and returns a
/// GitHub *compare* URL for the user to open, which is not the same thing as
/// opening a PR on their behalf.
pub fn creates_pull_request(cmd: &Command) -> bool {
    match cmd {
        Command::NewAgentTab { .. }
        | Command::RenameAgentTab { .. }
        | Command::CloseAgentTab { .. }
        | Command::PushBranch { .. }
        | Command::FinishLocalMerge { .. }
        | Command::RebaseWorktree { .. }
        | Command::PullBase
        | Command::CopyEnvFile
        | Command::AbandonWorktree { .. }
        | Command::NewChildTerminal
        | Command::NewAgentTerminal { .. }
        | Command::CloseAgentTerminal
        | Command::CloseChildTerminal
        | Command::SwitchAgentTab(_)
        | Command::SwitchChildTerminal(_)
        | Command::SetManualStatus(_)
        | Command::RestartAgent
        | Command::OpenShell
        | Command::ShowGitStatus
        | Command::ShowHelp
        | Command::ShowAbout
        | Command::ToggleSplitView
        | Command::OpenWorktreeInFileManager
        | Command::Quit => false,
    }
}

/// The app [`Command`] a route would dispatch, if it dispatches one. Used by the
/// boundary tests to reach through a [`PaletteAction`] to the command inside.
pub fn dispatched_command(route: &Route) -> Option<&Command> {
    match route {
        Route::Palette(PaletteAction::Dispatch(cmd)) => Some(cmd),
        Route::Palette(_)
        | Route::Server
        | Route::Selection(_)
        | Route::ActivityRead
        | Route::Rejected(_)
        | Route::NotSupported(_) => None,
    }
}
