//! The core's own view structs, built from a [`RemoteWorkspace`]
//! (`specs/DESKTOP_REMOTE_CONTROL_PLAN.md` §2.3).
//!
//! FlightDeck Desktop draws its sidebar, git strip, project tabs, palette and
//! dialogs from [`crate::view`] and [`crate::host`] structs that the local
//! `AppHost` builds from `AppState`. A remote window draws **the same structs**,
//! built here from the mirrored workspace instead — so the leaf views render a
//! remote instance without knowing it is remote, and there is one picture of
//! an agent row rather than two that drift.
//!
//! Also here, because it is the other half of the same translation: what a
//! remote window's palette row or dialog button *sends* (the wire command, and
//! the target it names), and which of them stay local because browsing is this
//! client's own business (R2).

use crate::app::commands::{Command, Selector};
use crate::git::status::{LineStats, WorktreeChanges, WorktreeStatus};
use crate::host::{
    ButtonRole, DialogButton, DialogKind, DialogRow, DialogView as HostDialog, GitStatusView,
    OverlayInput, OverlayKey, PaletteRow, PaletteView,
};
use crate::tui::palette::{FrontEndAction, PaletteAction, PaletteEntry};
use crate::view::{
    agent_badge, agent_status_text, AgentRowView, ChangeSummary, GitActions, GitStripAgent,
    GitStripView, ProjectStatus, ProjectTabView, TerminalRef, TerminalRole as RowRole,
    TerminalView as RowTerminal, UpstreamState,
};
use crate::web::commands::{lookup, Route, INVENTORY};
use crate::web::protocol::{
    self as wire, command as names, DialogBody, DialogId, GitBar, SessionPhase, SessionView,
    TerminalId, TerminalRole,
};

use super::mirror::RemoteWorkspace;
use super::Target;

// ---------------------------------------------------------------------------
// Sidebar, terminals, git strip, project tabs
// ---------------------------------------------------------------------------

/// A session's terminals as the sidebar lists them: the primary is `agent`,
/// further agents count up from 2 and shells from 1, in the host's order — the
/// labels [`crate::view::terminal_views`] gives a local tab.
pub fn terminal_rows(session: &SessionView) -> Vec<RowTerminal> {
    let mut out = Vec::new();
    let (mut agents, mut shells, mut child) = (2, 1, 0);
    for terminal in &session.terminals {
        let (target, role, label) = match terminal.role {
            TerminalRole::Primary => (TerminalRef::Primary, RowRole::Agent, "agent".to_string()),
            TerminalRole::Agent => {
                let label = format!("agent {agents}");
                agents += 1;
                child += 1;
                (TerminalRef::Child(child - 1), RowRole::Agent, label)
            }
            TerminalRole::Shell => {
                let label = format!("shell {shells}");
                shells += 1;
                child += 1;
                (TerminalRef::Child(child - 1), RowRole::Shell, label)
            }
        };
        out.push(RowTerminal {
            target,
            role,
            label,
            title: terminal.title.clone(),
        });
    }
    out
}

/// The wire terminal a sidebar [`TerminalRef`] stands for in `session`.
pub fn terminal_for(session: &SessionView, target: TerminalRef) -> Option<&wire::TerminalView> {
    match target {
        TerminalRef::Primary => session
            .terminals
            .iter()
            .find(|t| t.role == TerminalRole::Primary),
        TerminalRef::Child(i) => session
            .terminals
            .iter()
            .filter(|t| t.role != TerminalRole::Primary)
            .nth(i),
    }
}

/// The sidebar [`TerminalRef`] of wire terminal `id` in `session`.
pub fn terminal_ref(session: &SessionView, id: &TerminalId) -> Option<TerminalRef> {
    let terminal = session.terminals.iter().find(|t| &t.terminal_id == id)?;
    if terminal.role == TerminalRole::Primary {
        return Some(TerminalRef::Primary);
    }
    session
        .terminals
        .iter()
        .filter(|t| t.role != TerminalRole::Primary)
        .position(|t| &t.terminal_id == id)
        .map(TerminalRef::Child)
}

fn changes(git: &GitBar) -> Option<ChangeSummary> {
    git.collected.then_some(ChangeSummary {
        added: git.added,
        modified: git.modified,
        deleted: git.removed,
        files: git.files_changed,
        lines_added: git.lines_added,
        lines_removed: git.lines_removed,
    })
}

