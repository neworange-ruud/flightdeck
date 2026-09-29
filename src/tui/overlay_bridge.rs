//! The TUI's side of the host's neutral overlay API ([`crate::host::overlay`]).
//!
//! Two directions, both deliberately thin:
//!
//! * [`overlay_view`] **reads out** the interactive state the TUI already keeps
//!   on [`Ui`] — its prompt, palette, configuration manager and overlays — into
//!   the host's plain-data [`OverlayView`]. Nothing is derived twice: the
//!   dialog's words and buttons are the ones `prompt_dialog` built for the TUI
//!   to draw, the help screen is `help_doc`, the access overlay is the view
//!   `refresh_web_access_overlay` rebuilt this turn.
//! * [`apply_overlay_input`] **answers** it by driving the TUI's own handlers.
//!   A button press is the key the TUI's dialog button synthesizes, fed through
//!   `handle_key`; a palette run is the palette's own Enter; a named config
//!   edit goes through the same `stage_config_change` a browser's save does.
//!   That is FlightDeck Web's D13 rule (`apply_web_dialog`) applied to a second
//!   front-end: there is no second dialog engine, so a guard refuses the GUI
//!   in the same sentence it refuses the keyboard.
//!
//! It lives under `tui` rather than `host` because it speaks crossterm's key
//! vocabulary to the TUI's handlers; the host module itself names no
//! terminal-UI library type.

use std::sync::Mutex;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::contracts::error::{FlightDeckError, Result};
use crate::host::overlay::{
    AgentChoice, ButtonRole, ConfigView, DialogButton, DialogKind, DialogRow, DialogView,
    GitStatusView, MessageView, NewAgentForm, NewAgentTarget, OverlayInput, OverlayKey,
    OverlayView, PairingView, PaletteRow, PaletteView, WebAccessOverlay,
};
use crate::remote::pairing::{PairingPhase, PairingSession};
use crate::tui::config_manager::ConfigManager;
use crate::tui::palette::CommandPalette;
use crate::tui::render::{DialogAccel, UiOverlay};
use crate::web::credentials::CredentialStore;
use crate::{Env, Prompt, PromptState, Ui, Workspace};

// ---------------------------------------------------------------------------
// Reading out
// ---------------------------------------------------------------------------

/// The overlay on screen, in the order the TUI's key handler gives them input
/// (`handle_key`): an open prompt, the configuration manager, the access
/// overlay, the palette, then a plain overlay. Reading them in *input* order
/// means the view a front-end draws is always the one its next
/// [`OverlayInput`] answers. (The TUI draws the configuration manager above a
/// prompt in the one case both are up; they are otherwise exclusive.)
pub(crate) fn overlay_view(
    ui: &Ui,
    workspace: &Workspace,
    pairing: Option<&PairingSession>,
    credentials: &Mutex<CredentialStore>,
) -> Option<OverlayView> {
    let state = &workspace.active_project().state;
    let use_f2 = state.config.ui.use_f2_to_leave_terminal_focus;
    if let Some(open) = &ui.prompt {
        return Some(OverlayView::Dialog(dialog_view(open)));
    }
    if let Some(cm) = &ui.config {
        return Some(OverlayView::Config(config_view(cm)));
    }
    if ui.web_access.is_none() {
        if let Some(palette) = &ui.palette {
            return Some(OverlayView::Palette(palette_view(palette, use_f2)));
        }
    }
    match &ui.overlay {
        UiOverlay::None => None,
        UiOverlay::Palette(palette) => Some(OverlayView::Palette(palette_view(palette, use_f2))),
        UiOverlay::Config(cm) => Some(OverlayView::Config(config_view(cm))),
        UiOverlay::Help => Some(OverlayView::Help(crate::tui::help::help_doc(
            use_f2,
            state.isolated,
        ))),
        UiOverlay::About => Some(OverlayView::About(crate::tui::help::about_doc())),
        UiOverlay::GitStatus { status, pr_url } => Some(OverlayView::GitStatus(GitStatusView {
            status: status.clone(),
            pr_url: pr_url.clone(),
        })),
        // `Ui::overlay` only ever holds a notification dialog: a prompt's dialog
        // lives on `Ui::prompt`, read above.
        UiOverlay::Dialog(dialog) => Some(OverlayView::Message(MessageView {
            text: dialog.title.clone(),
        })),
        UiOverlay::Remote(view) => {
            Some(OverlayView::Pairing(PairingView {
                status_line: view.status_line.clone(),
                code: view.code.clone(),
                // The payload the TUI encodes as half-block art, straight from the
                // session, so a front-end draws its own QR of the same string.
                qr_payload: view.code.as_ref().and(pairing).and_then(|session| {
                    match session.phase() {
                        PairingPhase::Displaying { qr_payload, .. } => Some(qr_payload.clone()),
                        _ => None,
                    }
                }),
                seconds_remaining: view.seconds_remaining,
                done: view.done,
                failed: view.failed,
            }))
        }
        UiOverlay::WebAccess(view) => Some(OverlayView::WebAccess(WebAccessOverlay {
            view: view.clone(),
            qr_payload: web_access_payload(ui, view, credentials),
        })),
    }
}

