use super::*;
use crate::testing::FakeFs;

const HOME: &str = "/home/u";

fn conf(fs: &FakeFs) -> Confinement {
    Confinement::new(fs, Path::new(HOME)).unwrap()
}

fn names(listing: &Listing) -> Vec<&str> {
    listing.entries.iter().map(|e| e.name.as_str()).collect()
}

fn base() -> FakeFs {
    FakeFs::new()
        .with_dir("/home/u/code/app/.git")
        .with_dir("/home/u/code/lib")
        .with_dir("/home/u/Docs")
        .with_dir("/home/u/.cache")
        .with_dir("/outside/secret")
        .with_file("/home/u/notes.txt", "x")
}

#[test]
fn confinement_canonicalises_home_through_a_link() {
    let fs = FakeFs::new().with_dir("/data/u");
    fs.symlink(Path::new("/data/u"), Path::new("/home/u"))
        .unwrap();
    let c = conf(&fs);
    assert_eq!(c.home(), Path::new("/data/u"));
    assert_eq!(c.resolve(&fs, Some("~")).unwrap(), PathBuf::from("/data/u"));
}

#[test]
fn confinement_needs_an_existing_home() {
    assert!(Confinement::new(&FakeFs::new(), Path::new(HOME)).is_err());
}

#[test]
fn resolve_none_empty_and_tilde_are_home() {
    let fs = base();
    let c = conf(&fs);
    for req in [None, Some(""), Some("  "), Some("~")] {
        assert_eq!(c.resolve(&fs, req).unwrap(), PathBuf::from(HOME));
    }
}

#[test]
fn resolve_tilde_relative_and_absolute_inside() {
    let fs = base();
    let c = conf(&fs);
    for req in ["~/code", "code", "/home/u/code", "code/../code/./"] {
        assert_eq!(
            c.resolve(&fs, Some(req)).unwrap(),
            PathBuf::from("/home/u/code"),
            "{req}"
        );
    }
}

#[test]
fn resolve_refuses_dotdot_escape_and_outside_absolutes() {
    let fs = base();
    let c = conf(&fs);
    for req in [
        "..",
        "../..",
        "~/../..",
        "/outside/secret",
        "/",
        "code/../../../outside",
    ] {
        assert!(
            matches!(
                c.resolve(&fs, Some(req)),
                Err(ProjectOpError::OutsideHome(_))
            ),
            "{req}"
        );
    }
}

#[test]
fn resolve_does_not_reveal_whether_outside_paths_exist() {
    let fs = base();
    let c = conf(&fs);
    assert!(matches!(
        c.resolve(&fs, Some("/outside/nothing")),
        Err(ProjectOpError::OutsideHome(_))
    ));
}

#[test]
fn resolve_missing_inside_is_not_found() {
    let fs = base();
    assert!(matches!(
        conf(&fs).resolve(&fs, Some("~/nope")),
        Err(ProjectOpError::NotFound(_))
    ));
}

#[test]
fn resolve_refuses_a_symlink_out_of_home() {
    let fs = base();
    fs.symlink(Path::new("/outside/secret"), Path::new("/home/u/escape"))
        .unwrap();
    // A relative target that climbs out.
    fs.symlink(Path::new("../../outside"), Path::new("/home/u/climb"))
        .unwrap();
    let c = conf(&fs);
    for req in ["escape", "~/climb/secret", "/home/u/escape"] {
        assert!(
            matches!(
                c.resolve(&fs, Some(req)),
                Err(ProjectOpError::OutsideHome(_))
            ),
            "{req}"
        );
    }
}

#[test]
fn resolve_allows_a_symlink_inside_home() {
    let fs = base();
    fs.symlink(Path::new("/home/u/code"), Path::new("/home/u/abs"))
        .unwrap();
    fs.symlink(Path::new("code/app"), Path::new("/home/u/rel"))
        .unwrap();
    let c = conf(&fs);
    assert_eq!(
        c.resolve(&fs, Some("abs/lib")).unwrap(),
        PathBuf::from("/home/u/code/lib")
    );
    assert_eq!(
        c.resolve(&fs, Some("rel")).unwrap(),
        PathBuf::from("/home/u/code/app")
    );
}

