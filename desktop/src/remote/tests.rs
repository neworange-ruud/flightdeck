//! A remote window, driven through GPUI's headless test platform from a
//! recorded snapshot (`RemoteClient::scripted`): no socket, the real views.
//!
//! What it proves is the M2 seam: the local window's sidebar, git strip and
//! terminal element draw a remote instance from the core's view structs, a
//! click or a key does what it does locally but against the mirror, and what
//! reaches the host is exactly the wire frame the plan names — keystrokes by
//! terminal id, commands with an explicit target, dialog answers by id.

use flightdeck::contracts::{AgentTabPosition, InterpretedStatus, TabId};
use flightdeck::host::{HostEvent, OverlayInput, OverlayView};
use flightdeck::web::client::{LinkEnd, LinkEvent, LinkOut, LinkState, RemoteClient, Script};
use flightdeck::web::protocol::{
    ClientMsg, Delta, DialogBody, DialogKey, DialogOrigin, DialogView, Geometry, GitBar,
    ProjectView, Seat, Selection, SessionPhase, SessionStatus, SessionView, Snapshot, StatusBucket,
    TermBytes, TerminalRole, TerminalView, PROTOCOL_VERSION,
};
use gpui::{AppContext, Entity, Modifiers, TestAppContext, VisualTestContext};
use gpui_component::Root;

use super::window::RemoteWindow;
use super::RemoteModel;

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

fn session(id: &str, name: &str, shells: &[&str]) -> SessionView {
    let mut terminals = vec![terminal(
        id,
        &format!("{id}:primary"),
        TerminalRole::Primary,
    )];
    terminals.extend(
        shells
            .iter()
            .map(|shell| terminal(id, shell, TerminalRole::Shell)),
    );
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
        git: GitBar {
            branch: Some(format!("flightdeck/{name}")),
            collected: true,
            has_upstream: true,
            upstream: Some(format!("origin/flightdeck/{name}")),
            ..GitBar::default()
        },
        terminals,
        lifecycle_reporting: true,
        recovered: false,
        attached_existing_branch: false,
    }
}

/// The recorded snapshot: one project, two sessions, the host looking at the
/// second (`search`).
fn snapshot() -> Snapshot {
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
            sessions: vec![
                session("s1", "login", &["s1:child:1"]),
                session("s2", "search", &[]),
            ],
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
        commands: flightdeck::web::commands::inventory(),
        help: None,
        about: None,
        update: None,
        sidebar_position: AgentTabPosition::default(),
    }
}

fn open(app: &mut TestAppContext) -> (Entity<RemoteModel>, Script, &mut VisualTestContext) {
    let (model, script, _, cx) = open_on(app, snapshot());
    (model, script, cx)
}

/// [`open`] from `snapshot`, with the window's view.
fn open_on(
    app: &mut TestAppContext,
    snapshot: Snapshot,
) -> (
    Entity<RemoteModel>,
    Script,
    Entity<RemoteWindow>,
    &mut VisualTestContext,
) {
    app.update(|cx| {
        gpui_component::init(cx);
        crate::theme::init(cx);
        flightdeck_desktop::keys::register(cx, crate::commands::keymap());
        flightdeck_desktop::overlays::register(cx, crate::commands::keymap());
    });
    let (client, script) = RemoteClient::scripted();
    script.snapshot(snapshot);
    let model = app.new(|_| {
        RemoteModel::new(
            client,
            "studio".to_string(),
            "192.168.2.20:7420".to_string(),
        )
    });
    let for_window = model.clone();
    let window_view = std::rc::Rc::new(std::cell::RefCell::new(None));
    let slot = window_view.clone();
    let (_root, cx) = app.add_window_view(|window, cx| {
        let view = cx.new(|cx| RemoteWindow::new(for_window, cx));
        *slot.borrow_mut() = Some(view.clone());
        Root::new(view, window, cx)
    });
    turn(&model, cx);
    let window_view = window_view
        .borrow_mut()
        .take()
        .expect("the window was built");
    (model, script, window_view, cx)
}

fn turn(model: &Entity<RemoteModel>, cx: &mut VisualTestContext) {
    model.update(cx, |m, cx| m.turn(cx));
    cx.run_until_parked();
}

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let bounds = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is not on screen"));
    cx.simulate_click(bounds.center(), Modifiers::none());
    cx.run_until_parked();
}