/// The code-bearing URL the access overlay's QR encodes, exactly when the view
/// is drawing a code (so hiding the code hides this too).
fn web_access_payload(
    ui: &Ui,
    view: &crate::web::access::WebAccessView,
    credentials: &Mutex<CredentialStore>,
) -> Option<String> {
    view.code.as_ref()?;
    let access = ui.web_access.as_ref()?;
    let store = credentials.lock().ok()?;
    access.authenticated_url(store.bootstrap_code()?)
}

/// An open prompt, read out.
fn dialog_view(open: &PromptState) -> DialogView {
    let dialog = &open.dialog;
    let mut lines = dialog.title.split('\n').map(str::to_string);
    let title = lines.next().unwrap_or_default();
    let body: Vec<String> = lines.collect();
    let buttons = dialog
        .buttons
        .iter()
        .enumerate()
        .map(|(i, button)| DialogButton {
            id: crate::dialog_accel_key(button.accel),
            label: button.label.clone(),
            role: if button.cancels() {
                ButtonRole::Cancel
            } else if is_destructive(&open.prompt, button.accel) {
                ButtonRole::Destructive
            } else if i == 0 {
                ButtonRole::Primary
            } else {
                ButtonRole::Secondary
            },
            default: button.accel == DialogAccel::Enter,
        })
        .collect();
    DialogView {
        id: open.id.clone(),
        kind: dialog_kind(&open.prompt),
        title,
        body,
        input: dialog.input.clone(),
        list: dialog
            .list
            .iter()
            .map(|item| DialogRow {
                label: item.label.clone(),
                selected: item.selected,
            })
            .collect(),
        list_filter: crate::dialog_list_filters(&open.prompt),
        buttons,
        origin: open.origin.clone(),
        origin_label: dialog.origin.clone(),
    }
}

/// Whether the button behind `accel` destroys work, stops processes or
/// rewrites history. Exhaustive, so a prompt added later must say.
fn is_destructive(prompt: &Prompt, accel: DialogAccel) -> bool {
    match prompt {
        // SPECS §5/§15 abandon, §5.1 rebase, §15's merge-back (it removes the
        // worktree and stops a running agent), and the three that stop
        // processes or forget a pairing.
        Prompt::AbandonConfirm { .. }
        | Prompt::RebaseConfirm { .. }
        | Prompt::MergeConfirm { .. }
        | Prompt::CloseChildConfirm { .. }
        | Prompt::CloseProjectConfirm { .. }
        | Prompt::UnpairConfirm
        | Prompt::QuitConfirm => accel == DialogAccel::Char('y'),
        // SPECS §25: of the close actions, only force-terminating kills.
        Prompt::CloseTab { actions } => actions
            .iter()
            .position(|a| *a == crate::app::commands::CloseAction::ForceTerminate)
            .is_some_and(|i| crate::digit_accel(i) == accel),
        // The sidebar menu's `a` only opens the abandon confirmation, which is
        // where the destructive answer is.
        Prompt::CloseAgentChoice { .. }
        | Prompt::PushConfirm
        | Prompt::NewAgentForm { .. }
        | Prompt::SelectChildAgent { .. }
        | Prompt::RenameTab { .. }
        | Prompt::SetManualStatus
        | Prompt::OpenProject { .. }
        | Prompt::ChangeProjectBase { .. } => false,
    }
}

