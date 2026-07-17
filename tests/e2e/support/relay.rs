//! Launches the **real** `flightdeck-relay` binary for the E2E harness.
//!
//! `remote/` is its own Cargo workspace (see `remote/Cargo.toml`), separate
//! from the root `flightdeck` crate that owns this test target, so the relay
//! can't be pulled in as a library dependency of the test binary — it has to
//! be built and run as a subprocess, exactly like a real deployment would run
//! it. This mirrors `cargo run -p flightdeck-relay` (see the plan's "what
//! already exists" notes) but runs the prebuilt binary directly instead of
//! going through `cargo run` on every spawn, which is both faster and avoids
//! cargo's own stdout/stderr chatter interleaving with the relay's.
//!
//! Confirmed relay facts (do not assume, these are load-bearing):
//! - Binary name is `flightdeck-relay` (`remote/relay/Cargo.toml` `[[bin]]`,
//!   `name = "flightdeck-relay"`).
//! - Port comes from the `PORT` env var, default `8080`
//!   (`remote/relay/src/config.rs::Config::from_env`, reads `env::var("PORT")`;
//!   `main.rs` binds `0.0.0.0:<port>`).
//! - Liveness probe is `GET /healthz` returning the plain-text body `ok`
//!   (`remote/relay/src/handlers.rs::healthz`, wired at `/healthz` in
//!   `remote/relay/src/lib.rs`).
#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// How long [`RelayHandle::spawn_on`] will poll `/healthz` before giving up.
const HEALTHZ_TIMEOUT: Duration = Duration::from_secs(30);
/// Delay between healthz poll attempts.
const HEALTHZ_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// A running `flightdeck-relay` subprocess, bound to `127.0.0.1:<port>`.
///
/// Kills the relay process on [`Drop`], so a test that spawns one and lets it
/// go out of scope (including on panic, via unwind) never leaks a relay
/// process into the next test run.
pub struct RelayHandle {
    child: Child,
    port: u16,
}

impl RelayHandle {
    /// Spawn the relay on an OS-chosen free port.
    ///
    /// Picks the port by binding a `TcpListener` to `127.0.0.1:0` and reading
    /// back the assigned port, then dropping the listener before the relay
    /// binds it. There's an inherent (tiny) TOCTOU window between the drop
    /// and the relay's own bind — acceptable for a test harness, matches the
    /// same trade-off other free-port pickers make.
    pub fn spawn() -> Self {
        let port = pick_free_port();
        Self::spawn_on(port)
    }

    /// Spawn the relay bound to a specific port.
    ///
    /// Builds `flightdeck-relay` once per test process (via a [`OnceLock`]),
    /// then runs the prebuilt binary with `PORT=<port>` and polls `/healthz`
    /// until it answers `ok` or [`HEALTHZ_TIMEOUT`] elapses.
    pub fn spawn_on(port: u16) -> Self {
        let bin = ensure_relay_built();

        let child = Command::new(&bin)
            .env("PORT", port.to_string())
            .stdin(Stdio::null())
            // Inherited (not piped): the relay's tracing output is useful on
            // test failure, and piping would require a drain thread to avoid
            // the child blocking once the pipe buffer fills.
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap_or_else(|err| {
                panic!("failed to spawn relay binary at {}: {err}", bin.display())
            });

        let handle = RelayHandle { child, port };
        handle.wait_for_healthz();
        handle
    }

