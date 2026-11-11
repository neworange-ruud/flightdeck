//! The upload panel's transitions: picking, asking, conflicts, the transfer,
//! stale answers dropped and refusals said.

use super::*;
use crate::web::protocol::{ErrorCode, UploadPlanFile};

fn local(name: &str, size: u64) -> LocalFile {
    LocalFile {
        path: PathBuf::from("/local").join(name),
        name: name.to_string(),
        size,
    }
}

fn picked(names: &[(&str, u64)]) -> Vec<Picked> {
    names
        .iter()
        .map(|(name, size)| Picked::File(local(name, *size)))
        .collect()
}

fn ack(seq: u64, outcome: AckOutcome, detail: Option<&str>) -> Ack {
    Ack {
        seq,
        outcome,
        detail: detail.map(str::to_string),
    }
}

fn row(name: &str, final_name: &str, ticket: Option<&str>, exists: bool) -> UploadPlanFile {
    UploadPlanFile {
        name: name.to_string(),
        final_name: final_name.to_string(),
        ticket: ticket.map(str::to_string),
        exists,
    }
}

fn plan(seq: u64, files: Vec<UploadPlanFile>) -> UploadPlan {
    UploadPlan {
        seq,
        dir: "/home/u/proj".to_string(),
        files,
        conflicts: false,
        refusal: None,
    }
}

/// A panel with `a.png` (10 bytes) and `b.txt` (20 bytes), asked as seq 5.
fn requested() -> UploadPanel {
    let mut panel = UploadPanel::new("~/proj");
    panel.add(picked(&[("a.png", 10), ("b.txt", 20)]));
    let request = panel.request().expect("a request");
    assert_eq!(request.on_conflict, OnConflict::Ask);
    panel.requested(Some(5), request.on_conflict);
    panel
}

/// [`requested`], granted.
fn uploading() -> UploadPanel {
    let mut panel = requested();
    assert!(panel.on_plan(plan(
        5,
        vec![
            row("a.png", "a.png", Some("t-a"), false),
            row("b.txt", "b.txt", Some("t-b"), false),
        ],
    )));
    panel
}

// -- choosing -------------------------------------------------------------

#[test]
fn files_are_listed_once_per_name_and_folders_are_refused() {
    let mut panel = UploadPanel::new("~");
    assert!(panel.add(picked(&[("a.png", 1), ("b.png", 2)])));
    assert_eq!(panel.note, None);
    let mut again = picked(&[("a.png", 99)]);
    again.push(Picked::Folder(PathBuf::from("/local/dir")));
    again.push(Picked::Folder(PathBuf::from("/local/dir2")));
    again.push(Picked::Unreadable(
        PathBuf::from("/local/gone"),
        "no such file".into(),
    ));
    assert!(panel.add(again));
    let names: Vec<&str> = panel.files.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, vec!["a.png", "b.png"]);
    assert_eq!(panel.files[0].size, 1, "the first pick stays");
    let note = panel.note.clone().unwrap();
    assert!(note.contains("a.png is already in the list."), "{note}");
    assert_eq!(note.matches(FOLDER_REFUSED).count(), 1, "said once: {note}");
    assert!(note.contains("Could not read /local/gone"), "{note}");
    assert_eq!(panel.total_bytes(), 3);

    assert!(panel.remove(0));
    assert!(!panel.remove(5));
    assert_eq!(panel.files.len(), 1);
    assert!(panel.set_dir("~/elsewhere"));
    assert_eq!(panel.dir, "~/elsewhere");
}

#[test]
fn nothing_to_upload_is_said_and_nothing_sent() {
    let mut panel = UploadPanel::new("~");
    assert_eq!(panel.request(), None);
    assert_eq!(panel.note.as_deref(), Some(NO_FILES));
    assert!(panel.is_choosing());
}

#[test]
fn a_request_that_could_not_be_sent_stays_choosing() {
    let mut panel = UploadPanel::new("~");
    panel.add(picked(&[("a", 1)]));
    let request = panel.request().unwrap();
    panel.requested(None, request.on_conflict);
    assert!(panel.is_choosing());
    assert_eq!(panel.note.as_deref(), Some(NOT_SENT));
}

#[test]
fn the_request_names_the_folder_and_every_file() {
    let mut panel = UploadPanel::new("  ~/proj ");
    panel.add(picked(&[("a.png", 10), ("b.txt", 20)]));
    let request = panel.request().unwrap();
    assert_eq!(request.dir, "~/proj");
    assert_eq!(
        request.files,
        vec![
            UploadFile {
                name: "a.png".into(),
                size: 10
            },
            UploadFile {
                name: "b.txt".into(),
                size: 20
            },
        ]
    );
}