fn agent_choices(agents: &[(String, String)]) -> Vec<AgentChoice> {
    agents
        .iter()
        .map(|(key, name)| AgentChoice {
            key: key.clone(),
            name: name.clone(),
        })
        .collect()
}

/// The typed facts behind one prompt.
fn dialog_kind(prompt: &Prompt) -> DialogKind {
    match prompt {
        Prompt::NewAgentForm {
            agents,
            selected,
            branch,
            existing_branches,
            branch_selected,
            use_existing_branch,
            run_on_base,
            base_branch,
        } => DialogKind::NewAgent(NewAgentForm {
            agents: agent_choices(agents),
            selected_agent: *selected,
            target: if *run_on_base {
                NewAgentTarget::Base
            } else if *use_existing_branch {
                NewAgentTarget::ExistingBranch
            } else {
                NewAgentTarget::NewBranch
            },
            branch: branch.clone(),
            matching_branches: crate::matching_branches(existing_branches, branch)
                .into_iter()
                .cloned()
                .collect(),
            selected_branch: *branch_selected,
            has_existing_branches: !existing_branches.is_empty(),
            base_branch: base_branch.clone(),
        }),
        Prompt::SelectChildAgent { agents } => DialogKind::NewAgentChild {
            agents: agent_choices(agents),
        },
        Prompt::RenameTab { .. } => DialogKind::RenameSession,
        Prompt::SetManualStatus => DialogKind::SetManualStatus,
        Prompt::CloseTab { actions } => DialogKind::CloseSession {
            actions: actions.clone(),
        },
        Prompt::CloseChildConfirm { label } => DialogKind::CloseTerminal {
            label: label.clone(),
        },
        Prompt::CloseAgentChoice { index } => DialogKind::CloseSessionChoice { index: *index },
        Prompt::PushConfirm => DialogKind::ConfirmPush,
        Prompt::AbandonConfirm { dirty } => DialogKind::ConfirmAbandon { dirty: *dirty },
        Prompt::MergeConfirm {
            agent_branch,
            base_branch,
            primary_running,
        } => DialogKind::ConfirmMerge {
            agent_branch: agent_branch.clone(),
            base_branch: base_branch.clone(),
            primary_running: *primary_running,
        },
        Prompt::RebaseConfirm {
            agent_branch,
            base_branch,
            drift,
            primary_running,
        } => DialogKind::ConfirmRebase {
            agent_branch: agent_branch.clone(),
            base_branch: base_branch.clone(),
            drift: *drift,
            primary_running: *primary_running,
        },
        Prompt::OpenProject { browse } => DialogKind::OpenProject {
            dir: browse.dir.clone(),
        },
        Prompt::ChangeProjectBase { .. } => DialogKind::ChangeProjectBase,
        Prompt::CloseProjectConfirm { index } => DialogKind::CloseProject { index: *index },
        Prompt::UnpairConfirm => DialogKind::UnpairPhone,
        Prompt::QuitConfirm => DialogKind::ConfirmQuit,
    }
}

/// The palette, read out with each row's keycap from the keymap table.
fn palette_view(palette: &CommandPalette, use_f2: bool) -> PaletteView {
    let keymap = crate::app::keymap::Keymap::for_this_platform(use_f2);
    PaletteView {
        filter: palette.filter().to_string(),
        selected: palette.selected_index(),
        entries: palette
            .filtered()
            .into_iter()
            .map(|entry| PaletteRow {
                section: entry.group,
                label: entry.label,
                action: entry.action.clone(),
                keycap: entry.keycap(keymap),
            })
            .collect(),
    }
}

/// The configuration manager, read out for the scope it is showing.
fn config_view(cm: &ConfigManager) -> ConfigView {
    ConfigView {
        project_name: cm.project_name().to_string(),
        scope: cm.scope(),
        path: cm.current_path(),
        rows: cm.rows(),
        inherited: cm.inherited_rows(),
        selected: cm.selected_index(),
        editing: cm.is_editing(),
        dirty: cm.dirty(),
        status: cm.status().map(str::to_string),
    }
}

