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

pub(crate) fn terminal(session: &str, id: &str, role: TerminalRole) -> TerminalView {
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

pub(crate) fn session(id: &str, name: &str) -> SessionView {
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
        activity: None,
        prompt: None,
    }
}

/// One project with two sessions, the host looking at the second; a worker
/// when `worker`, with `messages` kept for the next attach.
pub(crate) fn snapshot(worker: bool, messages: Vec<HostMessage>) -> Snapshot {
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

// -- Mission control (`remote-control-pgy3`) -----------------------------------

/// The commands sent so far, as `(seq, name, args)`.
fn commands_sent(script: &mut Script) -> Vec<(u64, String, Option<serde_json::Value>)> {
    frames(script)
        .into_iter()
        .filter_map(|f| match f {
            ClientMsg::Command(c) => Some((c.seq, c.name, c.args)),
            _ => None,
        })
        .collect()
}

fn selected_tab(session: &RemoteSession) -> Option<String> {
    session.mission_selection().map(|k| k.tab_id)
}

#[test]
fn mission_control_browses_this_front_ends_selection_and_sends_nothing() {
    use crate::app::commands::Command as AppCommand;
    let (mut session, mut script) = open(snapshot(false, Vec::new()));
    script.sent();
    session.apply(HostEvent::FocusTerminal);
    assert_eq!(session.mode(), InputMode::Terminal);
    session.toggle_main_view();
    assert_eq!(session.main_view(), MainView::Mission);
    assert_eq!(session.mode(), InputMode::App, "it lands on the grid");
    // Both sessions are working: tiles in name order, `search` (the host's
    // selection, and so this front-end's first) already on one.
    let view = session.mission_view();
    let names: Vec<&str> = view.tiles.iter().map(|t| t.row.name.as_str()).collect();
    assert_eq!(names, ["login", "search"]);
    assert_eq!(selected_tab(&session).as_deref(), Some("s2"));

    // Left steps back a tile, in this front-end only.
    let left = Action::Dispatch(AppCommand::SwitchChildTerminal(Selector::Prev));
    assert!(session.mission_move(&left));
    assert_eq!(selected_tab(&session).as_deref(), Some("s1"));
    // Enter on a tile is FocusTerminal's own: not a move.
    assert!(!session.mission_move(&Action::FocusTerminal));
    session.apply(HostEvent::FocusTerminal);
    assert_eq!(session.mode(), InputMode::Terminal, "the focus view");
    // Alt-2 swaps the open session, staying in the focus view.
    let alt_2 = Action::Dispatch(AppCommand::SwitchAgentTab(Selector::Index(1)));
    assert!(session.mission_move(&alt_2));
    assert_eq!(selected_tab(&session).as_deref(), Some("s2"));
    assert_eq!(session.mode(), InputMode::Terminal);
    assert!(session.session_terminal(&view.tiles[0].key).is_some());
    // Back to the grid, then to Projects on the same session.
    session.apply(HostEvent::FocusApp);
    session.toggle_main_view();
    assert_eq!(session.main_view(), MainView::Projects);
    assert_eq!(
        session
            .workspace()
            .selected_session()
            .map(|s| s.name.as_str()),
        Some("search")
    );
    assert!(
        commands_sent(&mut script).is_empty(),
        "browsing never reaches the host"
    );
}

#[test]
fn the_view_and_scope_are_this_front_ends_and_outlive_a_reconnect() {
    let (mut session, _script) = open(snapshot(false, Vec::new()));
    session.set_dialer(Rc::new(|| RemoteClient::scripted().0));
    let scope = MissionScope {
        window: crate::persistence::workspace::RecentScope::Week,
        project: Some("/repo".to_string()),
    };
    session.set_mission_scope(scope.clone());
    session.set_main_view(MainView::Mission);
    // From the SSH shell's tab, asking for the view it already has shows it.
    session.show_ssh_shell();
    assert_eq!(session.tab(), Tab::SshShell);
    session.set_main_view(MainView::Mission);
    assert_eq!(session.tab(), Tab::Project);
    session.reconnect();
    assert_eq!(session.mission_scope(), &scope);
    assert_eq!(session.main_view(), MainView::Mission);
}

#[test]
fn a_tiles_answer_goes_to_the_host_and_a_refusal_offers_it_again() {
    use crate::view::{PromptAnswer, PromptShape};
    use crate::web::protocol::{
        Ack, AckOutcome, PendingPrompt, PromptAnswerView, PromptButtonView, PromptShapeView,
    };
    let mut snap = snapshot(false, Vec::new());
    let waiting = &mut snap.projects[0].sessions[0];
    waiting.status.interpreted = InterpretedStatus::WaitingForInput;
    waiting.status.bucket = StatusBucket::Waiting;
    waiting.prompt = Some(Box::new(PendingPrompt {
        prompt_id: "p1".to_string(),
        shape: PromptShapeView::Binary,
        text: "Run it?".to_string(),
        buttons: vec![
            PromptButtonView {
                label: "Approve".to_string(),
                answer: PromptAnswerView::Approve,
            },
            PromptButtonView {
                label: "Deny".to_string(),
                answer: PromptAnswerView::Deny,
            },
        ],
    }));
    let (mut session, mut script) = open(snap);
    script.sent();
    let tile = session.mission_view().tiles[0].clone();
    assert_eq!(tile.row.name, "login");
    let prompt = tile.prompt.clone().expect("the host's prompt");
    assert_eq!(prompt.shape, PromptShape::Binary);

    let deny = PromptReply {
        key: tile.key.clone(),
        prompt_id: prompt.prompt_id.clone(),
        answer: PromptAnswer::Deny,
    };
    session.apply(HostEvent::AnswerPrompt(deny.clone()));
    let sent = commands_sent(&mut script);
    assert_eq!(sent.len(), 1);
    let (seq, name, args) = sent[0].clone();
    assert_eq!(name, names::ANSWER_PROMPT);
    assert_eq!(
        args,
        Some(serde_json::json!({
            "session_id": "s1",
            "prompt_id": "p1",
            "answer": { "kind": "deny" },
        }))
    );
    assert_eq!(session.mission_view().tiles[0].prompt, None, "answered");
    // A second click before the host answers sends nothing more.
    session.apply(HostEvent::AnswerPrompt(deny));
    assert!(commands_sent(&mut script).is_empty());

    // The host refuses (someone else is typing): the buttons come back, and
    // the reason is shown.
    script.send(LinkEvent::Ack(
        Ack {
            seq,
            outcome: AckOutcome::Rejected,
            detail: Some("desktop is typing on the host — the answer was not sent.".into()),
        },
        crate::web::client::AckFor::Command,
    ));
    session.pump();
    assert!(session.mission_view().tiles[0].prompt.is_some());
    assert!(matches!(
        session.overlay(),
        Some(OverlayView::Message(m)) if m.text.contains("typing")
    ));
}

// -- uploads (v8, `remote-control-usem`) ----------------------------------------

#[test]
fn the_upload_panel_opens_only_on_a_worker_that_takes_files() {
    let (mut session, _script) = open(snapshot(false, Vec::new()));
    session.open_upload();
    assert!(session.upload().is_none());
    assert_eq!(session.notice(), Some(UPLOAD_NOT_A_WORKER));

    let mut v7 = snapshot(true, Vec::new());
    v7.protocol_version = 7;
    let (mut session, _script) = open(v7);
    session.open_upload();
    assert!(session.upload().is_none());
    assert_eq!(session.notice(), Some(UPLOAD_WORKER_TOO_OLD));

    let (mut session, _script) = open(snapshot(true, Vec::new()));
    session.open_upload();
    let panel = session.upload().expect("the panel opens");
    assert_eq!(
        panel.dir, "/repo/.flightdeck/worktrees/search",
        "aimed at the session on screen"
    );
}

#[test]
fn an_upload_asks_the_worker_and_folds_in_its_answer() {
    use crate::web::client::upload::UploadPhase;
    use crate::web::protocol::{worker_command, UploadPlan, UploadPlanFile};

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("notes.md");
    std::fs::write(&file, "hello").unwrap();

    let (mut session, mut script) = open(snapshot(true, Vec::new()));
    script.sent();
    session.open_upload_with(&[file.clone(), dir.path().to_path_buf()]);
    let panel = session.upload().unwrap();
    assert_eq!(panel.files.len(), 1, "the folder is refused");
    assert!(panel.note.as_deref().unwrap().contains("Folders"));

    session.start_upload();
    let sent = commands_sent(&mut script);
    let [(seq, name, Some(args))] = sent.as_slice() else {
        panic!("one begin_upload: {sent:?}");
    };
    assert_eq!(name, worker_command::BEGIN_UPLOAD);
    assert_eq!(args["dir"], "/repo/.flightdeck/worktrees/search");
    assert_eq!(args["files"][0]["name"], "notes.md");
    assert_eq!(args["files"][0]["size"], 5);
    assert_eq!(args["on_conflict"], "ask");

    // The worker grants it. This scripted client has nowhere to send bytes,
    // so the file fails — in words, without a panic or a stuck panel.
    script.send(LinkEvent::UploadPlan(UploadPlan {
        seq: *seq,
        dir: "/repo/.flightdeck/worktrees/search".into(),
        files: vec![UploadPlanFile {
            name: "notes.md".into(),
            final_name: "notes.md".into(),
            ticket: Some("t".into()),
            exists: false,
        }],
        conflicts: false,
        refusal: None,
    }));
    session.pump();
    let panel = session.upload().unwrap();
    assert!(
        matches!(&panel.phase, UploadPhase::Done { failed, .. }
            if failed == &vec![("notes.md".to_string(), UPLOAD_NO_ACCESS.to_string())]),
        "{:?}",
        panel.phase
    );
    session.close_upload();
    assert!(session.upload().is_none());
}

#[test]
fn a_refused_upload_is_the_panels_note_not_a_message_overlay() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a.txt");
    std::fs::write(&file, "x").unwrap();
    let (mut session, mut script) = open(snapshot(true, Vec::new()));
    script.sent();
    session.open_upload_with(&[file]);
    session.start_upload();
    let (seq, _, _) = commands_sent(&mut script).remove(0);
    script.send(LinkEvent::Ack(
        crate::web::protocol::Ack {
            seq,
            outcome: AckOutcome::Rejected,
            detail: Some("~/x is outside your home folder.".into()),
        },
        crate::web::client::AckFor::Command,
    ));
    session.pump();
    let panel = session.upload().unwrap();
    assert!(panel.is_choosing());
    assert_eq!(
        panel.note.as_deref(),
        Some("~/x is outside your home folder.")
    );
    assert!(
        !matches!(session.overlay(), Some(OverlayView::Message(_))),
        "the panel says it"
    );
}

