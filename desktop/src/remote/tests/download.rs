//! A worker window's Transfer Files panel, Download tab (protocol v9,
//! `remote-control-pz6d`): the tabs and the palette rows, browsing the
//! worker and ticking files, the clash question, the destination, refusals,
//! and — against a real `GET` endpoint on localhost — the bytes landing in a
//! temporary destination.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use flightdeck::host::{HostEvent, OverlayInput, OverlayView};
use flightdeck::tui::palette::{FrontEndAction, PaletteAction};
use flightdeck::web::client::download::{DownloadPhase, ItemState};
use flightdeck::web::client::upload_http::HttpAccess;
use flightdeck::web::client::{AccessToken, DownloadPanel, LinkEvent, RemoteClient};
use flightdeck::web::protocol::{
    DirEntryView, DirListing, DownloadPlan, DownloadPlanFile, FileEntryView, DOWNLOAD_MIN_VERSION,
};
use gpui::{Entity, TestAppContext, VisualTestContext};

use super::worker::{sent_commands, worker_snapshot};
use super::{click, open_client, open_on, turn};
use crate::remote::{RemoteModel, TransferTab};

const START: &str = "/repo/.flightdeck/worktrees/search";

fn download_row() -> HostEvent {
    HostEvent::RunPaletteAction(PaletteAction::FrontEnd(FrontEndAction::DownloadFiles))
}

fn panel(model: &Entity<RemoteModel>, cx: &mut VisualTestContext) -> Option<DownloadPanel> {
    model.read_with(cx, |m, _| m.download().cloned())
}

fn tab(model: &Entity<RemoteModel>, cx: &mut VisualTestContext) -> Option<TransferTab> {
    model.read_with(cx, |m, _| m.transfer_tab())
}

fn palette_labels(model: &Entity<RemoteModel>, cx: &mut VisualTestContext) -> Vec<&'static str> {
    model.update(cx, |m, _| m.apply(HostEvent::OpenPalette));
    let Some(OverlayView::Palette(view)) = model.read_with(cx, |m, _| m.overlay()) else {
        panic!("the palette is open");
    };
    let labels = view.entries.iter().map(|row| row.label).collect();
    model.update(cx, |m, _| m.apply(HostEvent::Overlay(OverlayInput::Cancel)));
    labels
}

/// The worker commands sent named `name`, as `(seq, args)`.
fn sent(script: &mut flightdeck::web::client::Script, name: &str) -> Vec<(u64, serde_json::Value)> {
    sent_commands(script)
        .into_iter()
        .filter(|(_, n, _)| n == name)
        .map(|(seq, _, args)| (seq, args.unwrap_or_default()))
        .collect()
}

fn listing(seq: u64, path: &str, files: &[(&str, u64)]) -> LinkEvent {
    LinkEvent::DirListing(DirListing {
        seq,
        path: path.to_string(),
        parent: Some("/home/dev".to_string()),
        home: "/home/dev".to_string(),
        entries: vec![DirEntryView {
            name: "docs".to_string(),
            path: format!("{path}/docs"),
            is_git_repo: false,
            is_open_project: false,
        }],
        files: files
            .iter()
            .map(|(name, size)| FileEntryView {
                name: name.to_string(),
                path: format!("{path}/{name}"),
                size: *size,
            })
            .collect(),
        truncated: false,
    })
}

/// Open the Download tab from the palette, answer its first browse with
/// `files`, and point it at a fresh destination folder here.
fn open_download(
    model: &Entity<RemoteModel>,
    script: &mut flightdeck::web::client::Script,
    cx: &mut VisualTestContext,
    files: &[(&str, u64)],
) -> tempfile::TempDir {
    script.sent();
    model.update(cx, |m, cx| m.dispatch(download_row(), cx));
    cx.run_until_parked();
    let browses = sent(script, "browse_dir");
    assert_eq!(browses.len(), 1, "{browses:?}");
    assert_eq!(browses[0].1["path"], START);
    assert_eq!(browses[0].1["include_files"], true);
    script.send(listing(browses[0].0, START, files));
    turn(model, cx);
    let dest = tempfile::tempdir().unwrap();
    let shown = dest.path().display().to_string();
    model.update(cx, |m, cx| {
        m.set_download_dest(&shown);
        cx.notify();
    });
    cx.run_until_parked();
    dest
}

