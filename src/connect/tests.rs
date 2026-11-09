//! `flightdeck connect` without a socket: the command line, and the terminal
//! UI driven over a scripted link (`RemoteClient::scripted`) and drawn into a
//! test backend.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use ratatui::Terminal;

use super::ui::{ConnectUi, Flow};
use super::*;
use crate::contracts::UiConfig;
use crate::web::client::session::tests::snapshot;
use crate::web::client::session::Tab;
use crate::web::client::{LinkEnd, LinkEvent, LinkOut, LinkState, Script};
use crate::web::protocol::{
    ClientMsg, Delta, DialogBody, DialogKey, DialogOrigin, DialogView, HostKind,
};

const AREA: Rect = Rect {
    x: 0,
    y: 0,
    width: 120,
    height: 36,
};

// -- the command line ---------------------------------------------------------

fn args(s: &[&str]) -> Vec<String> {
    s.iter().map(|a| a.to_string()).collect()
}

#[test]
fn the_command_line_names_a_remote_pairs_or_lists() {
    assert_eq!(
        parse(&args(&[])),
        Ok(Request::Connect {
            target: None,
            seat: SeatRequest::Write
        })
    );
    assert_eq!(
        parse(&args(&["studio", "--observe"])),
        Ok(Request::Connect {
            target: Some("studio".into()),
            seat: SeatRequest::Observe
        })
    );
    assert_eq!(
        parse(&args(&["--pair", "100.64.0.2"])),
        Ok(Request::Pair {
            address: "100.64.0.2:7420".into(),
            seat: SeatRequest::Write
        })
    );
    assert_eq!(parse(&args(&["--list"])), Ok(Request::List));
    assert_eq!(parse(&args(&["-h"])), Ok(Request::Help));
}

#[test]
fn a_bad_command_line_is_refused_in_words() {
    assert!(parse(&args(&["--pair"])).unwrap_err().contains("address"));
    assert!(parse(&args(&["--pair", "a", "b"])).is_err());
    assert!(parse(&args(&["--bogus"])).unwrap_err().contains("--bogus"));
    assert!(parse(&args(&["a", "b"])).is_err());
}

fn saved(label: &str, address: &str) -> SavedRemote {
    SavedRemote {
        label: label.to_string(),
        address: address.to_string(),
        token: "t".to_string(),
        last_seen: None,
        viewer_id: None,
        host_version: None,
        warned_unencrypted: false,
        ssh_target: None,
        host_kind: Some(HostKind::Worker),
    }
}

#[test]
fn a_remote_is_found_by_its_name_or_its_address() {
    let mut file = RemotesFile::default();
    file.upsert(saved("Studio", "100.64.0.2:7420"));
    file.upsert(saved("lab", "100.64.0.9:7420"));
    assert_eq!(find(&file, "studio").unwrap().address, "100.64.0.2:7420");
    assert_eq!(find(&file, "100.64.0.9").unwrap().label, "lab");
    assert_eq!(find(&file, "http://100.64.0.9:7420/").unwrap().label, "lab");
    assert!(find(&file, "nowhere").is_none());
    assert_eq!(host_label("100.64.0.2:7420"), "100.64.0.2");
    assert_eq!(host_label("[fd7a::1]:7420"), "fd7a::1");
}

// -- the terminal UI ----------------------------------------------------------

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

/// A UI on a scripted link, F2 leaving terminal focus on every platform.
fn open(worker: bool) -> (ConnectUi, Script) {
    let (client, script) = RemoteClient::scripted();
    let ui_config = UiConfig {
        use_f2_to_leave_terminal_focus: true,
        ..UiConfig::default()
    };
    let mut ui = ConnectUi::new(client, "studio", "100.64.0.2:7420", None, ui_config);
    ui.set_notifier(Rc::new(crate::testing::FakeNotifier::new()));
    script.snapshot(snapshot(worker, Vec::new()));
    ui.pump(AREA);
    (ui, script)
}

fn frames(script: &mut Script) -> Vec<ClientMsg> {
    script
        .sent()
        .into_iter()
        .filter_map(LinkOut::into_frame)
        .collect()
}

fn inputs(script: &mut Script) -> Vec<(String, Vec<u8>)> {
    frames(script)
        .into_iter()
        .filter_map(|f| match f {
            ClientMsg::Input(i) => Some((i.terminal_id.as_str().to_string(), i.data)),
            _ => None,
        })
        .collect()
}