#[test]
fn the_list_is_frozen_while_the_worker_is_asked() {
    let mut panel = requested();
    assert!(!panel.add(picked(&[("c", 1)])));
    assert!(!panel.remove(0));
    assert!(!panel.set_dir("~"));
    assert_eq!(panel.request(), None);
}

// -- the worker's answers -------------------------------------------------

#[test]
fn a_stale_plan_is_dropped() {
    let mut panel = requested();
    assert!(!panel.on_plan(plan(4, vec![row("a.png", "a.png", Some("t"), false)])));
    assert!(matches!(panel.phase, UploadPhase::Requested { seq: 5, .. }));
}

#[test]
fn a_refused_plan_goes_back_to_the_list_with_the_reason() {
    let mut panel = requested();
    let mut refused = plan(5, Vec::new());
    refused.refusal = Some("~/x is outside your home folder.".into());
    assert!(panel.on_plan(refused));
    assert!(panel.is_choosing());
    assert_eq!(
        panel.note.as_deref(),
        Some("~/x is outside your home folder.")
    );
    assert_eq!(panel.files.len(), 2, "the list is kept");
}

#[test]
fn a_rejected_ack_or_an_error_ends_the_request() {
    let mut panel = requested();
    assert!(panel.on_ack(&ack(
        5,
        AckOutcome::Rejected,
        Some("Not enough free space.")
    )));
    assert!(panel.is_choosing());
    assert_eq!(panel.note.as_deref(), Some("Not enough free space."));

    let mut panel = requested();
    let error = WireError {
        seq: Some(5),
        ..WireError::new(
            ErrorCode::NotSupported,
            "Uploading files needs a newer FlightDeck",
        )
    };
    assert!(panel.on_error(&error));
    assert!(panel.note.as_deref().unwrap().contains("newer FlightDeck"));

    // Not the panel's.
    let mut panel = requested();
    assert!(!panel.on_ack(&ack(99, AckOutcome::Rejected, None)));
    assert!(matches!(panel.phase, UploadPhase::Requested { .. }));
    // An applied ack is the panel's but says nothing the plan will not.
    assert!(panel.on_ack(&ack(
        5,
        AckOutcome::Applied,
        Some("2 of those names are taken")
    )));
    assert!(matches!(panel.phase, UploadPhase::Requested { .. }));
}

#[test]
fn a_dropped_link_ends_an_unanswered_request_but_not_a_transfer() {
    let mut panel = requested();
    assert!(panel.link_lost());
    assert_eq!(panel.note.as_deref(), Some(LINK_LOST));
    let mut panel = uploading();
    assert!(!panel.link_lost());
    assert!(panel.is_uploading());
}

// -- conflicts --------------------------------------------------------------

#[test]
fn taken_names_are_asked_about_and_resolved_by_asking_again() {
    let mut panel = requested();
    let mut conflicted = plan(
        5,
        vec![
            row("a.png", "a.png", None, true),
            row("b.txt", "b.txt", None, false),
        ],
    );
    conflicted.conflicts = true;
    assert!(panel.on_plan(conflicted));
    assert_eq!(
        panel.phase,
        UploadPhase::Asking {
            dir: "/home/u/proj".into(),
            conflicts: vec!["a.png".into()],
        }
    );

    let again = panel.resolve(Resolution::KeepBoth).unwrap();
    assert_eq!(again.on_conflict, OnConflict::KeepBoth);
    assert_eq!(again.files.len(), 2);
    panel.requested(Some(6), again.on_conflict);
    assert!(matches!(
        panel.phase,
        UploadPhase::Requested {
            seq: 6,
            on_conflict: OnConflict::KeepBoth
        }
    ));
    // The answer to the first ask is stale now.
    assert!(!panel.on_plan(plan(5, vec![row("a.png", "a.png", Some("x"), true)])));
    assert!(panel.on_plan(plan(
        6,
        vec![
            row("a.png", "a (1).png", Some("t-a"), true),
            row("b.txt", "b.txt", Some("t-b"), false),
        ],
    )));
    let UploadPhase::Uploading { items, .. } = &panel.phase else {
        panic!("{:?}", panel.phase);
    };
    assert_eq!(items[0].final_name, "a (1).png");
    // The superseded seq's ack is still recognised as the panel's.
    assert!(panel.on_ack(&ack(5, AckOutcome::Applied, None)));
}

#[test]
fn replace_asks_again_and_cancel_goes_back_to_the_list() {
    let mut panel = requested();
    let mut conflicted = plan(5, vec![row("a.png", "a.png", None, true)]);
    conflicted.conflicts = true;
    panel.on_plan(conflicted.clone());
    assert_eq!(
        panel.resolve(Resolution::Replace).map(|r| r.on_conflict),
        Some(OnConflict::Replace)
    );
    assert_eq!(panel.resolve(Resolution::Cancel), None);
    assert!(panel.is_choosing());
    assert_eq!(
        panel.resolve(Resolution::Replace),
        None,
        "nothing to resolve"
    );
}