fn plan(seq: u64, files: &[(&str, u64)]) -> LinkEvent {
    LinkEvent::DownloadPlan(DownloadPlan {
        seq,
        dir: START.to_string(),
        files: files
            .iter()
            .map(|(name, size)| DownloadPlanFile {
                name: name.to_string(),
                size: *size,
                ticket: Some(format!("ticket-{name}")),
            })
            .collect(),
        refusal: None,
    })
}

// --- tabs and entry points ------------------------------------------------

#[gpui::test]
fn a_v8_worker_has_no_download_tab_or_row(app: &mut TestAppContext) {
    let mut v8 = worker_snapshot();
    v8.protocol_version = DOWNLOAD_MIN_VERSION - 1;
    let (model, _script, _window, cx) = open_on(app, v8);
    let labels = palette_labels(&model, cx);
    assert!(labels.contains(&"Upload Files…"));
    assert!(!labels.contains(&"Download Files…"));
    click(cx, "remote-transfer-files");
    assert!(cx.debug_bounds("transfer-panel").is_some());
    assert!(
        cx.debug_bounds("transfer-tabs").is_none(),
        "no tabs: upload only"
    );
    cx.simulate_keystrokes("tab");
    assert_eq!(tab(&model, cx), Some(TransferTab::Upload));
    assert!(panel(&model, cx).is_none());
}

#[gpui::test]
fn the_palettes_download_row_opens_the_download_tab(app: &mut TestAppContext) {
    let (model, mut script, _window, cx) = open_on(app, worker_snapshot());
    assert!(palette_labels(&model, cx).contains(&"Download Files…"));
    let _dest = open_download(&model, &mut script, cx, &[("a.txt", 3)]);
    assert_eq!(tab(&model, cx), Some(TransferTab::Download));
    assert!(cx.debug_bounds("transfer-tabs").is_some());
    assert!(cx.debug_bounds("download-folder-0").is_some());
    assert!(cx.debug_bounds("download-file-0").is_some());
    assert!(cx.debug_bounds("download-path").is_some());
}

#[gpui::test]
fn tabs_switch_by_click_and_key_and_the_panel_reopens_on_the_last(app: &mut TestAppContext) {
    let (model, mut script, _window, cx) = open_on(app, worker_snapshot());
    let _dest = open_download(&model, &mut script, cx, &[("a.txt", 3)]);
    click(cx, "transfer-tab-upload");
    assert_eq!(tab(&model, cx), Some(TransferTab::Upload));
    assert!(cx.debug_bounds("upload-empty").is_some());
    cx.simulate_keystrokes("tab");
    assert_eq!(tab(&model, cx), Some(TransferTab::Download));
    assert!(
        cx.debug_bounds("download-file-0").is_some(),
        "as it was left"
    );
    cx.simulate_keystrokes("escape");
    assert_eq!(tab(&model, cx), None);
    // The titlebar opens it again on Download, the tab it was last on.
    click(cx, "remote-transfer-files");
    assert_eq!(tab(&model, cx), Some(TransferTab::Download));
}

// --- browsing and choosing ----------------------------------------------------

