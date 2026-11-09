//! [`RemoteSession`] against a scripted link: no socket, no front-end. The
//! Desktop's remote-window tests drive the same model through GPUI; these
//! pin the rules the terminal UI's `flightdeck connect` relies on.

use super::*;
use crate::contracts::{AgentTabPosition, InterpretedStatus};
use crate::testing::FakeNotifier;
use crate::web::client::{LinkEvent, LinkOut, Script};
use crate::web::protocol::{
    ClientMsg, Geometry, GitBar, HostMessage, ProjectView, Seat, Selection, SessionPhase,
    SessionStatus, SessionView, Snapshot, StatusBucket, TerminalView, WorkerSnapshot,
    PROTOCOL_VERSION,
};

fn terminal(session: &str, id: &str, role: TerminalRole) -> TerminalView {
    TerminalView {
        terminal_id: id.into(),
        session_id: TabId(session.to_string()),
        role,
        title: "claude".to_string(),
        geometry: Geometry { cols: 80, rows: 24 },
        byte_len: 0,
        replay_from: 0,
        alive: true,
        exit_code: None,
    }
}

fn session(id: &str, name: &str) -> SessionView {
    SessionView {
        session_id: TabId(id.to_string()),
        project_id: "/repo".into(),
        name: name.to_string(),
        agent: "claude".to_string(),
        agent_display_name: "Claude Code".to_string(),
        phase: SessionPhase::Ready,
        status: SessionStatus {
            interpreted: InterpretedStatus::Working,
            manual: None,
            bucket: StatusBucket::InProgress,
            running_time_secs: 30,
        },
        git: GitBar::default(),
        terminals: vec![terminal(
            id,
            &format!("{id}:primary"),
            TerminalRole::Primary,
        )],
        lifecycle_reporting: true,
        recovered: false,
        attached_existing_branch: false,
        worktree_path: format!("/repo/.flightdeck/worktrees/{name}"),
    }
}

/// One project with two sessions, the host looking at the second; a worker
/// when `worker`, with `messages` kept for the next attach.
fn snapshot(worker: bool, messages: Vec<HostMessage>) -> Snapshot {
    Snapshot {
        protocol_version: PROTOCOL_VERSION,
        host_version: "1.0.0".to_string(),
        server_time_ms: 0,
        viewer_id: "viewer-1".into(),
        seat: Seat::Writing,
        seats: Vec::new(),
        last_input_seq: 0,
        projects: vec![ProjectView {
            project_id: "/repo".into(),
            name: "repo".to_string(),
            root: "/repo".to_string(),
            base_branch: "main".to_string(),
            dot: None,
            sessions: vec![session("s1", "login"), session("s2", "search")],
        }],
        selection: Selection {
            project_id: Some("/repo".into()),
            session_id: Some(TabId("s2".to_string())),
            terminal_id: Some("s2:primary".into()),
            split_view: false,
        },
        geometry: Geometry { cols: 80, rows: 24 },
        replay_capacity_bytes: 65_536,
        activity: Vec::new(),
        dialog: None,
        commands: crate::web::commands::inventory(),
        help: None,
        about: None,
        update: None,
        sidebar_position: AgentTabPosition::default(),
        host_machine: None,
        host_kind: worker.then_some(HostKind::Worker),
        worker: worker.then(|| WorkerSnapshot {
            home: "/home/dev".to_string(),
            shell: None,
            health: None,
            status: Default::default(),
            connections: Default::default(),
            host_messages: messages,
        }),
    }
}

fn open(snapshot: Snapshot) -> (RemoteSession, Script) {
    let (client, script) = RemoteClient::scripted();
    let mut session = RemoteSession::new(
        client,
        "studio".to_string(),
        "100.64.0.2:7420".to_string(),
        TerminalProfile::TUI,
    );
    session.set_notifier(Rc::new(FakeNotifier::new()));
    script.snapshot(snapshot);
    session.pump();
    (session, script)
}

fn frames(script: &mut Script) -> Vec<ClientMsg> {
    script
        .sent()
        .into_iter()
        .filter_map(LinkOut::into_frame)
        .collect()
}

