//! The front-end-neutral overlay model: every modal the TUI can put on screen,
//! as plain data a GPUI (or any other) front-end renders itself, and the
//! inputs that answer them.
//!
//! **This is a read-out, not a second dialog system.** Every view here is built
//! from the one interactive state the TUI already keeps (its prompt, palette,
//! configuration manager, access overlay and pairing overlay) by
//! `crate::tui::overlay_bridge`, and every [`OverlayInput`] is applied through
//! the very handlers the TUI's keyboard reaches — a confirm is the keypress the
//! desktop's own dialog button synthesizes, a palette run is the palette's own
//! Enter. It is the same rule FlightDeck Web's D13 dialogs follow
//! (`crate::web::protocol::DialogView`, `crate::web::commands::DialogAct`), so
//! a dialog answered from the GUI, the TUI or a browser takes one code path and
//! reads the same guard sentence on refusal.
//!
//! Where a model type was already free of any drawing library it is reused
//! as-is rather than restated: [`HelpDoc`]/[`AboutDoc`], the configuration
//! manager's [`ConfigRow`], the access overlay's [`WebAccessView`], the D13
//! [`DialogId`]/[`DialogOrigin`] and the palette's [`PaletteAction`].
//!
//! Like the rest of [`crate::host`], nothing in this file names a terminal-UI
//! library type.

use std::path::PathBuf;

use crate::app::commands::CloseAction;
use crate::git::status::WorktreeStatus;
use crate::tui::config_manager::{ConfigRow, ConfigScope, FieldValue};
use crate::tui::help::{AboutDoc, HelpDoc};
use crate::tui::palette::PaletteAction;
use crate::web::access::{AccessKey, WebAccessView};
use crate::web::protocol::{DialogId, DialogOrigin, UpdateNotice};

/// The one overlay currently on screen, as [`crate::host::AppHost::overlay`]
/// reports it. At most one is ever shown, in the precedence the TUI draws them:
/// the configuration manager, then the palette, then an open dialog, then a
/// plain overlay (message, help, about, git status, pairing, web access).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverlayView {
    /// A one-line notification (SPECS §22): an outcome, a refusal, an error.
    /// Any input dismisses it, exactly as any key does in the TUI.
    Message(MessageView),
    /// An interactive prompt: a confirmation, a text prompt, or a choice list.
    Dialog(DialogView),
    /// The command palette (SPECS §22).
    Palette(PaletteView),
    /// The help screen, built by the same [`crate::tui::help::help_doc`] the
    /// TUI's overlay draws.
    Help(HelpDoc),
    /// The About dialog ([`crate::tui::help::about_doc`]).
    About(AboutDoc),
    /// SPECS §21's git status panel for the selected Agent Session Tab.
    GitStatus(GitStatusView),
    /// The configuration manager (SPECS §8).
    Config(ConfigView),
    /// The browser access overlay (`specs/WEB_INTERFACE.md` D5, design `2a`).
    WebAccess(WebAccessOverlay),
    /// The phone pairing overlay (Settings → Remote).
    Pairing(PairingView),
}

/// A notification's text, verbatim. The TUI prefixes refusals with `Refused:`,
/// errors with `Error:` and warnings with `WARNING:`, so those words are part
/// of the copy rather than a separate field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageView {
    /// The message, with any embedded line breaks kept.
    pub text: String,
}

