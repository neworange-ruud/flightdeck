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
    HostMachine, ProjectView, Seat, Selection, SessionPhase, SessionStatus, SessionView, Snapshot,
    StatusBucket, TermBytes, TerminalRole, TerminalView, PROTOCOL_VERSION,
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
        worktree_path: format!("/repo/.flightdeck/worktrees/{name}"),
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
        host_machine: Some(HostMachine {
            user: Some("dev".to_string()),
            os: "linux".to_string(),
        }),
        host_kind: None,
        worker: None,
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

fn open_in_vscode() -> HostEvent {
    HostEvent::RunPaletteAction(flightdeck::tui::palette::PaletteAction::FrontEnd(
        flightdeck::tui::palette::FrontEndAction::OpenInVsCode,
    ))
}

/// The window opens the session it shows on the host, over SSH as the host's
/// own account, in this machine's VS Code — and the host is never asked.
#[gpui::test]
fn open_in_vscode_opens_the_shown_session_over_ssh(app: &mut TestAppContext) {
    let (model, mut script, cx) = open(app);
    commands(&mut script);
    let urls = model.update(cx, |m, _| {
        m.apply(open_in_vscode());
        m.take_open_urls()
    });
    assert_eq!(
        urls,
        ["vscode://vscode-remote/ssh-remote+dev@192.168.2.20/repo/.flightdeck/worktrees/search"]
    );
    assert!(commands(&mut script).is_empty(), "nothing goes to the host");
}