#[gpui::test]
fn folders_open_and_files_tick_by_click_and_keyboard(app: &mut TestAppContext) {
    let (model, mut script, _window, cx) = open_download_window(app);
    let _dest = open_download(&model, &mut script, cx, &[("a.txt", 3), ("b.bin", 2048)]);

    // A file row ticks; the footer counts what is ticked.
    click(cx, "download-file-1");
    assert_eq!(panel(&model, cx).unwrap().selected, ["b.bin"]);
    assert!(cx.debug_bounds("download-selected").is_some());

    // ↓ to the first file (folders come first), Space ticks it.
    cx.simulate_keystrokes("down down space");
    assert_eq!(panel(&model, cx).unwrap().selected, ["a.txt", "b.bin"]);
    // `a` with everything ticked unticks all.
    cx.simulate_keystrokes("a");
    assert!(panel(&model, cx).unwrap().selected.is_empty());

    // Show hidden lists the same folder again, with dotfiles.
    script.sent();
    click(cx, "download-hidden");
    let browses = sent(&mut script, "browse_dir");
    assert_eq!(browses[0].1["show_hidden"], true);
    assert_eq!(browses[0].1["path"], START);

    // A folder opens on click; Up and Home browse too.
    click(cx, "download-folder-0");
    let browses = sent(&mut script, "browse_dir");
    assert_eq!(browses.last().unwrap().1["path"], format!("{START}/docs"));
    click(cx, "overlay-button-download-home");
    let browses = sent(&mut script, "browse_dir");
    assert!(browses[0].1.get("path").is_none(), "home");
    assert_eq!(browses[0].1["include_files"], true);
}

/// A v9 worker's window (the scripted client).
fn open_download_window(
    app: &mut TestAppContext,
) -> (
    Entity<RemoteModel>,
    flightdeck::web::client::Script,
    Entity<super::RemoteWindow>,
    &mut VisualTestContext,
) {
    open_on(app, worker_snapshot())
}

#[gpui::test]
fn download_is_disabled_with_nothing_ticked(app: &mut TestAppContext) {
    let (model, mut script, _window, cx) = open_download_window(app);
    let _dest = open_download(&model, &mut script, cx, &[("a.txt", 3)]);
    script.sent();
    click(cx, "overlay-button-download-start");
    assert!(sent(&mut script, "begin_download").is_empty());
    assert!(cx.debug_bounds("download-note").is_some(), "says why");
}

#[gpui::test]
fn names_already_here_ask_replace_keep_both_or_cancel(app: &mut TestAppContext) {
    let (model, mut script, _window, cx) = open_download_window(app);
    let dest = open_download(&model, &mut script, cx, &[("a.txt", 3)]);
    std::fs::write(dest.path().join("a.txt"), "mine").unwrap();
    click(cx, "download-file-0");
    script.sent();
    click(cx, "overlay-button-download-start");
    assert!(cx.debug_bounds("download-clashes").is_some());
    assert!(cx.debug_bounds("download-clash-0").is_some());
    assert!(sent(&mut script, "begin_download").is_empty(), "not yet");

    click(cx, "overlay-button-download-clash-cancel");
    assert!(panel(&model, cx).unwrap().is_browsing());

    click(cx, "overlay-button-download-start");
    cx.simulate_keystrokes("k");
    let asked = sent(&mut script, "begin_download");
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].1["dir"], START);
    assert_eq!(asked[0].1["names"], serde_json::json!(["a.txt"]));
    let DownloadPhase::Requested { planned, .. } = panel(&model, cx).unwrap().phase else {
        panic!()
    };
    assert_eq!(planned[0].final_name, "a (1).txt");
    assert!(cx.debug_bounds("download-asking-worker").is_some());

    // While the worker is asked, the tabs stay put.
    click(cx, "transfer-tab-upload");
    assert_eq!(tab(&model, cx), Some(TransferTab::Download));

    // A refusal is the tab's note, back on the folder.
    script.send(LinkEvent::DownloadPlan(DownloadPlan {
        seq: asked[0].0,
        dir: START.to_string(),
        files: Vec::new(),
        refusal: Some("a.txt is not in your home folder.".to_string()),
    }));
    turn(&model, cx);
    assert!(panel(&model, cx).unwrap().is_browsing());
    assert!(cx.debug_bounds("download-note").is_some());
}