fn upstream(git: &GitBar) -> UpstreamState {
    match (git.collected, git.has_upstream) {
        (false, _) => UpstreamState::Unknown,
        (true, false) => UpstreamState::None,
        (true, true) => UpstreamState::Tracking {
            upstream: git
                .upstream
                .clone()
                .unwrap_or_else(|| "upstream".to_string()),
            ahead: git.ahead,
            behind: git.behind,
        },
    }
}

/// The selected project's agent rows, `selected` on this client's own
/// selection (R2).
pub fn agent_rows(ws: &RemoteWorkspace) -> Vec<AgentRowView> {
    let Some(project) = ws.selected_project() else {
        return Vec::new();
    };
    let selected = ws.selected_session().map(|s| s.session_id.clone());
    project
        .sessions
        .iter()
        .enumerate()
        .map(|(index, session)| AgentRowView {
            id: session.session_id.0.clone(),
            index,
            alt_index: (index < 9).then(|| index as u8 + 1),
            name: session.name.clone(),
            agent_name: session.agent_display_name.clone(),
            branch: session.git.branch.clone().unwrap_or_default(),
            badge: agent_badge(session.status.interpreted),
            manual_status: session.status.manual,
            status_text: match session.status.manual {
                Some(manual) => manual.as_str().to_string(),
                None => agent_status_text(session.status.interpreted).to_string(),
            },
            status_since_secs: Some(session.status.running_time_secs),
            creating: session.phase == SessionPhase::Creating,
            selected: selected.as_ref() == Some(&session.session_id),
            unread: false,
            changes: changes(&session.git),
            upstream: upstream(&session.git),
            base_drift: session.git.drift,
            recovered: session.recovered,
            attached_existing_branch: session.attached_existing_branch,
            terminals: terminal_rows(session),
        })
        .collect()
}

/// The selected session's focused terminal as the sidebar marks it: `None`
/// for its agent, `Some(i)` for child `i`.
pub fn focused_child(ws: &RemoteWorkspace) -> Option<usize> {
    let session = ws.selected_session()?;
    let terminal = ws.selected_terminal()?;
    match terminal_ref(session, &terminal.terminal_id)? {
        TerminalRef::Primary => None,
        TerminalRef::Child(i) => Some(i),
    }
}

/// The index, among the selected project's sessions, of the one the host
/// itself is looking at (R7's marker), if it is in this project.
pub fn host_marker(ws: &RemoteWorkspace) -> Option<usize> {
    let project = ws.selected_project()?;
    project
        .sessions
        .iter()
        .position(|s| ws.host_is_viewing(&s.session_id))
}

/// The git strip for the selected session.
pub fn git_strip(ws: &RemoteWorkspace) -> GitStripView {
    let base = ws
        .selected_project()
        .map(|p| p.base_branch.clone())
        .unwrap_or_default();
    let agent = ws.selected_session().map(|session| {
        let ready = session.phase == SessionPhase::Ready;
        let target_branch = base.clone();
        GitStripAgent {
            name: session.name.clone(),
            branch: session.git.branch.clone().unwrap_or_default(),
            target_is_default: true,
            target_branch,
            changes: changes(&session.git),
            upstream: upstream(&session.git),
            base_drift: session.git.drift,
            actions: GitActions {
                push: ready,
                // Pull Base is refused from a remote surface (SPECS §5.2,
                // `PULL_BASE_REFUSAL`), so it is never offered live.
                pull_base: false,
                finish: ready,
            },
        }
    });
    GitStripView {
        default_branch: base,
        default_branch_valid: true,
        agent,
    }
}

