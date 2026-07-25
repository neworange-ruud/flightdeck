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

/// How long [`RelayHandle::restart`] waits for the killed relay to stop
/// answering on its port before rebinding it.
const PORT_RELEASE_TIMEOUT: Duration = Duration::from_secs(10);

/// A running `flightdeck-relay` subprocess, bound to `127.0.0.1:<port>`.
///
/// Kills the relay process on [`Drop`], so a test that spawns one and lets it
/// go out of scope (including on panic, via unwind) never leaks a relay
/// process into the next test run.
pub struct RelayHandle {
    child: Child,
    port: u16,
    /// The `FLIGHTDECK_RELAY_STORE` spec this relay runs with, or `None` for the
    /// default in-memory store. Replayed verbatim by [`Self::restart`] so a
    /// persistent relay comes back up on the same database.
    store_spec: Option<String>,
    /// Owns the persistent store's directory for the handle's whole lifetime,
    /// including across a [`Self::restart`] — dropping it would delete the very
    /// database the restart is supposed to reopen.
    _store_dir: Option<tempfile::TempDir>,
    /// Path to the SQLite database backing this relay, when it runs the
    /// persistent store. Exposed so a test can simulate partial state loss
    /// (see [`Self::db_path`]).
    db_path: Option<PathBuf>,
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
        Self::spawn_inner(port, None, None)
    }

    /// Spawn the relay with the **persistent** SQLite store
    /// (`FLIGHTDECK_RELAY_STORE=sqlite:<tmp>/relay.db`), on an OS-chosen free
    /// port. This is what the hosted relay runs (remote-control-vp2), and it is
    /// the only mode in which [`Self::restart`] can preserve pairings, claim
    /// tokens and per-stream seq watermarks across the restart.
    pub fn spawn_persistent() -> Self {
        let dir = tempfile::tempdir().expect("create temp dir for the relay's sqlite store");
        let db = dir.path().join("relay.db");
        let spec = format!("sqlite:{}", db.display());
        Self::spawn_inner(pick_free_port(), Some(spec), Some((dir, db)))
    }

    /// Shared spawn path: run the prebuilt binary with `PORT` (plus the store
    /// spec, when persistent) and block until `/healthz` answers `ok`.
    fn spawn_inner(
        port: u16,
        store_spec: Option<String>,
        store: Option<(tempfile::TempDir, PathBuf)>,
    ) -> Self {
        let child = spawn_child(port, store_spec.as_deref());
        let (store_dir, db_path) = match store {
            Some((dir, db)) => (Some(dir), Some(db)),
            None => (None, None),
        };
        let handle = RelayHandle {
            child,
            port,
            store_spec,
            _store_dir: store_dir,
            db_path,
        };
        handle.wait_for_healthz();
        handle
    }

    /// Kill this relay and bring a fresh process back up **on the same port**,
    /// with the same store spec — the test-harness equivalent of a container
    /// reschedule / redeploy (remote-control-bbf).
    ///
    /// The port is deliberately reused: the desktop under test has the relay URL
    /// baked into its `.flightdeck/config.toml` (see `support::desktop`), so a
    /// new port would look like a permanently-dead relay rather than a restarted
    /// one. Blocks until the old process has stopped answering and the new one
    /// answers `/healthz`.
    pub fn restart(&mut self) {
        self.stop();
        self.child = spawn_child(self.port, self.store_spec.as_deref());
        self.wait_for_healthz();
    }

    /// Kill the relay process and block until its port is free again, leaving the
    /// relay **down**. Idempotent. Between a `stop` and a [`Self::restart`] the
    /// store file is not open by anyone, which is the only safe window for a test
    /// to edit it (see [`Self::db_path`]).
    pub fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.wait_for_port_release();
    }

    /// Path to the relay's SQLite database, for a relay spawned via
    /// [`Self::spawn_persistent`]. Only meaningful while the relay process is
    /// **stopped** (between a kill and a restart): the store opens the file with
    /// the no-locking `unix-none` VFS, so concurrent outside writes are unsafe.
    /// Used to simulate partial state loss across a redeploy.
    pub fn db_path(&self) -> &Path {
        self.db_path
            .as_deref()
            .expect("db_path is only available for a relay spawned with spawn_persistent()")
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

    /// Block until nothing answers on this relay's port, so the replacement
    /// process can bind it. Called after the kill in [`Self::restart`]: the OS
    /// releases a listening socket as soon as the owning process dies, so this
    /// normally returns on the first poll — it exists to turn a slow teardown
    /// into a wait rather than an "address already in use" flake.
    fn wait_for_port_release(&self) {
        let deadline = Instant::now() + PORT_RELEASE_TIMEOUT;
        while Instant::now() < deadline {
            if TcpStream::connect_timeout(
                &(std::net::Ipv4Addr::LOCALHOST, self.port).into(),
                Duration::from_millis(200),
            )
            .is_err()
            {
                return;
            }
            std::thread::sleep(HEALTHZ_POLL_INTERVAL);
        }
        panic!(
            "port {} was still accepting connections {:?} after the relay was killed",
            self.port, PORT_RELEASE_TIMEOUT
        );
    }
}

/// Launch one relay child process on `port`, optionally with a
/// `FLIGHTDECK_RELAY_STORE` spec. Does not wait for readiness — callers poll
/// `/healthz`.
fn spawn_child(port: u16, store_spec: Option<&str>) -> Child {
    let bin = ensure_relay_built();
    let mut cmd = Command::new(&bin);
    cmd.env("PORT", port.to_string())
        .stdin(Stdio::null())
        // Inherited (not piped): the relay's tracing output is useful on
        // test failure, and piping would require a drain thread to avoid
        // the child blocking once the pipe buffer fills.
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    if let Some(spec) = store_spec {
        cmd.env("FLIGHTDECK_RELAY_STORE", spec);
    }
    cmd.spawn()
        .unwrap_or_else(|err| panic!("failed to spawn relay binary at {}: {err}", bin.display()))
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
