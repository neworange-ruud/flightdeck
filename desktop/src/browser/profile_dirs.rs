//! Deleting a WebView2 profile's folder (`specs/SESSION_BROWSER_PRD.md` B19;
//! `remote-control-cefv.12.4`), as plain file handling so it is tested on
//! any platform.
//!
//! WebView2 keeps a profile in `<data folder>\EBWebView\WV2Profile_<name>`
//! (seen, not documented; a bare `<name>` is matched too). Its
//! own delete call needs a live web view of that profile, and when a
//! worktree is abandoned its browsers are already closed. So [`forget`]
//! renames the folder aside first, which Windows refuses while the browser
//! process still holds a file in it: either the whole folder goes, or none
//! of it and the name is kept in the `pending` list, for [`sweep`] to
//! delete when the app next starts, before any browser process holds it.

use std::path::{Path, PathBuf};

/// The suffix a folder being deleted is renamed to.
const DELETED: &str = ".deleted";

/// Delete profile `name`'s folder under `data_dir` now if it can be, else
/// at the next [`sweep`].
pub fn forget(data_dir: &Path, pending: &Path, name: &str) {
    let Some(dir) = profile_dir(data_dir, name) else {
        return;
    };
    let folder = dir.file_name().map(|f| f.to_string_lossy().into_owned());
    let aside = dir.with_file_name(format!(
        "{}{DELETED}-{}",
        folder.as_deref().unwrap_or(name),
        std::process::id()
    ));
    if std::fs::rename(&dir, &aside).is_ok() {
        // Renamed whole: what cannot go now, the next sweep takes.
        let _ = std::fs::remove_dir_all(&aside);
    } else {
        let mut names = read_pending(pending);
        if !names.iter().any(|n| n == name) {
            names.push(name.to_string());
            write_pending(pending, &names);
        }
    }
}

/// Delete the profiles [`forget`] could not, and what is left of those it
/// renamed aside. Run before any web view of `data_dir` starts.
pub fn sweep(data_dir: &Path, pending: &Path) {
    let mut kept = Vec::new();
    for name in read_pending(pending) {
        match profile_dir(data_dir, &name) {
            Some(dir) if std::fs::remove_dir_all(&dir).is_err() => kept.push(name),
            _ => {}
        }
    }
    write_pending(pending, &kept);
    if let Ok(entries) = std::fs::read_dir(data_dir.join("EBWebView")) {
        for entry in entries.flatten() {
            let leftover = entry
                .file_name()
                .to_str()
                .is_some_and(|n| n.contains(DELETED));
            if leftover {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }
}

/// The prefix WebView2 gives a named profile's folder.
const PROFILE_PREFIX: &str = "WV2Profile_";

/// Profile `name`'s folder, `WV2Profile_<name>` or `<name>`, matched without
/// regard to case (Windows paths).
fn profile_dir(data_dir: &Path, name: &str) -> Option<PathBuf> {
    let prefixed = format!("{PROFILE_PREFIX}{name}");
    std::fs::read_dir(data_dir.join("EBWebView"))
        .ok()?
        .flatten()
        .find(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|n| n.eq_ignore_ascii_case(&prefixed) || n.eq_ignore_ascii_case(name))
                && entry.path().is_dir()
        })
        .map(|entry| entry.path())
}

fn read_pending(pending: &Path) -> Vec<String> {
    std::fs::read_to_string(pending)
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

fn write_pending(pending: &Path, names: &[String]) {
    if names.is_empty() {
        let _ = std::fs::remove_file(pending);
        return;
    }
    if let Some(dir) = pending.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(pending, names.join("\n") + "\n");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A data folder with profiles `names`, each holding a file.
    fn data_folder(names: &[&str]) -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        for name in names {
            let dir = root.path().join("EBWebView").join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("Cookies"), "c").unwrap();
        }
        root
    }

    #[test]
    fn a_forgotten_profile_goes_and_the_others_stay() {
        // WebView2's own layout.
        let root = data_folder(&["WV2Profile_fdaaa", "WV2Profile_fdbbb", "Default"]);
        let pending = root.path().join("forget.txt");
        forget(root.path(), &pending, "fdaaa");
        let mut left: Vec<String> = std::fs::read_dir(root.path().join("EBWebView"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(
            left,
            vec!["Default".to_string(), "WV2Profile_fdbbb".to_string()]
        );
        assert!(!pending.exists(), "nothing left for the sweep");
    }

    #[test]
    fn a_bare_profile_folder_is_found_too() {
        let root = data_folder(&["fdaaa", "fdbbb"]);
        let pending = root.path().join("forget.txt");
        forget(root.path(), &pending, "fdaaa");
        let left: Vec<String> = std::fs::read_dir(root.path().join("EBWebView"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left, vec!["fdbbb".to_string()]);
        assert!(!pending.exists(), "nothing left for the sweep");
    }

    #[test]
    fn a_profile_that_is_not_there_is_nothing_to_do() {
        let root = data_folder(&["fdbbb"]);
        let pending = root.path().join("forget.txt");
        forget(root.path(), &pending, "fdaaa");
        forget(tempfile::tempdir().unwrap().path(), &pending, "fdaaa");
        assert!(!pending.exists());
        assert!(root.path().join("EBWebView").join("fdbbb").exists());
    }

    #[test]
    fn the_sweep_deletes_what_was_pending_and_what_was_left_aside() {
        let root = data_folder(&[
            "WV2Profile_fdaaa",
            "wv2profile_FDCCC",
            "WV2Profile_fdbbb",
            "WV2Profile_fdddd.deleted-42",
        ]);
        let pending = root.path().join("forget.txt");
        // As forget leaves it when the browser process held a file.
        std::fs::write(&pending, "fdaaa\nfdccc\nfdgone\n").unwrap();
        sweep(root.path(), &pending);
        let mut left: Vec<String> = std::fs::read_dir(root.path().join("EBWebView"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(
            left,
            vec!["WV2Profile_fdbbb".to_string()],
            "case-insensitive too"
        );
        assert!(!pending.exists(), "all done");
    }

    #[test]
    fn a_name_is_pending_once() {
        let root = tempfile::tempdir().unwrap();
        let pending = root.path().join("forget.txt");
        write_pending(&pending, &["fdaaa".to_string()]);
        let mut names = read_pending(&pending);
        assert_eq!(names, vec!["fdaaa".to_string()]);
        names.push("fdbbb".into());
        write_pending(&pending, &names);
        assert_eq!(read_pending(&pending).len(), 2);
        write_pending(&pending, &[]);
        assert!(!pending.exists());
    }
}
