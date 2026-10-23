use super::*;
use crate::contracts::RealFs;
use crate::testing::FakeFs;
use std::time::{Duration, Instant};

fn line(phase: &str, percent: Option<u8>) -> CloneLine {
    CloneLine {
        phase: phase.to_string(),
        percent,
    }
}

#[test]
fn parse_progress_table() {
    let cases: &[(&str, Option<CloneLine>)] = &[
        (
            "Receiving objects:  45% (450/1000), 1.2 MiB | 2 MiB/s",
            Some(line("Receiving objects", Some(45))),
        ),
        (
            "Receiving objects: 100% (1000/1000), done.",
            Some(line("Receiving objects", Some(100))),
        ),
        (
            "Resolving deltas:   3% (1/30)",
            Some(line("Resolving deltas", Some(3))),
        ),
        (
            "remote: Counting objects: 100% (5/5), done.",
            Some(line("Counting objects", Some(100))),
        ),
        (
            "  remote: Compressing objects:  50% (1/2)  ",
            Some(line("Compressing objects", Some(50))),
        ),
        ("Cloning into 'x'...", None),
        ("fatal: repository '/x' does not exist", None),
        ("fatal: unable to access 'https://h/100%25/': nope", None),
        ("Receiving objects: done", None),
        (": 50%", None),
        ("", None),
    ];
    for (input, want) in cases {
        assert_eq!(&parse_progress(input), want, "{input}");
    }
}

#[test]
fn derive_name_table() {
    let cases: &[(&str, Option<&str>)] = &[
        ("https://github.com/o/repo.git", Some("repo")),
        ("https://github.com/o/repo", Some("repo")),
        ("https://github.com/o/repo/", Some("repo")),
        ("git@github.com:o/repo.git", Some("repo")),
        ("git@host:repo.git", Some("repo")),
        ("ssh://git@h:22/o/my.repo-2_x.git", Some("my.repo-2_x")),
        ("/srv/git/thing.git", Some("thing")),
        ("  https://h/o/r.git  ", Some("r")),
        ("https://h/o/..", None),
        ("https://h/o/.", None),
        ("https://h/o/.git", None),
        ("https://h/o/re po", None),
        ("https://h/o/re$po", None),
        ("", None),
        ("/", None),
    ];
    for (input, want) in cases {
        assert_eq!(derive_name(input).as_deref(), *want, "{input:?}");
    }
}

fn plan_fixture() -> (FakeFs, Confinement) {
    let fs = FakeFs::new().with_dir("/home/u/code/taken");
    let c = Confinement::new(&fs, Path::new("/home/u")).unwrap();
    (fs, c)
}

#[test]
fn plan_clone_derives_or_takes_a_name() {
    let (fs, c) = plan_fixture();
    let root = Path::new("/home/u/code");
    assert_eq!(
        plan_clone(&fs, &c, "https://h/o/new.git", None, root).unwrap(),
        PathBuf::from("/home/u/code/new")
    );
    assert_eq!(
        plan_clone(&fs, &c, "https://h/o/new.git", Some(" mine "), root).unwrap(),
        PathBuf::from("/home/u/code/mine")
    );
    assert_eq!(
        plan_clone(&fs, &c, "https://h/o/new.git", Some(""), root).unwrap(),
        PathBuf::from("/home/u/code/new")
    );
}

#[test]
fn plan_clone_refusals() {
    let (fs, c) = plan_fixture();
    let root = Path::new("/home/u/code");
    for url in ["", "   ", "--upload-pack=evil", "-x"] {
        assert!(plan_clone(&fs, &c, url, None, root).is_err(), "{url:?}");
    }
    // Underivable and invalid names.
    assert!(plan_clone(&fs, &c, "https://h/o/..", None, root).is_err());
    for name in ["..", "a/b", "x y", "."] {
        assert!(
            plan_clone(&fs, &c, "https://h/o/r.git", Some(name), root).is_err(),
            "{name}"
        );
    }
    // Name in use.
    let e = plan_clone(&fs, &c, "https://h/o/taken.git", None, root).unwrap_err();
    assert!(
        e.contains("/home/u/code/taken") && e.contains("already exists"),
        "{e}"
    );
    // Root outside home, existing or not.
    for outside in ["/outside", "/elsewhere/new", "/home/u/../x"] {
        let e = plan_clone(&fs, &c, "https://h/o/r.git", None, Path::new(outside)).unwrap_err();
        assert!(e.contains("home"), "{outside}: {e}");
    }
    assert!(fs.writes().is_empty(), "refusals create nothing");
}