#[test]
fn browse_lists_dirs_only_hides_hidden_and_sorts_case_insensitively() {
    let fs = base().with_dir("/home/u/apple").with_dir("/home/u/Zebra");
    let c = conf(&fs);
    let l = browse(&fs, &c, None, false).unwrap();
    assert_eq!(names(&l), ["apple", "code", "Docs", "Zebra"]);
    assert_eq!(l.path, PathBuf::from(HOME));
    assert_eq!(l.parent, None);
    assert!(!l.truncated);
    let l = browse(&fs, &c, Some("~"), true).unwrap();
    assert_eq!(names(&l), [".cache", "apple", "code", "Docs", "Zebra"]);
}

#[test]
fn browse_flags_repositories_and_reports_the_parent() {
    let fs = base();
    let c = conf(&fs);
    let l = browse(&fs, &c, Some("code"), false).unwrap();
    assert_eq!(names(&l), ["app", "lib"]);
    assert!(l.entries[0].is_git_repo);
    assert!(!l.entries[1].is_git_repo);
    assert_eq!(l.entries[0].path, PathBuf::from("/home/u/code/app"));
    assert_eq!(l.parent, Some(PathBuf::from(HOME)));
}

#[test]
fn browse_a_worktree_style_git_file_is_a_repo() {
    let fs = FakeFs::new().with_file("/home/u/wt/.git", "gitdir: x");
    assert!(browse(&fs, &conf(&fs), None, false).unwrap().entries[0].is_git_repo);
}

#[test]
fn browse_omits_entries_escaping_home_and_dangling_links() {
    let fs = base();
    fs.symlink(Path::new("/outside/secret"), Path::new("/home/u/escape"))
        .unwrap();
    fs.symlink(Path::new("/home/u/gone"), Path::new("/home/u/dangling"))
        .unwrap();
    fs.symlink(Path::new("/home/u/code"), Path::new("/home/u/shortcut"))
        .unwrap();
    let l = browse(&fs, &conf(&fs), None, false).unwrap();
    assert_eq!(names(&l), ["code", "Docs", "shortcut"]);
}

#[test]
fn browse_refuses_outside_missing_and_files() {
    let fs = base();
    let c = conf(&fs);
    assert!(matches!(
        browse(&fs, &c, Some("/outside"), false),
        Err(ProjectOpError::OutsideHome(_))
    ));
    assert!(matches!(
        browse(&fs, &c, Some("nope"), false),
        Err(ProjectOpError::NotFound(_))
    ));
    assert!(matches!(
        browse(&fs, &c, Some("notes.txt"), false),
        Err(ProjectOpError::NotADirectory(_))
    ));
}

#[test]
fn browse_truncates_at_the_cap() {
    let mut fs = FakeFs::new().with_dir(HOME);
    for i in 0..MAX_ENTRIES + 5 {
        fs = fs.with_dir(format!("/home/u/d{i:05}"));
    }
    let l = browse(&fs, &conf(&fs), None, false).unwrap();
    assert_eq!(l.entries.len(), MAX_ENTRIES);
    assert!(l.truncated);
    assert_eq!(l.entries[0].name, "d00000");
}

#[test]
fn scan_finds_repos_to_depth_two_and_stops_at_them() {
    let fs = FakeFs::new()
        .with_dir("/home/u/p/a/.git")
        .with_dir("/home/u/p/a/inner/.git")
        .with_dir("/home/u/p/group/b/.git")
        .with_dir("/home/u/p/group/deep/c/.git")
        .with_dir("/home/u/p/.hidden/h/.git")
        .with_dir("/home/u/p/plain");
    let found = scan(&fs, &conf(&fs), Path::new("/home/u/p"), 2).unwrap();
    let got: Vec<_> = found.iter().map(|e| e.path.clone()).collect();
    assert_eq!(
        got,
        [
            PathBuf::from("/home/u/p/a"),
            PathBuf::from("/home/u/p/group/b")
        ]
    );
    assert!(found.iter().all(|e| e.is_git_repo));
}

#[test]
fn scan_depth_one_does_not_go_below_the_root() {
    let fs = FakeFs::new()
        .with_dir("/home/u/p/a/.git")
        .with_dir("/home/u/p/g/b/.git");
    let found = scan(&fs, &conf(&fs), Path::new("/home/u/p"), 1).unwrap();
    assert_eq!(found.len(), 1);
}

