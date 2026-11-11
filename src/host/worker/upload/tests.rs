use super::*;
use crate::testing::FakeFs;

const HOME: &str = "/home/u";

fn conf(fs: &FakeFs) -> Confinement {
    Confinement::new(fs, Path::new(HOME)).unwrap()
}

fn base() -> FakeFs {
    FakeFs::new()
        .with_dir("/home/u/proj/assets")
        .with_file("/home/u/proj/assets/logo.png", "x")
        .with_file("/home/u/proj/assets/logo (1).png", "x")
        .with_file("/home/u/proj/assets/.env", "x")
        .with_file("/home/u/proj/assets/Makefile", "x")
        .with_dir("/home/u/proj/assets/build")
        .with_file("/home/u/notes.txt", "x")
        .with_dir("/outside/secret")
}

fn files(names: &[&str]) -> Vec<UploadFile> {
    names
        .iter()
        .map(|name| UploadFile {
            name: name.to_string(),
            size: 100,
        })
        .collect()
}

const PLENTY: Option<u64> = Some(1 << 40);

fn plan_in(
    fs: &FakeFs,
    dir: &str,
    names: &[&str],
    on_conflict: OnConflict,
) -> Result<Plan, String> {
    plan(fs, &conf(fs), dir, &files(names), on_conflict, |_| PLENTY)
}

fn finals(plan: &Plan) -> Vec<(&str, bool, bool)> {
    let files = match plan {
        Plan::Ready { files, .. } | Plan::Conflicts { files, .. } => files,
    };
    files
        .iter()
        .map(|f| (f.final_name.as_str(), f.exists, f.replace))
        .collect()
}

// -- names ------------------------------------------------------------------

#[test]
fn a_name_is_one_path_component() {
    for good in [
        "a.txt",
        ".env",
        "Makefile",
        "résumé (final).pdf",
        "..hidden",
        "a..b",
    ] {
        assert_eq!(validate_name(good), Ok(()), "{good}");
    }
    let long = "x".repeat(MAX_NAME_BYTES);
    assert_eq!(validate_name(&long), Ok(()));
    for bad in [
        "",
        ".",
        "..",
        "a/b",
        "/etc/passwd",
        "..\\x",
        "a\\b",
        "nul\0byte",
        "line\nbreak",
    ] {
        assert!(validate_name(bad).is_err(), "{bad:?} must be refused");
    }
    assert!(validate_name(&"x".repeat(MAX_NAME_BYTES + 1)).is_err());
    // 255 bytes, not 255 characters.
    assert!(validate_name(&"é".repeat(128)).is_err());
}

#[test]
fn a_path_is_refused_in_words_about_folders() {
    let why = validate_name("src/main.rs").unwrap_err();
    assert!(why.contains("not a file name"), "{why}");
}

#[test]
fn a_request_has_one_to_256_distinct_valid_names() {
    assert!(check_request(&[]).unwrap_err().contains("at least one"));
    let many: Vec<UploadFile> = (0..=MAX_FILES)
        .map(|i| UploadFile {
            name: format!("f{i}"),
            size: 1,
        })
        .collect();
    assert!(check_request(&many).unwrap_err().contains("at most 256"));
    assert_eq!(check_request(&many[..MAX_FILES]), Ok(()));
    let twice = check_request(&files(&["a", "b", "a"])).unwrap_err();
    assert!(twice.contains("twice"), "{twice}");
    assert!(check_request(&files(&["ok", "../escape"])).is_err());
}

// -- keep both --------------------------------------------------------------

#[test]
fn a_numbered_copy_goes_before_the_extension() {
    assert_eq!(keep_both_name("photo.png", 1), "photo (1).png");
    assert_eq!(keep_both_name("photo.png", 12), "photo (12).png");
    assert_eq!(keep_both_name("Makefile", 1), "Makefile (1)");
    assert_eq!(keep_both_name(".env", 1), ".env (1)");
    assert_eq!(keep_both_name(".env.local", 2), ".env (2).local");
    assert_eq!(keep_both_name("a.tar.gz", 1), "a.tar (1).gz");
    assert_eq!(keep_both_name("trailing.", 1), "trailing. (1)");
}

#[test]
fn keep_both_takes_the_first_free_number() {
    let fs = base();
    let plan = plan_in(
        &fs,
        "~/proj/assets",
        &["logo.png", ".env", "Makefile", "new.txt"],
        OnConflict::KeepBoth,
    )
    .unwrap();
    assert_eq!(
        finals(&plan),
        vec![
            // `logo (1).png` is on disk already.
            ("logo (2).png", true, false),
            (".env (1)", true, false),
            ("Makefile (1)", true, false),
            ("new.txt", false, false),
        ]
    );
}

#[test]
fn keep_both_avoids_names_the_batch_already_uses() {
    let fs = base();
    // `logo (2).png` is free on disk, but asked for by name in this batch.
    let plan = plan_in(
        &fs,
        "~/proj/assets",
        &["logo.png", "logo (2).png"],
        OnConflict::KeepBoth,
    )
    .unwrap();
    assert_eq!(
        finals(&plan),
        vec![
            ("logo (3).png", true, false),
            ("logo (2).png", false, false)
        ]
    );
}