/// One tab per host project, `active` on this client's own selection.
pub fn project_tabs(ws: &RemoteWorkspace) -> Vec<ProjectTabView> {
    let selected = ws.selected_project().map(|p| p.project_id.clone());
    ws.projects
        .iter()
        .map(|project| ProjectTabView {
            name: project.name.clone(),
            status: ProjectStatus::aggregate(project.sessions.iter().map(|s| s.status.interpreted)),
            agent_count: project.sessions.len(),
            active: selected.as_ref() == Some(&project.project_id),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// What a command does from a remote window
// ---------------------------------------------------------------------------

/// Browsing this client does on its own (R2): never sent to the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalAction {
    Session(Selector),
    Terminal(Selector),
    Project(Selector),
    Help,
    About,
    /// A palette row the desktop itself performs — another window, or
    /// another remote ([`PaletteAction::FrontEnd`]).
    FrontEnd(FrontEndAction),
}

/// Which target a sent command names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// The selected terminal (closing a child).
    Terminal,
    /// The selected session, else the selected project.
    Session,
    /// The selected project.
    Project,
    /// Nothing: the whole instance (quit).
    Instance,
}

/// What a palette row or a chord does in a remote window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemoteAction {
    Local(LocalAction),
    Send {
        name: &'static str,
        scope: Scope,
    },
    /// The host has no way to do this for a remote surface; the sentence says
    /// why.
    Unavailable(&'static str),
}

/// Why the host's Quit is not offered.
pub const QUIT_UNAVAILABLE: &str =
    "A remote window does not quit the host's FlightDeck. Quit it on the host.";

/// Why split view is not offered: it lays out the *host's* screen.
pub const SPLIT_VIEW_UNAVAILABLE: &str =
    "Split view arranges the host's own screen, so it is not offered from a remote window.";

/// What `action` does from a remote window.
pub fn route_palette(action: &PaletteAction) -> RemoteAction {
    if let PaletteAction::Dispatch(command) = action {
        if let Some(route) = route_command(command) {
            return route;
        }
    }
    match action {
        PaletteAction::SwitchProjectNext => {
            RemoteAction::Local(LocalAction::Project(Selector::Next))
        }
        PaletteAction::SwitchProjectPrev => {
            RemoteAction::Local(LocalAction::Project(Selector::Prev))
        }
        PaletteAction::FrontEnd(front_end) => {
            RemoteAction::Local(LocalAction::FrontEnd(*front_end))
        }
        _ => match wire_name(action) {
            Some(name) => RemoteAction::Send {
                name,
                scope: scope_of(name),
            },
            None => RemoteAction::Unavailable(
                "The host does not offer this command to a remote window.",
            ),
        },
    }
}

/// What a keymap command does from a remote window, where that differs from
/// "send the palette row it is" (browsing, overlays drawn locally, and the
/// commands whose empty payload means "ask").
pub fn route_command(command: &Command) -> Option<RemoteAction> {
    let send = |name| {
        Some(RemoteAction::Send {
            name,
            scope: scope_of(name),
        })
    };
    match command {
        Command::SwitchAgentTab(sel) => Some(RemoteAction::Local(LocalAction::Session(*sel))),
        Command::SwitchChildTerminal(sel) => Some(RemoteAction::Local(LocalAction::Terminal(*sel))),
        Command::ShowHelp => Some(RemoteAction::Local(LocalAction::Help)),
        Command::ShowAbout => Some(RemoteAction::Local(LocalAction::About)),
        Command::ToggleSplitView => Some(RemoteAction::Unavailable(SPLIT_VIEW_UNAVAILABLE)),
        // The keymap's payload-carrying commands open their dialog on the host,
        // exactly as their palette rows do.
        Command::NewAgentTab { .. } => send(names::NEW_AGENT_SESSION_TAB),
        Command::RenameAgentTab { .. } => send(names::RENAME_AGENT_SESSION_TAB),
        Command::CloseAgentTab { .. } => send(names::CLOSE_AGENT_SESSION_TAB),
        Command::SetManualStatus(_) => send(names::SET_MANUAL_STATUS),
        Command::NewAgentTerminal { .. } => send(names::NEW_AGENT),
        // Quitting means this app wherever it is asked from; the host's
        // FlightDeck is quit at the host.
        Command::Quit { .. } => Some(RemoteAction::Unavailable(QUIT_UNAVAILABLE)),
        _ => wire_name(&PaletteAction::Dispatch(command.clone())).and_then(send),
    }
}

/// The wire name whose [`Route::Palette`] row carries `action`.
fn wire_name(action: &PaletteAction) -> Option<&'static str> {
    INVENTORY
        .iter()
        .find(|spec| matches!(&spec.route, Route::Palette(a) if a == action))
        .map(|spec| spec.name)
}

fn scope_of(name: &str) -> Scope {
    match name {
        names::CLOSE_CHILD_TERMINAL | names::CLOSE_AGENT => Scope::Terminal,
        names::NEW_AGENT_SESSION_TAB | names::CLOSE_PROJECT => Scope::Project,
        names::OPEN_PROJECT | names::QUIT => Scope::Instance,
        _ => Scope::Session,
    }
}

/// The target `scope` names on this client's selection, if it names one.
pub fn target(ws: &RemoteWorkspace, scope: Scope) -> Option<Target> {
    let project = || {
        ws.selected_project()
            .map(|p| Target::Project(p.project_id.clone()))
    };
    let session = || {
        ws.selected_session()
            .map(|s| Target::Session(s.session_id.clone()))
    };
    match scope {
        Scope::Instance => None,
        Scope::Project => project(),
        Scope::Session => session().or_else(project),
        Scope::Terminal => ws
            .selected_terminal()
            .map(|t| Target::Terminal(t.terminal_id.clone()))
            .or_else(session)
            .or_else(project),
    }
}