    /// The port the relay is bound to.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Base HTTP URL, e.g. `http://127.0.0.1:PORT`.
    pub fn http_base(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// WebSocket URL for the relay's `/ws` endpoint.
    pub fn ws_url(&self) -> String {
        format!("ws://127.0.0.1:{}/ws", self.port)
    }

    /// One-shot `GET /healthz` check against the running relay. `spawn_on`
    /// already blocks until this is true once; exposed separately so callers
    /// (and this module's own smoke test) can assert on it explicitly at any
    /// later point too.
    pub fn healthz_ok(&self) -> bool {
        healthz_once(self.port).is_ok()
    }

    /// Poll `GET /healthz` until it returns body `ok`, or panic with a clear
    /// message after [`HEALTHZ_TIMEOUT`].
    fn wait_for_healthz(&self) {
        let deadline = Instant::now() + HEALTHZ_TIMEOUT;
        let mut last_err = String::from("no attempt made");

        while Instant::now() < deadline {
            match healthz_once(self.port) {
                Ok(()) => return,
                Err(err) => last_err = err,
            }
            std::thread::sleep(HEALTHZ_POLL_INTERVAL);
        }

        panic!(
            "relay on port {} did not answer GET /healthz with \"ok\" within {:?}; last error: {last_err}",
            self.port, HEALTHZ_TIMEOUT
        );
    }
}

impl Drop for RelayHandle {
    fn drop(&mut self) {
        // Best-effort: if the process already exited there's nothing to kill,
        // and a failed kill/wait here must never panic mid-unwind.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Build `flightdeck-relay` exactly once per test process and return the path
/// to the resulting binary. Subsequent calls (from later tests, or repeated
/// `RelayHandle::spawn` calls) reuse the cached path without re-invoking
/// cargo.
fn ensure_relay_built() -> PathBuf {
    static BUILD: OnceLock<Result<PathBuf, String>> = OnceLock::new();

    match BUILD.get_or_init(build_relay_binary) {
        Ok(path) => path.clone(),
        Err(err) => panic!("{err}"),
    }
}

/// Run `cargo build -p flightdeck-relay` against the `remote/` workspace and
/// return the path to the built debug binary.
fn build_relay_binary() -> Result<PathBuf, String> {
    let repo_root = repo_root();
    let relay_manifest = repo_root.join("remote/relay/Cargo.toml");
    if !relay_manifest.is_file() {
        return Err(format!(
            "expected relay manifest at {} — is this running from the flightdeck repo root?",
            relay_manifest.display()
        ));
    }

    let status = Command::new("cargo")
        .args(["build", "-p", "flightdeck-relay", "--manifest-path"])
        .arg(&relay_manifest)
        .status()
        .map_err(|err| format!("failed to run `cargo build -p flightdeck-relay`: {err}"))?;

    if !status.success() {
        return Err(format!(
            "`cargo build -p flightdeck-relay` exited with {status}"
        ));
    }

    // `remote/` is its own Cargo workspace (see remote/Cargo.toml), so its
    // build artifacts land under remote/target, not the root target dir.
    let bin_name = if cfg!(windows) {
        "flightdeck-relay.exe"
    } else {
        "flightdeck-relay"
    };
    let bin_path = repo_root.join("remote/target/debug").join(bin_name);
    if !bin_path.is_file() {
        return Err(format!(
            "cargo build succeeded but the relay binary is missing at {}",
            bin_path.display()
        ));
    }

    Ok(bin_path)
}

/// The root `flightdeck` crate's manifest directory, i.e. the repo root —
/// available at compile time since this file is compiled as part of the root
/// crate's test target.
fn repo_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// Bind an ephemeral port, read it back, and release it immediately so the
/// relay can bind it in turn.
fn pick_free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind an ephemeral TCP port to pick a free port for the relay");
    listener
        .local_addr()
        .expect("read local address of ephemeral listener")
        .port()
}

/// Issue one raw `GET /healthz` over a plain `TcpStream` (no new HTTP-client
/// dependency — see the task's constraint to prefer `TcpStream` for this) and
/// check the response is a `200` with body `ok`.
fn healthz_once(port: u16) -> Result<(), String> {
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .map_err(|err| format!("connect to 127.0.0.1:{port} failed: {err}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|err| format!("set_read_timeout failed: {err}"))?;
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(|err| format!("set_write_timeout failed: {err}"))?;

    let request =
        format!("GET /healthz HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .map_err(|err| format!("write healthz request failed: {err}"))?;

    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .map_err(|err| format!("read healthz response failed: {err}"))?;
    let response = String::from_utf8_lossy(&response);

    let status_ok = response.starts_with("HTTP/1.1 200") || response.starts_with("HTTP/1.0 200");
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.trim())
        .unwrap_or("");

    if status_ok && body == "ok" {
        Ok(())
    } else {
        Err(format!("unexpected healthz response: {response:?}"))
    }
}
