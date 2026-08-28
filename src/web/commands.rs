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
//!   reason. D16's two desktop-only actions land here: their effect is a window
//!   on the host's screen, so a fake `Applied` would be a lie.
//! * [`Route::Dialog`] — D13's shared dialog. The dialog is app state, published
//!   to both surfaces with the origin that opened it, and either surface can
//!   confirm or cancel; these two rows are how a browser does it.
//! * [`Route::NotSupported`] — this build has no browser-side surface for it, and
//!   the refusal names the task that owns one. What is left after D13, the git
//!   family and the destructive confirmation landed is the configuration manager
//!   (`remote-control-ll5.6`), help/about (`.8`), `show_git_status` — which is
//!   not a dialog at all, nothing is being asked, so there is nothing to answer
//!   — and `pull_base`, which is a boundary decision rather than a missing
//!   surface (see [`PULL_BASE_REFUSAL`]).
//!
//! ## Destroying work from a browser takes two steps (artboard 1g)
//!
//! `abandon_worktree` and `quit` are palette rows here like any other, and both
//! carry their **unconfirmed** value, so choosing either can only raise D13's
//! shared question. What is new in `remote-control-ll5.4` is the second step
//! behind that question: a browser answering the destroying button must also
//! type the session's — or the project's — own name, exactly.
//!
//! The trigger is **the surface being remote**, not the command being
//! destructive, which is what artboard 1g's step 2 says in as many words
//! ("This browser is remote. Type the session name to run the rebase on the
//! host."). So the desktop's dialogs are untouched — nothing reaches step 2
//! there — and from a browser the gate covers the three answers that destroy
//! work or rewrite history: **Abandon Worktree**, **Rebase Worktree** (SPECS
//! §5.1's sanctioned rewrite) and **Quit** (D16). Push and Finish / Local Merge
//! stay one-step: neither rewrites history nor discards anything, so 1g's
//! friction would be ceremony. See [`BrowserConfirm`] for the mechanism and
//! `specs/WEB_INTERFACE.md` §6.5 R13 for the ruling.
//!
//! ## The git-ownership boundary holds by construction (SPECS §5)
//!
//! The git family dispatches from a browser (`remote-control-ll5.5`), and §5
//! still holds without a runtime check standing over it. Two properties do the
//! work, and both are asserted in this module's tests.
//!
//! **1. A forwarding row ignores the frame's `args` entirely.** The action —
//! and every `confirm` flag inside it — comes from this table, so a browser
//! cannot smuggle `confirm: true` into `RebaseWorktree` any more than it can
//! into `AbandonWorktree`. [`confirmation_of`] names the three states a command
//! *value* can be in, and no row in [`INVENTORY`] may carry
//! [`Confirmation::Given`].
//!
//! **2. The exception §5.1 grants is the only one, and it is checkable.** Of
//! every browser-reachable row, exactly one dispatches a command that
//! [`rewrites_history`]: `rebase_worktree`, carrying
//! `RebaseWorktree { confirm: false }`. §5.1 sanctions the worktree rebase as
//! *user-initiated and explicitly confirmed* — "the first dispatch always
//! returns a confirmation prompt before anything is rewritten" — so an
//! unconfirmed dispatch cannot rewrite anything. It can only ask, and D13
//! publishes the question to both surfaces, with the origin that raised it,
//! before anyone answers. The invariant is therefore:
//!
//! > **No browser-reachable route may rewrite history except through a route
//! > whose dispatched command is [`Confirmation::Pending`] and therefore lands
//! > on §5.1's confirmation prompt; and no browser-reachable route may create a
//! > pull request, ever, with no exception.**
//!
//! [`Command::PullBase`] is the row that clause excludes, and it excludes it
//! *by construction rather than by name*: §5.2 gives pull-base no confirmation
//! step, so its value is [`Confirmation::None`] and there is no unconfirmed
//! variant the table could carry. See [`PULL_BASE_REFUSAL`] for the decision and
//! `specs/WEB_INTERFACE.md` §6.5 R11 for the reasoning behind it.

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
    /// D13: answer the dialog that is already open on both surfaces. Applied by
    /// the TUI, by synthesising the very keypress the desktop's own dialog
    /// buttons synthesise — so there is no second dialog engine to drift.
    Dialog(DialogAct),
    /// Refused with [`crate::web::protocol::AckOutcome::Rejected`] and this
    /// reason: the host implements it, but its effect must not — or cannot —
    /// land for a browser.
    Rejected(&'static str),
    /// Refused with [`crate::web::protocol::ErrorCode::NotSupported`] and this
    /// reason: this build has no browser-side surface for it yet.
    NotSupported(&'static str),
}

/// Which half of D13's shared dialog a [`Route::Dialog`] row answers.
///
/// There are two and only two, because that is what D13 grants a browser: a
/// dialog is app state, and either surface may **confirm** or **cancel** it.
/// Anything else a dialog can do — moving a radio, typing into a field, toggling
/// `run from base` — arrives as arguments on the confirm, not as a command of its
/// own, so a half-driven dialog can never be a state the two surfaces disagree
/// about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DialogAct {
    /// Take the dialog's primary action, or the button `choice` names.
    Confirm,
    /// Dismiss it with no decision.
    Cancel,
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

/// Why a desktop-only action cannot be run from a browser (D16). One sentence,
/// so `Open Worktree in File Manager` and `edit in $EDITOR` refuse identically.
pub const HOST_ONLY_REFUSAL: &str =
    "This opens a window on the machine running FlightDeck, which is not the \
     machine this browser is on. Run it from the desktop.";

// ===========================================================================
// Artboard 1g: the second step a remote surface takes (`remote-control-ll5.4`)
// ===========================================================================

/// What a **browser** must do to confirm one open dialog.
///
/// The desktop is not described here at all, and that is the ruling: 1g's step 2
/// exists because *"this browser is remote"*, so it is a property of the surface
/// answering, never of the prompt itself. `src/lib.rs`'s `browser_confirm_gate`
/// is the exhaustive classification over the prompt family; this type is the
/// vocabulary it answers in, and lives here because every sentence a browser
/// reads is worded in this module.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrowserConfirm {
    /// The dialog's own buttons are the whole confirmation, exactly as on the
    /// desktop. Every dialog that neither destroys work nor rewrites history.
    OneStep,
    /// One of the dialog's buttons is behind artboard 1g's typed-name step.
    /// Every *other* button on the same dialog — and cancelling, always — is
    /// still one press away.
    TypedName(TypedNameGate),
}