/// The palette rows a remote window offers: this build's own rows, kept to the
/// ones the host's inventory forwards and a remote window can use, plus the
/// desktop's own front-end rows (a remote window only exists in the desktop).
pub fn palette_entries(ws: &RemoteWorkspace) -> Vec<&'static PaletteEntry> {
    crate::tui::palette::rows(true)
        .filter(|entry| match route_palette(&entry.action) {
            RemoteAction::Local(_) => true,
            RemoteAction::Unavailable(_) => false,
            RemoteAction::Send { name, .. } => {
                // Offered by the host *and* forwarded by it, not a row that
                // could only come back refused.
                ws.command(name).is_some_and(|c| c.refusal.is_none())
                    && lookup(name).is_some_and(|spec| matches!(spec.route, Route::Palette(_)))
                    && name != names::CHANGE_PROJECT_DEFAULT_BASE
            }
        })
        .collect()
}

/// The palette overlay, filtered like the local one (case-insensitive
/// substring on the label).
pub fn palette_view(ws: &RemoteWorkspace, filter: &str, selected: usize) -> PaletteView {
    let keymap = crate::app::keymap::Keymap::for_this_platform(false);
    let needle = filter.to_lowercase();
    let entries: Vec<PaletteRow> = palette_entries(ws)
        .into_iter()
        .filter(|e| needle.is_empty() || e.label.to_lowercase().contains(&needle))
        .map(|entry| PaletteRow {
            section: entry.group,
            label: entry.label,
            action: entry.action.clone(),
            keycap: entry.keycap(keymap),
        })
        .collect();
    PaletteView {
        filter: filter.to_string(),
        selected: selected.min(entries.len().saturating_sub(1)),
        entries,
    }
}

// ---------------------------------------------------------------------------
// The shared dialog (D13), answered from a remote window
// ---------------------------------------------------------------------------

/// What this client has typed or chosen into the host's open dialog, before
/// answering it. The host's dialog is not touched until the answer goes out:
/// a confirm carries the whole draft, as a browser's does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DialogDraft {
    pub dialog_id: DialogId,
    /// The dialog's own field, when it has one.
    pub text: Option<String>,
    pub list_index: Option<usize>,
    /// The name typed for artboard 1g's second step, when the dialog has one.
    pub typed_name: String,
}

/// The dialog's structured body, if it carried one.
pub fn dialog_body(view: &wire::DialogView) -> DialogBody {
    view.body
        .clone()
        .and_then(|b| serde_json::from_value(b).ok())
        .unwrap_or_default()
}

impl DialogDraft {
    /// A fresh draft for `view`: its field and list as the host has them.
    pub fn new(view: &wire::DialogView) -> DialogDraft {
        let body = dialog_body(view);
        DialogDraft {
            dialog_id: view.dialog_id.clone(),
            text: body.input,
            list_index: body.list.iter().position(|c| c.selected),
            typed_name: String::new(),
        }
    }
}

/// `view` as the desktop's dialog card draws it, with this client's draft.
pub fn dialog_overlay(view: &wire::DialogView, draft: &DialogDraft) -> HostDialog {
    let body = dialog_body(view);
    let mut lines = view.title.lines();
    let title = lines.next().unwrap_or_default().to_string();
    let mut text: Vec<String> = lines.map(str::to_string).collect();
    let gate = body.confirm_gate.clone();
    let mut input = draft.text.clone();
    if let Some(gate) = &gate {
        text.push(gate.instruction.clone());
        text.push(format!("Type `{}` to enable it.", gate.expected));
        if input.is_none() {
            input = Some(draft.typed_name.clone());
        }
    }
    if let Some(refusal) = &body.refusal {
        text.push(refusal.clone());
    }
    let mut primary_given = false;
    let buttons = body
        .buttons
        .iter()
        .filter(|key| body.confirmable || key.cancels)
        .map(|key| {
            let gated = gate.as_ref().is_some_and(|g| g.key == key.key);
            let role = if key.cancels {
                ButtonRole::Cancel
            } else if gated {
                ButtonRole::Destructive
            } else if !primary_given {
                primary_given = true;
                ButtonRole::Primary
            } else {
                ButtonRole::Secondary
            };
            DialogButton {
                id: key.key.clone(),
                label: key.label.clone(),
                role,
                default: key.key == "Enter",
            }
        })
        .collect();
    HostDialog {
        id: view.dialog_id.clone(),
        kind: DialogKind::Remote {
            kind: view.kind.clone(),
        },
        title,
        body: text,
        input,
        list: body
            .list
            .iter()
            .enumerate()
            .map(|(i, choice)| DialogRow {
                label: choice.label.clone(),
                selected: draft.list_index.map_or(choice.selected, |s| s == i),
            })
            .collect(),
        list_filter: body.list_filter,
        buttons,
        origin: view.origin.clone(),
        origin_label: match &view.origin {
            wire::DialogOrigin::Desktop => Some("opened on the host".to_string()),
            wire::DialogOrigin::Browser { label, .. } => Some(format!("opened from {label}")),
        },
    }
}