#[test]
fn the_folder_picker_lists_the_folder_asked_for_and_scans_nothing() {
    let (mut session, mut script) = open(snapshot(true, Vec::new()));
    script.sent();
    session.open_folder_picker("/repo/.flightdeck/worktrees/search");
    let sent = commands_sent(&mut script);
    let names: Vec<&str> = sent.iter().map(|(_, name, _)| name.as_str()).collect();
    assert_eq!(names, ["browse_dir"], "no scan: it chooses a folder");
    assert_eq!(
        sent[0].2.as_ref().unwrap()["path"],
        "/repo/.flightdeck/worktrees/search"
    );
    let picker = session.picker().expect("the picker is open");
    assert_eq!(picker.home, "/home/dev");
    assert!(picker.scanned.is_none());

    // `~` or nothing lists the worker's home.
    for start in ["~", "  "] {
        session.open_folder_picker(start);
        let sent = commands_sent(&mut script);
        assert!(
            sent[0].2.as_ref().unwrap().get("path").is_none(),
            "{start:?}"
        );
    }
}

// -- downloads (v9, `remote-control-pz6d`) ---------------------------------------

mod downloads {
    use super::*;
    use crate::web::client::download::{DownloadPhase, NOT_GRANTED};
    use crate::web::protocol::{
        worker_command, Ack, DirListing, DownloadPlan, DownloadPlanFile, FileEntryView,
        DOWNLOAD_MIN_VERSION,
    };

