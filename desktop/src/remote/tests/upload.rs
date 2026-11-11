//! A worker window's upload entry points and panel (`specs/WORKER_PRD.md`
//! W34, protocol v8): the titlebar button and the palette row, the file
//! chooser, files dropped on the window, the panel's phases, its Choose… in
//! the project picker, and — against a real `PUT` endpoint on localhost — the
//! bytes going out.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use flightdeck::host::{HostEvent, OverlayInput, OverlayView};
use flightdeck::tui::palette::{FrontEndAction, PaletteAction};
use flightdeck::web::client::upload::{ItemState, UploadPhase};
use flightdeck::web::client::upload_http::HttpAccess;
use flightdeck::web::client::{AccessToken, LinkEvent, RemoteClient, UploadPanel};
use flightdeck::web::protocol::{
    DirEntryView, DirListing, UploadPlan, UploadPlanFile, UPLOAD_MIN_VERSION,
};
use gpui::{Entity, ExternalPaths, FileDropEvent, TestAppContext, VisualTestContext};

use super::worker::{sent_commands, worker_snapshot};
use super::{click, open_client, open_on, snapshot, turn};
use crate::remote::RemoteModel;

const DEFAULT_DIR: &str = "/repo/.flightdeck/worktrees/search";

fn upload_row() -> HostEvent {
    HostEvent::RunPaletteAction(PaletteAction::FrontEnd(FrontEndAction::UploadFiles))
}

/// Files on this machine, `(name, contents)`, in a fresh folder.
fn local_files(files: &[(&str, &str)]) -> (tempfile::TempDir, Vec<PathBuf>) {
    let dir = tempfile::tempdir().unwrap();
    let paths = files
        .iter()
        .map(|(name, body)| {
            let path = dir.path().join(name);
            std::fs::write(&path, body).unwrap();
            path
        })
        .collect();
    (dir, paths)
}

fn panel(model: &Entity<RemoteModel>, cx: &mut VisualTestContext) -> Option<UploadPanel> {
    model.read_with(cx, |m, _| m.upload().cloned())
}

/// Open the panel with `paths`, as a drop does.
fn open_panel(model: &Entity<RemoteModel>, cx: &mut VisualTestContext, paths: &[PathBuf]) {
    model.update(cx, |m, cx| {
        m.open_upload_with(paths);
        cx.notify();
    });
    cx.run_until_parked();
}

