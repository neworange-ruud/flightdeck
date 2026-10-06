//! The mapping from a mirrored workspace to the core's view structs, and what
//! a remote window's commands and dialog answers send.

use super::*;
use crate::contracts::{AgentTabPosition, InterpretedStatus, TabId};
use crate::host::OverlayInput;
use crate::view::AgentBadge;
use crate::web::protocol::{
    ConfirmGate, DialogChoice, DialogKey, DialogOrigin, Geometry, ProjectView, Seat, Selection,
    SessionStatus, Snapshot, StatusBucket,
};

fn terminal(session: &str, id: &str, role: TerminalRole) -> wire::TerminalView {
    wire::TerminalView {
        terminal_id: id.into(),
        session_id: TabId(session.to_string()),
        role,
        title: format!("{id} title"),
        geometry: Geometry {
            cols: 100,
            rows: 30,
        },
        byte_len: 0,
        replay_from: 0,
        alive: true,
        exit_code: None,
    }
}

fn session(id: &str, name: &str, status: InterpretedStatus, git: GitBar) -> SessionView {
    SessionView {
        session_id: TabId(id.to_string()),
        project_id: "/repo".into(),
        name: name.to_string(),
        agent: "claude".to_string(),
        agent_display_name: "Claude Code".to_string(),
        phase: SessionPhase::Ready,
        status: SessionStatus {
            interpreted: status,
            manual: None,
            bucket: StatusBucket::from_interpreted(status),
            running_time_secs: 125,
        },
        git,
        terminals: vec![
            terminal(id, &format!("{id}:primary"), TerminalRole::Primary),
            terminal(id, &format!("{id}:child:1"), TerminalRole::Shell),
            terminal(id, &format!("{id}:child:2"), TerminalRole::Agent),
        ],
        lifecycle_reporting: true,
        recovered: false,
        attached_existing_branch: false,
    }
}

fn git() -> GitBar {
    GitBar {
        branch: Some("flightdeck/login".to_string()),
        added: 1,
        modified: 2,
        removed: 0,
        ahead: 3,
        behind: 0,
        drift: 4,
        has_upstream: true,
        files_changed: 3,
        collected: true,
        upstream: Some("origin/flightdeck/login".to_string()),
        lines_added: 40,
        lines_removed: 2,
    }
}

fn workspace() -> RemoteWorkspace {
    let mut ws = RemoteWorkspace::new();
    ws.apply_snapshot(Snapshot {
        protocol_version: wire::PROTOCOL_VERSION,
        host_version: "1.0.0".to_string(),
        server_time_ms: 0,
        viewer_id: "v".into(),
        seat: Seat::Writing,
        seats: Vec::new(),
        last_input_seq: 0,
        projects: vec![ProjectView {
            project_id: "/repo".into(),
            name: "repo".to_string(),
            root: "/repo".to_string(),
            base_branch: "main".to_string(),
            dot: None,
            sessions: vec![
                session("s1", "login", InterpretedStatus::Working, git()),
                session(
                    "s2",
                    "search",
                    InterpretedStatus::WaitingForInput,
                    GitBar::default(),
                ),
            ],
        }],
        selection: Selection {
            project_id: Some("/repo".into()),
            session_id: Some(TabId("s2".to_string())),
            terminal_id: None,
            split_view: false,
        },
        geometry: Geometry {
            cols: 100,
            rows: 30,
        },
        replay_capacity_bytes: 0,
        activity: Vec::new(),
        dialog: None,
        commands: crate::web::commands::inventory(),
        help: None,
        about: None,
        update: None,
        sidebar_position: AgentTabPosition::default(),
    });
    ws
}