/// The command frames the window sent, as `(name, args)`.
fn commands(script: &mut Script) -> Vec<(String, Option<serde_json::Value>)> {
    script
        .sent()
        .into_iter()
        .filter_map(LinkOut::into_frame)
        .filter_map(|frame| match frame {
            ClientMsg::Command(c) => Some((c.name, c.args)),
            _ => None,
        })
        .collect()
}

#[gpui::test]
fn the_sidebar_git_strip_and_host_marker_draw_from_the_snapshot(app: &mut TestAppContext) {
    let (model, _script, cx) = open(app);
    assert!(cx.debug_bounds("agent-row-0").is_some());
    assert!(cx.debug_bounds("agent-row-1").is_some());
    assert!(
        cx.debug_bounds("agent-row-2").is_none(),
        "two sessions, two rows"
    );
    assert!(
        cx.debug_bounds("host-marker-1").is_some(),
        "R7: the host is looking at search"
    );
    assert!(cx.debug_bounds("host-marker-0").is_none());
    assert!(
        cx.debug_bounds("git-push").is_some(),
        "the shared git strip"
    );
    assert!(cx.debug_bounds("remote-project-tab-0").is_some());
    assert!(cx.debug_bounds("remote-identity").is_some());
    let selected = model.read_with(cx, |m, _| {
        m.workspace().selected_session().map(|s| s.name.clone())
    });
    assert_eq!(selected.as_deref(), Some("search"), "starts on the host's");
}

#[gpui::test]
fn streamed_bytes_render_in_the_mirrored_terminal(app: &mut TestAppContext) {
    let (model, script, cx) = open(app);
    script.send(LinkEvent::Bytes(TermBytes::live(
        "s2:primary".into(),
        0,
        b"hello from the host\r\n".to_vec(),
    )));
    turn(&model, cx);
    let screen = model.read_with(cx, |m, _| {
        m.active_terminal()
            .map(|t| t.screen().contents())
            .unwrap_or_default()
    });
    assert!(screen.contains("hello from the host"), "{screen:?}");
    let size = model.read_with(cx, |m, _| m.active_terminal().map(|t| t.screen().size()));
    assert_eq!(size, Some((24, 80)), "the host's grid, not this window's");
}

#[gpui::test]
fn clicking_a_row_browses_locally_and_sends_nothing(app: &mut TestAppContext) {
    let (model, mut script, cx) = open(app);
    script.sent();
    click(cx, "agent-row-0");
    let (selected, host_still) = model.read_with(cx, |m, _| {
        (
            m.workspace().selected_session().map(|s| s.name.clone()),
            m.workspace().host_is_viewing(&TabId("s2".to_string())),
        )
    });
    assert_eq!(selected.as_deref(), Some("login"));
    assert!(host_still, "the host's own selection did not move");
    assert!(commands(&mut script).is_empty(), "browsing is local (R2)");

    // The selected session's shell row focuses that terminal, locally too.
    click(cx, "terminal-row-1");
    let terminal = model.read_with(cx, |m, _| {
        m.workspace()
            .selected_terminal()
            .map(|t| t.terminal_id.as_str().to_string())
    });
    assert_eq!(terminal.as_deref(), Some("s1:child:1"));
    assert!(commands(&mut script).is_empty());
}

#[gpui::test]
fn typing_sends_input_for_the_terminal_on_screen(app: &mut TestAppContext) {
    let (model, mut script, cx) = open(app);
    model.update(cx, |m, cx| {
        m.focus_terminal(flightdeck::view::TerminalRef::Primary);
        m.dispatch(HostEvent::TerminalInput(b"ls\r".to_vec()), cx);
    });
    let inputs: Vec<(String, Vec<u8>)> = script
        .sent()
        .into_iter()
        .filter_map(LinkOut::into_frame)
        .filter_map(|frame| match frame {
            ClientMsg::Input(i) => Some((i.terminal_id.as_str().to_string(), i.data)),
            _ => None,
        })
        .collect();
    assert_eq!(inputs, vec![("s2:primary".to_string(), b"ls\r".to_vec())]);
}