#[gpui::test]
fn choose_asks_the_platform_for_a_folder(app: &mut TestAppContext) {
    let (model, mut script, _window, cx) = open_download_window(app);
    let _dest = open_download(&model, &mut script, cx, &[("a.txt", 3)]);
    let elsewhere = tempfile::tempdir().unwrap();
    let reply = elsewhere.path().to_path_buf();
    click(cx, "overlay-button-download-choose");
    assert!(cx.did_prompt_for_paths());
    cx.simulate_path_prompt_response(move |options| {
        assert!(options.directories && !options.files && !options.multiple);
        Some(vec![reply.clone()])
    });
    cx.run_until_parked();
    assert_eq!(
        panel(&model, cx).unwrap().dest,
        elsewhere.path().display().to_string()
    );
}

#[gpui::test]
fn a_destination_typed_in_the_field_is_where_files_go(app: &mut TestAppContext) {
    let (model, mut script, window, cx) = open_download_window(app);
    let _dest = open_download(&model, &mut script, cx, &[("a.txt", 3)]);
    let view = window.read_with(cx, |w, _| w.upload_view()).unwrap();
    cx.update(|window, cx| view.update(cx, |v, cx| v.clear_dest(window, cx)));
    cx.run_until_parked();
    cx.simulate_input("~/inbox");
    assert_eq!(panel(&model, cx).unwrap().dest, "~/inbox");
}

// --- the bytes ------------------------------------------------------------

/// A worker's download door on localhost for `count` files: each `GET
/// /download/ticket-<name>` is answered with `body(name)` — or, once `hold`
/// is given, the first answer stops halfway and the door holds on.
fn download_door(
    count: usize,
    hold: Option<mpsc::Receiver<()>>,
) -> (String, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for _ in 0..count {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut raw = Vec::new();
            let mut buf = [0u8; 4096];
            while !raw.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = stream.read(&mut buf).unwrap_or(0);
                if n == 0 {
                    return;
                }
                raw.extend_from_slice(&buf[..n]);
            }
            let head = String::from_utf8_lossy(&raw).into_owned();
            let name = head
                .split_whitespace()
                .nth(1)
                .and_then(|path| path.strip_prefix("/download/ticket-"))
                .unwrap_or_default()
                .to_string();
            let _ = tx.send(head);
            let body = body(&name);
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\n\r\n",
                body.len()
            );
            if let Some(hold) = &hold {
                let _ = stream.write_all(&body[..body.len() / 2]);
                let _ = stream.flush();
                let _ = hold.recv();
                return;
            }
            let _ = stream.write_all(&body);
        }
    });
    (address, rx)
}

fn body(name: &str) -> Vec<u8> {
    format!("contents of {name}\n").repeat(1000).into_bytes()
}

fn open_with_door<'a>(
    app: &'a mut TestAppContext,
    address: &str,
) -> (
    Entity<RemoteModel>,
    flightdeck::web::client::Script,
    Entity<super::RemoteWindow>,
    &'a mut VisualTestContext,
) {
    let (mut client, script) = RemoteClient::scripted();
    client.set_http_access(Some(HttpAccess {
        address: address.to_string(),
        token: AccessToken::new("secret-token"),
        user_agent: "FlightDeck Desktop test".to_string(),
    }));
    open_client(app, client, script, worker_snapshot())
}

