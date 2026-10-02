//! Another instance of the app: File ▸ New window, the palette's "New
//! Window" row and the macOS Dock menu.
//!
//! The app is one window per process (`app::run`), so a second window is a
//! second process. That is what lets one instance run local projects while
//! another is a remote control for a FlightDeck on another machine: each has
//! its own window, Dock/taskbar entry and lifetime, and quitting one leaves the
//! other running.
//!
//! The new instance always opens on the launcher ([`LAUNCHER_FLAG`]), never on
//! the project of the working directory: two hosts on one project would run
//! two sets of agents in the same worktrees.
//!
//! ## Per OS
//!
//! - **macOS, from a `.app`**: `open -n <bundle> --args --launcher`. Asking
//!   LaunchServices for a *new* instance (`-n`) is the only way to get one —
//!   opening the bundle again from Finder or the Dock just activates the
//!   running instance — and the instance it starts is a proper app of its own
//!   (its own Dock icon, not a child of this process).
//! - **Everywhere else** (Linux, Windows, a bare cargo build on macOS): run this
//!   executable again with `--launcher`, detached from this process, so a
//!   terminal's Ctrl-C or this instance quitting does not take it down.

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The command-line flag that opens the launcher whatever the working
/// directory is.
pub const LAUNCHER_FLAG: &str = "--launcher";

/// How to start a new instance: a program and its arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spawn {
    pub program: PathBuf,
    pub args: Vec<OsString>,
}

/// The `.app` bundle `exe` runs from, if any
/// (`<name>.app/Contents/MacOS/<binary>`).
fn bundle_of(exe: &Path) -> Option<&Path> {
    exe.ancestors()
        .find(|a| a.extension() == Some(OsStr::new("app")))
        .filter(|bundle| exe.starts_with(bundle.join("Contents").join("MacOS")))
}

/// How to start another instance of the app whose executable is `exe`, on
/// macOS when `macos` is set. Pure, so every OS's answer is testable anywhere.
pub fn plan(exe: &Path, macos: bool) -> Spawn {
    if macos {
        if let Some(bundle) = bundle_of(exe) {
            return Spawn {
                program: PathBuf::from("/usr/bin/open"),
                args: vec![
                    "-n".into(),
                    bundle.as_os_str().to_owned(),
                    "--args".into(),
                    LAUNCHER_FLAG.into(),
                ],
            };
        }
    }
    Spawn {
        program: exe.to_path_buf(),
        args: vec![LAUNCHER_FLAG.into()],
    }
}

/// Start another instance of this app on its launcher.
///
/// It starts in the home folder (the working directory a launch from Finder
/// or the Start menu has), with no terminal attached, in its own process
/// group. The child is reaped on a background thread so it never lingers as
/// a zombie of this process.
pub fn spawn_new_instance() -> io::Result<()> {
    let exe = std::env::current_exe()?;
    let spawn = plan(&exe, cfg!(target_os = "macos"));
    let mut command = Command::new(&spawn.program);
    command
        .args(&spawn.args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(home) = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }) {
        command.current_dir(home);
    }
    detach(&mut command);
    let mut child = command.spawn()?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Its own process group: a Ctrl-C in the terminal this instance was started
/// from must not reach the new one.
#[cfg(unix)]
fn detach(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

/// No console, and its own process group, for the same reason.
#[cfg(windows)]
fn detach(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(spawn: &Spawn) -> Vec<&str> {
        spawn.args.iter().map(|a| a.to_str().unwrap()).collect()
    }

    #[test]
    fn a_macos_bundle_asks_launchservices_for_a_new_instance() {
        let exe = Path::new("/Applications/FlightDeck.app/Contents/MacOS/flightdeck-desktop");
        let spawn = plan(exe, true);
        assert_eq!(spawn.program, Path::new("/usr/bin/open"));
        assert_eq!(
            args(&spawn),
            ["-n", "/Applications/FlightDeck.app", "--args", "--launcher"]
        );
    }

    #[test]
    fn a_bare_macos_binary_runs_itself_again() {
        let exe = Path::new("/Users/u/flightdeck/target/release/flightdeck-desktop");
        let spawn = plan(exe, true);
        assert_eq!(spawn.program, exe);
        assert_eq!(args(&spawn), ["--launcher"]);
    }

    #[test]
    fn a_folder_named_like_a_bundle_is_not_one() {
        // `.app` in the path, but the binary is not in its Contents/MacOS.
        let exe = Path::new("/Users/u/my.app/target/debug/flightdeck-desktop");
        assert_eq!(plan(exe, true).program, exe);
    }

    #[test]
    fn linux_and_windows_run_the_executable_again() {
        for exe in [
            Path::new("/usr/bin/flightdeck-desktop"),
            Path::new(r"C:\Program Files\FlightDeck\flightdeck-desktop.exe"),
        ] {
            let spawn = plan(exe, false);
            assert_eq!(spawn.program, exe);
            assert_eq!(args(&spawn), ["--launcher"]);
        }
        // Not macOS: even a bundle-shaped path is just an executable.
        let bundled = Path::new("/x/FlightDeck.app/Contents/MacOS/flightdeck-desktop");
        assert_eq!(plan(bundled, false).program, bundled);
    }
}