#[test]
fn keys_go_to_the_terminal_on_screen_by_its_wire_id() {
    let (mut session, mut script) = open(snapshot(false, Vec::new()));
    script.sent();
    assert!(
        session.active_terminal().is_some(),
        "s2's primary is mirrored"
    );
    session.apply(HostEvent::TerminalInput(b"ls\r".to_vec()));
    let inputs: Vec<(String, Vec<u8>)> = frames(&mut script)
        .into_iter()
        .filter_map(|f| match f {
            ClientMsg::Input(i) => Some((i.terminal_id.as_str().to_string(), i.data)),
            _ => None,
        })
        .collect();
    assert_eq!(inputs, vec![("s2:primary".to_string(), b"ls\r".to_vec())]);
}

#[test]
fn browsing_is_local_and_sends_nothing() {
    let (mut session, mut script) = open(snapshot(false, Vec::new()));
    script.sent();
    session.select_agent(0);
    assert_eq!(
        session
            .workspace()
            .selected_session()
            .map(|s| s.name.as_str()),
        Some("login")
    );
    assert_eq!(session.mode(), InputMode::App);
    assert!(frames(&mut script).is_empty());
}

#[test]
fn a_worker_is_sent_the_pane_size_once_per_size() {
    let (mut session, mut script) = open(snapshot(true, Vec::new()));
    script.sent();
    let size = PtySize {
        rows: 40,
        cols: 120,
    };
    session.set_viewport(size);
    session.pump();
    session.pump();
    let resizes: Vec<_> = frames(&mut script)
        .into_iter()
        .filter_map(|f| match f {
            ClientMsg::ResizeTerminal(r) => Some((r.terminal_id.as_str().to_string(), r.cols)),
            _ => None,
        })
        .collect();
    assert_eq!(resizes, vec![("s2:primary".to_string(), 120)]);
}

#[test]
fn any_other_host_keeps_its_own_geometry() {
    let (mut session, mut script) = open(snapshot(false, Vec::new()));
    script.sent();
    session.set_viewport(PtySize {
        rows: 40,
        cols: 120,
    });
    session.pump();
    assert!(!frames(&mut script)
        .iter()
        .any(|f| matches!(f, ClientMsg::ResizeTerminal(_))));
}

#[test]
fn messages_kept_while_away_come_back_as_one_overlay() {
    let kept = vec![HostMessage {
        id: 1,
        at_ms: 0,
        level: MessageLevel::Error,
        text: "Clone failed: repository not found".to_string(),
    }];
    let (session, _script) = open(snapshot(true, kept));
    match session.overlay() {
        Some(OverlayView::Message(m)) => assert_eq!(
            m.text,
            "1 message while you were away\nError: Clone failed: repository not found"
        ),
        other => panic!("expected the away overlay, got {other:?}"),
    }
}

#[test]
fn front_end_palette_rows_are_handed_back_not_sent() {
    let (mut session, mut script) = open(snapshot(false, Vec::new()));
    script.sent();
    session.apply(HostEvent::RunPaletteAction(
        crate::tui::palette::PaletteAction::FrontEnd(FrontEndAction::OpenInVsCode),
    ));
    assert_eq!(
        session.take_front_end_actions(),
        vec![FrontEndAction::OpenInVsCode]
    );
    assert!(frames(&mut script).is_empty());
}

#[test]
fn a_takeover_ends_the_link_and_reconnect_dials_again() {
    let (mut session, script) = open(snapshot(true, Vec::new()));
    script.send(LinkEvent::State(LinkState::Ended(LinkEnd::TakenOver {
        by: "laptop".to_string(),
    })));
    session.pump();
    assert_eq!(session.link_label(), "taken over by laptop");
    assert!(session.ended().is_some());

    let dialled = Rc::new(std::cell::Cell::new(0));
    let count = dialled.clone();
    session.set_dialer(Rc::new(move || {
        count.set(count.get() + 1);
        RemoteClient::scripted().0
    }));
    session.reconnect();
    assert_eq!(dialled.get(), 1);
    assert!(session.ended().is_none());
    assert!(session.active_terminal().is_none(), "mirrors start over");
}