fn names(panel: &UploadPanel) -> Vec<String> {
    panel.files.iter().map(|f| f.name.clone()).collect()
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

/// Drop `paths` on the window, as the platform delivers a file drag.
fn drop_paths(cx: &mut VisualTestContext, paths: &[PathBuf]) {
    let position = cx
        .debug_bounds("remote-drop-target")
        .expect("the window takes drops")
        .center();
    cx.simulate_event(FileDropEvent::Entered {
        position,
        paths: ExternalPaths(paths.iter().cloned().collect()),
    });
    cx.simulate_event(FileDropEvent::Pending { position });
    cx.simulate_event(FileDropEvent::Submit { position });
    cx.run_until_parked();
}

/// The `begin_upload`s sent, as `(seq, args)`.
fn begin_uploads(script: &mut flightdeck::web::client::Script) -> Vec<(u64, serde_json::Value)> {
    sent_commands(script)
        .into_iter()
        .filter(|(_, name, _)| name == "begin_upload")
        .map(|(seq, _, args)| (seq, args.unwrap_or_default()))
        .collect()
}

fn plan_file(name: &str, final_name: &str, ticket: Option<&str>, exists: bool) -> UploadPlanFile {
    UploadPlanFile {
        name: name.to_string(),
        final_name: final_name.to_string(),
        ticket: ticket.map(str::to_string),
        exists,
    }
}

fn plan(seq: u64, files: Vec<UploadPlanFile>, conflicts: bool) -> LinkEvent {
    LinkEvent::UploadPlan(UploadPlan {
        seq,
        dir: DEFAULT_DIR.to_string(),
        files,
        conflicts,
        refusal: None,
    })
}

// --- the entry points ---------------------------------------------------

#[gpui::test]
fn a_host_that_is_not_a_worker_offers_no_upload(app: &mut TestAppContext) {
    let (model, _script, _window, cx) = open_on(app, snapshot());
    assert!(cx.debug_bounds("remote-upload-files").is_none());
    assert!(cx.debug_bounds("remote-drop-target").is_none());
    assert!(!palette_labels(&model, cx).contains(&"Upload Files…"));
}

#[gpui::test]
fn a_v7_worker_offers_no_upload(app: &mut TestAppContext) {
    let mut v7 = worker_snapshot();
    v7.protocol_version = UPLOAD_MIN_VERSION - 1;
    let (model, _script, _window, cx) = open_on(app, v7);
    assert!(model.read_with(cx, |m, _| m.is_worker()));
    assert!(cx.debug_bounds("remote-upload-files").is_none());
    assert!(cx.debug_bounds("remote-drop-target").is_none());
    assert!(!palette_labels(&model, cx).contains(&"Upload Files…"));
}

#[gpui::test]
fn a_v8_worker_offers_upload_in_the_titlebar_and_the_palette(app: &mut TestAppContext) {
    let (model, _script, _window, cx) = open_on(app, worker_snapshot());
    let button = cx
        .debug_bounds("remote-upload-files")
        .expect("Upload files… in the titlebar");
    let command = cx.debug_bounds("remote-command-field").unwrap();
    assert!(button.right() <= command.left(), "before the command field");
    assert!(cx.debug_bounds("remote-drop-target").is_some());
    assert!(palette_labels(&model, cx).contains(&"Upload Files…"));
}

#[gpui::test]
fn the_palette_row_asks_for_files_and_opens_the_panel_with_them(app: &mut TestAppContext) {
    let (_dir, paths) = local_files(&[("notes.md", "hello"), ("data.csv", "1,2,3\n")]);
    let (model, _script, _window, cx) = open_on(app, worker_snapshot());
    model.update(cx, |m, cx| m.dispatch(upload_row(), cx));
    cx.run_until_parked();
    assert!(cx.did_prompt_for_paths(), "the platform's file chooser");
    let reply = paths.clone();
    cx.simulate_path_prompt_response(move |options| {
        assert!(options.files && !options.directories && options.multiple);
        Some(reply.clone())
    });
    cx.run_until_parked();
    let panel = panel(&model, cx).expect("the panel opened");
    assert_eq!(names(&panel), ["notes.md", "data.csv"]);
    assert_eq!(panel.files[0].size, 5);
    assert_eq!(panel.dir, DEFAULT_DIR, "the worktree on screen");
    assert!(cx.debug_bounds("upload-panel").is_some());
    assert!(cx.debug_bounds("upload-file-1").is_some());
}

#[gpui::test]
fn the_titlebar_button_asks_for_files_and_a_cancelled_chooser_opens_nothing(
    app: &mut TestAppContext,
) {
    let (model, _script, _window, cx) = open_on(app, worker_snapshot());
    click(cx, "remote-upload-files");
    assert!(cx.did_prompt_for_paths());
    cx.simulate_path_prompt_response(|_| None);
    cx.run_until_parked();
    assert!(panel(&model, cx).is_none());
    assert!(cx.debug_bounds("upload-panel").is_none());
}

#[gpui::test]
fn dropped_files_open_the_panel_and_folders_are_left_out(app: &mut TestAppContext) {
    let (dir, paths) = local_files(&[("a.txt", "a")]);
    let folder = dir.path().join("photos");
    std::fs::create_dir(&folder).unwrap();
    let (model, _script, _window, cx) = open_on(app, worker_snapshot());
    drop_paths(cx, &[paths[0].clone(), folder]);
    let panel_now = panel(&model, cx).expect("the drop opened the panel");
    assert_eq!(names(&panel_now), ["a.txt"]);
    assert!(panel_now.note.as_deref().unwrap().contains("Folders"));
    assert!(cx.debug_bounds("upload-note").is_some());

    // A second drop, on the open panel, adds to its list.
    let (_more, more) = local_files(&[("b.txt", "bb")]);
    drop_paths(cx, &more);
    assert_eq!(names(&panel(&model, cx).unwrap()), ["a.txt", "b.txt"]);

    // The list is edited where it is drawn.
    click(cx, "upload-remove-0");
    assert_eq!(names(&panel(&model, cx).unwrap()), ["b.txt"]);
}

// --- the panel ----------------------------------------------------------

#[gpui::test]
fn upload_asks_the_worker_for_the_worktree_on_screen(app: &mut TestAppContext) {
    let (_dir, paths) = local_files(&[("a.txt", "abc")]);
    let (model, mut script, _window, cx) = open_on(app, worker_snapshot());
    script.sent();
    open_panel(&model, cx, &paths);
    click(cx, "overlay-button-upload-start");
    let sent = begin_uploads(&mut script);
    assert_eq!(sent.len(), 1, "{sent:?}");
    let args = &sent[0].1;
    assert_eq!(args["dir"], DEFAULT_DIR);
    assert_eq!(
        args["files"],
        serde_json::json!([{ "name": "a.txt", "size": 3 }])
    );
    assert_eq!(args["on_conflict"], "ask");
    assert!(cx.debug_bounds("upload-asking-worker").is_some());

    // A refusal is the panel's note, back on the list.
    script.send(LinkEvent::UploadPlan(UploadPlan {
        seq: sent[0].0,
        dir: String::new(),
        files: Vec::new(),
        conflicts: false,
        refusal: Some("~/x is outside your home folder.".to_string()),
    }));
    turn(&model, cx);
    let now = panel(&model, cx).unwrap();
    assert!(now.is_choosing());
    assert_eq!(
        now.note.as_deref(),
        Some("~/x is outside your home folder.")
    );
}

#[gpui::test]
fn upload_is_disabled_with_no_files_and_esc_closes_the_panel(app: &mut TestAppContext) {
    let (model, mut script, _window, cx) = open_on(app, worker_snapshot());
    open_panel(&model, cx, &[]);
    assert!(cx.debug_bounds("upload-empty").is_some());
    script.sent();
    click(cx, "overlay-button-upload-start");
    assert!(begin_uploads(&mut script).is_empty(), "nothing to send");
    cx.simulate_keystrokes("escape");
    assert!(panel(&model, cx).is_none());
    assert!(cx.debug_bounds("upload-panel").is_none());
}

#[gpui::test]
fn taken_names_ask_replace_keep_both_or_cancel(app: &mut TestAppContext) {
    let (_dir, paths) = local_files(&[("a.txt", "abc")]);
    let (model, mut script, _window, cx) = open_on(app, worker_snapshot());
    script.sent();
    open_panel(&model, cx, &paths);
    click(cx, "overlay-button-upload-start");
    let seq = begin_uploads(&mut script)[0].0;
    script.send(plan(
        seq,
        vec![plan_file("a.txt", "a.txt", None, true)],
        true,
    ));
    turn(&model, cx);
    assert!(cx.debug_bounds("upload-conflicts").is_some());
    assert!(cx.debug_bounds("upload-conflict-0").is_some());
    for button in [
        "overlay-button-upload-replace",
        "overlay-button-upload-keep-both",
        "overlay-button-upload-conflict-cancel",
    ] {
        assert!(cx.debug_bounds(button).is_some(), "{button}");
    }

    // Cancel: back to the list, nothing sent.
    click(cx, "overlay-button-upload-conflict-cancel");
    assert!(panel(&model, cx).unwrap().is_choosing());
    assert!(begin_uploads(&mut script).is_empty());

    // Asked again, Keep both asks once more with keep_both.
    click(cx, "overlay-button-upload-start");
    let seq = begin_uploads(&mut script)[0].0;
    script.send(plan(
        seq,
        vec![plan_file("a.txt", "a.txt", None, true)],
        true,
    ));
    turn(&model, cx);
    click(cx, "overlay-button-upload-keep-both");
    let sent = begin_uploads(&mut script);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].1["on_conflict"], "keep_both");
    assert_eq!(sent[0].1["dir"], DEFAULT_DIR);

    // And Replace, by its key, asks with replace.
    script.send(plan(
        sent[0].0,
        vec![plan_file("a.txt", "a.txt", None, true)],
        true,
    ));
    turn(&model, cx);
    cx.simulate_keystrokes("r");
    assert_eq!(begin_uploads(&mut script)[0].1["on_conflict"], "replace");
}

