//! Desktop-under-PTY launcher for the FlightDeck Remote E2E harness (issue c3m.5).
//!
//! The desktop is the real FlightDeck TUI (`target/debug/flightdeck`, the root
//! crate's binary — note `remote/` is a *separate* cargo workspace, so this is
//! `cargo build` at the repo root, not `-p` anything). It only ever reads
//! crossterm events from a real terminal and renders to one, so it can't run
//! headless: we give it a real terminal by spawning it as the child of a PTY
//! (`portable-pty`, already a root dependency — see `src/terminal/pty.rs` for
//! the production use of the same crate).
//!
//! Nothing here touches the keyboard. The desktop is driven entirely from the
//! phone/relay side once paired; this launcher's only job is to boot it
//! deterministically and hermetically:
//!
//! - **`cwd` = the fixture repo** (built by `scripts/e2e/make-fixture-project.sh`,
//!   sibling issue c3m.2). The desktop reads that repo's
//!   `.flightdeck/config.toml`, which enables remote + points `relay_url` at the
//!   harness relay and the `claude` agent slot at the fake-agent stub.
//! - **`HOME` = a fresh temp dir.** Both `remote_state_path()`
//!   (`src/remote/state.rs`) and the global config path (`src/config/load.rs`)
//!   resolve through `$HOME`, so overriding it fully sandboxes `~/.flightdeck`
//!   and the seeded global config to a throwaway directory. The [`tempfile::TempDir`]
//!   is owned by the handle, so it is removed on drop.
//! - **`FLIGHTDECK_REMOTE_AUTOPAIR=<code>`** (default `4729`): the c3m.1 seam.
//!   With remote enabled (the fixture config does this) the desktop offers
//!   pairing non-interactively on the first tick with this exact 4-digit claim
//!   token, so no keypress and no random code — a phone driver (c3m.6/c3m.7) can
//!   claim `4729` deterministically.
//! - a sane `TERM` and a PTY size big enough for the TUI to render.
//!
//! A reader thread continuously drains the PTY master into a shared byte buffer
//! ([`output_snapshot`](DesktopHandle::output_snapshot) /
//! [`wait_for_output`](DesktopHandle::wait_for_output) read it back). The child
//! is killed on [`Drop`] and the reader thread joined, so no `flightdeck`
//! process (and no temp `$HOME`) is leaked into the next test.
#![allow(dead_code)]

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use portable_pty::{Child, ChildKiller, CommandBuilder, MasterPty, PtySize};
use tempfile::TempDir;

/// Default autopair code baked into the launcher (matches the plan / c3m.1
/// seam). Exactly four ASCII digits, as the seam requires.
pub const DEFAULT_AUTOPAIR_CODE: &str = "4729";

/// PTY geometry the desktop renders into. Wide + tall enough that the pairing
/// overlay and its code render without being truncated.
const PTY_ROWS: u16 = 40;
const PTY_COLS: u16 = 120;

/// Poll interval for [`DesktopHandle::wait_for_output`].
const OUTPUT_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// A running desktop FlightDeck TUI under a PTY, booted hermetically for the
/// E2E harness.
///
/// Kills the child and joins the output-reader thread on [`Drop`], and owns the
/// temporary `$HOME` so it is cleaned up too — a test that lets a handle go out
/// of scope (including on panic) leaks neither a `flightdeck` process nor a
/// temp directory.
pub struct DesktopHandle {
    /// The PTY child (the `flightdeck` process). `Option` so [`Drop`] can take
    /// it to `wait()` after killing.
    child: Option<Box<dyn Child + Send + Sync>>,
    /// Independent kill handle for the child (clone taken at spawn), usable
    /// even while `child` is borrowed for `wait`.
    killer: Box<dyn ChildKiller + Send + Sync>,
    /// The PTY master. Kept alive for the lifetime of the handle; the reader
    /// thread reads from an independent cloned reader, not this.
    _master: Box<dyn MasterPty + Send>,
    /// Accumulated PTY output. Shared with the reader thread.
    output: Arc<Mutex<Vec<u8>>>,
    /// Set to signal the reader thread to stop (belt-and-braces; the thread
    /// also stops on PTY EOF once the child is reaped).
    reader_stop: Arc<AtomicBool>,
    /// The output-reader thread, joined on drop.
    reader: Option<JoinHandle<()>>,
    /// Owned temp `$HOME`; its `Drop` removes the directory.
    _home: TempDir,
    /// The fixture repo the desktop runs against (its `cwd`).
    cwd: PathBuf,
    /// The autopair claim code the desktop was launched with.
    autopair_code: String,
}