/// Artboard 1g's step 2, as the host states it before a browser can pass it.
///
/// It guards **one button**, not the dialog. Every gated dialog in this build is
/// a `y`/`n` confirmation, so the distinction looks free — but it is what states
/// the rule: the gate stands in front of the *answer that destroys work*, never
/// in front of the question. A gate on a button that merely opens the next
/// dialog would cost a typed name for nothing, and a name typed for nothing is a
/// name typed without reading.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TypedNameGate {
    /// The [`crate::web::protocol::DialogKey::key`] this stands in front of.
    pub key: &'static str,
    /// Whose name must be typed. Resolved against the live workspace, so what
    /// the browser is shown and what the host checks come from one place.
    pub subject: GateSubject,
    /// 1g step 2's sentence, rendered verbatim by the browser.
    pub instruction: &'static str,
}

/// What a [`TypedNameGate`] asks the user to name.
///
/// Always something the browser is *already looking at* — 1g draws the expected
/// name as the field's own hint, so a gate whose subject the user cannot see
/// would be a riddle rather than a confirmation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GateSubject {
    /// The Agent Session Tab the dialog is about: the one that is selected.
    /// Every session-scoped guarded command acts on the selection, including
    /// the sidebar's close menu, which switches to the row it names first.
    SelectedSession,
    /// The active project. Quit does not belong to one session; it stops the
    /// whole deck, and the project name is what the browser has on screen.
    ActiveProject,
}

/// 1g's own sentence, for the rebase it draws (SPECS §5.1).
pub const GATE_REBASE_INSTRUCTION: &str =
    "This browser is remote. Type the session name to run the rebase on the host.";