/// An open prompt (SPECS §22, §25): the TUI's dialog read out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialogView {
    /// Stable for the life of this prompt (D13). A replaced prompt gets a new
    /// one, so a front-end can tell "the same dialog, updated" from "a new
    /// dialog".
    pub id: DialogId,
    /// Which prompt this is, with the facts it asks about.
    pub kind: DialogKind,
    /// The question, verbatim: the first line of the TUI's dialog text. SPECS
    /// §5's guard copy ("Rewrites history; aborts on conflict.") lives here.
    pub title: String,
    /// The remaining lines of the TUI's dialog text, if any.
    pub body: Vec<String>,
    /// The text field's current contents, or `None` when the prompt has none.
    /// `Some("")` is an empty field, not an absent one.
    pub input: Option<String>,
    /// The choice rows (the agent radio, a filtered branch list, subfolders).
    pub list: Vec<DialogRow>,
    /// Whether `input` filters `list` (case-insensitive substring match).
    pub list_filter: bool,
    /// The buttons, in display order.
    pub buttons: Vec<DialogButton>,
    /// Who opened it (D13). A browser-opened dialog should say so.
    pub origin: DialogOrigin,
    /// The TUI's origin line (`opened from browser · 192.168.2.20`), or `None`
    /// for a dialog this desktop opened.
    pub origin_label: Option<String>,
}

/// One row of a dialog's list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialogRow {
    /// Rendered verbatim (the agent radio's rows carry their `(•)` marker).
    pub label: String,
    /// The highlighted row.
    pub selected: bool,
}

/// One dialog button.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialogButton {
    /// What [`OverlayInput::Choose`] names to press it: the TUI accelerator
    /// (`y`, `1`, `Enter`, `Tab`, `Esc`), the same key FlightDeck Web's
    /// `DialogKey::key` carries.
    pub id: String,
    /// Rendered verbatim.
    pub label: String,
    /// What pressing it means.
    pub role: ButtonRole,
    /// Whether [`OverlayInput::Submit`] (Enter) presses it.
    pub default: bool,
}

/// What a dialog button does, for a front-end that styles buttons by intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonRole {
    /// The dialog's first, affirmative answer.
    Primary,
    /// Any other answer that decides the dialog.
    Secondary,
    /// An answer that destroys work, stops processes or rewrites history:
    /// abandoning a worktree, a rebase, a merge-back that removes the
    /// worktree, force-terminating, closing a project or a terminal, quitting,
    /// unpairing.
    Destructive,
    /// Dismisses the dialog with no decision.
    Cancel,
}

/// Which prompt a [`DialogView`] is, carrying the facts it is about in typed
/// form so a front-end need not parse the title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialogKind {
    /// The New Agent Session Tab form (SPECS §4, §22).
    NewAgent(NewAgentForm),
    /// Pick the backend for an extra agent in the selected session.
    NewAgentChild {
        /// Each registered agent, in registry order; button `1` is the first.
        agents: Vec<AgentChoice>,
    },
    /// Rename the selected Agent Session Tab.
    RenameSession,
    /// Pick a manual status override.
    SetManualStatus,
    /// How to handle running processes when closing the tab (SPECS §25).
    CloseSession {
        /// The offered actions; button `1` is the first.
        actions: Vec<CloseAction>,
    },
    /// Confirm closing a child terminal.
    CloseTerminal {
        /// Its display name, e.g. `shell 2`.
        label: String,
    },
    /// The sidebar `✕` menu: abandon, close, or cancel.
    CloseSessionChoice {
        /// The Agent Session Tab it is about.
        index: usize,
    },
    /// Push despite uncommitted changes (SPECS §14).
    ConfirmPush,
    /// Abandon the worktree (SPECS §5/§15).
    ConfirmAbandon {
        /// It has uncommitted changes that would be discarded.
        dirty: bool,
    },
    /// Finish / Local Merge (SPECS §15).
    ConfirmMerge {
        agent_branch: String,
        base_branch: String,
        /// The agent is still running and will be stopped.
        primary_running: bool,
    },
    /// Rebase Worktree (SPECS §5.1).
    ConfirmRebase {
        agent_branch: String,
        base_branch: String,
        /// Commits the base has moved on by since the worktree was created.
        drift: u32,
        primary_running: bool,
    },
    /// The project-folder browser.
    OpenProject {
        /// The folder being browsed.
        dir: PathBuf,
    },
    /// Pick the project's default base branch.
    ChangeProjectBase,
    /// Confirm closing an open project.
    CloseProject {
        /// The project it is about.
        index: usize,
    },
    /// Confirm forgetting the paired phone.
    UnpairPhone,
    /// Confirm quitting (only a browser's unconfirmed quit opens this).
    ConfirmQuit,
}