fn turn_until(
    model: &Entity<RemoteModel>,
    cx: &mut VisualTestContext,
    done: impl Fn(&DownloadPanel) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        turn(model, cx);
        if panel(model, cx).as_ref().is_some_and(&done) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out: {:?}",
            panel(model, cx).map(|p| p.phase)
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn names_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[gpui::test]
fn granted_files_come_in_one_by_one_and_land_in_the_destination(app: &mut TestAppContext) {
    let (address, heads) = download_door(2, None);
    let (model, mut script, _window, cx) = open_with_door(app, &address);
    let files = [("a.txt", body("a.txt").len() as u64), ("b.bin", 7)];
    let dest = open_download(&model, &mut script, cx, &files);
    cx.simulate_keystrokes("a");
    script.sent();
    cx.simulate_keystrokes("enter");
    let asked = sent(&mut script, "begin_download");
    assert_eq!(asked[0].1["names"], serde_json::json!(["a.txt", "b.bin"]));
    script.send(plan(asked[0].0, &files));
    turn_until(&model, cx, |p| {
        matches!(p.phase, DownloadPhase::Done { .. })
    });

    let first = heads.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(
        first.starts_with("GET /download/ticket-a.txt HTTP/1.1"),
        "{first}"
    );
    assert!(
        first.contains("flightdeck_web=secret-token"),
        "the link's cookie"
    );
    let second = heads.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(second.starts_with("GET /download/ticket-b.bin "));

    assert_eq!(
        std::fs::read(dest.path().join("a.txt")).unwrap(),
        body("a.txt")
    );
    assert_eq!(
        std::fs::read(dest.path().join("b.bin")).unwrap(),
        body("b.bin")
    );
    assert_eq!(
        names_in(dest.path()),
        ["a.txt", "b.bin"],
        "no temporary files"
    );

    let DownloadPhase::Done { saved, failed, .. } = panel(&model, cx).unwrap().phase else {
        unreachable!()
    };
    assert_eq!(saved.len(), 2);
    assert!(failed.is_empty());
    assert!(cx.debug_bounds("download-done").is_some());
    assert!(cx.debug_bounds("download-saved-1").is_some());
    assert!(cx.debug_bounds("overlay-button-download-reveal").is_some());

    // Download more: browsing the same folder again, nothing ticked.
    script.sent();
    click(cx, "overlay-button-download-more");
    let now = panel(&model, cx).unwrap();
    assert!(now.is_browsing() && now.selected.is_empty());
    assert_eq!(sent(&mut script, "browse_dir")[0].1["path"], START);
    click(cx, "overlay-button-download-cancel");
    assert_eq!(tab(&model, cx), None);
}

#[gpui::test]
fn the_downloading_tab_shows_progress_and_esc_twice_cancels(app: &mut TestAppContext) {
    let (release, held) = mpsc::channel();
    let (address, _heads) = download_door(1, Some(held));
    let (model, mut script, _window, cx) = open_with_door(app, &address);
    let files = [("a.txt", body("a.txt").len() as u64), ("b.bin", 7)];
    let dest = open_download(&model, &mut script, cx, &files);
    cx.simulate_keystrokes("a enter");
    let asked = sent(&mut script, "begin_download");
    script.send(plan(asked[0].0, &files));
    // Half the first file is in; the worker holds on.
    turn_until(&model, cx, |p| match &p.phase {
        DownloadPhase::Downloading { items, .. } => items[0].received > 0,
        _ => false,
    });
    assert!(cx.debug_bounds("download-item-0").is_some());
    assert!(cx.debug_bounds("download-item-1").is_some());
    assert!(cx.debug_bounds("download-total").is_some());
    let DownloadPhase::Downloading { items, .. } = panel(&model, cx).unwrap().phase else {
        unreachable!()
    };
    assert_eq!(items[0].state, ItemState::Receiving);
    assert_eq!(items[1].state, ItemState::Waiting);

    // No switching tabs mid-transfer.
    cx.simulate_keystrokes("tab");
    assert_eq!(tab(&model, cx), Some(TransferTab::Download));

    // One stray Esc never cuts a download off; the second does.
    cx.simulate_keystrokes("escape");
    assert!(panel(&model, cx).unwrap().is_downloading());
    assert!(cx.debug_bounds("download-cancel-armed").is_some());
    cx.simulate_keystrokes("escape");
    assert_eq!(panel(&model, cx).unwrap().phase, DownloadPhase::Cancelled);
    assert!(cx.debug_bounds("download-cancelled").is_some());

    // The half-written file is removed.
    let deadline = Instant::now() + Duration::from_secs(10);
    while !names_in(dest.path()).is_empty() {
        assert!(Instant::now() < deadline, "{:?}", names_in(dest.path()));
        std::thread::sleep(Duration::from_millis(10));
    }
    drop(release);

    // Esc now closes the panel.
    cx.simulate_keystrokes("escape");
    assert_eq!(tab(&model, cx), None);
}
