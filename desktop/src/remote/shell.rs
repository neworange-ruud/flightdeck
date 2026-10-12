//! A remote window's SSH shell tab: a plain login shell on the host, outside
//! any project.
//!
//! It is not one of the host's terminals. This machine runs `ssh` to the
//! remote's SSH target — the one "Open in VS Code" uses
//! ([`flightdeck::host::vscode::ssh_target`]) — on a PTY of its own, so it
//! reaches the host the way that does (over Tailscale, for a tailnet
//! address) and needs the host to accept SSH. The host's FlightDeck never
//! sees it, and nothing about it crosses the link.

use std::path::PathBuf;

use flightdeck::contracts::{PtyBackend, PtySize, Result};
use flightdeck::terminal::session::Terminal;

/// The SSH client, found on `PATH` (Windows ships OpenSSH's as `ssh.exe`).
pub const SSH: &str = "ssh";

/// The argv for a shell on `target`: `ssh <target>`, which asks the host for
/// a login shell on a PTY. A target from `~/.ssh/config` brings its options.
pub fn ssh_args(target: &str) -> Vec<String> {
    vec![target.to_string()]
}

/// Start `ssh <target>` on `backend`, in this machine's home directory (so a
/// relative path in the user's SSH config resolves as it does in their own
/// terminal).
pub fn spawn(backend: &dyn PtyBackend, target: &str) -> Result<Terminal> {
    Terminal::spawn(
        backend,
        crate::terminal::EMULATOR,
        SSH,
        &ssh_args(target),
        &crate::terminal::terminal_env(),
        &home(),
        PtySize { rows: 24, cols: 80 },
    )
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| PathBuf::from("."))
}
