//! The Download tab's transitions: browsing, ticking, the local clash
//! question, the worker's plan, the transfer, stale answers dropped and
//! refusals said.

use super::*;
use crate::web::protocol::{DownloadPlanFile, ErrorCode};

fn file(name: &str, size: u64) -> FileEntryView {
    FileEntryView {
        name: name.to_string(),
        path: format!("/home/u/proj/{name}"),
        size,
    }
}

fn folder(name: &str) -> DirEntryView {
    DirEntryView {
        name: name.to_string(),
        path: format!("/home/u/proj/{name}"),
        is_git_repo: false,
        is_open_project: false,
    }
}

fn listing(seq: u64, path: &str, files: Vec<FileEntryView>) -> DirListing {
    DirListing {
        seq,
        path: path.to_string(),
        parent: Some("/home/u".to_string()),
        home: "/home/u".to_string(),
        entries: vec![folder("src")],
        files,
        truncated: false,
    }
}

fn ack(seq: u64, outcome: AckOutcome, detail: Option<&str>) -> Ack {
    Ack {
        seq,
        outcome,
        detail: detail.map(str::to_string),
    }
}

fn row(name: &str, size: u64, ticket: Option<&str>) -> DownloadPlanFile {
    DownloadPlanFile {
        name: name.to_string(),
        size,
        ticket: ticket.map(str::to_string),
    }
}

fn plan(seq: u64, files: Vec<DownloadPlanFile>) -> DownloadPlan {
    DownloadPlan {
        seq,
        dir: "/home/u/proj".to_string(),
        files,
        refusal: None,
    }
}

fn dest() -> PathBuf {
    PathBuf::from("/local/Downloads")
}

/// A panel listing `/home/u/proj` (seq 1) with `a.png` (10) and `b.txt` (20).
fn browsing() -> DownloadPanel {
    let mut panel = DownloadPanel::new("~/proj", "/home/u", "/local/Downloads");
    panel.browse_sent(Some(1));
    assert!(panel.loading);
    assert!(panel.on_listing(listing(
        1,
        "/home/u/proj",
        vec![file("a.png", 10), file("b.txt", 20)]
    )));
    panel
}

/// [`browsing`] with both ticked, asked as seq 5 (nothing in the way here).
fn requested() -> DownloadPanel {
    let mut panel = browsing();
    assert!(panel.select_all());
    let request = panel.request(Some(dest()), |_| false).expect("a request");
    assert_eq!(request.dir, "/home/u/proj");
    assert_eq!(request.names, ["a.png", "b.txt"]);
    panel.requested(Some(5));
    panel
}

/// [`requested`], granted.
fn downloading() -> DownloadPanel {
    let mut panel = requested();
    assert!(panel.on_plan(plan(
        5,
        vec![row("a.png", 10, Some("t-a")), row("b.txt", 20, Some("t-b"))],
    )));
    panel
}

// -- browsing -------------------------------------------------------------

#[test]
fn a_listing_fills_the_folder_and_a_stale_one_is_dropped() {
    let mut panel = DownloadPanel::new("~", "", "/local");
    panel.browse_sent(Some(1));
    panel.browse_sent(Some(2));
    // The answer to the first browse came late: not what is on screen now.
    assert!(!panel.on_listing(listing(1, "/home/u/old", vec![file("x", 1)])));
    assert!(!panel.listed);
    assert!(panel.on_listing(listing(2, "/home/u/proj", vec![file("a.png", 10)])));
    assert_eq!(panel.cwd, "/home/u/proj");
    assert_eq!(panel.home, "/home/u");
    assert_eq!(panel.parent.as_deref(), Some("/home/u"));
    assert_eq!(panel.folders[0].name, "src");
    assert_eq!(panel.files[0].name, "a.png");
    assert!(!panel.loading);
    // Answered once: the same listing again is not taken.
    assert!(!panel.on_listing(listing(2, "/home/u/proj", Vec::new())));
}

