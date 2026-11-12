use std::path::{Path, PathBuf};

use super::*;
use crate::testing::FakeFs;

const HOME: &str = "/home/u";

fn conf(fs: &FakeFs) -> Confinement {
    Confinement::new(fs, Path::new(HOME)).unwrap()
}

fn base() -> FakeFs {
    let fs = FakeFs::new()
        .with_file("/home/u/proj/a.txt", "hello")
        .with_file("/home/u/proj/b.bin", "xy")
        .with_file("/home/u/proj/.env", "K=V")
        .with_dir("/home/u/proj/notes")
        .with_file("/home/u/other/c.txt", "ccc")
        .with_file("/outside/secret", "s");
    fs.symlink(Path::new("/outside/secret"), Path::new("/home/u/proj/link"))
        .unwrap();
    fs.symlink(
        Path::new("/home/u/other/c.txt"),
        Path::new("/home/u/proj/c"),
    )
    .unwrap();
    fs.symlink(
        Path::new("/home/u/proj/gone"),
        Path::new("/home/u/proj/dangling"),
    )
    .unwrap();
    fs
}

fn names(names: &[&str]) -> Vec<String> {
    names.iter().map(|n| n.to_string()).collect()
}

fn plan_in(fs: &FakeFs, dir: &str, asked: &[&str]) -> Result<Planned, String> {
    plan(fs, &conf(fs), dir, &names(asked))
}

#[test]
fn granted_files_are_canonical_with_their_sizes_in_the_order_asked() {
    let fs = base();
    let planned = plan_in(&fs, "~/proj", &["b.bin", "a.txt", ".env", "c"]).unwrap();
    assert_eq!(planned.dir, PathBuf::from("/home/u/proj"));
    let got: Vec<(&str, &Path, u64)> = planned
        .files
        .iter()
        .map(|f| (f.name.as_str(), f.path.as_path(), f.size))
        .collect();
    assert_eq!(
        got,
        vec![
            ("b.bin", Path::new("/home/u/proj/b.bin"), 2),
            ("a.txt", Path::new("/home/u/proj/a.txt"), 5),
            (".env", Path::new("/home/u/proj/.env"), 3),
            // A link inside home is followed: the grant is for its target.
            ("c", Path::new("/home/u/other/c.txt"), 3),
        ]
    );
}

#[test]
fn how_many_is_one_to_the_cap() {
    let fs = base();
    assert_eq!(
        plan_in(&fs, "~/proj", &[]),
        Err("Choose at least one file to download.".to_string())
    );
    let many: Vec<String> = (0..=MAX_FILES).map(|i| format!("f{i}")).collect();
    let refused = plan(&fs, &conf(&fs), "~/proj", &many).unwrap_err();
    assert_eq!(
        refused,
        format!("That is 257 files; download at most {MAX_FILES} at a time.")
    );
}

#[test]
fn a_bad_or_repeated_name_is_refused_in_download_words() {
    let fs = base();
    let refused = plan_in(&fs, "~/proj", &["a.txt", "../proj/a.txt"]).unwrap_err();
    assert_eq!(
        refused,
        "\"../proj/a.txt\" is a path, not a file name: download files, not folders."
    );
    assert_eq!(
        plan_in(&fs, "~/proj", &[""]).unwrap_err(),
        "A file to download has no name."
    );
    assert!(plan_in(&fs, "~/proj", &[".."])
        .unwrap_err()
        .contains("cannot be a file name"));
    assert_eq!(
        plan_in(&fs, "~/proj", &["a.txt", "a.txt"]).unwrap_err(),
        "a.txt is in the list twice: one file per name."
    );
}

#[test]
fn the_folder_is_confined_like_opening_a_project() {
    let fs = base().with_dir("/outside/dir");
    assert_eq!(
        plan_in(&fs, "/outside/dir", &["a.txt"]).unwrap_err(),
        "/outside/dir is outside your home folder."
    );
    assert_eq!(
        plan_in(&fs, "~/missing", &["a.txt"]).unwrap_err(),
        "~/missing does not exist."
    );
    assert_eq!(
        plan_in(&fs, "~/proj/a.txt", &["a.txt"]).unwrap_err(),
        "~/proj/a.txt is not a folder."
    );
}

#[test]
fn the_first_bad_file_refuses_the_whole_request_naming_it() {
    let fs = base();
    for (asked, said) in [
        (
            vec!["a.txt", "notes"],
            "notes is a folder; download files, not folders.",
        ),
        (vec!["x.txt", "a.txt"], "x.txt is no longer there."),
        (
            vec!["a.txt", "link"],
            "link points outside your home folder.",
        ),
        (vec!["dangling"], "dangling is no longer there."),
    ] {
        assert_eq!(
            plan_in(&fs, "~/proj", &asked),
            Err(said.to_string()),
            "{asked:?}"
        );
    }
}

#[test]
fn a_real_special_file_is_not_downloadable() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("h");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("ok.txt"), "fine").unwrap();
    let fifo = std::ffi::CString::new(home.join("pipe").to_str().unwrap()).unwrap();
    // SAFETY: a valid NUL-terminated path; mkfifo has no other preconditions.
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    let fs = crate::contracts::RealFs;
    let c = Confinement::new(&fs, &home).unwrap();
    let ok = plan(&fs, &c, "~", &names(&["ok.txt"])).unwrap();
    assert_eq!(ok.files[0].size, 4);
    assert_eq!(
        plan(&fs, &c, "~", &names(&["pipe"])).unwrap_err(),
        "pipe is not a regular file, so it cannot be downloaded."
    );
}