impl DesktopHandle {
    /// Spawn the desktop against `fixture_dir` with the default autopair code
    /// ([`DEFAULT_AUTOPAIR_CODE`]).
    ///
    /// The relay port is *not* passed here on purpose: the fixture's
    /// `.flightdeck/config.toml` already bakes `relay_url` (that's what
    /// `make-fixture-project.sh` writes from its `PORT`), so the desktop learns
    /// the relay from the project config it reads out of `cwd`.
    pub fn spawn(fixture_dir: &Path) -> Self {
        Self::spawn_with_autopair(fixture_dir, DEFAULT_AUTOPAIR_CODE)
    }

    /// Spawn the desktop against `fixture_dir` with an explicit autopair code.
    ///
    /// Panics if `autopair_code` is not exactly four ASCII digits — the c3m.1
    /// seam ignores anything else, so an invalid code here would silently
    /// produce a desktop that never offers pairing, which is never what a test
    /// wants.
    pub fn spawn_with_autopair(fixture_dir: &Path, autopair_code: &str) -> Self {
        assert!(
            autopair_code.len() == 4 && autopair_code.bytes().all(|b| b.is_ascii_digit()),
            "autopair code must be exactly four ASCII digits, got {autopair_code:?}"
        );

        let desktop_bin = ensure_desktop_built();
        let home = tempfile::Builder::new()
            .prefix("flightdeck-e2e-home")
            .tempdir()
            .expect("create temp HOME for the desktop");

        let pty_system = portable_pty::native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: PTY_ROWS,
                cols: PTY_COLS,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("openpty for the desktop TUI");

        let mut cmd = CommandBuilder::new(&desktop_bin);
        cmd.cwd(fixture_dir);
        // Hermetic sandbox: HOME → temp isolates ~/.flightdeck + global config.
        cmd.env("HOME", home.path());
        // The c3m.1 autopair seam: deterministic, non-interactive pairing offer.
        cmd.env("FLIGHTDECK_REMOTE_AUTOPAIR", autopair_code);
        // A real terminal type so ratatui/crossterm render normally.
        cmd.env("TERM", "xterm-256color");

        let child = pair
            .slave
            .spawn_command(cmd)
            .unwrap_or_else(|err| panic!("failed to spawn {}: {err}", desktop_bin.display()));

        // Drop the slave once the child holds it, so PTY EOF propagates when the
        // child exits (mirrors src/terminal/pty.rs).
        drop(pair.slave);

        let killer = child.clone_killer();
        let reader = pair
            .master
            .try_clone_reader()
            .expect("clone the desktop PTY reader");

        let output: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
        let reader_stop = Arc::new(AtomicBool::new(false));

        let thread_output = Arc::clone(&output);
        let thread_stop = Arc::clone(&reader_stop);
        let reader_thread = std::thread::Builder::new()
            .name("desktop-pty-reader".to_string())
            .spawn(move || {
                let mut reader = reader;
                let mut chunk = [0u8; 8192];
                loop {
                    if thread_stop.load(Ordering::Relaxed) {
                        break;
                    }
                    match reader.read(&mut chunk) {
                        Ok(0) => break, // EOF: child exited and PTY closed.
                        Ok(n) => {
                            if let Ok(mut buf) = thread_output.lock() {
                                buf.extend_from_slice(&chunk[..n]);
                            } else {
                                break; // poisoned — nothing useful left to do.
                            }
                        }
                        Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(_) => break,
                    }
                }
            })
            .expect("spawn desktop PTY reader thread");

        DesktopHandle {
            child: Some(child),
            killer,
            _master: pair.master,
            output,
            reader_stop,
            reader: Some(reader_thread),
            _home: home,
            cwd: fixture_dir.to_path_buf(),
            autopair_code: autopair_code.to_string(),
        }
    }