#[gpui::test]
fn choose_browses_the_workers_folders_and_sets_the_target(app: &mut TestAppContext) {
    let (_dir, paths) = local_files(&[("a.txt", "abc")]);
    let (model, mut script, _window, cx) = open_on(app, worker_snapshot());
    open_panel(&model, cx, &paths);
    script.sent();
    click(cx, "overlay-button-upload-choose");
    let sent = sent_commands(&mut script);
    let names: Vec<&str> = sent.iter().map(|c| c.1.as_str()).collect();
    assert_eq!(names, ["browse_dir"], "no scan, no clone: a folder");
    assert_eq!(sent[0].2.as_ref().unwrap()["path"], DEFAULT_DIR);
    assert!(model.read_with(cx, |m, _| m.choosing_upload_dir()));
    assert!(cx.debug_bounds("remote-picker").is_some());
    assert!(
        cx.debug_bounds("upload-panel").is_none(),
        "hidden meanwhile"
    );

    script.send(LinkEvent::DirListing(DirListing {
        seq: sent[0].0,
        path: "/home/dev/data".to_string(),
        parent: Some("/home/dev".to_string()),
        home: "/home/dev".to_string(),
        entries: vec![DirEntryView {
            name: "app".to_string(),
            path: "/home/dev/data/app".to_string(),
            is_git_repo: true,
            is_open_project: false,
        }],
        truncated: false,
    }));
    turn(&model, cx);
    assert!(cx.debug_bounds("remote-picker-choose-here").is_some());
    assert!(cx.debug_bounds("remote-picker-open-here").is_none());
    assert!(cx.debug_bounds("remote-picker-open-0").is_none(), "no Open");
    assert!(cx.debug_bounds("remote-picker-clone").is_none(), "no clone");

    click(cx, "remote-picker-choose-here");
    assert!(model.read_with(cx, |m, _| m.picker().is_none()));
    assert_eq!(panel(&model, cx).unwrap().dir, "/home/dev/data");
    assert!(
        cx.debug_bounds("upload-panel").is_some(),
        "back to the panel"
    );

    script.sent();
    click(cx, "overlay-button-upload-start");
    assert_eq!(begin_uploads(&mut script)[0].1["dir"], "/home/dev/data");
}

