//! Open a directory in the OS file manager (Finder / Explorer / the desktop's
//! `xdg-open` handler).
//!
//! Mirrors `tui::clipboard`: a per-OS default with a config escape hatch. The
//! platform launcher and the non-blocking spawn itself live in
//! [`crate::tui::opener`], shared with the browser.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use crate::tui::{opener, platform};

/// Resolve the launcher program and its fixed arguments.
///
/// An empty or whitespace-only `configured` value yields the per-OS default.
/// Otherwise the value is split on whitespace into a program plus arguments —
/// no shell, no quote handling.
pub fn launcher(configured: &str) -> (String, Vec<String>) {
    let mut parts = configured.split_whitespace().map(str::to_string);
    match parts.next() {
        Some(program) => (program, parts.collect()),
        None => (opener::default_program().to_string(), Vec::new()),
    }
}

/// Open `path` in the file manager. Returns `Err` with a user-facing message
/// when the launcher cannot be started.
pub fn open(path: &Path, configured: &str) -> Result<(), String> {
    let (program, args) = launcher(configured);
    let path = native_separators(path, platform::IS_WINDOWS);
    opener::spawn_detached(&program, &args, path.as_os_str(), "file manager")
}

/// On Windows, rewrite every `/` in `path` as `\`. Stored worktree paths are
/// `/`-separated, and Explorer reads a `/` as the start of a switch: handed
/// `D:\repo\.flightdeck/worktrees/x` it silently opens Documents instead.
fn native_separators(path: &Path, windows: bool) -> Cow<'_, Path> {
    match path.to_str() {
        Some(s) if windows && s.contains('/') => Cow::Owned(PathBuf::from(s.replace('/', "\\"))),
        _ => Cow::Borrowed(path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_config_uses_the_platform_default() {
        let (program, args) = launcher("");
        assert!(args.is_empty());
        if cfg!(target_os = "macos") {
            assert_eq!(program, "open");
        } else if cfg!(target_os = "windows") {
            assert_eq!(program, "explorer.exe");
        } else {
            assert_eq!(program, "xdg-open");
        }
    }

    #[test]
    fn whitespace_only_config_uses_the_platform_default() {
        let (program, args) = launcher("   \t ");
        let (default_program, _) = launcher("");
        assert_eq!(program, default_program);
        assert!(args.is_empty());
    }

    #[test]
    fn configured_program_overrides_the_default() {
        let (program, args) = launcher("nautilus");
        assert_eq!(program, "nautilus");
        assert!(args.is_empty());
    }

    #[test]
    fn configured_command_splits_into_program_and_args() {
        // No shell is involved: the value is split on whitespace so a launcher
        // that needs fixed arguments still works.
        let (program, args) = launcher("flatpak run org.gnome.Nautilus");
        assert_eq!(program, "flatpak");
        assert_eq!(args, vec!["run", "org.gnome.Nautilus"]);
    }

    #[test]
    fn windows_paths_get_backslashes_only() {
        // Explorer reads any `/` as a switch and silently opens Documents, and
        // stored worktree paths are `/`-separated.
        let mixed = Path::new(r"D:\repo\.flightdeck/worktrees/my-task");
        assert_eq!(
            native_separators(mixed, true),
            Path::new(r"D:\repo\.flightdeck\worktrees\my-task")
        );
    }

    #[test]
    fn other_platforms_keep_the_path_as_is() {
        let path = Path::new("/home/me/repo/.flightdeck/worktrees/my-task");
        assert_eq!(native_separators(path, false), path);
    }

    #[test]
    fn missing_program_reports_an_error_naming_it() {
        let err = open(
            std::path::Path::new("/tmp"),
            "flightdeck-no-such-file-manager",
        )
        .expect_err("spawning a nonexistent program must fail");
        assert!(
            err.contains("flightdeck-no-such-file-manager"),
            "error should name the command, got: {err}"
        );
    }
}