    /// The fixture repo the desktop is running against.
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// The temp `$HOME` the desktop was launched with (where its sandboxed
    /// `~/.flightdeck` lives).
    pub fn home(&self) -> &Path {
        self._home.path()
    }

    /// The autopair claim code the desktop offers pairing with.
    pub fn autopair_code(&self) -> &str {
        &self.autopair_code
    }

    /// A UTF-8 (lossy) snapshot of everything the desktop has written to the
    /// PTY so far. Escape sequences are included verbatim; callers that want to
    /// assert on rendered text should search for a substring rather than expect
    /// an exact frame.
    pub fn output_snapshot(&self) -> String {
        let buf = self.output.lock().expect("desktop output buffer poisoned");
        String::from_utf8_lossy(&buf).into_owned()
    }

    /// Number of raw bytes drained from the PTY so far.
    pub fn output_len(&self) -> usize {
        self.output
            .lock()
            .expect("desktop output buffer poisoned")
            .len()
    }

    /// Poll the accumulated output until it contains `substring`, or `timeout`
    /// elapses. Returns `true` if the substring was seen.
    ///
    /// PTY screen-scraping is inherently brittle (ratatui interleaves cursor
    /// moves and styling between glyph runs), so prefer this only for stable,
    /// contiguous runs — e.g. the 4-digit autopair code, which the pairing
    /// overlay prints as one span.
    pub fn wait_for_output(&self, substring: &str, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if self.output_snapshot().contains(substring) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(OUTPUT_POLL_INTERVAL);
        }
    }

    /// Whether the desktop child is still running (has not exited).
    pub fn is_running(&mut self) -> bool {
        match self.child.as_mut() {
            Some(child) => matches!(child.try_wait(), Ok(None)),
            None => false,
        }
    }
}