// ---------------------------------------------------------------------------
// Answering
// ---------------------------------------------------------------------------

/// Apply one [`OverlayInput`] through the TUI's own handlers. An input that
/// does not fit the overlay on screen is refused with
/// [`FlightDeckError::Refused`] and changes nothing.
pub(crate) fn apply_overlay_input(
    input: OverlayInput,
    workspace: &mut Workspace,
    env: &Env,
    ui: &mut Ui,
) -> Result<()> {
    match input {
        OverlayInput::Key(key) => press(key_code(key), workspace, env, ui),
        OverlayInput::Submit => press(KeyCode::Enter, workspace, env, ui),
        OverlayInput::Cancel => press(KeyCode::Esc, workspace, env, ui),
        OverlayInput::Choose(id) => choose(&id, workspace, env, ui),
        OverlayInput::SetText(text) => set_text(&text, workspace, env, ui),
        OverlayInput::SelectRow(index) => select_row(index, workspace, env, ui),
        OverlayInput::PaletteFilter(text) => {
            let palette = open_palette(ui)?;
            palette.set_filter(text);
            Ok(())
        }
        OverlayInput::PaletteRun(action) => {
            let palette = open_palette(ui)?;
            if !crate::palette_offers(palette, &action) {
                return Err(refused(
                    "The palette does not offer that command right now.",
                ));
            }
            crate::confirm_palette_action(action, workspace, env, ui)
        }
        OverlayInput::ConfigSet { scope, key, value } => {
            let cm = ui
                .config
                .as_mut()
                .ok_or_else(|| refused("The configuration manager is not open."))?;
            crate::stage_config_change(cm, scope, &key, value).map_err(FlightDeckError::Refused)
        }
        OverlayInput::ConfigSave => {
            require_config(ui)?;
            crate::save_config_manager(workspace, env, ui)
        }
        OverlayInput::ConfigEditRaw => {
            require_config(ui)?;
            crate::config_edit_raw(workspace, ui);
            Ok(())
        }
        OverlayInput::WebAccess(key) => {
            if ui.web_access.is_none() {
                return Err(refused("The web access overlay is not open."));
            }
            crate::apply_web_access_key(key, ui);
            Ok(())
        }
    }
}

fn refused(reason: &str) -> FlightDeckError {
    FlightDeckError::Refused(reason.to_string())
}

fn open_palette(ui: &mut Ui) -> Result<&mut CommandPalette> {
    ui.palette
        .as_mut()
        .ok_or_else(|| refused("The command palette is not open."))
}

fn require_config(ui: &Ui) -> Result<()> {
    match ui.config {
        Some(_) => Ok(()),
        None => Err(refused("The configuration manager is not open.")),
    }
}

fn key_code(key: OverlayKey) -> KeyCode {
    match key {
        OverlayKey::Enter => KeyCode::Enter,
        OverlayKey::Esc => KeyCode::Esc,
        OverlayKey::Tab => KeyCode::Tab,
        OverlayKey::Backspace => KeyCode::Backspace,
        OverlayKey::Up => KeyCode::Up,
        OverlayKey::Down => KeyCode::Down,
        OverlayKey::Left => KeyCode::Left,
        OverlayKey::Right => KeyCode::Right,
        OverlayKey::Char(c) => KeyCode::Char(c),
    }
}

fn accel_code(accel: DialogAccel) -> KeyCode {
    match accel {
        DialogAccel::Char(c) => KeyCode::Char(c),
        DialogAccel::Enter => KeyCode::Enter,
        DialogAccel::Esc => KeyCode::Esc,
        DialogAccel::Tab => KeyCode::Tab,
    }
}

/// One keypress into whatever overlay captures input, through `handle_key` —
/// the TUI's own entry point, so the prompt's D13 decision record, the help
/// screen's dismissal and the access overlay's alphabet all behave as they do
/// for a real key. With no overlay open it is ignored: an overlay key must
/// never reach the main key map.
fn press(code: KeyCode, workspace: &mut Workspace, env: &Env, ui: &mut Ui) -> Result<()> {
    if !ui.modal_active() {
        return Ok(());
    }
    crate::handle_key(KeyEvent::new(code, KeyModifiers::NONE), workspace, env, ui).map(|_| ())
}

