//! View-model fixtures for the overlays: each dialog as the host reads it
//! out, its copy taken verbatim from the core's `prompt_dialog`, and the
//! palette built from the TUI's own `CommandPalette`.
//!
//! Shared by the tests and by the `spike-snapshot` render check (which draws
//! each one offscreen to look at the pixels); compiled for nothing else.

use std::path::PathBuf;

use flightdeck::app::commands::CloseAction;
use flightdeck::app::keymap::Keymap;
use flightdeck::host::{
    AgentChoice, ButtonRole, DialogButton, DialogKind, DialogRow, DialogView, NewAgentForm,
    NewAgentTarget, PaletteRow, PaletteView,
};
use flightdeck::tui::palette::CommandPalette;
use flightdeck::web::protocol::{DialogId, DialogOrigin};

fn b(id: &str, label: &str, role: ButtonRole, default: bool) -> DialogButton {
    DialogButton {
        id: id.to_string(),
        label: label.to_string(),
        role,
        default,
    }
}

fn dialog(kind: DialogKind, title: &str, buttons: Vec<DialogButton>) -> DialogView {
    DialogView {
        id: DialogId::new("d1"),
        kind,
        title: title.to_string(),
        body: Vec::new(),
        input: None,
        list: Vec::new(),
        list_filter: false,
        buttons,
        origin: DialogOrigin::Desktop,
        origin_label: None,
    }
}

fn rows(labels: &[&str], selected: usize) -> Vec<DialogRow> {
    labels
        .iter()
        .enumerate()
        .map(|(i, l)| DialogRow {
            label: l.to_string(),
            selected: i == selected,
        })
        .collect()
}

pub fn abandon(dirty: bool) -> DialogView {
    let (title, yes) = if dirty {
        (
            "The worktree has uncommitted changes. Discard them and abandon it?",
            "Abandon (force)",
        )
    } else {
        ("Abandon this worktree?", "Abandon")
    };
    dialog(
        DialogKind::ConfirmAbandon { dirty },
        title,
        vec![
            b("y", yes, ButtonRole::Destructive, false),
            b("n", "Cancel", ButtonRole::Cancel, false),
        ],
    )
}

pub fn rebase() -> DialogView {
    dialog(
        DialogKind::ConfirmRebase {
            agent_branch: "flightdeck/ui-redesign".into(),
            base_branch: "main".into(),
            drift: 3,
            primary_running: true,
        },
        "Rebase flightdeck/ui-redesign onto main (target advanced 3 commits); agent is running — \
         its HEAD will be rewritten? Rewrites history; aborts on conflict.",
        vec![
            b("y", "Rebase", ButtonRole::Destructive, false),
            b("n", "Cancel", ButtonRole::Cancel, false),
        ],
    )
}

pub fn merge() -> DialogView {
    dialog(
        DialogKind::ConfirmMerge {
            agent_branch: "flightdeck/ui-redesign".into(),
            base_branch: "main".into(),
            primary_running: false,
        },
        "Merge flightdeck/ui-redesign into main then remove the worktree?",
        vec![
            b("y", "Merge", ButtonRole::Destructive, false),
            b("n", "Cancel", ButtonRole::Cancel, false),
        ],
    )
}

pub fn close_session() -> DialogView {
    let actions = vec![
        CloseAction::CtrlCPrimary,
        CloseAction::CtrlCAll,
        CloseAction::ForceTerminate,
        CloseAction::IfAllStopped,
    ];
    dialog(
        DialogKind::CloseSession { actions },
        "Close tab — how should running processes be handled?",
        vec![
            b("1", "Ctrl-C primary", ButtonRole::Primary, false),
            b("2", "Ctrl-C all", ButtonRole::Secondary, false),
            b("3", "force terminate", ButtonRole::Destructive, false),
            b("4", "if all stopped", ButtonRole::Secondary, false),
            b("Esc", "Cancel", ButtonRole::Cancel, false),
        ],
    )
}

pub fn quit() -> DialogView {
    dialog(
        DialogKind::ConfirmQuit,
        "Quit FlightDeck? Every agent it is running is stopped.",
        vec![
            b("y", "Quit", ButtonRole::Destructive, false),
            b("n", "Cancel", ButtonRole::Cancel, false),
        ],
    )
}