    const START: &str = "/repo/.flightdeck/worktrees/search";

    fn listing(seq: u64, path: &str, files: &[&str]) -> LinkEvent {
        LinkEvent::DirListing(DirListing {
            seq,
            path: path.to_string(),
            parent: Some("/home/dev".to_string()),
            home: "/home/dev".to_string(),
            entries: Vec::new(),
            files: files
                .iter()
                .map(|name| FileEntryView {
                    name: name.to_string(),
                    path: format!("{path}/{name}"),
                    size: 3,
                })
                .collect(),
            truncated: false,
        })
    }

    /// A v9 worker's session with the Download tab open on [`START`], its
    /// listing in (`a.txt`, `b.txt`), saving into a fresh folder here.
    fn browsing() -> (RemoteSession, Script, tempfile::TempDir) {
        let (mut session, mut script) = open(snapshot(true, Vec::new()));
        script.sent();
        session.open_download();
        let sent = commands_sent(&mut script);
        let [(seq, name, Some(args))] = sent.as_slice() else {
            panic!("one browse: {sent:?}");
        };
        assert_eq!(name, worker_command::BROWSE_DIR);
        assert_eq!(args["path"], START);
        assert_eq!(args["include_files"], true);
        script.send(listing(*seq, START, &["a.txt", "b.txt"]));
        session.pump();
        let dest = tempfile::tempdir().unwrap();
        session.set_download_dest(&dest.path().display().to_string());
        (session, script, dest)
    }