/// The same sentence for the two abandon answers (SPECS §5/§15).
pub const GATE_ABANDON_INSTRUCTION: &str =
    "This browser is remote. Type the session name to abandon the worktree on \
     the host.";

/// And for quit, which is not about one session (D16).
pub const GATE_QUIT_INSTRUCTION: &str =
    "This browser is remote. Type the project name to stop FlightDeck and every \
     agent running on the host.";

/// The refusal for a confirm that never took step 2 — the browser pressed the
/// destroying button and sent no name at all.
///
/// It repeats the instruction rather than saying "denied", because the frame is
/// not an attack: it is what an older browser, or a reconnecting one, sends. The
/// last clause is the part that matters most, and it is a statement of fact —
/// the gate is checked before a single key reaches the prompt.
pub fn gate_step_refusal(gate: &TypedNameGate, expected: &str) -> String {
    format!(
        "{} Send it as `confirm_name`, spelled exactly `{expected}`. Nothing has \
         happened.",
        gate.instruction
    )
}

/// The refusal for a name that does not match, character for character.
///
/// The comparison is exact — no trimming, no case folding, no normalisation —
/// because a name that needs correcting before it matches is a name that was not
/// read. Git branch names are case-sensitive, and a fold would accept a name the
/// host does not have.
pub fn gate_mismatch_refusal(typed: &str, expected: &str) -> String {
    format!(
        "`{typed}` is not `{expected}`. The name must match exactly, character \
         for character — nothing has happened."
    )
}

/// The refusal for a gate whose subject the host cannot name right now: the tab
/// it was about is gone, or the dialog outlived what it asked about.
///
/// Refusing is the only safe answer. A gate with no name to check is not a gate,
/// and confirming past it would destroy something nobody named.
pub const GATE_UNRESOLVED_REFUSAL: &str =
    "The host can no longer name what this would destroy — the session it asked \
     about is gone. Cancel this dialog and start again.";

/// Why `Show Git Status` is still refused: it is not a dialog, and it has no
/// browser design yet.
///
/// It was grouped with the dialog family before this task because it opens a
/// desktop overlay. It is not one of D13's dialogs — nothing is being asked, so
/// there is nothing to confirm or cancel — and design turn 3 owns what the
/// browser shows instead. Refused with that said out loud rather than dispatched
/// into an overlay only the desktop can read.
pub const UNDESIGNED_OVERLAY_REFUSAL: &str =
    "Git status opens a read-only overlay on the desktop, and the browser has no \
     design for it yet (design turn 3). It is not one of D13's shared dialogs: \
     nothing is being asked, so there is nothing to answer from here.";