#[test]
fn ticks_are_kept_in_list_order_and_clear_on_navigation() {
    let mut panel = browsing();
    assert!(panel.toggle("b.txt"));
    assert!(panel.toggle("a.png"));
    assert_eq!(panel.selected, ["a.png", "b.txt"], "list order");
    assert_eq!(panel.selected_bytes(), 30);
    assert!(panel.toggle("b.txt"));
    assert_eq!(panel.selected, ["a.png"]);
    assert!(!panel.toggle("nope"), "only files listed");

    // The same folder listed again (show hidden) keeps what is still there.
    panel.browse_sent(Some(2));
    panel.on_listing(listing(2, "/home/u/proj", vec![file("a.png", 10)]));
    assert_eq!(panel.selected, ["a.png"]);

    // Another folder clears them.
    panel.browse_sent(Some(3));
    panel.on_listing(listing(3, "/home/u/proj/src", vec![file("a.png", 1)]));
    assert!(panel.selected.is_empty());
}

#[test]
fn select_all_ticks_every_file_then_none() {
    let mut panel = browsing();
    assert!(panel.select_all());
    assert_eq!(panel.selected, ["a.png", "b.txt"]);
    assert!(panel.select_all());
    assert!(panel.selected.is_empty());
    let mut empty = DownloadPanel::new("~", "", "/local");
    assert!(!empty.select_all(), "nothing to tick");
}

#[test]
fn a_refused_browse_is_the_note() {
    let mut panel = browsing();
    panel.browse_sent(Some(2));
    assert!(panel.on_ack(&ack(
        2,
        AckOutcome::Rejected,
        Some("~/x is outside your home folder.")
    )));
    assert!(!panel.loading);
    assert_eq!(
        panel.note.as_deref(),
        Some("~/x is outside your home folder.")
    );
    assert_eq!(panel.cwd, "/home/u/proj", "still on the folder it had");
    assert!(
        !panel.on_ack(&ack(99, AckOutcome::Rejected, None)),
        "not ours"
    );
}

#[test]
fn a_browse_that_could_not_be_sent_says_so() {
    let mut panel = DownloadPanel::new("~", "", "/local");
    panel.browse_sent(None);
    assert!(!panel.loading);
    assert_eq!(panel.note.as_deref(), Some(NOT_SENT));
}

// -- asking ---------------------------------------------------------------

#[test]
fn nothing_ticked_or_no_destination_sends_nothing() {
    let mut panel = browsing();
    assert_eq!(panel.request(Some(dest()), |_| false), None);
    assert_eq!(panel.note.as_deref(), Some(NOTHING_SELECTED));
    panel.toggle("a.png");
    assert_eq!(panel.request(None, |_| false), None);
    assert_eq!(panel.note.as_deref(), Some(NO_DESTINATION));
    assert!(panel.is_browsing());
}

#[test]
fn names_here_are_asked_about_before_the_worker() {
    let mut panel = browsing();
    panel.select_all();
    assert_eq!(panel.request(Some(dest()), |n| n == "b.txt"), None);
    assert_eq!(
        panel.phase,
        DownloadPhase::Asking {
            dest: dest(),
            clashes: vec!["b.txt".to_string()]
        }
    );
    // Cancel: back to browsing, ticks kept, nothing to send.
    assert_eq!(panel.resolve(Resolution::Cancel, |_| true), None);
    assert!(panel.is_browsing());
    assert_eq!(panel.selected.len(), 2);
}

#[test]
fn replace_overwrites_only_the_names_taken() {
    let mut panel = browsing();
    panel.select_all();
    panel.request(Some(dest()), |n| n == "b.txt");
    let request = panel
        .resolve(Resolution::Replace, |n| n == "b.txt")
        .unwrap();
    assert_eq!(request.names, ["a.png", "b.txt"]);
    panel.requested(Some(7));
    let DownloadPhase::Requested { planned, .. } = &panel.phase else {
        panic!("{:?}", panel.phase)
    };
    assert_eq!(
        planned
            .iter()
            .map(|p| (p.final_name.as_str(), p.replace))
            .collect::<Vec<_>>(),
        [("a.png", false), ("b.txt", true)]
    );
}