#[test]
fn a_taken_folder_name_gets_a_numbered_file_under_keep_both() {
    let fs = base();
    let plan = plan_in(&fs, "~/proj/assets", &["build"], OnConflict::KeepBoth).unwrap();
    assert_eq!(finals(&plan), vec![("build (1)", true, false)]);
}

// -- conflicts --------------------------------------------------------------

#[test]
fn ask_answers_with_the_taken_names_and_grants_nothing() {
    let fs = base();
    let plan = plan_in(
        &fs,
        "~/proj/assets",
        &["logo.png", "new.txt"],
        OnConflict::Ask,
    )
    .unwrap();
    assert!(matches!(plan, Plan::Conflicts { .. }), "{plan:?}");
    assert_eq!(
        finals(&plan),
        vec![("logo.png", true, false), ("new.txt", false, false)]
    );
}

#[test]
fn ask_with_nothing_taken_is_ready_at_once() {
    let fs = base();
    let plan = plan_in(&fs, "~/proj/assets", &["a.txt", "b.txt"], OnConflict::Ask).unwrap();
    let Plan::Ready { dir, files } = &plan else {
        panic!("{plan:?}");
    };
    assert_eq!(dir, Path::new("/home/u/proj/assets"));
    assert!(files
        .iter()
        .all(|f| !f.exists && !f.replace && f.size == 100));
}

#[test]
fn replace_overwrites_files_but_never_a_folder() {
    let fs = base();
    let plan = plan_in(
        &fs,
        "~/proj/assets",
        &["logo.png", "new.txt"],
        OnConflict::Replace,
    )
    .unwrap();
    assert!(matches!(plan, Plan::Ready { .. }));
    assert_eq!(
        finals(&plan),
        vec![("logo.png", true, true), ("new.txt", false, true)]
    );
    let why = plan_in(&fs, "~/proj/assets", &["build"], OnConflict::Replace).unwrap_err();
    assert!(why.contains("is a folder"), "{why}");
}

// -- the folder ---------------------------------------------------------------

#[test]
fn the_folder_must_exist_inside_home() {
    let fs = base();
    fs.symlink(Path::new("/outside/secret"), Path::new("/home/u/escape"))
        .unwrap();
    for dir in ["/outside/secret", "~/escape", "/", "~/../.."] {
        let why = plan_in(&fs, dir, &["a"], OnConflict::Ask).unwrap_err();
        assert!(why.contains("outside your home folder"), "{dir}: {why}");
    }
    let why = plan_in(&fs, "~/nope", &["a"], OnConflict::Ask).unwrap_err();
    assert!(why.contains("does not exist"), "{why}");
    let why = plan_in(&fs, "~/notes.txt", &["a"], OnConflict::Ask).unwrap_err();
    assert!(why.contains("is not a folder"), "{why}");
    // Home itself is a fine target.
    let plan = plan_in(&fs, "~", &["a"], OnConflict::Ask).unwrap();
    assert!(matches!(plan, Plan::Ready { ref dir, .. } if dir == Path::new(HOME)));
}

#[test]
fn names_are_checked_before_the_folder_is_looked_at() {
    let fs = base();
    let why = plan_in(&fs, "/outside/secret", &["../x"], OnConflict::Ask).unwrap_err();
    assert!(why.contains("not a file name"), "{why}");
}

// -- space ----------------------------------------------------------------------

#[test]
fn the_upload_must_leave_the_margin_free() {
    assert_eq!(check_space(100, None), Ok(()), "unknown is not a refusal");
    assert_eq!(check_space(100, Some(DISK_MARGIN_BYTES + 100)), Ok(()));
    let why = check_space(101, Some(DISK_MARGIN_BYTES + 100)).unwrap_err();
    assert!(why.contains("Not enough free space"), "{why}");
    assert!(why.contains("256 MB spare"), "{why}");
}

#[test]
fn a_full_disk_refuses_the_whole_batch_for_the_target_folder() {
    let fs = base();
    let mut asked = None;
    let why = plan(
        &fs,
        &conf(&fs),
        "~/proj/assets",
        &files(&["a", "b"]),
        OnConflict::Ask,
        |dir| {
            asked = Some(dir.to_path_buf());
            Some(DISK_MARGIN_BYTES + 150)
        },
    )
    .unwrap_err();
    assert!(why.contains("the files need 200 B"), "{why}");
    assert_eq!(asked.as_deref(), Some(Path::new("/home/u/proj/assets")));
}

#[test]
fn a_path_reads_from_home() {
    let home = Path::new(HOME);
    assert_eq!(tilde(home, Path::new("/home/u")), "~");
    assert_eq!(
        tilde(home, Path::new("/home/u/proj/assets")),
        "~/proj/assets"
    );
    assert_eq!(tilde(home, Path::new("/srv/x")), "/srv/x");
}