/// One registered agent backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentChoice {
    /// The registry key (`claude`, `codex`, …).
    pub key: String,
    /// The display name (`Claude Code`, `Codex`, …).
    pub name: String,
}

/// The New Agent Session Tab form's state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAgentForm {
    /// Every registered agent, in registry order.
    pub agents: Vec<AgentChoice>,
    /// Index into `agents` of the chosen one.
    pub selected_agent: usize,
    /// Where the session will run. `Tab` cycles it.
    pub target: NewAgentTarget,
    /// The typed text: the new branch name, or the filter over existing
    /// branches. Always empty for [`NewAgentTarget::Base`].
    pub branch: String,
    /// The existing local branches `branch` matches (existing-branch target).
    pub matching_branches: Vec<String>,
    /// Index into `matching_branches` of the chosen one.
    pub selected_branch: usize,
    /// Whether any non-base local branch exists; when not, `Tab` skips the
    /// existing-branch target.
    pub has_existing_branches: bool,
    /// The project's base branch (the base target runs on it).
    pub base_branch: String,
}

/// The New Agent form's three targets, in `Tab` order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewAgentTarget {
    /// Create a new branch and worktree named after the typed text.
    NewBranch,
    /// Check out an existing local branch in a new worktree.
    ExistingBranch,
    /// Run directly on the base branch in the project root (no worktree).
    Base,
}

/// The command palette's state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaletteView {
    /// The filter text.
    pub filter: String,
    /// Index into `entries` of the highlighted row.
    pub selected: usize,
    /// The rows the filter and the current state admit, in display order.
    pub entries: Vec<PaletteRow>,
}

/// One palette row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaletteRow {
    /// The group heading it sits under (`Projects`, `Agent Session`, …).
    pub section: &'static str,
    /// The label the user filters on.
    pub label: &'static str,
    /// What [`OverlayInput::PaletteRun`] runs.
    pub action: PaletteAction,
    /// The key that does the same thing (`Ctrl-p`), from the keymap table.
    pub keycap: Option<String>,
}

/// SPECS §21's panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitStatusView {
    /// What `collect_status` found.
    pub status: WorktreeStatus,
    /// SPECS §14's compare URL, when the remote is on GitHub.
    pub pr_url: Option<String>,
}

/// The configuration manager's state for the scope it is showing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigView {
    /// The project whose override file is the project scope.
    pub project_name: String,
    /// Which layer is being edited.
    pub scope: ConfigScope,
    /// That layer's file, or `None` for the global scope with no home dir.
    pub path: Option<PathBuf>,
    /// The curated fields for this scope, each with its value and where the
    /// value comes from (set here / from global / default).
    pub rows: Vec<ConfigRow>,
    /// What clearing each row would leave behind, index-aligned with `rows`.
    pub inherited: Vec<ConfigRow>,
    /// Index into `rows` of the highlighted field.
    pub selected: usize,
    /// Whether a text field is being edited inline.
    pub editing: bool,
    /// Unsaved edits in either scope.
    pub dirty: bool,
    /// The last action's one-line outcome (`Saved.`).
    pub status: Option<String>,
}

/// The access overlay, plus the string its QR encodes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebAccessOverlay {
    /// The same render model the TUI draws, rebuilt every turn.
    pub view: WebAccessView,
    /// The code-bearing URL the QR encodes, so a front-end can draw its own
    /// QR. `Some` exactly when `view.code` is: hiding the code hides it too.
    pub qr_payload: Option<String>,
}