// -- the transfer -----------------------------------------------------------

#[test]
fn files_go_one_at_a_time_and_progress_adds_up() {
    let mut panel = uploading();
    assert_eq!(panel.progress_bytes(), Some((0, 30)));
    let first = panel.next_job().expect("the first file");
    assert_eq!(
        first,
        UploadJob {
            index: 0,
            local: PathBuf::from("/local/a.png"),
            ticket: "t-a".into(),
            size: 10,
        }
    );
    assert_eq!(panel.next_job(), None, "one at a time");
    assert!(panel.progress(0, 4));
    assert!(!panel.progress(0, 4), "no change, no repaint");
    assert!(!panel.progress(1, 4), "not sending yet");
    assert_eq!(panel.progress_bytes(), Some((4, 30)));
    assert!(panel.finished(0, Ok("/home/u/proj/a.png".into())));
    assert_eq!(panel.progress_bytes(), Some((10, 30)));
    assert!(!panel.finished(0, Ok("again".into())), "once");

    let second = panel.next_job().expect("the second file");
    assert_eq!(second.index, 1);
    assert!(panel.finished(1, Err("The worker ran out of disk space.".into())));
    assert_eq!(
        panel.phase,
        UploadPhase::Done {
            dir: "/home/u/proj".into(),
            written: vec!["a.png".into()],
            failed: vec![("b.txt".into(), "The worker ran out of disk space.".into())],
        }
    );
    assert_eq!(panel.next_job(), None);
    panel.start_over();
    assert!(panel.is_choosing() && panel.files.is_empty());
    assert_eq!(panel.dir, "~/proj", "the folder is kept");
}

#[test]
fn a_plan_row_for_a_file_not_in_the_list_is_ignored() {
    let mut panel = requested();
    assert!(panel.on_plan(plan(
        5,
        vec![
            row("a.png", "a.png", Some("t-a"), false),
            row("zzz", "zzz", Some("t-z"), false),
            row("b.txt", "b.txt", None, false),
        ],
    )));
    let UploadPhase::Uploading { items, .. } = &panel.phase else {
        panic!("{:?}", panel.phase);
    };
    assert_eq!(items.len(), 1);
    let mut empty = requested();
    assert!(empty.on_plan(plan(5, Vec::new())));
    assert!(empty.is_choosing());
    assert!(empty.note.is_some());
}

#[test]
fn cancelling_says_whether_a_transfer_must_be_stopped() {
    let mut panel = uploading();
    assert!(!panel.clone().cancel(), "granted, nothing sending yet");
    panel.next_job();
    assert!(panel.cancel());
    assert_eq!(panel.phase, UploadPhase::Cancelled);
    assert!(
        !panel.finished(0, Ok("late".into())),
        "a late end is dropped"
    );
    let mut panel = requested();
    assert!(!panel.cancel());
    assert_eq!(panel.phase, UploadPhase::Cancelled);
}

// -- helpers ---------------------------------------------------------------------

#[test]
fn the_default_folder_follows_the_selection_then_home() {
    use crate::web::client::session::tests::snapshot;
    let mut ws = RemoteWorkspace::new();
    assert_eq!(default_dir(&ws), "~", "nothing known yet");

    // The selected session's worktree.
    ws.apply_snapshot(snapshot(true, Vec::new()));
    assert_eq!(default_dir(&ws), "/repo/.flightdeck/worktrees/search");

    // A session from a host that does not say where it runs: its project.
    let mut no_path = snapshot(true, Vec::new());
    for session in &mut no_path.projects[0].sessions {
        session.worktree_path.clear();
    }
    let mut ws = RemoteWorkspace::new();
    ws.apply_snapshot(no_path);
    assert_eq!(default_dir(&ws), "/repo");

    // No project: the worker's home.
    let mut empty = snapshot(true, Vec::new());
    empty.projects.clear();
    let mut ws = RemoteWorkspace::new();
    ws.apply_snapshot(empty);
    assert_eq!(default_dir(&ws), "/home/dev");
}

#[test]
fn inspecting_tells_a_file_from_a_folder() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("notes.md");
    std::fs::write(&file, "hello").unwrap();
    assert_eq!(
        inspect(&file),
        Picked::File(LocalFile {
            path: file.clone(),
            name: "notes.md".into(),
            size: 5,
        })
    );
    assert_eq!(
        inspect(dir.path()),
        Picked::Folder(dir.path().to_path_buf())
    );
    assert!(matches!(
        inspect(&dir.path().join("nope")),
        Picked::Unreadable(..)
    ));
}