fn commands(script: &mut Script) -> Vec<(String, Option<serde_json::Value>)> {
    frames(script)
        .into_iter()
        .filter_map(|f| match f {
            ClientMsg::Command(c) => Some((c.name, c.args)),
            _ => None,
        })
        .collect()
}

fn drawn(ui: &ConnectUi) -> String {
    let mut term = Terminal::new(TestBackend::new(AREA.width, AREA.height)).unwrap();
    term.draw(|frame| ui.draw(frame, 0)).unwrap();
    let buffer = term.backend().buffer().clone();
    (0..AREA.height)
        .map(|y| {
            (0..AREA.width)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn before_the_host_answers_it_says_it_is_connecting() {
    let (client, _script) = RemoteClient::scripted();
    let ui = ConnectUi::new(
        client,
        "studio",
        "100.64.0.2:7420",
        None,
        UiConfig::default(),
    );
    let screen = drawn(&ui);
    assert!(
        screen.contains("Connecting to studio (100.64.0.2:7420)"),
        "{screen}"
    );
}

#[test]
fn the_host_is_drawn_with_the_tuis_own_widgets_under_a_remote_header() {
    let (ui, _script) = open(false);
    let screen = drawn(&ui);
    assert!(screen.contains("⇄ FLIGHTDECK"), "{screen}");
    assert!(screen.contains("studio · live"), "{screen}");
    // The sidebar's rows are the host's sessions.
    assert!(screen.contains("login"), "{screen}");
    assert!(screen.contains("search"), "{screen}");
    // The project row is the host's.
    assert!(screen.contains("repo"), "{screen}");
}

#[test]
fn keys_reach_the_hosts_terminal_on_screen() {
    let (mut ui, mut script) = open(false);
    script.sent();
    assert_eq!(ui.key(key(KeyCode::Char('l'))), Flow::Continue);
    assert_eq!(
        inputs(&mut script),
        vec![("s2:primary".to_string(), b"l".to_vec())]
    );
}

#[test]
fn ctrl_q_quits_without_asking_the_host_anything() {
    let (mut ui, mut script) = open(false);
    script.sent();
    assert_eq!(ui.key(ctrl('q')), Flow::Quit);
    assert!(commands(&mut script).is_empty());
}

#[test]
fn the_palette_offers_the_hosts_rows_but_not_the_desktops_windows() {
    let (mut ui, _script) = open(false);
    ui.key(ctrl('g'));
    let screen = drawn(&ui);
    assert!(screen.contains("Command Palette"), "{screen}");
    assert!(!screen.contains("New Window"), "{screen}");
    assert!(!screen.contains("Connect to Remote"), "{screen}");
    assert!(!screen.contains("Rename Remote"), "{screen}");
    // Filtering narrows it; Esc closes it.
    for c in "help".chars() {
        ui.key(key(KeyCode::Char(c)));
    }
    assert!(drawn(&ui).contains("> help"));
    ui.key(key(KeyCode::Esc));
    assert!(!drawn(&ui).contains("Command Palette"));
}

#[test]
fn the_hosts_dialog_is_drawn_here_and_answered_by_its_key() {
    let (mut ui, mut script) = open(false);
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
    ui.pump(AREA);
    let screen = drawn(&ui);
    assert!(screen.contains("Close shell 1?"), "{screen}");
    assert!(screen.contains("[y] Close"), "{screen}");
    assert!(screen.contains("opened on the host"), "{screen}");

    script.sent();
    ui.key(key(KeyCode::Char('y')));
    assert_eq!(
        commands(&mut script),
        vec![(
            "dialog_confirm".to_string(),
            Some(serde_json::json!({ "dialog_id": "dialog-3", "choice": "y" }))
        )]
    );
}

#[test]
fn a_worker_s_shell_is_the_first_project_tab_and_takes_every_key() {
    let (mut ui, mut script) = open(true);
    let (tabs, active) = ui.project_tabs();
    assert_eq!(tabs[0].name, "worker shell");
    assert_eq!(tabs[1].name, "repo");
    assert_eq!(active, 1);
    assert!(drawn(&ui).contains("studio (worker)"));

    // Shift-Left steps back onto the shell.
    ui.key(KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT));
    assert_eq!(ui.session().tab(), Tab::WorkerShell);
    script.sent();
    ui.key(key(KeyCode::Char('x')));
    assert_eq!(
        inputs(&mut script),
        vec![(
            crate::web::protocol::WORKER_SHELL_TERMINAL_ID.to_string(),
            b"x".to_vec()
        )]
    );

    // The leave-focus key goes back to the project rather than nowhere.
    ui.key(key(KeyCode::F(2)));
    assert_eq!(ui.session().tab(), Tab::Project);
}

#[test]
fn a_worker_is_sized_to_this_terminal_s_pane() {
    // The first turn already sends it.
    let (mut ui, mut script) = open(true);
    let resized: Vec<(String, u16, u16)> = frames(&mut script)
        .into_iter()
        .filter_map(|f| match f {
            ClientMsg::ResizeTerminal(r) => {
                Some((r.terminal_id.as_str().to_string(), r.cols, r.rows))
            }
            _ => None,
        })
        .collect();
    // Sent once for the terminal on screen, at the pane's size: the screen
    // less the sidebar and the chrome rows.
    assert_eq!(resized.len(), 1, "{resized:?}");
    let (id, cols, rows) = &resized[0];
    assert_eq!(id, "s2:primary");
    assert!(*cols < AREA.width && *cols > 60, "cols {cols}");
    assert!(*rows < AREA.height && *rows > 20, "rows {rows}");
    // Once per size: the next turn sends nothing.
    ui.pump(AREA);
    assert!(!frames(&mut script)
        .iter()
        .any(|f| matches!(f, ClientMsg::ResizeTerminal(_))));
}

#[test]
fn open_project_on_a_worker_is_the_picker_and_clones_a_url() {
    let (mut ui, mut script) = open(true);
    ui.key(ctrl('g'));
    for c in "open project".chars() {
        ui.key(key(KeyCode::Char(c)));
    }
    ui.key(key(KeyCode::Enter));
    assert!(ui.session().picker().is_some(), "the picker opened");
    let screen = drawn(&ui);
    assert!(screen.contains("Open a project"), "{screen}");

    // Tab twice: scanned → browse → clone.
    ui.key(key(KeyCode::Tab));
    ui.key(key(KeyCode::Tab));
    ui.paste("git@github.com:me/app.git".to_string());
    script.sent();
    ui.key(key(KeyCode::Enter));
    let sent = commands(&mut script);
    assert!(
        sent.iter().any(|(name, args)| name == "clone_project"
            && args
                .as_ref()
                .is_some_and(|a| a["url"] == "git@github.com:me/app.git")),
        "{sent:?}"
    );
    ui.key(key(KeyCode::Esc));
    assert!(ui.session().picker().is_none());
}

#[test]
fn a_takeover_ends_on_a_screen_that_offers_reconnect() {
    let (mut ui, script) = open(true);
    script.send(LinkEvent::State(LinkState::Ended(LinkEnd::TakenOver {
        by: "laptop".to_string(),
    })));
    ui.pump(AREA);
    let screen = drawn(&ui);
    assert!(screen.contains("taken over by laptop"), "{screen}");
    assert!(screen.contains("reconnect"), "{screen}");

    let dialled = Rc::new(std::cell::Cell::new(0));
    let count = dialled.clone();
    ui.set_dialer(Rc::new(move || {
        count.set(count.get() + 1);
        RemoteClient::scripted().0
    }));
    assert_eq!(ui.key(key(KeyCode::Char('r'))), Flow::Continue);
    assert_eq!(dialled.get(), 1);
    assert!(drawn(&ui).contains("Connecting to studio"));
}

#[test]
fn a_newer_worker_is_told_to_update_this_flightdeck() {
    let (client, script) = RemoteClient::scripted();
    let mut ui = ConnectUi::new(
        client,
        "studio",
        "100.64.0.2:7420",
        None,
        UiConfig::default(),
    );
    let mut snap = snapshot(true, Vec::new());
    snap.host_version = "999.0.0".to_string();
    script.snapshot(snap);
    ui.pump(AREA);
    let notice = ui.session().notice().unwrap_or_default().to_string();
    assert!(notice.contains("999.0.0"), "{notice}");
    assert!(notice.contains("Update this FlightDeck"), "{notice}");
}
