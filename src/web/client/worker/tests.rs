//! The project picker's transitions: requests sent, answers folded in, stale
//! answers dropped, and every refusal on the error line.

use super::*;
use crate::web::protocol::ErrorCode;

fn listing(seq: u64, path: &str, parent: Option<&str>, names: &[&str]) -> DirListing {
    DirListing {
        seq,
        path: path.to_string(),
        parent: parent.map(str::to_string),
        home: "/home/u".to_string(),
        entries: names
            .iter()
            .map(|name| DirEntryView {
                name: name.to_string(),
                path: format!("{path}/{name}"),
                is_git_repo: name.ends_with("-repo"),
                is_open_project: false,
            })
            .collect(),
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

#[test]
fn browsing_shows_the_latest_listing_and_drops_a_stale_one() {
    let mut picker = ProjectPicker::new("/home/u");
    picker.browse_sent(Some(3));
    assert!(picker.is_busy());
    // The person moved on before the first answer came back.
    picker.browse_sent(Some(4));
    assert!(!picker.on_listing(listing(3, "/home/u", None, &["old"])));
    assert!(picker.cwd.is_none());

    assert!(picker.on_listing(listing(4, "/home/u/src", Some("/home/u"), &["a-repo", "b"])));
    assert!(!picker.is_busy());
    assert_eq!(picker.cwd.as_deref(), Some("/home/u/src"));
    assert_eq!(picker.parent.as_deref(), Some("/home/u"));
    let names: Vec<(&str, bool)> = picker
        .entries
        .iter()
        .map(|e| (e.name.as_str(), e.is_git_repo))
        .collect();
    assert_eq!(names, vec![("a-repo", true), ("b", false)]);

    // The applied ack for a browse says nothing the listing did not, and the
    // superseded browse's ack is the picker's too.
    assert!(picker.on_ack(&ack(4, AckOutcome::Applied, None)));
    assert!(picker.on_ack(&ack(3, AckOutcome::Applied, None)));
    assert!(picker.error.is_none());
}

#[test]
fn a_refused_browse_is_the_error_line_and_ends_the_request() {
    let mut picker = ProjectPicker::new("/home/u");
    picker.browse_sent(Some(7));
    assert!(picker.on_ack(&ack(
        7,
        AckOutcome::Rejected,
        Some("/etc is outside your home folder.")
    )));
    assert_eq!(
        picker.error.as_deref(),
        Some("/etc is outside your home folder.")
    );
    assert!(!picker.is_busy());
    // A late listing for the refused request changes nothing.
    assert!(!picker.on_listing(listing(7, "/etc", None, &["x"])));
}

#[test]
fn a_request_that_could_not_be_sent_says_so() {
    let mut picker = ProjectPicker::new("/home/u");
    picker.scan_sent(None);
    assert_eq!(picker.error.as_deref(), Some(NOT_SENT));
    assert!(!picker.is_busy());
    // The next request that does go out clears the line.
    picker.scan_sent(Some(1));
    assert!(picker.error.is_none());
}

#[test]
fn a_scan_lists_repositories_and_a_refused_one_says_why() {
    let mut picker = ProjectPicker::new("/home/u");
    picker.scan_sent(Some(2));
    assert!(picker.on_scan(ProjectScan {
        seq: 2,
        root: "/home/u/projects".to_string(),
        root_exists: true,
        repos: vec![ScannedRepo {
            name: "app".to_string(),
            path: "/home/u/projects/app".to_string(),
            is_open_project: true,
        }],
        refusal: None,
    }));
    let scanned = picker.scanned.clone().unwrap();
    assert!(scanned.root_exists);
    assert_eq!(scanned.repos[0].name, "app");
    assert!(picker.error.is_none());

    picker.scan_sent(Some(5));
    assert!(picker.on_scan(ProjectScan {
        seq: 5,
        root: "/srv".to_string(),
        root_exists: true,
        repos: Vec::new(),
        refusal: Some("The projects root /srv is outside your home folder.".to_string()),
    }));
    assert_eq!(
        picker.error.as_deref(),
        Some("The projects root /srv is outside your home folder.")
    );
}

#[test]
fn opening_a_project_reports_the_hosts_words_or_its_refusal() {
    let mut picker = ProjectPicker::new("/home/u");
    picker.open_sent(Some(9), "/home/u/src/a-repo");
    assert!(picker.on_ack(&ack(
        9,
        AckOutcome::Applied,
        Some("Opened project 'a-repo'.")
    )));
    assert_eq!(picker.status.as_deref(), Some("Opened project 'a-repo'."));
    assert_eq!(
        picker.opened,
        Some(OpenedProject {
            path: "/home/u/src/a-repo".to_string(),
            project_id: None,
        })
    );

    let mut picker = ProjectPicker::new("/home/u");
    picker.open_sent(Some(10), "/home/u/notes");
    assert!(picker.on_ack(&ack(
        10,
        AckOutcome::Rejected,
        Some("/home/u/notes is not a git repository.")
    )));
    assert_eq!(
        picker.error.as_deref(),
        Some("/home/u/notes is not a git repository.")
    );
    assert!(picker.opened.is_none());
    // Somebody else's ack is not the picker's.
    assert!(!picker.on_ack(&ack(11, AckOutcome::Rejected, Some("no"))));
}

#[test]
fn a_clone_shows_progress_then_opens_or_shows_gits_own_words() {
    let mut picker = ProjectPicker::new("/home/u");
    picker.clone_sent(Some(12), "git@github.com:me/app.git");
    assert!(picker.is_busy());
    assert!(picker.on_ack(&ack(
        12,
        AckOutcome::Applied,
        Some("Cloning into /home/u/projects/app")
    )));
    // Another window's clone is not this picker's.
    assert!(!picker.on_clone_progress(CloneProgress {
        clone_id: 99,
        url: "x".to_string(),
        phase: "Receiving objects".to_string(),
        percent: Some(10),
    }));
    assert!(picker.on_clone_event(CloneEvent::Progress(CloneProgress {
        clone_id: 12,
        url: "git@github.com:me/app.git".to_string(),
        phase: "Receiving objects".to_string(),
        percent: Some(45),
    })));
    let clone = picker.clone.clone().unwrap();
    assert_eq!(
        (clone.phase.as_str(), clone.percent),
        ("Receiving objects", Some(45))
    );
    assert!(picker.on_clone_event(CloneEvent::Result(CloneResult {
        clone_id: 12,
        url: "git@github.com:me/app.git".to_string(),
        ok: true,
        path: Some("/home/u/projects/app".to_string()),
        project_id: Some("/home/u/projects/app".into()),
        error: None,
    })));
    assert!(!picker.is_busy());
    assert_eq!(
        picker.clone.as_ref().unwrap().done,
        Some(Ok("/home/u/projects/app".to_string()))
    );
    assert_eq!(
        picker.opened.as_ref().and_then(|o| o.project_id.clone()),
        Some("/home/u/projects/app".into())
    );

    let mut picker = ProjectPicker::new("/home/u");
    picker.clone_sent(Some(13), "git@github.com:me/private.git");
    let words = "git@github.com: Permission denied (publickey).";
    assert!(picker.on_clone_result(CloneResult {
        clone_id: 13,
        url: "git@github.com:me/private.git".to_string(),
        ok: false,
        path: None,
        project_id: None,
        error: Some(words.to_string()),
    }));
    assert_eq!(picker.error.as_deref(), Some(words));
    assert_eq!(
        picker.clone.as_ref().unwrap().done,
        Some(Err(words.to_string()))
    );
    assert!(picker.opened.is_none());
}

#[test]
fn a_refused_clone_or_a_malformed_request_ends_it_with_the_reason() {
    let mut picker = ProjectPicker::new("/home/u");
    picker.clone_sent(Some(20), "-oops");
    assert!(picker.on_ack(&ack(
        20,
        AckOutcome::Rejected,
        Some("That is not a repository URL.")
    )));
    assert!(!picker.is_busy());
    assert_eq!(
        picker.error.as_deref(),
        Some("That is not a repository URL.")
    );

    let mut picker = ProjectPicker::new("/home/u");
    picker.browse_sent(Some(21));
    let mut error = WireError::new(ErrorCode::NotSupported, "browse_dir: path must be a string");
    error.seq = Some(21);
    assert!(picker.on_error(&error));
    assert_eq!(
        picker.error.as_deref(),
        Some("browse_dir: path must be a string")
    );
    assert!(!picker.is_busy());
}