#[test]
fn scan_missing_root_is_empty_and_outside_root_is_refused() {
    let fs = base();
    let c = conf(&fs);
    assert_eq!(scan(&fs, &c, Path::new("/home/u/none"), 2).unwrap(), vec![]);
    assert_eq!(
        scan(&fs, &c, Path::new("/home/u/notes.txt"), 2).unwrap(),
        vec![]
    );
    assert!(matches!(
        scan(&fs, &c, Path::new("/outside"), 2),
        Err(ProjectOpError::RootOutsideHome(_))
    ));
    assert!(matches!(
        scan(&fs, &c, Path::new("/elsewhere/missing"), 2),
        Err(ProjectOpError::RootOutsideHome(_))
    ));
}

#[test]
fn scan_does_not_follow_links_out_of_home() {
    let fs = base().with_dir("/outside/secret/r/.git");
    fs.symlink(Path::new("/outside/secret"), Path::new("/home/u/code/esc"))
        .unwrap();
    let found = scan(&fs, &conf(&fs), Path::new("/home/u/code"), 2).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].name, "app");
}

#[test]
fn validate_open_accepts_inside_dirs_and_refuses_the_rest() {
    let fs = base();
    let c = conf(&fs);
    assert_eq!(
        validate_open(&fs, &c, "~/code/lib").unwrap(),
        PathBuf::from("/home/u/code/lib")
    );
    assert!(matches!(
        validate_open(&fs, &c, "/outside/secret"),
        Err(ProjectOpError::OutsideHome(_))
    ));
    assert!(matches!(
        validate_open(&fs, &c, "notes.txt"),
        Err(ProjectOpError::NotADirectory(_))
    ));
    assert!(matches!(
        validate_open(&fs, &c, "missing"),
        Err(ProjectOpError::NotFound(_))
    ));
}

#[test]
fn errors_read_as_sentences() {
    let e = ProjectOpError::OutsideHome("/etc".into()).to_string();
    assert!(e.contains("/etc") && e.contains("home"));
}

#[test]
fn fake_canonicalize_loop_and_missing() {
    let fs = FakeFs::new().with_dir("/home/u");
    fs.symlink(Path::new("/home/u/b"), Path::new("/home/u/a"))
        .unwrap();
    fs.symlink(Path::new("/home/u/a"), Path::new("/home/u/b"))
        .unwrap();
    assert!(fs.canonicalize(Path::new("/home/u/a")).is_err());
    assert!(fs.canonicalize(Path::new("/home/u/zzz")).is_err());
    assert!(fs.canonicalize(Path::new("relative")).is_err());
    assert_eq!(
        fs.canonicalize(Path::new("/home/u/../u/./")).unwrap(),
        PathBuf::from("/home/u")
    );
}

/// The same symlink-escape cases against the real filesystem, to prove
/// `RealFs::canonicalize` and `FakeFs::canonicalize` agree.
#[cfg(unix)]
#[test]
fn real_symlink_escape_is_refused_and_inside_link_allowed() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let outside = tmp.path().join("outside");
    std::fs::create_dir_all(home.join("code/app/.git")).unwrap();
    std::fs::create_dir_all(outside.join("secret")).unwrap();
    std::os::unix::fs::symlink(&outside, home.join("escape")).unwrap();
    std::os::unix::fs::symlink("code/app", home.join("inside")).unwrap();
    std::os::unix::fs::symlink("../outside", home.join("relescape")).unwrap();

    let fs = crate::contracts::RealFs;
    let c = Confinement::new(&fs, &home).unwrap();
    for req in ["escape", "escape/secret", "relescape", "..", "code/../.."] {
        assert!(
            matches!(
                c.resolve(&fs, Some(req)),
                Err(ProjectOpError::OutsideHome(_))
            ),
            "{req}"
        );
    }
    let inside = c.resolve(&fs, Some("inside")).unwrap();
    assert_eq!(inside, c.home().join("code/app"));

    let l = browse(&fs, &c, None, false).unwrap();
    assert_eq!(names(&l), ["code", "inside"]);
    assert!(l.entries[1].is_git_repo);
    assert!(matches!(
        validate_open(&fs, &c, outside.to_str().unwrap()),
        Err(ProjectOpError::OutsideHome(_))
    ));
}