/// An SSH target saved for the remote by hand (an `~/.ssh/config` alias)
/// replaces the derived one, read when the action runs.
#[gpui::test]
fn open_in_vscode_uses_the_ssh_target_saved_for_the_remote(app: &mut TestAppContext) {
    use flightdeck::web::client::store::{save_remotes, RemotesFile, SavedRemote};
    let (model, _script, cx) = open(app);
    let fs = std::rc::Rc::new(flightdeck::testing::FakeFs::new());
    let path = std::path::PathBuf::from("/home/u/.flightdeck/remotes.json");
    let mut file = RemotesFile::default();
    file.upsert(SavedRemote {
        label: "studio".to_string(),
        address: "192.168.2.20:7420".to_string(),
        token: "t".to_string(),
        last_seen: None,
        viewer_id: None,
        host_version: None,
        warned_unencrypted: true,
        ssh_target: Some("studio-box".to_string()),
        host_kind: None,
    });
    save_remotes(fs.as_ref(), &path, &file).unwrap();
    let urls = model.update(cx, |m, _| {
        m.set_remotes(super::connect::Store {
            fs,
            path: Some(path),
        });
        m.apply(open_in_vscode());
        m.take_open_urls()
    });
    assert_eq!(
        urls,
        ["vscode://vscode-remote/ssh-remote+studio-box/repo/.flightdeck/worktrees/search"]
    );
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
fn a_copy_made_in_the_program_on_screen_lands_on_this_clipboard(app: &mut TestAppContext) {
    // Claude Code's fullscreen renderer selects with the mouse itself and
    // copies with OSC 52; on the host that write goes nowhere useful, so the
    // mirror makes it here, on the machine whose user selected.
    let (model, script, cx) = open(app);
    cx.update(|_, cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string("before".into())));
    let clipboard = |cx: &mut VisualTestContext| cx.read_from_clipboard().and_then(|c| c.text());

    // A terminal not on screen copies: nobody here selected anything.
    script.send(LinkEvent::Bytes(TermBytes::live(
        "s1:primary".into(),
        0,
        b"\x1b]52;c;b2xk\x07".to_vec(),
    )));
    turn(&model, cx);
    assert_eq!(clipboard(cx).as_deref(), Some("before"));

    script.send(LinkEvent::Bytes(TermBytes::live(
        "s2:primary".into(),
        0,
        b"\x1b]52;c;bmV3\x07".to_vec(),
    )));
    turn(&model, cx);
    assert_eq!(clipboard(cx).as_deref(), Some("new"));
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
            ssh_target: None,
            host_kind: None,
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

// --- the SSH shell tab ------------------------------------------------------------

/// A window with its SSH shell on a fake PTY: the backend, to read what was
/// spawned, and the handle of the first session it hands out.
fn open_with_shell(
    app: &mut TestAppContext,
) -> (
    Entity<RemoteModel>,
    Script,
    Entity<RemoteWindow>,
    std::rc::Rc<flightdeck::testing::FakePty>,
    flightdeck::testing::FakePtyHandle,
    &mut VisualTestContext,
) {
    let (model, script, window, cx) = open_on(app, snapshot());
    let pty = std::rc::Rc::new(flightdeck::testing::FakePty::new());
    let session = pty.queue_session();
    let backend: std::rc::Rc<dyn flightdeck::contracts::PtyBackend> = pty.clone();
    window.update(cx, |w, _| w.set_shell_pty(backend));
    (model, script, window, pty, session, cx)
}

/// What the window sent the host as terminal input, as `(terminal, bytes)`.
fn inputs(script: &mut Script) -> Vec<(String, Vec<u8>)> {
    script
        .sent()
        .into_iter()
        .filter_map(LinkOut::into_frame)
        .filter_map(|frame| match frame {
            ClientMsg::Input(i) => Some((i.terminal_id.as_str().to_string(), i.data)),
            _ => None,
        })
        .collect()
}

/// The shell tab runs `ssh` to the same target "Open in VS Code" uses, on
/// this machine, and draws it in place of the project's sidebar, git strip
/// and terminal. The host is never asked.
#[gpui::test]
fn the_shell_tab_runs_ssh_to_the_host_in_place_of_the_project(app: &mut TestAppContext) {
    let (model, mut script, window, pty, session, cx) = open_with_shell(app);
    script.sent();
    assert!(
        pty.spawns().is_empty(),
        "nothing starts until the tab is shown"
    );
    let shell_tab = cx.debug_bounds("remote-shell-tab").expect("the shell tab");
    let first_project = cx
        .debug_bounds("remote-project-tab-0")
        .expect("a project tab");
    assert!(
        shell_tab.right() <= first_project.left(),
        "the shell is the first tab: {shell_tab:?} vs {first_project:?}"
    );

    click(cx, "remote-shell-tab");
    let spawns = pty.spawns();
    assert_eq!(spawns.len(), 1);
    assert_eq!(
        (spawns[0].0.as_str(), spawns[0].1.as_slice()),
        ("ssh", ["dev@192.168.2.20".to_string()].as_slice())
    );
    assert!(model.read_with(cx, |m, _| m.shell_active()));
    assert!(cx.debug_bounds("agent-row-0").is_none(), "no sidebar");
    assert!(cx.debug_bounds("git-push").is_none(), "no git strip");
    assert!(
        cx.debug_bounds("remote-project-tab-0").is_some(),
        "the project tabs stay, to go back"
    );

    // Its output is drawn; nothing went to the host.
    session.push_output(b"dev@studio:~$ ".to_vec());
    let shell = window
        .read_with(cx, |w, _| w.shell_view())
        .expect("started");
    let screen = shell.update(cx, |view, cx| {
        view.poll_once(cx);
        view.owned_terminal()
            .map(|t| t.screen().contents())
            .unwrap_or_default()
    });
    assert!(screen.contains("dev@studio:~$"), "{screen:?}");
    assert!(script.sent().is_empty(), "the shell is not the host's");

    // A project tab goes back, and the shell keeps running for next time.
    click(cx, "remote-project-tab-0");
    assert!(!model.read_with(cx, |m, _| m.shell_active()));
    assert!(cx.debug_bounds("agent-row-0").is_some());
    click(cx, "remote-shell-tab");
    assert_eq!(pty.spawns().len(), 1, "the same session, not a new one");
}

/// Keys and pastes on the shell tab go to the shell; none reach the agent
/// the host's terminal (now hidden) belongs to.
#[gpui::test]
fn typing_and_pasting_in_the_shell_never_reach_the_hosts_terminal(app: &mut TestAppContext) {
    let (model, mut script, _window, _pty, session, cx) = open_with_shell(app);
    model.update(cx, |m, _| {
        m.focus_terminal(flightdeck::view::TerminalRef::Primary)
    });
    click(cx, "remote-shell-tab");
    script.sent();

    cx.simulate_keystrokes("l s enter");
    assert_eq!(session.input(), b"ls\r");

    // The table's Paste chord is a window action; on the shell tab it pastes
    // into the shell.
    cx.update(|_, cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string("pwd".into())));
    let paste = flightdeck_desktop::keys::KeymapAction::for_entry(
        crate::commands::keymap()
            .entry("Paste")
            .expect("Paste is in the table"),
    );
    cx.dispatch_action(paste);
    cx.run_until_parked();
    assert!(
        String::from_utf8_lossy(&session.input()).ends_with("pwd"),
        "{:?}",
        session.input()
    );

    // Input routed through the model is dropped, not sent to the hidden one.
    model.update(cx, |m, cx| {
        m.dispatch(HostEvent::TerminalInput(b"rm -rf\r".to_vec()), cx);
        m.dispatch(HostEvent::Paste("text".to_string()), cx);
    });
    assert!(inputs(&mut script).is_empty());
}

/// A host command from the shell tab would act on a session nobody is
/// looking at: it is refused with a word, and nothing is sent.
#[gpui::test]
fn a_host_command_from_the_shell_tab_is_refused(app: &mut TestAppContext) {
    let (model, mut script, _window, _pty, _session, cx) = open_with_shell(app);
    click(cx, "remote-shell-tab");
    script.sent();
    model.update(cx, |m, cx| {
        m.dispatch(
            HostEvent::Command(flightdeck::app::commands::Command::RestartAgent),
            cx,
        )
    });
    assert!(commands(&mut script).is_empty(), "nothing went to the host");
    let message = model.read_with(cx, |m, _| match m.overlay() {
        Some(OverlayView::Message(view)) => view.text,
        _ => String::new(),
    });
    assert!(message.contains("project tab"), "{message:?}");

    // Switching projects from the shell goes back to the project it left.
    model.update(cx, |m, cx| {
        m.dispatch(
            HostEvent::SwitchProject(flightdeck::app::commands::Selector::Next),
            cx,
        )
    });
    assert!(!model.read_with(cx, |m, _| m.shell_active()));
}