#[gpui::test]
fn a_command_names_the_session_this_window_shows(app: &mut TestAppContext) {
    let (model, mut script, cx) = open(app);
    click(cx, "agent-row-0");
    script.sent();
    // The git strip's Push performs the keymap entry, as its chord does.
    click(cx, "git-push");
    assert_eq!(
        commands(&mut script),
        vec![(
            "push_branch".to_string(),
            Some(serde_json::json!({ "session_id": "s1" }))
        )]
    );
    // And a palette row the same way.
    model.update(cx, |m, cx| {
        m.dispatch(
            HostEvent::Command(flightdeck::app::commands::Command::RestartAgent),
            cx,
        )
    });
    assert_eq!(
        commands(&mut script),
        vec![(
            "restart_agent".to_string(),
            Some(serde_json::json!({ "session_id": "s1" }))
        )]
    );
}

/// A remote window's palette offers the desktop's own rows too: another
/// remote, another window. They are this machine's to perform, so nothing is
/// sent to the host.
#[gpui::test]
fn the_palettes_front_end_rows_stay_on_this_machine(app: &mut TestAppContext) {
    use flightdeck::tui::palette::{FrontEndAction, PaletteAction};
    let (model, mut script, cx) = open(app);
    script.sent();
    model.update(cx, |m, _| m.apply(HostEvent::OpenPalette));
    let Some(OverlayView::Palette(view)) = model.read_with(cx, |m, _| m.overlay()) else {
        panic!("the palette is open");
    };
    let labels: Vec<&str> = view.entries.iter().map(|row| row.label).collect();
    assert!(labels.contains(&"Connect to Remote"), "{labels:?}");
    assert!(labels.contains(&"New Window"), "{labels:?}");

    model.update(cx, |m, _| {
        m.apply(HostEvent::Overlay(OverlayInput::PaletteRun(
            PaletteAction::FrontEnd(FrontEndAction::ConnectToRemote),
        )))
    });
    assert_eq!(
        model.update(cx, |m, _| m.take_front_end_actions()),
        [FrontEndAction::ConnectToRemote]
    );
    assert!(
        model.read_with(cx, |m, _| m.overlay()).is_none(),
        "the palette closed"
    );
    assert!(commands(&mut script).is_empty(), "nothing went to the host");
}

#[gpui::test]
fn the_hosts_dialog_is_shared_and_answered_by_id(app: &mut TestAppContext) {
    let (model, mut script, cx) = open(app);
    let body = DialogBody {
        buttons: vec![
            DialogKey {
                key: "y".to_string(),
                label: "Close".to_string(),
                cancels: false,
            },
            DialogKey {
                key: "n".to_string(),
                label: "Cancel".to_string(),
                cancels: true,
            },
        ],
        confirmable: true,
        ..DialogBody::default()
    };
    script.send(LinkEvent::Delta(Delta::DialogOpened(DialogView {
        dialog_id: "dialog-3".into(),
        kind: "close_child".to_string(),
        title: "Close shell 1?".to_string(),
        origin: DialogOrigin::Desktop,
        body: Some(serde_json::to_value(body).unwrap()),
    })));
    turn(&model, cx);
    let overlay = model.read_with(cx, |m, _| m.overlay());
    let Some(OverlayView::Dialog(view)) = overlay else {
        panic!("the shared dialog is up: {overlay:?}");
    };
    assert_eq!(view.title, "Close shell 1?");

    script.sent();
    model.update(cx, |m, cx| {
        m.dispatch(HostEvent::Overlay(OverlayInput::Choose("y".into())), cx)
    });
    assert_eq!(
        commands(&mut script),
        vec![(
            "dialog_confirm".to_string(),
            Some(serde_json::json!({ "dialog_id": "dialog-3", "choice": "y" }))
        )]
    );

    // The host closing it closes it here.
    script.send(LinkEvent::Delta(Delta::DialogClosed {
        dialog_id: "dialog-3".into(),
        outcome: flightdeck::web::protocol::DialogOutcome::Confirmed,
    }));
    turn(&model, cx);
    assert!(model.read_with(cx, |m, _| m.overlay()).is_none());
}

#[gpui::test]
fn a_revoked_link_says_so_and_offers_to_close(app: &mut TestAppContext) {
    let (model, script, cx) = open(app);
    script.send(LinkEvent::State(LinkState::Ended(LinkEnd::Revoked)));
    turn(&model, cx);
    assert!(cx.debug_bounds("remote-ended").is_some());
    click(cx, "remote-close-window");
    assert!(model.read_with(cx, |m, _| m.close_requested()));
}