/// A command a dialog answer sends.
#[derive(Clone, Debug, PartialEq)]
pub struct DialogReply {
    pub name: &'static str,
    pub args: serde_json::Value,
}

/// Apply one overlay input to the draft. Returns the command to send when the
/// input answers the dialog.
pub fn dialog_input(
    view: &wire::DialogView,
    draft: &mut DialogDraft,
    input: &OverlayInput,
) -> Option<DialogReply> {
    let body = dialog_body(view);
    let cancel = || DialogReply {
        name: names::DIALOG_CANCEL,
        args: serde_json::json!({ "dialog_id": view.dialog_id.as_str() }),
    };
    let rows = body.list.len();
    let step = |draft: &mut DialogDraft, down: bool| {
        if rows == 0 {
            return;
        }
        let current = draft.list_index.unwrap_or(0);
        draft.list_index = Some(if down {
            (current + 1).min(rows - 1)
        } else {
            current.saturating_sub(1)
        });
    };
    match input {
        OverlayInput::Cancel => Some(cancel()),
        OverlayInput::SetText(text) => {
            if draft.text.is_some() {
                draft.text = Some(text.clone());
            } else if body.confirm_gate.is_some() {
                draft.typed_name = text.clone();
            }
            None
        }
        OverlayInput::SelectRow(i) => {
            draft.list_index = Some((*i).min(rows.saturating_sub(1)));
            None
        }
        OverlayInput::Key(OverlayKey::Up) => {
            step(draft, false);
            None
        }
        OverlayInput::Key(OverlayKey::Down) => {
            step(draft, true);
            None
        }
        OverlayInput::Key(OverlayKey::Tab) => body
            .buttons
            .iter()
            .any(|b| b.key == "Tab")
            .then(|| confirm(view, draft, &body, "Tab")),
        OverlayInput::Submit => body
            .buttons
            .iter()
            .any(|b| b.key == "Enter")
            .then(|| confirm(view, draft, &body, "Enter")),
        OverlayInput::Choose(id) => match body.buttons.iter().find(|b| &b.key == id) {
            Some(button) if button.cancels => Some(cancel()),
            Some(_) => Some(confirm(view, draft, &body, id)),
            None => None,
        },
        _ => None,
    }
}

fn confirm(
    view: &wire::DialogView,
    draft: &DialogDraft,
    body: &DialogBody,
    choice: &str,
) -> DialogReply {
    let mut args = serde_json::json!({
        "dialog_id": view.dialog_id.as_str(),
        "choice": choice,
    });
    if let Some(text) = &draft.text {
        args["text"] = serde_json::json!(text);
    }
    if let Some(index) = draft.list_index {
        args["list_index"] = serde_json::json!(index);
    }
    if body.confirm_gate.as_ref().is_some_and(|g| g.key == choice) {
        args["confirm_name"] = serde_json::json!(draft.typed_name);
    }
    DialogReply {
        name: names::DIALOG_CONFIRM,
        args,
    }
}

// ---------------------------------------------------------------------------
// Read-only panels
// ---------------------------------------------------------------------------

/// SPECS §21's panel as the desktop's overlay holds it. The wire counts
/// changed files without splitting them, so they are shown as modified.
pub fn git_status(view: &wire::GitStatusView) -> GitStatusView {
    GitStatusView {
        status: WorktreeStatus {
            branch: view.branch.clone(),
            base_branch: view.base_branch.clone(),
            dirty: view.dirty,
            changes: WorktreeChanges {
                added: 0,
                modified: view.changed_files,
                deleted: 0,
            },
            lines: LineStats::default(),
            ahead: view.upstream.as_ref().map_or(0, |u| u.ahead),
            behind: view.upstream.as_ref().map_or(0, |u| u.behind),
            upstream: view.upstream.as_ref().map(|u| u.name.clone()),
            base_drift: view.base_drift,
            worktree_path: std::path::PathBuf::from(&view.worktree_path),
        },
        pr_url: view.compare_url.clone(),
    }
}

#[cfg(test)]
mod tests;