/// The phone pairing overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingView {
    /// The one-line status, verbatim.
    pub status_line: String,
    /// The code to type on the phone, while one is displayed.
    pub code: Option<String>,
    /// The full `fdr1:` payload the QR encodes, while one is displayed, so a
    /// front-end renders its own QR instead of the TUI's half-block art.
    pub qr_payload: Option<String>,
    /// Seconds until the code expires.
    pub seconds_remaining: Option<i64>,
    /// The phone joined.
    pub done: bool,
    /// Pairing could not complete.
    pub failed: bool,
}

/// One answer to the overlay on screen, for [`crate::host::HostEvent::Overlay`].
///
/// Each is applied through the handler the TUI's keyboard reaches for the same
/// overlay, so the outcome — including every guard's refusal message — is the
/// TUI's. An input that does not fit the overlay on screen (a `Choose` naming a
/// button the dialog does not show, a `PaletteRun` for a row the palette does
/// not offer) is refused with an error and changes nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverlayInput {
    /// Press the button whose [`DialogButton::id`] this is. `Enter` also
    /// dismisses a [`OverlayView::Message`].
    Choose(String),
    /// Replace the text field's contents (a dialog's input, the palette filter,
    /// or the configuration manager's inline edit).
    SetText(String),
    /// Highlight row `index` of the dialog list, the palette, or the
    /// configuration manager.
    SelectRow(usize),
    /// Enter: the dialog's default button, the palette's highlighted row, the
    /// configuration manager's toggle.
    Submit,
    /// Esc: dismiss without deciding.
    Cancel,
    /// Replace the palette filter.
    PaletteFilter(String),
    /// Run this palette action. It must be one the open palette offers in the
    /// current state (an isolated run hides project actions, for instance).
    PaletteRun(PaletteAction),
    /// Set (`Some`) or clear (`None`) one curated setting in a scope, through
    /// the configuration manager's own mutators. Not written until
    /// [`OverlayInput::ConfigSave`].
    ConfigSet {
        scope: ConfigScope,
        /// The row's `section.key` ([`ConfigRow::key`]).
        key: String,
        value: Option<FieldValue>,
    },
    /// Write the manager's edits and reload every project (the TUI's `s`).
    ConfigSave,
    /// Open the shown scope's file in `$EDITOR` (the TUI's `e`); see
    /// [`crate::host::AppHost::take_pending_editor`].
    ConfigEditRaw,
    /// One key of the access overlay's own alphabet.
    WebAccess(AccessKey),
    /// A plain key, for anything the variants above do not name (the folder
    /// browser's `→`/`←`, the agent radio's `↑`/`↓`, `Tab` in a form).
    Key(OverlayKey),
}

/// A key, with no library behind it. Only meaningful while an overlay is open;
/// with none open it is ignored rather than reaching the main key map.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayKey {
    Enter,
    Esc,
    Tab,
    Backspace,
    Up,
    Down,
    Left,
    Right,
    Char(char),
}

/// The non-modal hints the TUI shows in its status bar rather than as an
/// overlay. SPECS §30 is explicit that the update notice is "never a modal",
/// and an isolated run's badge is permanent chrome, so they are read here
/// rather than through [`OverlayView`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostNotices {
    /// A newer release, when the once-a-day check found one.
    pub update: Option<UpdateNotice>,
    /// This is an `--isolated` run (SPECS §32): the `ISOLATED` badge.
    pub isolated: bool,
}

/// The remote-access facts a status bar shows on its right-hand side ("web ·
/// 2 viewers", "phone paired", who holds the input lock), read out as plain
/// data. Like [`HostNotices`] it is chrome, not an overlay.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RemoteStatus {
    /// The embedded web interface is listening.
    pub web_running: bool,
    /// Browsers currently attached (observers included); zero when the
    /// interface is stopped.
    pub web_viewers: usize,
    /// A phone is paired: configured at start-up or joined this session.
    pub phone_paired: bool,
    /// Who holds the web input lock, when somebody other than this desktop
    /// could be typing (the same value as [`crate::host::AppHost::input_holder`]).
    pub input_holder: Option<String>,
}