/// Press a named button: the open prompt's, or a notification's `OK`.
fn choose(id: &str, workspace: &mut Workspace, env: &Env, ui: &mut Ui) -> Result<()> {
    let dialog = match (&ui.prompt, &ui.overlay) {
        (Some(open), _) => open.dialog.clone(),
        (None, UiOverlay::Dialog(dialog))
            if ui.config.is_none() && ui.web_access.is_none() && ui.palette.is_none() =>
        {
            dialog.clone()
        }
        _ => return Err(refused("No dialog is open.")),
    };
    // Only a button the dialog is showing, exactly as a browser's confirm is
    // bounded (`apply_web_dialog`).
    let accel = crate::dialog_accel_from_key(id)
        .filter(|accel| dialog.buttons.iter().any(|b| b.accel == *accel))
        .ok_or_else(|| {
            FlightDeckError::Refused(format!(
                "This dialog has no `{id}` button; it shows {}.",
                crate::button_keys(&dialog)
            ))
        })?;
    press(accel_code(accel), workspace, env, ui)
}

/// Replace a text field through ordinary keypresses: erase what is there, then
/// type the new text — so the handler's own rules (a filter resetting its
/// selection, a disabled field ignoring input) apply exactly as when typing.
fn set_text(text: &str, workspace: &mut Workspace, env: &Env, ui: &mut Ui) -> Result<()> {
    if text.chars().any(char::is_control) {
        return Err(refused("A text field takes printable characters only."));
    }
    let current = if let Some(open) = &ui.prompt {
        open.dialog
            .input
            .clone()
            .ok_or_else(|| refused("This dialog has no text field."))?
    } else if let Some(cm) = &ui.config {
        if !cm.is_editing() {
            return Err(refused("No configuration field is being edited."));
        }
        cm.rows()
            .get(cm.selected_index())
            .map(|row| row.value.clone())
            .unwrap_or_default()
    } else if let (None, Some(palette)) = (&ui.web_access, ui.palette.as_mut()) {
        palette.set_filter(text);
        return Ok(());
    } else {
        return Err(refused("No text field is open."));
    };
    for _ in current.chars() {
        press(KeyCode::Backspace, workspace, env, ui)?;
    }
    for c in text.chars() {
        press(KeyCode::Char(c), workspace, env, ui)?;
    }
    Ok(())
}

/// Highlight one row of the dialog list, the palette or the configuration
/// manager, through the same movement each already has.
fn select_row(index: usize, workspace: &mut Workspace, env: &Env, ui: &mut Ui) -> Result<()> {
    if let Some(open) = &ui.prompt {
        let len = open.dialog.list.len();
        if index >= len {
            return Err(FlightDeckError::Refused(format!(
                "This dialog has {len} rows, so row {index} names none of them."
            )));
        }
        // Driven to the top first, so the index is absolute (the handlers'
        // `Up` saturates at the first row).
        for _ in 0..len {
            press(KeyCode::Up, workspace, env, ui)?;
        }
        for _ in 0..index {
            press(KeyCode::Down, workspace, env, ui)?;
        }
        return Ok(());
    }
    if let Some(cm) = ui.config.as_mut() {
        let len = cm.rows().len();
        if index >= len {
            return Err(FlightDeckError::Refused(format!(
                "The configuration manager has {len} rows, so row {index} names none of them."
            )));
        }
        while cm.selected_index() != index {
            cm.select_next();
        }
        return Ok(());
    }
    if let (None, Some(palette)) = (&ui.web_access, ui.palette.as_mut()) {
        let len = palette.filtered().len();
        if index >= len {
            return Err(FlightDeckError::Refused(format!(
                "The palette lists {len} rows, so row {index} names none of them."
            )));
        }
        while palette.selected_index() != index {
            palette.select_next();
        }
        return Ok(());
    }
    Err(refused("No list is open."))
}