#[test]
fn agent_rows_carry_the_hosts_facts_and_this_clients_selection() {
    let mut ws = workspace();
    ws.select_session(&TabId("s1".to_string()));
    let rows = agent_rows(&ws);

    assert_eq!(rows.len(), 2);
    let login = &rows[0];
    assert!(login.selected, "this client's selection, not the host's");
    assert!(!rows[1].selected);
    assert_eq!(login.badge, AgentBadge::Working);
    assert_eq!(rows[1].badge, AgentBadge::WaitingAttention);
    assert_eq!(login.agent_name, "Claude Code");
    assert_eq!(login.branch, "flightdeck/login");
    assert_eq!(login.alt_index, Some(1));
    assert_eq!(login.status_since_secs, Some(125));
    let changes = login.changes.expect("collected");
    assert_eq!((changes.files, changes.lines_added), (3, 40));
    assert_eq!(
        login.upstream,
        UpstreamState::Tracking {
            upstream: "origin/flightdeck/login".to_string(),
            ahead: 3,
            behind: 0
        }
    );
    assert_eq!(rows[1].upstream, UpstreamState::Unknown, "not collected");
    assert_eq!(rows[1].changes, None);
    let labels: Vec<&str> = login.terminals.iter().map(|t| t.label.as_str()).collect();
    assert_eq!(labels, vec!["agent", "shell 1", "agent 2"]);

    assert_eq!(host_marker(&ws), Some(1), "the host is on search");
}

#[test]
fn terminal_refs_round_trip_through_wire_ids() {
    let ws = workspace();
    let session = ws.session(&TabId("s1".to_string())).unwrap();
    for target in [
        TerminalRef::Primary,
        TerminalRef::Child(0),
        TerminalRef::Child(1),
    ] {
        let id = &terminal_for(session, target).unwrap().terminal_id;
        assert_eq!(terminal_ref(session, id), Some(target));
    }
    assert!(terminal_for(session, TerminalRef::Child(2)).is_none());
}

#[test]
fn the_focused_child_follows_this_clients_terminal() {
    let mut ws = workspace();
    ws.select_terminal(&"s1:child:2".into());
    assert_eq!(focused_child(&ws), Some(1));
    ws.select_terminal(&"s1:primary".into());
    assert_eq!(focused_child(&ws), None);
}

#[test]
fn the_git_strip_names_the_selected_session_and_never_offers_pull_base() {
    let mut ws = workspace();
    ws.select_session(&TabId("s1".to_string()));
    let strip = git_strip(&ws);
    assert_eq!(strip.default_branch, "main");
    let agent = strip.agent.expect("a session is selected");
    assert_eq!(agent.name, "login");
    assert_eq!(agent.base_drift, 4);
    assert!(agent.actions.push && agent.actions.finish);
    assert!(!agent.actions.pull_base, "refused from a remote surface");
}

#[test]
fn project_tabs_roll_up_status_and_mark_this_clients_project() {
    let ws = workspace();
    let tabs = project_tabs(&ws);
    assert_eq!(tabs.len(), 1);
    assert!(tabs[0].active);
    assert_eq!(tabs[0].agent_count, 2);
    assert_eq!(tabs[0].status, ProjectStatus::NeedsAttention);
}

#[test]
fn browsing_stays_local_and_commands_name_their_target() {
    let mut ws = workspace();
    ws.select_terminal(&"s1:child:1".into());

    assert_eq!(
        route_command(&Command::SwitchAgentTab(Selector::Next)),
        Some(RemoteAction::Local(LocalAction::Session(Selector::Next)))
    );
    assert_eq!(
        route_palette(&PaletteAction::SwitchProjectNext),
        RemoteAction::Local(LocalAction::Project(Selector::Next))
    );
    assert!(matches!(
        route_command(&Command::ToggleSplitView),
        Some(RemoteAction::Unavailable(_))
    ));

    let RemoteAction::Send { name, scope } =
        route_palette(&PaletteAction::Dispatch(Command::RestartAgent))
    else {
        panic!("restart is sent");
    };
    assert_eq!(name, names::RESTART_AGENT);
    assert_eq!(
        target(&ws, scope),
        Some(Target::Session(TabId("s1".to_string())))
    );

    let RemoteAction::Send { name, scope } =
        route_palette(&PaletteAction::Dispatch(Command::CloseChildTerminal))
    else {
        panic!("close child is sent");
    };
    assert_eq!(name, names::CLOSE_CHILD_TERMINAL);
    assert_eq!(
        target(&ws, scope),
        Some(Target::Terminal("s1:child:1".into()))
    );

    let Some(RemoteAction::Send { name, scope }) = route_command(&Command::NewAgentTab {
        name: String::new(),
        agent_key: None,
    }) else {
        panic!("new agent is sent");
    };
    assert_eq!(name, names::NEW_AGENT_SESSION_TAB);
    assert_eq!(target(&ws, scope), Some(Target::Project("/repo".into())));
}