pub fn rename() -> DialogView {
    let mut d = dialog(
        DialogKind::RenameSession,
        "Rename this Agent Session Tab",
        vec![
            b("Enter", "Rename", ButtonRole::Primary, true),
            b("Esc", "Cancel", ButtonRole::Cancel, false),
        ],
    );
    d.input = Some("ui-redesign".into());
    d
}

pub fn open_project() -> DialogView {
    let mut d = dialog(
        DialogKind::OpenProject {
            dir: PathBuf::from("/Users/ruud/Projects"),
        },
        "Open project — /Users/ruud/Projects   (↑↓ select · → open folder · ← parent · Enter to \
         open · or type a path)",
        vec![
            b("Enter", "Open", ButtonRole::Primary, true),
            b("Esc", "Cancel", ButtonRole::Cancel, false),
        ],
    );
    d.input = Some(String::new());
    d.list = rows(
        &[
            "flightdeck",
            "neworange-brain",
            "planet-initium",
            "todo-dashboard",
        ],
        0,
    );
    d
}

fn agents() -> Vec<AgentChoice> {
    [
        ("claude", "Claude Code"),
        ("codex", "Codex"),
        ("opencode", "OpenCode"),
        ("cursor", "Cursor"),
    ]
    .iter()
    .map(|(k, n)| AgentChoice {
        key: k.to_string(),
        name: n.to_string(),
    })
    .collect()
}

pub fn new_agent(target: NewAgentTarget, branch: &str) -> DialogView {
    let existing = [
        "flightdeck/pull-base",
        "flightdeck/rebase-worktree",
        "gui/m2-shell",
    ];
    let matching: Vec<String> = existing
        .iter()
        .filter(|b| b.contains(branch))
        .map(|b| b.to_string())
        .collect();
    let form = NewAgentForm {
        agents: agents(),
        selected_agent: 0,
        target,
        branch: branch.to_string(),
        matching_branches: matching.clone(),
        selected_branch: 0,
        has_existing_branches: true,
        base_branch: "main".into(),
    };
    let (title, body, confirm, target_label) = match target {
        NewAgentTarget::NewBranch => (
            "New Agent Session Tab — new branch   (↑/↓ agent · type task name · Tab changes target)",
            vec![],
            "Create",
            "Target: new branch".to_string(),
        ),
        NewAgentTarget::ExistingBranch => (
            "New Agent Session Tab — existing branch   (type to filter · ↑/↓ select · Tab changes \
             target)",
            vec!["Agent: Claude Code".to_string()],
            "Use branch",
            "Target: existing branch".to_string(),
        ),
        NewAgentTarget::Base => (
            "New Agent Session Tab   (↑/↓ agent · Tab changes target)",
            vec!["Runs on base branch 'main' in the project root — no worktree.".to_string()],
            "Create",
            "Target: base (main)".to_string(),
        ),
    };
    let mut d = dialog(
        DialogKind::NewAgent(form),
        title,
        vec![
            b("Enter", confirm, ButtonRole::Primary, true),
            b("Tab", &target_label, ButtonRole::Secondary, false),
            b("Esc", "Cancel", ButtonRole::Cancel, false),
        ],
    );
    d.body = body;
    d.input = (target != NewAgentTarget::Base).then(|| branch.to_string());
    d.list = if target == NewAgentTarget::ExistingBranch {
        let labels: Vec<&str> = matching.iter().map(String::as_str).collect();
        rows(&labels, 0)
    } else {
        let labels = ["(•) Claude Code", "( ) Codex", "( ) OpenCode", "( ) Cursor"];
        rows(&labels, 0)
    };
    d.list_filter = target == NewAgentTarget::ExistingBranch;
    d
}

/// The palette as the host reads it out: the TUI's own `CommandPalette`,
/// with each row's keycap from the keymap table.
pub fn palette(filter: &str) -> PaletteView {
    let mut palette = CommandPalette::new();
    palette.set_filter(filter);
    let keymap = Keymap::for_this_platform(false);
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