#[gpui::test]
fn back_from_choose_keeps_the_folder(app: &mut TestAppContext) {
    let (_dir, paths) = local_files(&[("a.txt", "abc")]);
    let (model, _script, _window, cx) = open_on(app, worker_snapshot());
    open_panel(&model, cx, &paths);
    click(cx, "overlay-button-upload-choose");
    click(cx, "remote-picker-close");
    assert!(cx.debug_bounds("upload-panel").is_some());
    assert_eq!(panel(&model, cx).unwrap().dir, DEFAULT_DIR);
}

#[gpui::test]
fn a_folder_typed_in_the_field_is_the_target(app: &mut TestAppContext) {
    let (_dir, paths) = local_files(&[("a.txt", "abc")]);
    let (model, mut script, window, cx) = open_on(app, worker_snapshot());
    open_panel(&model, cx, &paths);
    let view = window.read_with(cx, |w, _| w.upload_view()).unwrap();
    cx.update(|window, cx| view.update(cx, |v, cx| v.clear_folder(window, cx)));
    cx.run_until_parked();
    cx.simulate_input("~/inbox");
    assert_eq!(panel(&model, cx).unwrap().dir, "~/inbox");
    script.sent();
    click(cx, "overlay-button-upload-start");
    assert_eq!(begin_uploads(&mut script)[0].1["dir"], "~/inbox");
}

// --- the bytes ------------------------------------------------------------

/// One `PUT` the door took: its request line and headers, and its body.
struct Put {
    head: String,
    body: Vec<u8>,
}

/// A worker's upload door on localhost for `count` files: each `PUT` is read
/// whole, then (once `gate` says so, when given) answered as written.
fn upload_door(count: usize, gate: Option<mpsc::Receiver<()>>) -> (String, mpsc::Receiver<Put>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for _ in 0..count {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut raw = Vec::new();
            let mut buf = [0u8; 8192];
            let head_end = loop {
                let n = stream.read(&mut buf).unwrap_or(0);
                if n == 0 {
                    return;
                }
                raw.extend_from_slice(&buf[..n]);
                if let Some(at) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                    break at + 4;
                }
            };
            let head = String::from_utf8_lossy(&raw[..head_end]).into_owned();
            let length: usize = head
                .lines()
                .find_map(|l| {
                    l.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|v| v.trim().parse().unwrap())
                })
                .unwrap();
            while raw.len() < head_end + length {
                let n = stream.read(&mut buf).unwrap_or(0);
                if n == 0 {
                    return;
                }
                raw.extend_from_slice(&buf[..n]);
            }
            let body = raw[head_end..head_end + length].to_vec();
            if let Some(gate) = &gate {
                if gate.recv().is_err() {
                    return;
                }
            }
            let ticket = head
                .split_whitespace()
                .nth(1)
                .and_then(|path| path.rsplit('/').next())
                .unwrap_or_default()
                .to_string();
            let reply =
                format!("{{\"ok\":true,\"path\":\"{DEFAULT_DIR}/{ticket}\",\"bytes\":{length}}}");
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                reply.len()
            );
            let _ = tx.send(Put { head, body });
        }
    });
    (address, rx)
}