/// Why `Pull Base` alone in the git family is refused (`remote-control-ll5.5`,
/// SPECS §5.2).
///
/// This is a decision, not an omission, and it is the one the module doc's
/// invariant turns on. `rebase_worktree` is exposable because §5.1 puts a
/// confirmation prompt in front of it and R7's forwarding rule makes that prompt
/// unforgeable from a frame: the table carries the *unconfirmed* variant, so the
/// first dispatch can only ask.
///
/// Pull base has no such step. §5.2 says so in as many words — "this is a global
/// action that never touches an Agent Tab's worktree, so it is not
/// confirmation-gated" — and it is guarded by preconditions instead (base branch
/// checked out, conflict aborted). Those preconditions bound the *damage*; they
/// do not make the invocation something anybody read first. And the
/// implementation does more than §5.2's summary: a dirty base folder is stashed,
/// pulled over and re-applied, so a single frame would move the user's own
/// uncommitted work through the stash with nothing shown to either surface
/// beforehand.
///
/// Inventing a browser-only confirmation was rejected for the reason this whole
/// module exists: it would be a second flow the desktop does not have, and D13's
/// dialog is shared precisely so there is only ever one. So the row is offered,
/// visible, and refused in words that name what would have to change.
pub const PULL_BASE_REFUSAL: &str =
    "Pull Base rebases your local base branch — and stashes, pulls over and \
     re-applies any uncommitted work in the base folder — with no confirmation \
     step to read first (SPECS §5.2). Rebase Worktree is offered here because \
     §5.1 puts a shared confirmation in front of it; this one has none, so run \
     it from the desktop (Ctrl-u).";

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
        route: Route::Palette(PaletteAction::OpenProject),
    },
    CommandSpec {
        name: names::CLOSE_PROJECT,
        label: "Close Project",
        group: "Projects",
        host_only: false,
        annotation: None,
        route: Route::Palette(PaletteAction::CloseProject),
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
        route: Route::Palette(PaletteAction::NewAgentTab),
    },
    CommandSpec {
        name: names::RENAME_AGENT_SESSION_TAB,
        label: "Rename Agent Session Tab",
        group: "Agent Session Tabs",
        host_only: false,
        annotation: None,
        route: Route::Palette(PaletteAction::RenameAgentTab),
    },
    CommandSpec {
        name: names::CLOSE_AGENT_SESSION_TAB,
        label: "Close Agent Session Tab",
        group: "Agent Session Tabs",
        host_only: false,
        annotation: None,
        route: Route::Palette(PaletteAction::CloseAgentTab),
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
    // SPECS §5.1's carve-out, and the one browser-reachable row that reaches a
    // history-rewriting command. It carries `confirm: false` — the same value
    // the desktop's palette row carries — so the first dispatch can only return
    // the confirmation prompt, which D13 then publishes to both surfaces. A
    // frame's `args` are ignored, so `confirm: true` is unreachable from a
    // browser by construction rather than by a check. See the module doc.
    CommandSpec {
        name: names::REBASE_WORKTREE,
        label: "Rebase Worktree",
        group: "Worktree",
        host_only: false,
        annotation: None,
        route: Route::Palette(PaletteAction::Dispatch(Command::RebaseWorktree {
            confirm: false,
        })),
    },
    // SPECS §5/§15: abandoning always asks first, even for a clean worktree, so
    // the row carries the unconfirmed value and the first dispatch can only open
    // D13's shared question. A browser's *answer* to that question is where
    // artboard 1g's second step stands (see [`BrowserConfirm`]).
    CommandSpec {
        name: names::ABANDON_WORKTREE,
        label: "Abandon Worktree",
        group: "Worktree",
        host_only: false,
        annotation: Some("destructive"),
        route: Route::Palette(PaletteAction::Dispatch(Command::AbandonWorktree {
            confirm: false,
        })),
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
    // SPECS §14: pushes the branch and hands back the GitHub *compare* URL. It
    // neither rewrites history nor opens a PR ([`creates_pull_request`] is
    // `false` for it, and says why), and `confirm: None` means a worktree with
    // uncommitted changes gets §14's warning dialog first rather than a silent
    // partial push.
    CommandSpec {
        name: names::PUSH_BRANCH,
        label: "Push Branch",
        group: "Git",
        host_only: false,
        annotation: None,
        route: Route::Palette(PaletteAction::Dispatch(Command::PushBranch {
            confirm: None,
        })),
    },
    // SPECS §15: guarded, and `confirm: false` so the §13 dirty-base refusal and
    // every §15 precondition are answered *before* anything merges. The
    // go-ahead is D13's shared confirmation, not this row.
    CommandSpec {
        name: names::FINISH_LOCAL_MERGE,
        label: "Finish / Local Merge",
        group: "Git",
        host_only: false,
        annotation: Some("destructive"),
        route: Route::Palette(PaletteAction::Dispatch(Command::FinishLocalMerge {
            confirm: false,
        })),
    },
    // SPECS §5.2: the one git row still refused, and the only one whose refusal
    // is a boundary decision rather than a missing browser surface.
    CommandSpec {
        name: names::PULL_BASE,
        label: "Pull Base",
        group: "Git",
        host_only: false,
        annotation: None,
        route: Route::NotSupported(PULL_BASE_REFUSAL),
    },
    CommandSpec {
        name: names::SHOW_GIT_STATUS,
        label: "Show Git Status",
        group: "Git",
        host_only: false,
        annotation: None,
        route: Route::NotSupported(UNDESIGNED_OVERLAY_REFUSAL),
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
        route: Route::Palette(PaletteAction::Dispatch(Command::CloseChildTerminal)),
    },
    CommandSpec {
        name: names::NEW_AGENT,
        label: "New Agent",
        group: "Terminals",
        host_only: false,
        annotation: None,
        route: Route::Palette(PaletteAction::NewAgentChild),
    },
    CommandSpec {
        name: names::CLOSE_AGENT,
        label: "Close Agent",
        group: "Terminals",
        host_only: false,
        annotation: None,
        route: Route::Palette(PaletteAction::Dispatch(Command::CloseAgentTerminal)),
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
        route: Route::Palette(PaletteAction::SetManualStatus),
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
        route: Route::Palette(PaletteAction::UnpairPhone),
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
        // Not 1g's family, and deliberately not given 1g's gate: this destroys
        // no work and rewrites nothing — it takes the surface away from the
        // browser asking, which is the one refusal a remote surface cannot read
        // the answer to. Refused outright, which is stricter than a gate.
        route: Route::Rejected(
            "Stopping the web interface would disconnect every browser, including \
             this one — you would not see how it went. Stop it from the desktop.",
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
    // D16: a `host only` badge is not enough for quit, so it is not badged and
    // not refused — it dispatches the value that can only ask. The desktop's own
    // row carries `Quit { confirm: true }` and still quits on the spot (SPECS
    // §23); this one raises the shared dialog, and a browser's `y` on it has to
    // pass 1g's typed-name step.
    CommandSpec {
        name: names::QUIT,
        label: "Quit",
        group: "Global",
        host_only: false,
        annotation: Some("destructive"),
        route: Route::Palette(PaletteAction::Dispatch(Command::Quit { confirm: false })),
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
    // -- D13's shared dialog ------------------------------------------------
    //
    // Not palette rows on either surface: the desktop answers a dialog with the
    // keyboard, and the browser answers it with these. They are in the table
    // because everything the browser may send is in the table — that is what
    // makes `src/web/server.rs`'s "refuse any name not in INVENTORY" a complete
    // check rather than a partial one — and they carry a `group` the palette
    // never renders for exactly the same reason `request_snapshot` does.
    CommandSpec {
        name: names::DIALOG_CONFIRM,
        label: "Confirm Dialog",
        group: "Session",
        host_only: false,
        annotation: Some("answers the open dialog"),
        route: Route::Dialog(DialogAct::Confirm),
    },
    CommandSpec {
        name: names::DIALOG_CANCEL,
        label: "Cancel Dialog",
        group: "Session",
        host_only: false,
        annotation: Some("answers the open dialog"),
        route: Route::Dialog(DialogAct::Cancel),
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
            Route::Server
            | Route::Selection(_)
            | Route::ActivityRead
            | Route::Palette(_)
            | Route::Dialog(_) => None,
        }
    }

    /// D13: whether this row answers the open dialog instead of being a palette
    /// row. Derived from the route rather than stored, so a new
    /// [`Route::Dialog`] row cannot forget to say so.
    pub fn answers_dialog(&self) -> bool {
        matches!(self.route, Route::Dialog(_))
    }

    /// What the browser must expand this row over, filling `run.args`.
    pub fn target(&self) -> Option<CommandTarget> {
        match self.route {
            Route::Selection(SelectionTarget::Project) => Some(CommandTarget::Project),
            Route::Selection(SelectionTarget::Session) => Some(CommandTarget::Session),
            Route::Selection(SelectionTarget::Terminal) => Some(CommandTarget::Terminal),
            Route::ActivityRead => Some(CommandTarget::UnreadActivity),
            Route::Server
            | Route::Palette(_)
            | Route::Dialog(_)
            | Route::Rejected(_)
            | Route::NotSupported(_) => None,
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
            answers_dialog: self.answers_dialog(),
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
        Command::Quit { .. } => Exposure::Wire(names::QUIT),

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

/// Where one [`Command`] **value** stands relative to the confirmation SPECS §5
/// requires before a guarded operation may land.
///
/// This is about the payload, not about the command's kind: `RebaseWorktree
/// { confirm: false }` and `RebaseWorktree { confirm: true }` are the same
/// command and opposite answers here. That is exactly the distinction the
/// boundary invariant needs, because what makes a browser-reachable rebase safe
/// is not *which* command the row names but *which value of it* the table
/// carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Confirmation {
    /// The command has a confirmation step and this value has not taken it. For
    /// the §5.1 carve-out that is the strong statement the spec makes — "the
    /// first dispatch always returns a confirmation prompt before anything is
    /// rewritten" — so dispatching a `Pending` value can only ask.
    Pending,
    /// The command has a confirmation step and this value carries it: the effect
    /// lands on dispatch, with nothing asked. No row in [`INVENTORY`] may carry
    /// one, which is what stops a frame smuggling one in.
    Given,
    /// The command has no confirmation step at all — either because it needs
    /// none, or because the spec gives it none ([`Command::PullBase`], SPECS
    /// §5.2). A `None` value therefore cannot satisfy the boundary's exception
    /// clause; there is no unconfirmed variant of it to carry.
    None,
}

/// Which of [`Confirmation`]'s three states a command value is in.
///
/// Exhaustive with no wildcard arm, for the same reason [`exposure_of`] is: a
/// new [`Command`] — or a new confirmation flag on an existing one — must say
/// where it stands before this crate compiles again.
pub fn confirmation_of(cmd: &Command) -> Confirmation {
    let pending = |unconfirmed: bool| {
        if unconfirmed {
            Confirmation::Pending
        } else {
            Confirmation::Given
        }
    };
    match cmd {
        // SPECS §5.1: the first dispatch returns the confirmation prompt.
        Command::RebaseWorktree { confirm } => pending(!confirm),
        // SPECS §15: the first dispatch checks the preconditions and asks.
        Command::FinishLocalMerge { confirm } => pending(!confirm),
        // SPECS §5/§15: abandoning always asks first, even for a clean worktree.
        Command::AbandonWorktree { confirm } => pending(!confirm),
        // SPECS §14: a worktree with uncommitted changes gets the warning and
        // the three-way choice; a clean one needs no second question, because
        // choosing the row *is* the explicit confirmation §14 asks for.
        Command::PushBranch { confirm } => pending(confirm.is_none()),
        // SPECS §5.2 gives pull-base no confirmation step. Stated here rather
        // than assumed, because it is the whole reason `pull_base` cannot be on
        // a dispatching route.
        Command::PullBase => Confirmation::None,
        Command::NewAgentTab { .. }
        | Command::RenameAgentTab { .. }
        | Command::CloseAgentTab { .. }
        | Command::CopyEnvFile
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
        | Command::OpenWorktreeInFileManager => Confirmation::None,
        // D16: quit needs more than a badge from a remote surface, and it gets
        // the shape every other guarded command already has — the first
        // dispatch asks (`specs/WEB_INTERFACE.md` §6.5 R13). The desktop's own
        // `Ctrl-q` dispatches the confirmed value and is unchanged.
        Command::Quit { confirm } => pending(!confirm),
    }
}

/// Whether an app command rewrites git history (SPECS §5, §5.1, §5.2).
///
/// Exhaustive, so a new [`Command`] must classify itself here too. The two
/// history-touching commands are the §5.1 worktree rebase carve-out and §5.2's
/// `git pull --rebase` on the base folder; both go through
/// [`crate::contracts::traits::GitExecutor`]'s single guarded op.
///
/// A `true` here does **not** mean "unreachable from a browser" — it means the
/// row must clear the module doc's exception clause: its dispatched value has to
/// be [`Confirmation::Pending`], so the dispatch can only ask. `RebaseWorktree`
/// clears it; `PullBase` cannot, having no unconfirmed variant to carry.
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
        | Command::Quit { .. } => false,
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
        | Command::Quit { .. } => false,
    }
}

/// The app [`Command`] a route would dispatch, if it dispatches one. Used by the
/// boundary tests to reach through a [`PaletteAction`] to the command inside.
pub fn dispatched_command(route: &Route) -> Option<&Command> {
    match route {
        Route::Palette(PaletteAction::Dispatch(cmd)) => Some(cmd),
        Route::Palette(_)
        | Route::Dialog(_)
        | Route::Server
        | Route::Selection(_)
        | Route::ActivityRead
        | Route::Rejected(_)
        | Route::NotSupported(_) => None,
    }
}