#[test]
fn plan_clone_refuses_a_root_that_links_out_of_home() {
    let (fs, c) = plan_fixture();
    let fs = fs.with_dir("/outside");
    fs.symlink(Path::new("/outside"), Path::new("/home/u/link"))
        .unwrap();
    let e = plan_clone(
        &fs,
        &c,
        "https://h/o/r.git",
        None,
        Path::new("/home/u/link/new"),
    )
    .unwrap_err();
    assert!(e.contains("home"), "{e}");
    assert!(!fs.exists(Path::new("/outside/new")));
}

#[test]
fn plan_clone_creates_a_missing_projects_root() {
    let (fs, c) = plan_fixture();
    let dest = plan_clone(
        &fs,
        &c,
        "https://h/o/r.git",
        None,
        Path::new("/home/u/Projects/work"),
    )
    .unwrap();
    assert_eq!(dest, PathBuf::from("/home/u/Projects/work/r"));
    assert!(fs.is_dir(Path::new("/home/u/Projects/work")));
}

fn wait_done(jobs: &mut CloneJobs) -> Vec<CloneEvent> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut all = Vec::new();
    while Instant::now() < deadline {
        all.extend(jobs.poll());
        if all.iter().any(|e| matches!(e, CloneEvent::Done { .. })) {
            return all;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("clone did not finish: {all:?}");
}

#[test]
fn clone_jobs_emit_progress_then_done() {
    let mut jobs = CloneJobs::new();
    assert!(jobs.poll().is_empty());
    let runner = Arc::new(FakeCloner {
        lines: vec![
            line("Receiving objects", Some(10)),
            line("Receiving objects", Some(90)),
        ],
        outcome: Ok(()),
    });
    jobs.start(7, "u".into(), PathBuf::from("/d"), runner);
    assert_eq!(
        wait_done(&mut jobs),
        vec![
            CloneEvent::Progress {
                id: 7,
                line: line("Receiving objects", Some(10))
            },
            CloneEvent::Progress {
                id: 7,
                line: line("Receiving objects", Some(90))
            },
            CloneEvent::Done {
                id: 7,
                result: Ok(PathBuf::from("/d"))
            },
        ]
    );
}

#[test]
fn clone_jobs_report_failure_and_keep_jobs_apart() {
    let mut jobs = CloneJobs::new();
    let bad = Arc::new(FakeCloner {
        lines: vec![],
        outcome: Err("fatal: nope".into()),
    });
    jobs.start(1, "u".into(), PathBuf::from("/d"), bad);
    assert_eq!(
        wait_done(&mut jobs),
        vec![CloneEvent::Done {
            id: 1,
            result: Err("fatal: nope".into())
        }]
    );
}

fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

#[test]
fn real_git_cloner_clones_a_local_bare_repo() {
    if !git_available() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let bare = tmp.path().join("origin.git");
    let made = Command::new("git")
        .args(["init", "--bare", "-q"])
        .arg(&bare)
        .status()
        .unwrap();
    assert!(made.success());
    let dest = tmp.path().join("work/origin");
    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
    let mut seen = Vec::new();
    RealGitCloner
        .clone_repo(bare.to_str().unwrap(), &dest, &mut |l| seen.push(l))
        .unwrap();
    assert!(dest.join(".git").exists());
    assert!(seen.iter().all(|l| l.percent.is_some()));
}

#[test]
fn real_git_cloner_reports_gits_own_message_and_cleans_up() {
    if !git_available() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("nope.git");
    let dest = tmp.path().join("dest");
    let e = RealGitCloner
        .clone_repo(missing.to_str().unwrap(), &dest, &mut |_| {})
        .unwrap_err();
    assert!(
        e.contains("does not exist") || e.contains("not a git repository"),
        "{e}"
    );
    assert!(!dest.exists(), "half-created destination is removed");
}

#[test]
fn real_git_cloner_leaves_a_preexisting_destination_alone() {
    if !git_available() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("dest");
    std::fs::create_dir(&dest).unwrap();
    std::fs::write(dest.join("keep.txt"), "x").unwrap();
    let e = RealGitCloner
        .clone_repo(
            tmp.path().join("nope.git").to_str().unwrap(),
            &dest,
            &mut |_| {},
        )
        .unwrap_err();
    assert!(!e.is_empty());
    assert!(dest.join("keep.txt").exists());
}

#[test]
fn real_plan_then_refuse_existing() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(home.join("code/there")).unwrap();
    let c = Confinement::new(&RealFs, &home).unwrap();
    let root = c.home().join("code");
    assert!(plan_clone(&RealFs, &c, "https://h/o/there.git", None, &root).is_err());
    let ok = plan_clone(&RealFs, &c, "https://h/o/here.git", None, &root).unwrap();
    assert_eq!(ok, root.join("here"));
}