#[test]
fn keep_both_numbers_names_free_here_and_within_the_batch() {
    let mut panel = DownloadPanel::new("~", "", "/local");
    panel.browse_sent(Some(1));
    panel.on_listing(listing(
        1,
        "/home/u/proj",
        vec![file("a.txt", 1), file("a (1).txt", 1), file("b", 1)],
    ));
    panel.select_all();
    // Here already: a.txt, a (2).txt and b. `a (1).txt` is free here but
    // the batch brings one, so a.txt's copy may not take it.
    let here = |n: &str| ["a.txt", "a (2).txt", "b"].contains(&n);
    panel.request(Some(dest()), here);
    assert!(
        matches!(panel.phase, DownloadPhase::Asking { ref clashes, .. }
        if clashes == &["a.txt".to_string(), "b".to_string()])
    );
    let request = panel.resolve(Resolution::KeepBoth, here).unwrap();
    assert_eq!(request.names, ["a.txt", "a (1).txt", "b"], "worker names");
    panel.requested(Some(4));
    let DownloadPhase::Requested { planned, .. } = &panel.phase else {
        panic!()
    };
    let finals: Vec<&str> = planned.iter().map(|p| p.final_name.as_str()).collect();
    assert_eq!(finals, ["a (3).txt", "a (1).txt", "b (1)"]);
    assert!(planned.iter().all(|p| !p.replace));
}

#[test]
fn a_request_that_could_not_be_sent_goes_back_to_browsing() {
    let mut panel = browsing();
    panel.toggle("a.png");
    panel.request(Some(dest()), |_| false).unwrap();
    panel.requested(None);
    assert!(panel.is_browsing());
    assert_eq!(panel.note.as_deref(), Some(NOT_SENT));
}

// -- the worker's answers -----------------------------------------------------

#[test]
fn a_plan_for_an_older_request_is_dropped() {
    let mut panel = requested();
    assert!(!panel.on_plan(plan(4, vec![row("a.png", 10, Some("t"))])));
    assert!(matches!(panel.phase, DownloadPhase::Requested { .. }));
}

#[test]
fn a_refusal_is_the_note_back_on_the_folder() {
    let mut panel = requested();
    let mut refused = plan(5, Vec::new());
    refused.refusal = Some("b.txt is a folder.".to_string());
    assert!(panel.on_plan(refused));
    assert!(panel.is_browsing());
    assert_eq!(panel.note.as_deref(), Some("b.txt is a folder."));
    assert_eq!(panel.selected.len(), 2, "the ticks stay for another try");
}

#[test]
fn a_rejected_request_or_an_error_frame_is_the_note() {
    let mut panel = requested();
    assert!(panel.on_ack(&ack(5, AckOutcome::Rejected, Some("Too old."))));
    assert!(panel.is_browsing());
    assert_eq!(panel.note.as_deref(), Some("Too old."));

    let mut panel = requested();
    assert!(panel.on_error(&WireError {
        seq: Some(5),
        ..WireError::new(ErrorCode::NotSupported, "`begin_download` needs `names`")
    }));
    assert_eq!(
        panel.note.as_deref(),
        Some("`begin_download` needs `names`")
    );
}

#[test]
fn the_link_dropping_ends_a_request_but_not_a_transfer() {
    let mut panel = requested();
    assert!(panel.link_lost());
    assert_eq!(panel.note.as_deref(), Some(LINK_LOST));
    let mut panel = downloading();
    assert!(!panel.link_lost());
    assert!(panel.is_downloading());
}

#[test]
fn a_plan_with_no_tickets_grants_nothing() {
    let mut panel = requested();
    assert!(panel.on_plan(plan(5, vec![row("a.png", 10, None)])));
    assert!(panel.is_browsing());
    assert_eq!(panel.note.as_deref(), Some(NOTHING_GRANTED));
}

// -- the transfer ---------------------------------------------------------------

