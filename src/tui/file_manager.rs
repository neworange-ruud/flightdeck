//! Open a directory in the OS file manager (Finder / Explorer / the desktop's
//! `xdg-open` handler).
//!
//! Mirrors `tui::clipboard`: a per-OS default with a config escape hatch, and a
//! spawn that never blocks the TUI. We deliberately do not wait for the
//! launcher to exit — a file manager with no already-running instance (GNOME
//! Files, for one) stays in the foreground for the life of its window, so
//! waiting would freeze FlightDeck. That means non-zero exit codes go
//! unreported; spawn failures (missing command, not executable) do not.

use std::path::Path;
use std::process::{Command, Stdio};

/// Resolve the launcher program and its fixed arguments.
///
/// An empty or whitespace-only `configured` value yields the per-OS default.
/// Otherwise the value is split on whitespace into a program plus arguments —
/// no shell, no quote handling.
pub fn launcher(configured: &str) -> (String, Vec<String>) {
    let mut parts = configured.split_whitespace().map(str::to_string);
    match parts.next() {
        Some(program) => (program, parts.collect()),
        None => (default_program().to_string(), Vec::new()),
    }
}

/// The platform's default opener.
fn default_program() -> &'static str {
    if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(target_os = "windows") {
        "explorer.exe"
    } else {
        "xdg-open"
    }
}

/// Open `path` in the file manager. Returns `Err` with a user-facing message
/// when the launcher cannot be started.
pub fn open(path: &Path, configured: &str) -> Result<(), String> {
    let (program, args) = launcher(configured);
    let child = Command::new(&program)
        .args(&args)
        .arg(path)
        // Null stdio: a chatty launcher must never write into the alternate
        // screen and corrupt the TUI.
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("could not open file manager: '{program}' failed to start ({e})"))?;

    // Reap the child off-thread. `xdg-open` exits immediately, and FlightDeck is
    // long-running, so an unwaited child would linger as a zombie for the rest
    // of the session. The thread lives only as long as the launcher process.
    std::thread::spawn(move || {
        let mut child = child;
        let _ = child.wait();
    });

    Ok(())
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