#[gpui::test]
fn the_seat_button_asks_the_host_for_another_seat(app: &mut TestAppContext) {
    let (_model, mut script, cx) = open(app);
    script.sent();
    click(cx, "remote-seat-observe");
    let attach: Vec<_> = script
        .sent()
        .into_iter()
        .filter(|out| matches!(out, LinkOut::Attach(_)))
        .collect();
    assert!(
        matches!(
            attach.as_slice(),
            [LinkOut::Attach(
                flightdeck::web::protocol::SeatRequest::Observe
            )]
        ),
        "{attach:?}"
    );
}

// ===========================================================================
// M3: connecting, saved remotes, seats and the link's states
// ===========================================================================

mod connect {
    use std::cell::RefCell;
    use std::path::{Path, PathBuf};
    use std::rc::Rc;

    use flightdeck::contracts::FileSystem;
    use flightdeck::testing::FakeFs;
    use flightdeck::web::client::store::{load_remotes, save_remotes, RemotesFile, SavedRemote};
    use flightdeck::web::client::{AccessToken, ExchangeError};
    use flightdeck::web::protocol::SeatRequest;
    use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};

    use crate::remote::connect::{ConnectView, Exchange, OpenRemote, Store};

    const PATH: &str = "/home/u/.flightdeck/remotes.json";

    struct Harness {
        fs: Rc<FakeFs>,
        exchanged: Rc<RefCell<Vec<(String, String, String)>>>,
        opened: Rc<RefCell<Vec<(SavedRemote, SeatRequest)>>>,
    }

    impl Harness {
        fn saved(&self) -> RemotesFile {
            load_remotes(self.fs.as_ref(), Path::new(PATH))
        }
    }

    fn open_with(
        app: &mut TestAppContext,
        seed: RemotesFile,
        refuse: bool,
    ) -> (Harness, Entity<ConnectView>, &mut VisualTestContext) {
        app.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
        });
        let fs = Rc::new(FakeFs::new());
        save_remotes(fs.as_ref(), Path::new(PATH), &seed).unwrap();
        let exchanged = Rc::new(RefCell::new(Vec::new()));
        let opened = Rc::new(RefCell::new(Vec::new()));
        let exchange: Exchange = {
            let exchanged = exchanged.clone();
            Rc::new(move |address, code, label| {
                exchanged.borrow_mut().push((address, code.clone(), label));
                Box::pin(async move {
                    if refuse {
                        Err(ExchangeError::Refused {
                            reason: "wrong_code".to_string(),
                            attempts_remaining: Some(2),
                            retry_after_ms: None,
                        })
                    } else {
                        Ok(AccessToken::new(format!("token-{code}")))
                    }
                })
            })
        };
        let open: OpenRemote = {
            let opened = opened.clone();
            Rc::new(move |remote, seat, _| opened.borrow_mut().push((remote, seat)))
        };
        let store = Store {
            fs: fs.clone() as Rc<dyn FileSystem>,
            path: Some(PathBuf::from(PATH)),
        };
        let (view, cx) = app.add_window_view(|window, cx| {
            ConnectView::new(None, store, exchange, open, window, cx)
        });
        cx.run_until_parked();
        (
            Harness {
                fs,
                exchanged,
                opened,
            },
            view,
            cx,
        )
    }

    fn click(cx: &mut VisualTestContext, selector: &'static str) {
        let bounds = cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is not on screen"));
        cx.simulate_click(bounds.center(), Modifiers::none());
        cx.run_until_parked();
    }

    fn fill(view: &Entity<ConnectView>, cx: &mut VisualTestContext, address: &str, code: &str) {
        cx.update(|window, cx| {
            view.update(cx, |v, cx| v.fill(address, code, window, cx));
        });
        cx.run_until_parked();
    }

    fn saved(address: &str, warned: bool) -> SavedRemote {
        SavedRemote {
            label: "studio".to_string(),
            address: address.to_string(),
            token: "stored-token".to_string(),
            last_seen: Some(10),
            viewer_id: None,
            host_version: None,
            warned_unencrypted: warned,
        }
    }

    #[gpui::test]
    fn pairing_exchanges_the_code_saves_the_token_and_opens_a_remote_window(
        app: &mut TestAppContext,
    ) {
        let (h, view, cx) = open_with(app, RemotesFile::default(), false);
        fill(&view, cx, "127.0.0.1", "4821");
        click(cx, "connect-pair");

        let exchanged = h.exchanged.borrow().clone();
        assert_eq!(exchanged.len(), 1);
        assert_eq!(exchanged[0].0, "127.0.0.1:7420", "the default port");
        assert_eq!(exchanged[0].1, "4821");
        assert!(exchanged[0].2.starts_with("FlightDeck Desktop/"));

        let opened = h.opened.borrow().clone();
        assert_eq!(opened.len(), 1);
        assert_eq!(opened[0].1, SeatRequest::Write, "R5: control by default");
        assert_eq!(opened[0].0.token, "token-4821");
        let file = h.saved();
        assert_eq!(file.get("127.0.0.1:7420").unwrap().token, "token-4821");
    }

    #[gpui::test]
    fn the_observe_seat_is_one_click_away(app: &mut TestAppContext) {
        let (h, view, cx) = open_with(app, RemotesFile::default(), false);
        click(cx, "connect-seat-observe");
        fill(&view, cx, "localhost:7420", "1111");
        click(cx, "connect-pair");
        assert_eq!(h.opened.borrow()[0].1, SeatRequest::Observe);
    }

    #[gpui::test]
    fn a_bad_address_or_code_is_refused_before_anything_is_sent(app: &mut TestAppContext) {
        let (h, view, cx) = open_with(app, RemotesFile::default(), false);
        fill(&view, cx, "host:notaport", "1234");
        click(cx, "connect-pair");
        assert!(view.read_with(cx, |v, _| v.error().is_some()));
        fill(&view, cx, "127.0.0.1", "12");
        click(cx, "connect-pair");
        assert!(view
            .read_with(cx, |v, _| v.error().map(str::to_string))
            .unwrap()
            .contains("4-digit"));
        assert!(h.exchanged.borrow().is_empty());
    }

    #[gpui::test]
    fn a_wrong_code_shows_the_hosts_reason(app: &mut TestAppContext) {
        let (h, view, cx) = open_with(app, RemotesFile::default(), true);
        fill(&view, cx, "127.0.0.1", "0000");
        click(cx, "connect-pair");
        let error = view.read_with(cx, |v, _| v.error().map(str::to_string));
        assert!(error.unwrap().contains("not the one the host is showing"));
        assert!(h.opened.borrow().is_empty());
        assert!(h.saved().remotes.is_empty());
    }

    #[gpui::test]
    fn a_lan_address_warns_once_that_the_link_is_unencrypted(app: &mut TestAppContext) {
        let (h, view, cx) = open_with(app, RemotesFile::default(), false);
        fill(&view, cx, "192.168.2.20", "4821");
        click(cx, "connect-pair");
        assert!(view.read_with(cx, |v, _| v.warning_open()));
        assert!(
            h.exchanged.borrow().is_empty(),
            "nothing sent before it is read"
        );

        click(cx, "connect-warning-accept");
        assert_eq!(h.opened.borrow().len(), 1);
        assert!(
            h.saved()
                .get("192.168.2.20:7420")
                .unwrap()
                .warned_unencrypted
        );
    }

    #[gpui::test]
    fn a_saved_remote_reconnects_with_its_token_and_no_code(app: &mut TestAppContext) {
        let mut seed = RemotesFile::default();
        seed.upsert(saved("192.168.2.20:7420", true));
        let (h, _view, cx) = open_with(app, seed, false);
        click(cx, "connect-saved-0");
        assert!(h.exchanged.borrow().is_empty(), "no code needed");
        let opened = h.opened.borrow().clone();
        assert_eq!(opened[0].0.token, "stored-token");
    }

    #[gpui::test]
    fn a_saved_remote_not_yet_warned_about_is_warned_about_first(app: &mut TestAppContext) {
        let mut seed = RemotesFile::default();
        seed.upsert(saved("192.168.2.20:7420", false));
        let (h, view, cx) = open_with(app, seed, false);
        click(cx, "connect-saved-0");
        assert!(view.read_with(cx, |v, _| v.warning_open()));
        click(cx, "connect-warning-back");
        assert!(h.opened.borrow().is_empty(), "Back connects nothing");
    }

    #[gpui::test]
    fn forgetting_a_remote_drops_its_token(app: &mut TestAppContext) {
        let mut seed = RemotesFile::default();
        seed.upsert(saved("192.168.2.20:7420", true));
        let (h, view, cx) = open_with(app, seed, false);
        click(cx, "connect-forget-0");
        assert!(h.saved().remotes.is_empty());
        assert!(view.read_with(cx, |v, _| v.saved().remotes.is_empty()));
    }
}