#[test]
fn files_come_in_one_at_a_time_and_a_failure_keeps_the_rest_going() {
    let mut panel = downloading();
    assert!(panel.is_busy());
    let first = panel.next_job().expect("the first file");
    assert_eq!(first.index, 0);
    assert_eq!(first.ticket, "t-a");
    assert_eq!(first.dest, dest());
    assert_eq!(first.final_name, "a.png");
    assert_eq!(first.size, 10);
    assert_eq!(panel.next_job(), None, "one at a time");

    // The worker's length wins over the plan's.
    assert!(panel.progress(0, 4, 12));
    assert_eq!(panel.progress_bytes(), Some((4, 32)));
    assert!(panel.finished(0, Err("The file changed on the worker.".to_string())));

    let second = panel.next_job().expect("the next one still goes");
    assert_eq!(second.index, 1);
    assert!(panel.finished(1, Ok(dest().join("b.txt"))));
    let DownloadPhase::Done {
        saved, failed, dir, ..
    } = &panel.phase
    else {
        panic!("{:?}", panel.phase)
    };
    assert_eq!(dir, "/home/u/proj");
    assert_eq!(saved, &[("b.txt".to_string(), dest().join("b.txt"))]);
    assert_eq!(
        failed,
        &[(
            "a.png".to_string(),
            "The file changed on the worker.".to_string()
        )]
    );
    assert!(!panel.is_busy());
}

#[test]
fn a_file_the_plan_left_out_fails_and_the_rest_come_in() {
    let mut panel = requested();
    panel.on_plan(plan(5, vec![row("b.txt", 20, Some("t-b"))]));
    let job = panel.next_job().unwrap();
    assert_eq!(job.index, 1, "a.png was not granted");
    panel.finished(1, Ok(dest().join("b.txt")));
    let DownloadPhase::Done { failed, .. } = &panel.phase else {
        panic!()
    };
    assert_eq!(failed, &[("a.png".to_string(), NOT_GRANTED.to_string())]);
}

#[test]
fn stale_transfer_events_change_nothing() {
    let mut panel = downloading();
    assert!(!panel.progress(0, 5, 10), "not started yet");
    assert!(!panel.finished(0, Ok(PathBuf::new())));
    panel.next_job();
    assert!(!panel.finished(9, Ok(PathBuf::new())));
}

#[test]
fn cancel_stops_and_start_over_browses_again() {
    let mut panel = downloading();
    panel.next_job();
    assert!(panel.cancel(), "a file was coming in");
    assert_eq!(panel.phase, DownloadPhase::Cancelled);
    panel.start_over();
    assert!(panel.is_browsing());
    assert!(panel.selected.is_empty());
    assert_eq!(panel.cwd, "/home/u/proj", "the same folder");
    let mut idle = requested();
    assert!(!idle.cancel(), "nothing was coming in");
}

// -- helpers ----------------------------------------------------------------

#[test]
fn folders_are_shown_with_a_tilde_for_home() {
    assert_eq!(tilde("/home/u", "/home/u"), "~");
    assert_eq!(tilde("/home/u/src/app", "/home/u/"), "~/src/app");
    assert_eq!(tilde("/home/user2/x", "/home/u"), "/home/user2/x");
    assert_eq!(tilde("/srv", ""), "/srv");
    assert_eq!(tilde(r"C:\Users\u\src", r"C:\Users\u"), r"~\src");
}

#[test]
fn a_destination_must_be_absolute_or_under_home() {
    assert_eq!(resolve_destination("  "), None);
    assert_eq!(resolve_destination("relative/dir"), None);
    let absolute = std::env::temp_dir();
    assert_eq!(
        resolve_destination(&absolute.display().to_string()),
        Some(absolute)
    );
    if let Some(home) = home_dir() {
        assert_eq!(resolve_destination("~"), Some(home.clone()));
        assert_eq!(
            resolve_destination("~/Downloads"),
            Some(home.join("Downloads"))
        );
    }
}

#[test]
fn a_name_is_taken_here_by_anything() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "x").unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    assert!(taken_here(dir.path(), "a.txt"));
    assert!(taken_here(dir.path(), "sub"));
    assert!(!taken_here(dir.path(), "b.txt"));
}