    #[test]
    fn the_download_tab_opens_only_on_a_worker_that_gives_files() {
        let (mut session, _script) = open(snapshot(false, Vec::new()));
        session.open_download();
        assert!(session.download().is_none());
        assert_eq!(session.transfer_tab(), None);
        assert_eq!(session.notice(), Some(UPLOAD_NOT_A_WORKER));

        // A v8 worker takes uploads but gives no downloads: the panel opens
        // on Upload, and its Download tab is refused.
        let mut v8 = snapshot(true, Vec::new());
        v8.protocol_version = DOWNLOAD_MIN_VERSION - 1;
        let (mut session, mut script) = open(v8);
        script.sent();
        session.open_download();
        assert!(session.download().is_none());
        assert_eq!(session.notice(), Some(DOWNLOAD_WORKER_TOO_OLD));
        session.open_transfer(Some(TransferTab::Download));
        assert_eq!(session.transfer_tab(), Some(TransferTab::Upload));
        assert!(!session.set_transfer_tab(TransferTab::Download));
        assert!(
            commands_sent(&mut script).is_empty(),
            "nothing asked of a v8 worker"
        );
    }

    #[test]
    fn the_panel_opens_on_the_tab_it_was_last_on() {
        let (mut session, _script) = open(snapshot(true, Vec::new()));
        session.open_transfer(None);
        assert_eq!(session.transfer_tab(), Some(TransferTab::Upload));
        assert!(session.set_transfer_tab(TransferTab::Download));
        assert_eq!(session.transfer_tab(), Some(TransferTab::Download));
        session.close_transfer();
        assert_eq!(session.transfer_tab(), None);
        assert!(session.upload().is_none() && session.download().is_none());
        session.open_transfer(None);
        assert_eq!(session.transfer_tab(), Some(TransferTab::Download));
    }

    #[test]
    fn a_listing_with_files_is_the_download_tabs_not_the_pickers() {
        let (mut session, mut script, _dest) = browsing();
        let panel = session.download().unwrap();
        assert_eq!(panel.cwd, START);
        assert_eq!(panel.files.len(), 2);
        assert!(panel.dest.len() > 1);

        // The picker open meanwhile keeps its own listing.
        session.open_folder_picker("~");
        let picker_seq = commands_sent(&mut script)[0].0;
        session.download_up();
        let up_seq = commands_sent(&mut script)[0].0;
        script.send(listing(picker_seq, "/home/dev", &[]));
        session.pump();
        assert_eq!(session.picker().unwrap().cwd.as_deref(), Some("/home/dev"));
        assert_eq!(session.download().unwrap().cwd, START, "not the picker's");
        script.send(listing(up_seq, "/home/dev", &["c.txt"]));
        session.pump();
        assert_eq!(session.download().unwrap().cwd, "/home/dev");
    }

