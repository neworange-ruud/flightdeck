//! Hand something to the desktop: a directory to the file manager, a URL to the
//! browser.
//!
//! Both go through the same platform launcher (`open` / `explorer.exe` /
//! `xdg-open`) and the same non-blocking spawn, so this module owns that
//! knowledge once. We deliberately never wait for the launcher to exit — a file
//! manager or browser with no already-running instance stays in the foreground
//! for the life of its window, so waiting would freeze FlightDeck. That means
//! non-zero exit codes go unreported; spawn failures (missing command, not
//! executable) do not.

use std::process::{Command, Stdio};

use crate::tui::platform;

/// The platform's default opener for both files and URLs.
///
/// Branches on the symmetric per-OS constants rather than raw `cfg!`, so a
/// fourth OS means adding a constant, not inverting a condition.
pub fn default_program() -> &'static str {
    if platform::IS_MACOS {
        "open"
    } else if platform::IS_WINDOWS {
        // `explorer.exe <url>` hands an http(s) URL to the default browser, the
        // same way it hands a path to Explorer.
        "explorer.exe"
    } else {
        "xdg-open"
    }
}

/// Spawn `program args… target` detached from the TUI, reaping the child
/// off-thread. `what` names the thing being opened in the error message.
///
/// Returns `Err` with a user-facing message when the launcher cannot be
/// started at all.
pub fn spawn_detached(
    program: &str,
    args: &[String],
    target: &std::ffi::OsStr,
    what: &str,
) -> Result<(), String> {
    let child = Command::new(program)
        .args(args)
        .arg(target)
        // Null stdio: a chatty launcher must never write into the alternate
        // screen and corrupt the TUI.
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("could not open {what}: '{program}' failed to start ({e})"))?;

    // FlightDeck is long-running, and `xdg-open` exits immediately, so an
    // unwaited child would linger as a zombie for the rest of the session. The
    // thread lives only as long as the launcher process.
    std::thread::spawn(move || {
        let mut child = child;
        let _ = child.wait();
    });

    Ok(())
}

/// Open `url` in the user's browser via the platform default handler.
///
/// Unlike the file manager there is no config escape hatch: a URL belongs to
/// whatever the desktop has registered for `http(s)`, not to a configured file
/// manager.
pub fn open_url(url: &str) -> Result<(), String> {
    spawn_detached(default_program(), &[], std::ffi::OsStr::new(url), "browser")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_program_matches_the_platform() {
        if platform::IS_MACOS {
            assert_eq!(default_program(), "open");
        } else if platform::IS_WINDOWS {
            assert_eq!(default_program(), "explorer.exe");
        } else {
            assert_eq!(default_program(), "xdg-open");
        }
    }

    #[test]
    fn missing_launcher_reports_an_error_naming_it_and_the_target_kind() {
        let err = spawn_detached(
            "flightdeck-no-such-browser",
            &[],
            std::ffi::OsStr::new("https://example.com"),
            "browser",
        )
        .expect_err("spawning a nonexistent program must fail");
        assert!(
            err.contains("flightdeck-no-such-browser"),
            "error should name the command, got: {err}"
        );
        assert!(
            err.contains("browser"),
            "error should say what failed to open, got: {err}"
        );
    }
}