#[gpui::test]
fn another_writer_holding_input_offers_take_over(app: &mut TestAppContext) {
    let (model, mut script, cx) = open(app);
    script.send(LinkEvent::Delta(Delta::Seats {
        you: Seat::Writing,
        seats: vec![
            flightdeck::web::protocol::SeatInfo {
                viewer_id: None,
                label: "desktop".to_string(),
                address: None,
                user_agent_label: None,
                seat: Seat::Writing,
                holds_input: true,
                since_ms: 0,
                is_you: false,
            },
            flightdeck::web::protocol::SeatInfo {
                viewer_id: Some("viewer-1".into()),
                label: "192.168.2.30 · FlightDeck Desktop".to_string(),
                address: None,
                user_agent_label: None,
                seat: Seat::Writing,
                holds_input: false,
                since_ms: 0,
                is_you: true,
            },
        ],
        server_time_ms: 1,
        you_were_preempted: false,
    }));
    turn(&model, cx);
    assert!(cx.debug_bounds("remote-seat-take-over").is_some());
    script.sent();
    click(cx, "remote-seat-take-over");
    let attach: Vec<_> = script
        .sent()
        .into_iter()
        .filter(|out| matches!(out, LinkOut::Attach(_)))
        .collect();
    assert!(
        matches!(
            attach.as_slice(),
            [LinkOut::Attach(
                flightdeck::web::protocol::SeatRequest::TakeOver
            )]
        ),
        "{attach:?}"
    );
}