#[test]
fn the_palette_offers_what_the_host_forwards_and_filters_like_the_local_one() {
    let ws = workspace();
    let labels: Vec<&str> = palette_entries(&ws).iter().map(|e| e.label).collect();
    assert!(labels.contains(&"Restart Agent"));
    assert!(labels.contains(&"Push Branch"));
    assert!(
        labels.contains(&"Switch Agent Session Tab"),
        "local browsing"
    );
    assert!(
        !labels.contains(&"Pull Base"),
        "the host refuses it remotely"
    );
    assert!(!labels.contains(&"Toggle Split View"));
    assert!(!labels.contains(&"Pair Phone"));
    assert!(!labels.contains(&"Quit"), "never the host's quit");
    assert!(
        !labels.contains(&"Open Worktree in File Manager"),
        "host only"
    );

    let view = palette_view(&ws, "push", 9);
    assert_eq!(view.entries.len(), 1);
    assert_eq!(view.entries[0].label, "Push Branch");
    assert_eq!(view.selected, 0, "clamped to the filtered list");
}

fn dialog(body: DialogBody) -> wire::DialogView {
    wire::DialogView {
        dialog_id: "dialog-7".into(),
        kind: "confirm_rebase".to_string(),
        title: "Rebase flightdeck/login onto main?\nRewrites history; aborts on conflict."
            .to_string(),
        origin: DialogOrigin::Desktop,
        body: Some(serde_json::to_value(body).unwrap()),
    }
}

fn keys(list: &[(&str, &str, bool)]) -> Vec<DialogKey> {
    list.iter()
        .map(|(key, label, cancels)| DialogKey {
            key: key.to_string(),
            label: label.to_string(),
            cancels: *cancels,
        })
        .collect()
}

#[test]
fn a_gated_dialog_asks_for_the_name_and_sends_it_with_the_gated_button() {
    let view = dialog(DialogBody {
        buttons: keys(&[("y", "Rebase", false), ("n", "Cancel", true)]),
        confirmable: true,
        confirm_gate: Some(ConfirmGate {
            key: "y".to_string(),
            expected: "login".to_string(),
            instruction: "This browser is remote. Type the session name.".to_string(),
        }),
        ..DialogBody::default()
    });
    let mut draft = DialogDraft::new(&view);
    let shown = dialog_overlay(&view, &draft);
    assert_eq!(shown.title, "Rebase flightdeck/login onto main?");
    assert!(shown.body[0].contains("Rewrites history"));
    assert!(shown.body.iter().any(|l| l.contains("`login`")));
    assert_eq!(shown.input.as_deref(), Some(""), "a field for the name");
    assert_eq!(shown.buttons[0].role, ButtonRole::Destructive);
    assert_eq!(shown.buttons[1].role, ButtonRole::Cancel);

    assert!(dialog_input(&view, &mut draft, &OverlayInput::SetText("login".into())).is_none());
    let reply = dialog_input(&view, &mut draft, &OverlayInput::Choose("y".into())).unwrap();
    assert_eq!(reply.name, names::DIALOG_CONFIRM);
    assert_eq!(
        reply.args,
        serde_json::json!({ "dialog_id": "dialog-7", "choice": "y", "confirm_name": "login" })
    );

    let reply = dialog_input(&view, &mut draft, &OverlayInput::Choose("n".into())).unwrap();
    assert_eq!(reply.name, names::DIALOG_CANCEL);
}