impl Drop for DesktopHandle {
    fn drop(&mut self) {
        // Tear the desktop down with SIGKILL, NOT portable-pty's `killer.kill()`
        // (which sends SIGHUP). The desktop now TRAPS SIGHUP/SIGTERM/SIGINT and
        // runs a graceful shutdown on them, so SIGHUP no longer terminates it —
        // and on macOS a session-leader desktop that begins a graceful exit
        // while this harness still holds the PTY master open (its own input
        // reader thread blocked reading the slave) wedges permanently in the
        // kernel exit path, so `child.wait()` would hang forever. SIGKILL is
        // uncatchable and kills it outright, closing the slave fds so the reader
        // thread EOFs and joins cleanly. All best-effort: never panic mid-unwind.
        self.reader_stop.store(true, Ordering::Relaxed);
        #[cfg(unix)]
        {
            if let Some(pid) = self.child.as_ref().and_then(|c| c.process_id()) {
                // SAFETY: a bare kill(2); an already-dead pid just yields ESRCH.
                unsafe {
                    libc::kill(pid as i32, libc::SIGKILL);
                }
            }
        }
        #[cfg(not(unix))]
        let _ = self.killer.kill();
        if let Some(mut child) = self.child.take() {
            let _ = child.wait();
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

/// Build a fixture project via `scripts/e2e/make-fixture-project.sh`, pointing
/// its `relay_url` at `relay_port` and its `claude` agent slot at the sibling
/// `fake-agent.sh` stub.
///
/// Returns the fixture repo path (the script's last stdout line) and the
/// [`TempDir`] that owns it, so cleanup happens when the caller drops the
/// `TempDir`. The temp dir is passed to the script as its target directory
/// (`$1`) so *this* process owns the lifetime rather than the script's own
/// `mktemp -d`.
///
/// This is the convenience path so a test can go relay → fixture → desktop in a
/// few lines:
/// ```ignore
/// let relay = RelayHandle::spawn();
/// let (fixture, _fixture_dir) = make_fixture(relay.port());
/// let desktop = DesktopHandle::spawn(&fixture);
/// ```
pub fn make_fixture(relay_port: u16) -> (PathBuf, TempDir) {
    let repo_root = repo_root();
    let script = repo_root.join("scripts/e2e/make-fixture-project.sh");
    let fake_agent = repo_root.join("scripts/e2e/fake-agent.sh");
    assert!(
        script.is_file(),
        "fixture generator script missing at {}",
        script.display()
    );

    let target = tempfile::Builder::new()
        .prefix("flightdeck-e2e-fixture")
        .tempdir()
        .expect("create temp dir for the fixture project");

    let output = Command::new("bash")
        .arg(&script)
        .arg(target.path())
        .env("PORT", relay_port.to_string())
        .env("FAKE_AGENT", &fake_agent)
        .output()
        .unwrap_or_else(|err| panic!("failed to run {}: {err}", script.display()));

    assert!(
        output.status.success(),
        "{} exited with {}\nstdout:\n{}\nstderr:\n{}",
        script.display(),
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let fixture_path = stdout
        .lines()
        .last()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .unwrap_or_else(|| {
            panic!(
                "{} printed no fixture path on stdout; stderr:\n{}",
                script.display(),
                String::from_utf8_lossy(&output.stderr)
            )
        });

    (PathBuf::from(fixture_path), target)
}

/// Build the root `flightdeck` desktop binary exactly once per test process and
/// return the path to it.
///
/// The root crate is built with a plain `cargo build` (no `-p`), and its
/// artifact lands under the root `target/debug`, unlike the relay which lives
/// in the separate `remote/` workspace.
fn ensure_desktop_built() -> PathBuf {
    static BUILD: OnceLock<Result<PathBuf, String>> = OnceLock::new();
    match BUILD.get_or_init(build_desktop_binary) {
        Ok(path) => path.clone(),
        Err(err) => panic!("{err}"),
    }
}

fn build_desktop_binary() -> Result<PathBuf, String> {
    let repo_root = repo_root();
    let manifest = repo_root.join("Cargo.toml");
    if !manifest.is_file() {
        return Err(format!(
            "expected the root crate manifest at {} — is this running from the flightdeck repo root?",
            manifest.display()
        ));
    }

    let status = Command::new("cargo")
        .arg("build")
        .arg("--manifest-path")
        .arg(&manifest)
        .status()
        .map_err(|err| format!("failed to run `cargo build` for the desktop binary: {err}"))?;
    if !status.success() {
        return Err(format!("`cargo build` (desktop) exited with {status}"));
    }

    let bin_name = if cfg!(windows) {
        "flightdeck.exe"
    } else {
        "flightdeck"
    };
    let bin_path = repo_root.join("target/debug").join(bin_name);
    if !bin_path.is_file() {
        return Err(format!(
            "cargo build succeeded but the desktop binary is missing at {}",
            bin_path.display()
        ));
    }
    Ok(bin_path)
}

/// The root `flightdeck` crate's manifest directory (= repo root), available at
/// compile time since this file compiles as part of the root crate's test
/// target.
fn repo_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::support::relay::RelayHandle;

    /// End-to-end boot smoke test: real relay → fixture pointed at it → real
    /// desktop under a PTY. Proves the launcher boots the desktop hermetically
    /// and its autopair pairing offer goes live, without any keyboard input and
    /// without the phone driver (c3m.6/c3m.7 exercise the full pairing round
    /// trip; this stays self-contained).
    ///
    /// # What this asserts, and what it deliberately does not
    ///
    /// The strongest deterministic signal available at the boot level is that
    /// the **pairing overlay is live** — i.e. `FLIGHTDECK_REMOTE_AUTOPAIR`
    /// fired, remote is enabled from the fixture config, and the desktop began
    /// a pairing offer on its own on the first tick. The overlay's border title
    /// (`Pair Phone`) renders as a contiguous run, so it survives ratatui's
    /// per-cell escape interleaving and is a stable substring to wait on. This
    /// is exactly the "wait_for_output sees the pairing overlay" acceptance the
    /// task allows, and it is robust to the pairing *phase* the desktop reaches.
    ///
    /// It does **not** assert the overlay advances to *displaying the 4-digit
    /// code*. Reaching that requires the relay's `pairing_offer_ok`, and there
    /// is a frame-ordering gap that prevents it for a fresh-identity desktop
    /// here — see the c3m.7 note at the end of this test.
    #[test]
    fn desktop_boots_and_offers_pairing() {
        let relay = RelayHandle::spawn();
        let (fixture, _fixture_dir) = make_fixture(relay.port());

        let mut desktop = DesktopHandle::spawn(&fixture);
        assert_eq!(desktop.autopair_code(), DEFAULT_AUTOPAIR_CODE);
        assert_eq!(desktop.cwd(), fixture.as_path());

        // Primary, deterministic assertion: the autopair pairing overlay goes
        // live. Generous timeout — first-run global-config seeding + the .gitignore
        // pass + the first relay connect all happen before the overlay renders.
        let saw_overlay = desktop.wait_for_output("Pair Phone", Duration::from_secs(20));

        // Backstop assertions that hold regardless of screen-scraping: the
        // desktop must still be alive (it did not crash on boot) and it must
        // have rendered *something* to the PTY (the reader thread is draining).
        assert!(
            desktop.is_running(),
            "desktop exited during boot; output so far:\n{}",
            desktop.output_snapshot()
        );
        assert!(
            desktop.output_len() > 0,
            "desktop produced no PTY output — did the TUI render at all?"
        );
        assert!(
            saw_overlay,
            "did not observe the autopair pairing overlay (\"Pair Phone\") within the timeout; \
             desktop still running = {}; output so far:\n{}",
            desktop.is_running(),
            desktop.output_snapshot()
        );

        // The relay is unaffected by the desktop boot and still healthy.
        assert!(relay.healthz_ok(), "relay should still answer /healthz ok");

        // NOTE for c3m.7 (full pairing + capability round trip via the phone
        // driver, reusing this DesktopHandle): the overlay above stays in the
        // "Offering" phase ("Requesting a pairing code from the relay…") and
        // never advances to "Code 4729". Root cause is a frame-ordering gap in
        // the *production* client, not this launcher:
        //   - The relay's desktop bootstrap (per remote/relay session.rs and its
        //     TestClient) is offer→auth: the pre-auth `pairing_offer` self-
        //     registers the desktop's device key (session.rs::on_pairing_offer),
        //     so the subsequent `auth_response` can be verified.
        //   - But src/remote/client.rs::run_session completes auth FIRST and only
        //     drains `RequestPairing` (→ sends `pairing_offer`) later, in `pump`.
        //     For a fresh identity the device is unregistered at auth_response
        //     time, so the relay replies AuthFailed "unknown device"
        //     (session.rs::on_auth_response ~:583) and closes; the client then
        //     reconnect-loops and the offer is never sent.
        // So a fresh-HOME desktop cannot currently complete pairing in-harness.
        // c3m.7 will need either a small production fix (send a pending offer as
        // a pre-auth bootstrap frame before `auth_response`) or to drive pairing
        // in whatever order the shipped client actually supports. This module
        // does not touch src/, so it stops at asserting the overlay is live.
        drop(desktop);
    }
}