/// A worker window whose uploads go to `address`.
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

/// Turn the window until `done` holds, the transfer running on the shared
/// runtime meanwhile.
fn turn_until(
    model: &Entity<RemoteModel>,
    cx: &mut VisualTestContext,
    done: impl Fn(&UploadPanel) -> bool,
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

fn granted(paths: &[PathBuf]) -> Vec<UploadPlanFile> {
    paths
        .iter()
        .map(|p| {
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            plan_file(&name, &name, Some(&format!("ticket-{name}")), false)
        })
        .collect()
}

fn file_name(path: &Path) -> String {
    path.file_name().unwrap().to_string_lossy().into_owned()
}

#[gpui::test]
fn granted_files_go_out_one_by_one_and_the_panel_says_how_it_ended(app: &mut TestAppContext) {
    let (_dir, paths) = local_files(&[("a.txt", "alpha"), ("b.bin", "bravo-bravo")]);
    let (address, puts) = upload_door(2, None);
    let (model, mut script, _window, cx) = open_with_door(app, &address);
    script.sent();
    open_panel(&model, cx, &paths);
    click(cx, "overlay-button-upload-start");
    let seq = begin_uploads(&mut script)[0].0;
    script.send(plan(seq, granted(&paths), false));
    turn_until(&model, cx, |p| matches!(p.phase, UploadPhase::Done { .. }));

    let first = puts.recv_timeout(Duration::from_secs(5)).unwrap();
    let second = puts.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(
        first.head.starts_with("PUT /upload/ticket-a.txt HTTP/1.1"),
        "{}",
        first.head
    );
    assert!(
        first.head.contains("flightdeck_web=secret-token"),
        "the link's cookie"
    );
    assert_eq!(first.body, b"alpha");
    assert!(second.head.starts_with("PUT /upload/ticket-b.bin "));
    assert_eq!(second.body, b"bravo-bravo");

    let UploadPhase::Done {
        written, failed, ..
    } = panel(&model, cx).unwrap().phase
    else {
        unreachable!()
    };
    assert_eq!(written, [file_name(&paths[0]), file_name(&paths[1])]);
    assert!(failed.is_empty());
    assert!(cx.debug_bounds("upload-done").is_some());

    // Upload more: an empty list, the same folder.
    click(cx, "overlay-button-upload-more");
    let now = panel(&model, cx).unwrap();
    assert!(now.is_choosing() && now.files.is_empty());
    assert_eq!(now.dir, DEFAULT_DIR);
    click(cx, "overlay-button-upload-cancel");
    assert!(panel(&model, cx).is_none());
}

#[gpui::test]
fn the_uploading_panel_shows_progress_and_esc_twice_cancels(app: &mut TestAppContext) {
    let (_dir, paths) = local_files(&[("a.txt", "alpha"), ("b.txt", "bravo")]);
    let (gate, held) = mpsc::channel();
    let (address, _puts) = upload_door(2, Some(held));
    let (model, mut script, _window, cx) = open_with_door(app, &address);
    script.sent();
    open_panel(&model, cx, &paths);
    click(cx, "overlay-button-upload-start");
    let seq = begin_uploads(&mut script)[0].0;
    script.send(plan(seq, granted(&paths), false));
    // Every byte of the first file is out; the worker has not answered yet.
    turn_until(&model, cx, |p| match &p.phase {
        UploadPhase::Uploading { items, .. } => items[0].sent == items[0].total,
        _ => false,
    });
    assert!(cx.debug_bounds("upload-item-0").is_some());
    assert!(cx.debug_bounds("upload-item-1").is_some());
    assert!(cx.debug_bounds("upload-total").is_some());
    let UploadPhase::Uploading { items, .. } = panel(&model, cx).unwrap().phase else {
        unreachable!()
    };
    assert_eq!(items[0].state, ItemState::Sending);
    assert_eq!(items[1].state, ItemState::Waiting);

    // One stray Esc never cuts an upload off: it asks for another.
    cx.simulate_keystrokes("escape");
    assert!(panel(&model, cx).unwrap().is_uploading());
    assert!(cx.debug_bounds("upload-cancel-armed").is_some());
    cx.simulate_keystrokes("escape");
    assert_eq!(panel(&model, cx).unwrap().phase, UploadPhase::Cancelled);
    assert!(cx.debug_bounds("upload-cancelled").is_some());
    drop(gate);

    // Esc now closes it.
    cx.simulate_keystrokes("escape");
    assert!(panel(&model, cx).is_none());
}