#[gpui::test]
fn reconnecting_and_host_quit_are_said_plainly(app: &mut TestAppContext) {
    let (model, script, cx) = open(app);
    script.send(LinkEvent::State(LinkState::Reconnecting {
        attempt: 2,
        retry_in_ms: 500,
    }));
    turn(&model, cx);
    let label = model.read_with(cx, |m, _| m.link_label());
    assert_eq!(label, "reconnecting · attempt 2 in 500 ms");
    assert!(
        cx.debug_bounds("remote-ended").is_none(),
        "it may still come back"
    );

    script.send(LinkEvent::State(LinkState::Ended(LinkEnd::HostQuit)));
    turn(&model, cx);
    assert!(cx.debug_bounds("remote-ended").is_some());
    assert!(
        cx.debug_bounds("remote-pair-again").is_none(),
        "a quit host needs no new code"
    );
}

#[gpui::test]
fn a_revoked_link_offers_to_pair_again(app: &mut TestAppContext) {
    let (model, script, cx) = open(app);
    script.send(LinkEvent::State(LinkState::Ended(LinkEnd::Revoked)));
    turn(&model, cx);
    assert!(cx.debug_bounds("remote-pair-again").is_some());
}

// --- a host grid larger than the window (D4, R17) ----------------------------

#[gpui::test]
fn a_host_grid_larger_than_the_window_pans_instead_of_clipping(app: &mut TestAppContext) {
    use gpui::{point, ScrollDelta, ScrollWheelEvent, TouchPhase};

    let mut big = snapshot();
    for session in &mut big.projects[0].sessions {
        for terminal in &mut session.terminals {
            terminal.geometry = Geometry {
                cols: 400,
                rows: 200,
            };
        }
    }
    let (model, script, window, cx) = open_on(app, big);
    let text: String = (1..=260).map(|n| format!("line {n}\r\n")).collect();
    script.send(LinkEvent::Bytes(TermBytes::live(
        "s2:primary".into(),
        0,
        text.into_bytes(),
    )));
    model.update(cx, |m, _| {
        m.focus_terminal(flightdeck::view::TerminalRef::Primary)
    });
    turn(&model, cx);
    let view = window.read_with(cx, |w, _| w.terminal_view());
    let state = |cx: &mut VisualTestContext| {
        let viewport = view.read_with(cx, |v, _| v.viewport());
        let history = model.read_with(cx, |m, _| {
            m.active_terminal().map_or(0, |t| t.screen().scrollback())
        });
        (viewport, history)
    };

    // The newest rows first: the grid's bottom, with its top out of view.
    let (viewport, _) = state(cx);
    assert!(
        viewport.overflow_rows > 0,
        "the test window is shorter than 200 rows"
    );
    assert!(viewport.overflow_cols > 0, "and narrower than 400 columns");
    assert_eq!(viewport.top + viewport.rows, 200, "anchored to the bottom");
    assert_eq!(viewport.left, 0);

    let m = view.read_with(cx, |v, _| v.metrics().unwrap());
    let inside = point(
        m.origin.x + m.width * f32::from(viewport.left + 2),
        m.origin.y + m.height * f32::from(viewport.top + 2),
    );
    let wheel = |cx: &mut VisualTestContext, x: f32, y: f32| {
        cx.simulate_event(ScrollWheelEvent {
            position: inside,
            delta: ScrollDelta::Lines(point(x, y)),
            modifiers: gpui::Modifiers::none(),
            touch_phase: TouchPhase::Moved,
        });
        cx.run_until_parked();
    };

    // A notch up pans three rows towards the grid's top, not into history.
    wheel(cx, 0., 1.);
    let (after, history) = state(cx);
    assert_eq!(after.top, viewport.top - 3);
    assert_eq!(history, 0);
    // Far enough up, the grid's top shows and then the history does.
    wheel(cx, 0., 200.);
    let (after, history) = state(cx);
    assert_eq!(after.top, 0);
    assert!(history > 0, "past the grid's top is the history");
    // Down again: out of the history first, then back to the bottom.
    wheel(cx, 0., -500.);
    let (after, history) = state(cx);
    assert_eq!((after.top, history), (viewport.top, 0));
    // Sideways pans the columns.
    wheel(cx, -2., 0.);
    let (after, _) = state(cx);
    assert_eq!(after.left, 6);

    // Typing brings the cursor (column 0 of the last row) back into view.
    wheel(cx, 0., 5.);
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let (after, _) = state(cx);
    assert_eq!((after.top, after.left), (viewport.top, 0));
}