/// An SSH session that ended (logged out, network gone) stays readable and
/// offers Reconnect, which starts a new one.
#[gpui::test]
fn an_ended_shell_offers_to_reconnect(app: &mut TestAppContext) {
    let (_model, _script, window, pty, session, cx) = open_with_shell(app);
    click(cx, "remote-shell-tab");
    assert!(cx.debug_bounds("remote-shell-reconnect").is_none());

    session.set_state(flightdeck::contracts::ProcessState::Exited(255));
    let shell = window
        .read_with(cx, |w, _| w.shell_view())
        .expect("started");
    shell.update(cx, |view, cx| view.poll_once(cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("remote-shell-ended").is_some());

    click(cx, "remote-shell-reconnect");
    assert_eq!(pty.spawns().len(), 2, "a new ssh");
    assert!(cx.debug_bounds("remote-shell-ended").is_none());
}

/// No `ssh` to run: the tab says so and can try again.
#[gpui::test]
fn a_shell_that_cannot_start_says_why(app: &mut TestAppContext) {
    let (_model, _script, _window, pty, _session, cx) = open_with_shell(app);
    pty.fail_next_spawn();
    click(cx, "remote-shell-tab");
    assert!(cx.debug_bounds("remote-shell-reconnect").is_some());
    click(cx, "remote-shell-reconnect");
    assert_eq!(pty.spawns().len(), 1, "the second try started it");
    assert!(cx.debug_bounds("remote-shell-reconnect").is_none());
}

/// Tab and Shift-Tab reach the program in the terminal — a shell completes
/// with them — rather than moving focus, which the widget layer binds them to
/// in every window.
#[gpui::test]
fn tab_reaches_the_terminal_for_completion(app: &mut TestAppContext) {
    let (model, mut script, _window, _pty, session, cx) = open_with_shell(app);
    model.update(cx, |m, _| {
        m.focus_terminal(flightdeck::view::TerminalRef::Primary)
    });
    cx.run_until_parked();
    script.sent();
    cx.simulate_keystrokes("g i t space c h tab shift-tab");
    let typed: Vec<u8> = inputs(&mut script)
        .into_iter()
        .flat_map(|(terminal, bytes)| {
            assert_eq!(terminal, "s2:primary");
            bytes
        })
        .collect();
    assert_eq!(typed, b"git ch\t\x1b[Z", "the host's terminal");

    click(cx, "remote-shell-tab");
    cx.simulate_keystrokes("c d space tab");
    assert_eq!(session.input(), b"cd \t", "the SSH shell");
}

// ===========================================================================
// A worker host (`specs/WORKER_PRD.md`): its shell tab, geometry, project
// picker, takeover, messages, notifications and version skew — and the
// configuration manager, which every host kind offers (design A1).
// ===========================================================================

mod worker {
    use std::cell::RefCell;
    use std::rc::Rc;

    use flightdeck::contracts::{NotificationSound, PtySize};
    use flightdeck::host::{HostEvent, OverlayInput, OverlayView};
    use flightdeck::testing::FakeNotifier;
    use flightdeck::tui::palette::PaletteAction;
    use flightdeck::web::client::{
        AckFor, LinkEnd, LinkEvent, LinkOut, LinkState, RemoteClient, Script,
    };
    use flightdeck::web::protocol::{
        Ack, AckOutcome, ClientMsg, CloneProgress, CloneResult, ConfigChange, ConfigFieldKind,
        ConfigOrigin, ConfigRowView, ConfigSaveRequest, ConfigScopeName, ConfigValue, ConfigView,
        Delta, DirEntryView, DirListing, Geometry, HostKind, HostMessage, MessageLevel,
        NotificationSoundView, NotificationView, ProjectScan, ProjectView, ScannedRepo, Snapshot,
        TermBytes, WorkerShellView, WorkerSnapshot, WorkerStatusView,
    };
    use gpui::{Entity, TestAppContext, VisualTestContext};

    use super::{click, commands, inputs, open_on, snapshot, turn};
    use crate::remote::{RemoteModel, Tab, WORKER_SHELL_COMMAND_REFUSAL};

    fn shell(alive: bool) -> WorkerShellView {
        WorkerShellView {
            terminal_id: "worker:shell".into(),
            title: "zsh".to_string(),
            geometry: Geometry { cols: 80, rows: 24 },
            byte_len: 0,
            replay_from: 0,
            alive,
            exit_code: (!alive).then_some(0),
        }
    }

    /// The recorded snapshot, from a worker running this app's FlightDeck.
    fn worker_snapshot() -> Snapshot {
        let mut snapshot = snapshot();
        snapshot.host_kind = Some(HostKind::Worker);
        snapshot.host_version = flightdeck::VERSION.to_string();
        snapshot.commands = flightdeck::web::commands::inventory_for(HostKind::Worker);
        snapshot.worker = Some(WorkerSnapshot {
            home: "/home/dev".to_string(),
            shell: Some(shell(true)),
            health: None,
            status: WorkerStatusView {
                projects_root: "/home/dev/flightdeck".to_string(),
                ..WorkerStatusView::default()
            },
            connections: Default::default(),
            host_messages: Vec::new(),
        });
        snapshot
    }

    /// A worker with no project open yet.
    fn empty_worker() -> Snapshot {
        let mut snapshot = worker_snapshot();
        snapshot.projects.clear();
        snapshot.selection = Default::default();
        snapshot
    }

    /// The sizes the window sent, as `(terminal, cols, rows)`.
    fn resizes(script: &mut Script) -> Vec<(String, u16, u16)> {
        script
            .sent()
            .into_iter()
            .filter_map(LinkOut::into_frame)
            .filter_map(|frame| match frame {
                ClientMsg::ResizeTerminal(r) => {
                    Some((r.terminal_id.as_str().to_string(), r.cols, r.rows))
                }
                _ => None,
            })
            .collect()
    }

    /// The commands sent, with their seqs.
    fn sent_commands(script: &mut Script) -> Vec<(u64, String, Option<serde_json::Value>)> {
        script
            .sent()
            .into_iter()
            .filter_map(LinkOut::into_frame)
            .filter_map(|frame| match frame {
                ClientMsg::Command(c) => Some((c.seq, c.name, c.args)),
                _ => None,
            })
            .collect()
    }

    fn message(model: &Entity<RemoteModel>, cx: &mut VisualTestContext) -> String {
        model.read_with(cx, |m, _| match m.overlay() {
            Some(OverlayView::Message(view)) => view.text,
            _ => String::new(),
        })
    }

    fn ack(seq: u64, outcome: AckOutcome, detail: &str) -> LinkEvent {
        LinkEvent::Ack(
            Ack {
                seq,
                outcome,
                detail: Some(detail.to_string()),
            },
            AckFor::Command,
        )
    }

    // --- the worker shell tab ---------------------------------------------

    #[gpui::test]
    fn a_worker_has_its_shell_tab_first_and_no_ssh_tab(app: &mut TestAppContext) {
        let (model, _script, _window, cx) = open_on(app, worker_snapshot());
        assert!(cx.debug_bounds("remote-shell-tab").is_none(), "no SSH tab");
        let shell_tab = cx
            .debug_bounds("remote-worker-shell-tab")
            .expect("the worker shell tab");
        let first_project = cx.debug_bounds("remote-project-tab-0").unwrap();
        assert!(shell_tab.right() <= first_project.left(), "first");

        click(cx, "remote-worker-shell-tab");
        assert_eq!(model.read_with(cx, |m, _| m.tab()), Tab::WorkerShell);
        assert!(cx.debug_bounds("agent-row-0").is_none(), "no sidebar");
        assert!(cx.debug_bounds("git-push").is_none(), "no git strip");
        click(cx, "remote-project-tab-0");
        assert_eq!(model.read_with(cx, |m, _| m.tab()), Tab::Project);
    }

    #[gpui::test]
    fn the_worker_shell_mirrors_and_takes_what_is_typed(app: &mut TestAppContext) {
        let (model, mut script, _window, cx) = open_on(app, worker_snapshot());
        click(cx, "remote-worker-shell-tab");
        script.send(LinkEvent::Bytes(TermBytes::live(
            "worker:shell".into(),
            0,
            b"dev@box:~$ ".to_vec(),
        )));
        turn(&model, cx);
        let screen = model.read_with(cx, |m, _| {
            m.active_terminal()
                .map(|t| t.screen().contents())
                .unwrap_or_default()
        });
        assert!(screen.contains("dev@box:~$"), "{screen:?}");

        script.sent();
        cx.simulate_keystrokes("l s enter");
        model.update(cx, |m, cx| m.dispatch(HostEvent::Paste("pwd".into()), cx));
        let typed: Vec<u8> = inputs(&mut script)
            .into_iter()
            .flat_map(|(terminal, bytes)| {
                assert_eq!(terminal, "worker:shell");
                bytes
            })
            .collect();
        assert!(typed.starts_with(b"ls\r"), "{typed:?}");
        assert!(String::from_utf8_lossy(&typed).contains("pwd"));
    }

    #[gpui::test]
    fn a_host_command_from_the_worker_shell_is_refused(app: &mut TestAppContext) {
        let (model, mut script, _window, cx) = open_on(app, worker_snapshot());
        click(cx, "remote-worker-shell-tab");
        script.sent();
        model.update(cx, |m, cx| {
            m.dispatch(
                HostEvent::Command(flightdeck::app::commands::Command::RestartAgent),
                cx,
            )
        });
        assert!(commands(&mut script).is_empty(), "nothing went to the host");
        assert_eq!(message(&model, cx), WORKER_SHELL_COMMAND_REFUSAL);
    }

    #[gpui::test]
    fn an_exited_worker_shell_offers_to_restart_it(app: &mut TestAppContext) {
        let (model, mut script, _window, cx) = open_on(app, worker_snapshot());
        click(cx, "remote-worker-shell-tab");
        assert!(cx.debug_bounds("remote-worker-shell-ended").is_none());
        script.send(LinkEvent::Delta(Delta::WorkerShell(shell(false))));
        turn(&model, cx);
        assert!(cx.debug_bounds("remote-worker-shell-ended").is_some());
        script.sent();
        click(cx, "remote-worker-shell-restart");
        let names: Vec<String> = commands(&mut script).into_iter().map(|c| c.0).collect();
        assert_eq!(names, ["restart_worker_shell"]);
    }

    // --- geometry (W5) ----------------------------------------------------

    #[gpui::test]
    fn a_worker_is_sent_the_pane_size_once_per_size(app: &mut TestAppContext) {
        let (model, mut script, _window, cx) = open_on(app, worker_snapshot());
        model.update(cx, |m, _| {
            m.focus_terminal(flightdeck::view::TerminalRef::Primary)
        });
        turn(&model, cx);
        turn(&model, cx);
        let sent = resizes(&mut script);
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert_eq!(sent[0].0, "s2:primary", "the terminal on screen");
        // The same size again, frame after frame: nothing more.
        turn(&model, cx);
        turn(&model, cx);
        assert!(resizes(&mut script).is_empty());

        // A new size is sent, once.
        let sent = model.update(cx, |m, _| {
            m.set_viewport(PtySize { rows: 10, cols: 40 });
            m.pump();
            m.pump();
            resizes(&mut script)
        });
        assert_eq!(sent, [("s2:primary".to_string(), 40, 10)]);

        // The worker shell, once on screen, is sized too.
        click(cx, "remote-worker-shell-tab");
        turn(&model, cx);
        let sent = resizes(&mut script);
        assert!(sent.iter().any(|(id, ..)| id == "worker:shell"), "{sent:?}");
    }

    #[gpui::test]
    fn a_host_that_is_not_a_worker_is_never_resized(app: &mut TestAppContext) {
        let (model, mut script, _window, cx) = open_on(app, snapshot());
        model.update(cx, |m, _| {
            m.focus_terminal(flightdeck::view::TerminalRef::Primary);
            m.set_viewport(PtySize { rows: 10, cols: 40 });
        });
        turn(&model, cx);
        turn(&model, cx);
        assert!(
            resizes(&mut script).is_empty(),
            "D4: the host owns geometry"
        );
    }

    // --- the project picker -----------------------------------------------

    #[gpui::test]
    fn the_picker_scans_browses_and_opens_a_repository(app: &mut TestAppContext) {
        let (model, mut script, _window, cx) = open_on(app, empty_worker());
        script.sent();
        click(cx, "remote-open-project");
        let sent = sent_commands(&mut script);
        let seq = |name: &str| sent.iter().find(|c| c.1 == name).map(|c| c.0).unwrap();
        let (scan, browse) = (seq("scan_projects"), seq("browse_dir"));
        assert!(cx.debug_bounds("remote-picker").is_some());

        script.send(LinkEvent::ProjectScan(ProjectScan {
            seq: scan,
            root: "/home/dev/flightdeck".to_string(),
            root_exists: true,
            repos: vec![ScannedRepo {
                name: "api".to_string(),
                path: "/home/dev/flightdeck/api".to_string(),
                is_open_project: false,
            }],
            refusal: None,
        }));
        script.send(LinkEvent::DirListing(DirListing {
            seq: browse,
            path: "/home/dev".to_string(),
            parent: None,
            home: "/home/dev".to_string(),
            entries: vec![
                DirEntryView {
                    name: "src".to_string(),
                    path: "/home/dev/src".to_string(),
                    is_git_repo: false,
                    is_open_project: false,
                },
                DirEntryView {
                    name: "app".to_string(),
                    path: "/home/dev/app".to_string(),
                    is_git_repo: true,
                    is_open_project: false,
                },
            ],
            truncated: false,
        }));
        turn(&model, cx);
        assert!(cx.debug_bounds("remote-picker-scanned-0").is_some());
        assert!(cx.debug_bounds("remote-picker-entry-1").is_some());
        assert!(
            cx.debug_bounds("remote-picker-open-0").is_none(),
            "src is no repository"
        );

        // A folder browses into it.
        click(cx, "remote-picker-entry-0");
        let sent = sent_commands(&mut script);
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].1, "browse_dir");
        assert_eq!(sent[0].2.as_ref().unwrap()["path"], "/home/dev/src");

        // A repository opens; a refusal is the worker's words.
        click(cx, "remote-picker-open-1");
        let sent = sent_commands(&mut script);
        assert_eq!(
            (sent[0].1.as_str(), sent[0].2.clone()),
            (
                "open_project_path",
                Some(serde_json::json!({ "path": "/home/dev/app" }))
            )
        );
        script.send(ack(
            sent[0].0,
            AckOutcome::Rejected,
            "Not a git repository: /home/dev/app",
        ));
        turn(&model, cx);
        assert!(cx.debug_bounds("remote-picker-error").is_some());
        assert!(message(&model, cx).is_empty(), "not a message overlay");

        click(cx, "remote-picker-open-1");
        let open = sent_commands(&mut script)[0].0;
        script.send(ack(open, AckOutcome::Applied, "Opened project 'app'."));
        script.send(LinkEvent::Delta(Delta::ProjectUpsert(ProjectView {
            project_id: "/home/dev/app".into(),
            name: "app".to_string(),
            root: "/home/dev/app".to_string(),
            base_branch: "main".to_string(),
            dot: None,
            sessions: Vec::new(),
        })));
        turn(&model, cx);
        assert!(
            model.read_with(cx, |m, _| m.picker().is_none()),
            "it closed"
        );
        assert!(cx.debug_bounds("remote-picker").is_none());
        let (selected, notice) = model.read_with(cx, |m, _| {
            (
                m.workspace().selected_project().map(|p| p.name.clone()),
                m.notice().map(str::to_string),
            )
        });
        assert_eq!(selected.as_deref(), Some("app"), "the window went to it");
        assert_eq!(notice.as_deref(), Some("Opened project 'app'."));
    }

    #[gpui::test]
    fn open_project_on_a_worker_is_its_picker(app: &mut TestAppContext) {
        let (model, mut script, _window, cx) = open_on(app, worker_snapshot());
        script.sent();
        model.update(cx, |m, cx| {
            m.dispatch(HostEvent::RunPaletteAction(PaletteAction::OpenProject), cx)
        });
        assert!(model.read_with(cx, |m, _| m.picker().is_some()));
        let names: Vec<String> = commands(&mut script).into_iter().map(|c| c.0).collect();
        assert!(!names.contains(&"open_project".to_string()), "{names:?}");
        assert!(names.contains(&"scan_projects".to_string()), "{names:?}");
    }

    #[gpui::test]
    fn a_clone_shows_its_progress_then_gits_own_error(app: &mut TestAppContext) {
        let (model, mut script, window, cx) = open_on(app, empty_worker());
        click(cx, "remote-open-project");
        script.sent();
        let picker = window
            .read_with(cx, |w, _| w.picker_view())
            .expect("the picker view");
        cx.update(|window, cx| {
            picker.update(cx, |view, cx| {
                view.fill_clone("git@github.com:me/api.git", "api", window, cx)
            })
        });
        click(cx, "remote-picker-clone");
        let sent = sent_commands(&mut script);
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].1, "clone_project");
        assert_eq!(
            sent[0].2,
            Some(serde_json::json!({ "url": "git@github.com:me/api.git", "name": "api" }))
        );
        let clone_id = sent[0].0;

        script.send(LinkEvent::CloneProgress(CloneProgress {
            clone_id,
            url: "git@github.com:me/api.git".to_string(),
            phase: "Receiving objects".to_string(),
            percent: Some(42),
        }));
        turn(&model, cx);
        assert!(cx.debug_bounds("remote-picker-clone-progress").is_some());
        let line = model.read_with(cx, |m, _| {
            crate::remote::projects::clone_line(m.picker().unwrap())
        });
        assert_eq!(
            line,
            Some((
                "Cloning git@github.com:me/api.git — Receiving objects 42%".to_string(),
                false
            ))
        );

        let git = "ERROR: Repository not found.\nfatal: Could not read from remote repository.";
        script.send(LinkEvent::CloneResult(CloneResult {
            clone_id,
            url: "git@github.com:me/api.git".to_string(),
            ok: false,
            path: None,
            project_id: None,
            error: Some(git.to_string()),
        }));
        turn(&model, cx);
        assert!(cx.debug_bounds("remote-picker-clone-error").is_some());
        let error = model.read_with(cx, |m, _| m.picker().and_then(|p| p.error.clone()));
        assert_eq!(error.as_deref(), Some(git), "git's words, verbatim");
    }

    #[gpui::test]
    fn a_finished_clone_opens_the_project(app: &mut TestAppContext) {
        let (model, mut script, _window, cx) = open_on(app, empty_worker());
        click(cx, "remote-open-project");
        script.sent();
        model.update(cx, |m, _| m.picker_clone("https://example.com/api.git", ""));
        let sent = sent_commands(&mut script);
        assert_eq!(
            sent[0].2,
            Some(serde_json::json!({ "url": "https://example.com/api.git" })),
            "no name: the URL's"
        );
        script.send(LinkEvent::CloneResult(CloneResult {
            clone_id: sent[0].0,
            url: "https://example.com/api.git".to_string(),
            ok: true,
            path: Some("/home/dev/flightdeck/api".to_string()),
            project_id: Some("/home/dev/flightdeck/api".into()),
            error: None,
        }));
        turn(&model, cx);
        assert!(model.read_with(cx, |m, _| m.picker().is_none()));
    }

    // --- takeover and a stopped worker ------------------------------------

    /// A window whose Reconnect dials a new scripted client, kept here.
    fn with_dialer(
        model: &Entity<RemoteModel>,
        cx: &mut VisualTestContext,
    ) -> Rc<RefCell<Vec<Script>>> {
        let dialled = Rc::new(RefCell::new(Vec::new()));
        let sink = dialled.clone();
        model.update(cx, |m, _| {
            m.set_dialer(Rc::new(move || {
                let (client, script) = RemoteClient::scripted();
                sink.borrow_mut().push(script);
                client
            }))
        });
        dialled
    }

    #[gpui::test]
    fn taken_over_says_by_whom_and_reconnect_takes_it_back(app: &mut TestAppContext) {
        let (model, script, _window, cx) = open_on(app, worker_snapshot());
        let dialled = with_dialer(&model, cx);
        script.send(LinkEvent::State(LinkState::Ended(LinkEnd::TakenOver {
            by: "laptop".to_string(),
        })));
        turn(&model, cx);
        assert!(cx.debug_bounds("remote-ended").is_some());
        assert_eq!(
            model.read_with(cx, |m, _| m.link_label()),
            "taken over by laptop"
        );

        click(cx, "remote-reconnect");
        assert_eq!(dialled.borrow().len(), 1, "dialled again");
        dialled.borrow()[0].snapshot(worker_snapshot());
        turn(&model, cx);
        assert!(cx.debug_bounds("remote-ended").is_none());
        assert!(model.read_with(cx, |m, _| m.client().state().is_live()));
        assert!(cx.debug_bounds("remote-worker-shell-tab").is_some());
    }

    #[gpui::test]
    fn a_stopped_worker_offers_to_reconnect(app: &mut TestAppContext) {
        let (model, script, _window, cx) = open_on(app, worker_snapshot());
        let dialled = with_dialer(&model, cx);
        script.send(LinkEvent::State(LinkState::Ended(LinkEnd::HostQuit)));
        turn(&model, cx);
        assert!(cx.debug_bounds("remote-ended").is_some());
        click(cx, "remote-reconnect");
        assert_eq!(dialled.borrow().len(), 1);
    }

    // --- host messages, notifications, version skew -----------------------

    fn host_message(id: u64, level: MessageLevel, text: &str) -> HostMessage {
        HostMessage {
            id,
            at_ms: 1_000 + id as i64,
            level,
            text: text.to_string(),
        }
    }

    #[gpui::test]
    fn messages_kept_while_away_come_back_as_one_overlay(app: &mut TestAppContext) {
        let mut snapshot = worker_snapshot();
        snapshot.worker.as_mut().unwrap().host_messages = vec![
            host_message(1, MessageLevel::Info, "FlightDeck 9.9.9 is available."),
            host_message(2, MessageLevel::Error, "Clone failed: repository not found"),
        ];
        let (model, _script, _window, cx) = open_on(app, snapshot);
        let text = message(&model, cx);
        assert!(
            text.starts_with("2 messages while you were away\n"),
            "{text:?}"
        );
        assert!(text.contains("Error: Clone failed"), "{text:?}");
    }

    #[gpui::test]
    fn a_new_message_is_a_notice_and_an_error_also_an_overlay(app: &mut TestAppContext) {
        let (model, script, _window, cx) = open_on(app, worker_snapshot());
        script.send(LinkEvent::Delta(Delta::HostMessage(host_message(
            1,
            MessageLevel::Info,
            "Merged login into main.",
        ))));
        turn(&model, cx);
        assert_eq!(
            model.read_with(cx, |m, _| m.notice().map(str::to_string)),
            Some("Merged login into main.".to_string())
        );
        assert!(message(&model, cx).is_empty(), "information is not modal");

        script.send(LinkEvent::Delta(Delta::HostMessage(host_message(
            2,
            MessageLevel::Error,
            "Could not start claude: not found",
        ))));
        turn(&model, cx);
        assert_eq!(message(&model, cx), "Could not start claude: not found");
    }

    #[gpui::test]
    fn a_workers_notification_is_posted_from_its_window(app: &mut TestAppContext) {
        let (model, script, _window, cx) = open_on(app, worker_snapshot());
        let notifier = Rc::new(FakeNotifier::new());
        model.update(cx, |m, _| m.set_notifier(notifier.clone()));
        script.send(LinkEvent::Delta(Delta::Notification(NotificationView {
            at_ms: 5,
            title: "login".to_string(),
            body: "Claude Code needs your input".to_string(),
            sound: NotificationSoundView::InputRequired,
        })));
        turn(&model, cx);
        let posted = notifier.posted();
        assert_eq!(posted.len(), 1);
        assert_eq!(posted[0].title, "studio: login", "named after the window");
        assert_eq!(posted[0].body, "Claude Code needs your input");
        assert_eq!(posted[0].sound, NotificationSound::InputRequired);
        turn(&model, cx);
        assert_eq!(notifier.posted().len(), 1, "once");
    }

    #[gpui::test]
    fn a_worker_on_another_version_shows_the_skew_banner(app: &mut TestAppContext) {
        let mut snapshot = worker_snapshot();
        snapshot.host_version = "0.0.1".to_string();
        let (model, _script, _window, cx) = open_on(app, snapshot);
        assert!(cx.debug_bounds("remote-version-skew").is_some());
        let warning = model.read_with(cx, |m, _| m.skew_warning()).unwrap();
        assert!(warning.starts_with("This worker runs FlightDeck 0.0.1; this app was built with"));
        assert!(warning.ends_with("`flightdeck update`, then `flightdeck worker restart`."));
    }

    #[gpui::test]
    fn the_same_version_or_another_host_kind_shows_no_banner(app: &mut TestAppContext) {
        let (_model, _script, _window, cx) = open_on(app, worker_snapshot());
        assert!(cx.debug_bounds("remote-version-skew").is_none());
    }

    #[gpui::test]
    fn a_tui_host_on_another_version_shows_no_banner(app: &mut TestAppContext) {
        // The recorded TUI host runs 1.0.0, not this build.
        let (_model, _script, _window, cx) = open_on(app, snapshot());
        assert!(cx.debug_bounds("remote-version-skew").is_none());
    }

    // --- the configuration manager (every host kind) ----------------------

    fn config_view(seq: u64, value: bool) -> ConfigView {
        let row = ConfigRowView {
            key: "notifications.enabled".to_string(),
            label: "Notifications".to_string(),
            kind: ConfigFieldKind::Bool,
            value: ConfigValue::Bool(value),
            choices: Vec::new(),
            origin: ConfigOrigin::Default,
            inherited: ConfigValue::Bool(false),
            inherited_origin: ConfigOrigin::Default,
        };
        ConfigView {
            seq,
            project_name: "repo".to_string(),
            global_path: Some("/home/dev/.flightdeck/config.toml".to_string()),
            project_path: "/repo/.flightdeck/config.toml".to_string(),
            project_rows: vec![row.clone()],
            global_rows: vec![row],
        }
    }

    #[gpui::test]
    fn the_hosts_configuration_is_edited_here_and_saved_there(app: &mut TestAppContext) {
        let (model, mut script, _window, cx) = open_on(app, snapshot());
        script.sent();
        model.update(cx, |m, cx| {
            m.dispatch(HostEvent::RunPaletteAction(PaletteAction::OpenConfig), cx)
        });
        let sent = sent_commands(&mut script);
        assert_eq!(
            (sent[0].1.as_str(), sent[0].2.clone()),
            (
                "open_configuration",
                Some(serde_json::json!({ "project_id": "/repo" }))
            ),
            "resolved for the project this window shows"
        );
        script.send(ack(sent[0].0, AckOutcome::Applied, "Opened configuration."));
        script.send(LinkEvent::Configuration(config_view(sent[0].0, false)));
        turn(&model, cx);
        let Some(OverlayView::Config(view)) = model.read_with(cx, |m, _| m.overlay()) else {
            panic!("the configuration card is up");
        };
        assert_eq!(view.rows[0].value, "off");

        // Toggle the row, then save: one command, naming the project.
        model.update(cx, |m, cx| {
            m.dispatch(HostEvent::Overlay(OverlayInput::Submit), cx);
            m.dispatch(HostEvent::Overlay(OverlayInput::ConfigSave), cx);
        });
        let sent = sent_commands(&mut script);
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].1, "open_configuration");
        let request: ConfigSaveRequest =
            serde_json::from_value(sent[0].2.clone().unwrap()).unwrap();
        assert_eq!(
            request,
            ConfigSaveRequest {
                changes: vec![ConfigChange {
                    scope: ConfigScopeName::Project,
                    key: "notifications.enabled".to_string(),
                    value: Some(ConfigValue::Bool(true)),
                }],
                project_id: Some("/repo".into()),
            }
        );

        // The host's answer is the card's new truth.
        script.send(LinkEvent::Configuration(config_view(sent[0].0, true)));
        turn(&model, cx);
        let Some(OverlayView::Config(view)) = model.read_with(cx, |m, _| m.overlay()) else {
            panic!("still up");
        };
        assert_eq!(view.rows[0].value, "on");
        assert!(!view.dirty);
        assert_eq!(view.status.as_deref(), Some("Saved."));

        // A refused save keeps the edit and says why on the card.
        model.update(cx, |m, cx| {
            m.dispatch(HostEvent::Overlay(OverlayInput::Submit), cx);
            m.dispatch(HostEvent::Overlay(OverlayInput::ConfigSave), cx);
        });
        let seq = sent_commands(&mut script)[0].0;
        script.send(ack(
            seq,
            AckOutcome::Rejected,
            "notifications.enabled: not a boolean",
        ));
        turn(&model, cx);
        let Some(OverlayView::Config(view)) = model.read_with(cx, |m, _| m.overlay()) else {
            panic!("still up");
        };
        assert!(view.dirty);
        assert_eq!(
            view.status.as_deref(),
            Some("notifications.enabled: not a boolean")
        );

        // Esc closes it.
        model.update(cx, |m, cx| {
            m.dispatch(HostEvent::Overlay(OverlayInput::Cancel), cx)
        });
        assert!(model.read_with(cx, |m, _| m.overlay()).is_none());
    }
}