#[test]
fn a_form_sends_its_draft_text_and_list_choice() {
    let view = dialog(DialogBody {
        input: Some("draft".to_string()),
        list: vec![
            DialogChoice {
                label: "Claude".to_string(),
                selected: true,
            },
            DialogChoice {
                label: "Codex".to_string(),
                selected: false,
            },
        ],
        buttons: keys(&[("Enter", "Start", false), ("Esc", "Cancel", true)]),
        confirmable: true,
        ..DialogBody::default()
    });
    let mut draft = DialogDraft::new(&view);
    assert_eq!(draft.list_index, Some(0));
    dialog_input(
        &view,
        &mut draft,
        &OverlayInput::SetText("fix login".into()),
    );
    dialog_input(&view, &mut draft, &OverlayInput::Key(OverlayKey::Down));
    let shown = dialog_overlay(&view, &draft);
    assert!(shown.list[1].selected && !shown.list[0].selected);
    assert!(shown.buttons[0].default, "Enter is the default");

    let reply = dialog_input(&view, &mut draft, &OverlayInput::Submit).unwrap();
    assert_eq!(
        reply.args,
        serde_json::json!({
            "dialog_id": "dialog-7",
            "choice": "Enter",
            "text": "fix login",
            "list_index": 1
        })
    );
    assert_eq!(
        dialog_input(&view, &mut draft, &OverlayInput::Cancel).map(|r| r.name),
        Some(names::DIALOG_CANCEL)
    );
}

#[test]
fn a_dialog_the_host_will_not_let_us_confirm_offers_only_cancel() {
    let view = dialog(DialogBody {
        buttons: keys(&[("y", "Abandon", false), ("n", "Cancel", true)]),
        confirmable: false,
        refusal: Some("The session it asked about is gone.".to_string()),
        ..DialogBody::default()
    });
    let shown = dialog_overlay(&view, &DialogDraft::new(&view));
    assert_eq!(shown.buttons.len(), 1);
    assert_eq!(shown.buttons[0].role, ButtonRole::Cancel);
    assert!(shown.body.iter().any(|l| l.contains("is gone")));
}

fn agent_radio(selected: usize) -> wire::DialogView {
    let mut view = dialog(DialogBody {
        input: Some(String::new()),
        list: ["Claude Code", "Codex CLI", "OpenCode"]
            .iter()
            .enumerate()
            .map(|(i, name)| DialogChoice {
                label: format!("{} {name}", if i == selected { "(•)" } else { "( )" }),
                selected: i == selected,
            })
            .collect(),
        buttons: keys(&[
            ("Enter", "Create", false),
            ("Esc", "Cancel", true),
            ("Tab", "Target: new branch", false),
        ]),
        confirmable: true,
        ..DialogBody::default()
    });
    view.kind = "new_agent".to_string();
    view
}

#[test]
fn the_agent_radio_marks_this_windows_choice_not_the_hosts() {
    let view = agent_radio(2);
    let mut draft = DialogDraft::new(&view);
    dialog_input(&view, &mut draft, &OverlayInput::Key(OverlayKey::Up));
    dialog_input(&view, &mut draft, &OverlayInput::Key(OverlayKey::Up));
    let labels: Vec<_> = dialog_overlay(&view, &draft)
        .list
        .into_iter()
        .map(|row| row.label)
        .collect();
    assert_eq!(labels, ["(•) Claude Code", "( ) Codex CLI", "( ) OpenCode"]);
}

#[test]
fn a_dialog_changed_in_place_replaces_the_draft_and_the_same_one_keeps_it() {
    let view = agent_radio(0);
    let mut draft = DialogDraft::new(&view);
    dialog_input(
        &view,
        &mut draft,
        &OverlayInput::SetText("fix login".into()),
    );
    dialog_input(&view, &mut draft, &OverlayInput::Key(OverlayKey::Down));
    assert!(!draft.follow(&view.clone()), "re-sent unchanged");
    assert_eq!(draft.text.as_deref(), Some("fix login"));

    // The host took the answer's `Tab`: base target, Codex chosen, no field.
    let mut moved = agent_radio(1);
    let mut body = dialog_body(&moved);
    body.input = None;
    moved.title = "New Agent Session Tab\nRuns on base branch 'main'.".to_string();
    moved.body = Some(serde_json::to_value(body).unwrap());
    assert!(draft.follow(&moved));
    assert_eq!(draft.text, None, "no field to send text into");
    assert_eq!(draft.list_index, Some(1));
    assert!(dialog_overlay(&moved, &draft).body[0].contains("base branch"));
}