// --- pasting an image -----------------------------------------------------------

#[gpui::test]
fn a_pasted_image_goes_to_the_host_with_the_input(app: &mut TestAppContext) {
    use gpui::{ClipboardEntry, ClipboardItem, Image, ImageFormat};

    let (model, mut script, cx) = open(app);
    model.update(cx, |m, _| {
        m.focus_terminal(flightdeck::view::TerminalRef::Primary)
    });
    script.sent();
    let shot = |bytes: Vec<u8>| ClipboardItem {
        entries: vec![ClipboardEntry::Image(Image::from_bytes(
            ImageFormat::Png,
            bytes,
        ))],
    };
    let paste = |cx: &mut VisualTestContext| {
        let model = model.clone();
        cx.update(|_, cx| crate::commands::perform_remote_id("Paste", &model, cx));
        cx.run_until_parked();
    };

    cx.update(|_, cx| cx.write_to_clipboard(shot(vec![0x89, b'P'])));
    paste(cx);
    let inputs: Vec<_> = script
        .sent()
        .into_iter()
        .filter_map(LinkOut::into_frame)
        .filter_map(|frame| match frame {
            ClientMsg::Input(i) => Some(i),
            _ => None,
        })
        .collect();
    assert_eq!(inputs.len(), 1);
    assert_eq!(inputs[0].terminal_id.as_str(), "s2:primary");
    assert!(inputs[0].data.is_empty(), "an older host types nothing");
    let image = inputs[0].image.as_ref().expect("the image rides the input");
    assert_eq!(
        (image.format.as_str(), &image.data[..]),
        ("png", &[0x89, b'P'][..])
    );

    // One the host would refuse never leaves this machine, and says why.
    let too_big = flightdeck::tui::clipboard::MAX_PASTED_IMAGE_BYTES + 1;
    cx.update(|_, cx| cx.write_to_clipboard(shot(vec![0; too_big])));
    paste(cx);
    assert!(script
        .sent()
        .into_iter()
        .filter_map(LinkOut::into_frame)
        .next()
        .is_none());
    let message = model.read_with(cx, |m, _| match m.overlay() {
        Some(OverlayView::Message(view)) => view.text,
        _ => String::new(),
    });
    assert!(message.contains("up to 10 MB"), "{message:?}");
}