    #[test]
    fn hidden_home_and_up_browse_with_files() {
        let (mut session, mut script, _dest) = browsing();
        session.download_toggle_hidden();
        session.download_home();
        let sent = commands_sent(&mut script);
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[0].2.as_ref().unwrap()["path"], START);
        assert_eq!(sent[0].2.as_ref().unwrap()["show_hidden"], true);
        assert!(sent[1].2.as_ref().unwrap().get("path").is_none(), "home");
        assert_eq!(sent[1].2.as_ref().unwrap()["include_files"], true);
    }

    #[test]
    fn a_name_already_here_is_asked_about_and_keep_both_asks_the_worker() {
        let (mut session, mut script, dest) = browsing();
        std::fs::write(dest.path().join("a.txt"), "mine").unwrap();
        session.download_select_all();
        session.start_download();
        assert!(
            matches!(&session.download().unwrap().phase,
                DownloadPhase::Asking { clashes, .. } if clashes == &["a.txt".to_string()]),
            "{:?}",
            session.download().unwrap().phase
        );
        assert!(commands_sent(&mut script).is_empty(), "nothing asked yet");

        session.resolve_download(Resolution::KeepBoth);
        let sent = commands_sent(&mut script);
        let [(seq, name, Some(args))] = sent.as_slice() else {
            panic!("one begin_download: {sent:?}");
        };
        assert_eq!(name, worker_command::BEGIN_DOWNLOAD);
        assert_eq!(args["dir"], START);
        assert_eq!(args["names"], serde_json::json!(["a.txt", "b.txt"]));
        let DownloadPhase::Requested { planned, .. } = &session.download().unwrap().phase else {
            panic!()
        };
        assert_eq!(planned[0].final_name, "a (1).txt");

        // While the worker is asked, the tab does not change.
        assert!(session.transfer_busy());
        assert!(!session.set_transfer_tab(TransferTab::Upload));
        assert_eq!(session.transfer_tab(), Some(TransferTab::Download));

        // Granted: this scripted client has nowhere to fetch from, so each
        // file fails in words — and the next is still tried.
        script.send(LinkEvent::DownloadPlan(DownloadPlan {
            seq: *seq,
            dir: START.to_string(),
            files: vec![DownloadPlanFile {
                name: "a.txt".into(),
                size: 3,
                ticket: Some("t-a".into()),
            }],
            refusal: None,
        }));
        session.pump();
        let DownloadPhase::Done { saved, failed, .. } = &session.download().unwrap().phase else {
            panic!("{:?}", session.download().unwrap().phase)
        };
        assert!(saved.is_empty());
        assert_eq!(
            failed,
            &[
                ("a.txt".to_string(), DOWNLOAD_NO_ACCESS.to_string()),
                ("b.txt".to_string(), NOT_GRANTED.to_string()),
            ]
        );
        assert_eq!(
            std::fs::read_to_string(dest.path().join("a.txt")).unwrap(),
            "mine",
            "left alone"
        );
        assert!(session.set_transfer_tab(TransferTab::Upload), "free again");

        // Download more: browsing the same folder afresh.
        session.set_transfer_tab(TransferTab::Download);
        session.download_more();
        assert!(session.download().unwrap().is_browsing());
        assert_eq!(commands_sent(&mut script)[0].1, worker_command::BROWSE_DIR);
    }

    #[test]
    fn a_refused_download_is_the_tabs_note_not_a_message_overlay() {
        let (mut session, mut script, _dest) = browsing();
        session.download_toggle("b.txt");
        session.start_download();
        let (seq, _, _) = commands_sent(&mut script).remove(0);
        script.send(LinkEvent::Ack(
            Ack {
                seq,
                outcome: AckOutcome::Rejected,
                detail: Some("b.txt is not in your home folder.".into()),
            },
            crate::web::client::AckFor::Command,
        ));
        session.pump();
        let panel = session.download().unwrap();
        assert!(panel.is_browsing());
        assert_eq!(
            panel.note.as_deref(),
            Some("b.txt is not in your home folder.")
        );
        assert!(!matches!(session.overlay(), Some(OverlayView::Message(_))));
    }

    #[test]
    fn a_destination_that_is_not_a_folder_here_sends_nothing() {
        let (mut session, mut script, dest) = browsing();
        session.set_download_dest(&dest.path().join("nope").display().to_string());
        session.download_toggle("a.txt");
        session.start_download();
        assert!(commands_sent(&mut script).is_empty());
        assert_eq!(
            session.download().unwrap().note.as_deref(),
            Some(crate::web::client::download::NO_DESTINATION)
        );
    }

    #[test]
    fn cancel_and_close_stop_everything() {
        let (mut session, mut script, _dest) = browsing();
        session.download_toggle("a.txt");
        session.start_download();
        assert_eq!(commands_sent(&mut script).len(), 1);
        session.cancel_download();
        assert_eq!(session.download().unwrap().phase, DownloadPhase::Cancelled);
        session.close_transfer();
        assert!(session.download().is_none());
        assert_eq!(session.transfer_tab(), None);
    }
}
