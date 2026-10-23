//! FlightDeck Worker, end to end (`specs/WORKER_PRD.md` §8, W30).
//!
//! Every test drives the **real binary**: `flightdeck worker run` in a fresh
//! `$HOME`, the `flightdeck worker …` CLI against it, and the real native
//! client (`flightdeck::web::client::RemoteClient`) over a loopback socket —
//! the same client FlightDeck Desktop uses. Nothing in-process stands in for
//! the worker, so what passes here is what a Desktop would see.
//!
//! The worker listens on `127.0.0.1:0` through the debug-only
//! `FLIGHTDECK_WORKER_TEST_LISTEN` seam (a test has no tailnet). Agents are
//! `scripts/e2e/fake-agent.sh`, which writes `working` / `idle` to the agent
//! status file and echoes what it is sent; the worker shell is `/bin/sh`.
//!
//! Each test owns its home, under `/tmp` and short: the control socket's path
//! must fit `sun_path` (104 bytes on macOS).

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::time::{Duration, Instant};

use flightdeck::contracts::PtySession;
use flightdeck::contracts::{InterpretedStatus, PtySize, RealFs};
use flightdeck::remote::runtime;
use flightdeck::web::client::{
    exchange_code, AccessToken, CloneEvent, LinkConfig, LinkEnd, LinkState, RemoteClient,
    StreamPty, Target,
};
use flightdeck::web::protocol::{
    command as names, Ack, AckOutcome, ConnectionKind, Geometry, HostKind, ProjectId, SeatRequest,
    SessionView, TerminalId, TerminalRole, WORKER_SHELL_TERMINAL_ID,
};
use flightdeck::worker::control::{DeviceReport, PairingCode};
use flightdeck::worker::lock::{worker_running, LOCAL_HOST_REFUSAL};
use flightdeck::worker::paths::WorkerPaths;
use flightdeck::worker::status::{WorkerState, WorkerStatusReport};

/// How long anything may take: agents, git and process start-up are slow on a
/// loaded CI runner, and a generous deadline costs nothing when it passes.
const WAIT: Duration = Duration::from_secs(45);
/// How often a wait looks again.
const STEP: Duration = Duration::from_millis(20);
/// The startup grace during which a worker raises no finish notification
/// (`NOTIFY_STARTUP_GRACE_MS`, 4 s), plus margin.
const NOTIFY_GRACE: Duration = Duration::from_millis(5_000);

/// The label a test Desktop pairs and attaches with: the worker shows the
/// device in brackets (design A11).
fn desktop(device: &str) -> String {
    format!("FlightDeck Desktop/test ({device})")
}

// ---------------------------------------------------------------------------
// The worker process
// ---------------------------------------------------------------------------

/// A worker for one test: its own home, its own process.
struct Worker {
    home: tempfile::TempDir,
    /// `worker run` in the foreground, when this test started one.
    child: Option<Child>,
    /// Whether this test started a supervisor (`worker start`), which `Drop`
    /// then stops.
    supervised: bool,
    /// The bound `ip:port` of the current run.
    addr: String,
    /// How many times `worker run` was started, for the stderr file names.
    runs: u32,
}

impl Worker {
    /// A fresh home with a config, and a worker running in it.
    fn start() -> Worker {
        let mut worker = Worker::prepare();
        worker.run();
        worker
    }

    /// A fresh home with a config, nothing running yet.
    fn prepare() -> Worker {
        let home = tempfile::Builder::new()
            .prefix("fdw")
            .tempdir_in("/tmp")
            .expect("a temp home under /tmp");
        let dir = home.path().join(".flightdeck");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.toml"), config_toml()).unwrap();
        Worker {
            home,
            child: None,
            supervised: false,
            addr: String::new(),
            runs: 0,
        }
    }

    fn home(&self) -> &Path {
        self.home.path()
    }

    fn paths(&self) -> WorkerPaths {
        WorkerPaths::from_home(self.home())
    }

    /// The binary, with this worker's environment.
    fn command(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_flightdeck"));
        cmd.current_dir(self.home())
            .env("HOME", self.home())
            .env("SHELL", "/bin/sh")
            .env("FLIGHTDECK_WORKER_TEST_LISTEN", "127.0.0.1:0")
            .envs(git_env(self.home()))
            .stdin(Stdio::null());
        cmd
    }

    /// Run `flightdeck <args>` to completion.
    fn cli(&self, args: &[&str]) -> Output {
        self.command()
            .args(args)
            .output()
            .expect("the flightdeck binary runs")
    }

    /// Run a `--json` command and parse what it printed.
    fn cli_json<T: serde::de::DeserializeOwned>(&self, args: &[&str]) -> T {
        let out = self.cli(args);
        assert!(
            out.status.success(),
            "`flightdeck {}` failed ({}): {}{}",
            args.join(" "),
            out.status,
            String::from_utf8_lossy(&out.stderr),
            self.log_tail()
        );
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!(
                "`flightdeck {}` printed no JSON ({e}): {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stdout)
            )
        })
    }

    fn status(&self) -> WorkerStatusReport {
        self.cli_json(&["worker", "status", "--json"])
    }

    /// Start `worker run` in the foreground and wait for it to listen.
    fn run(&mut self) {
        self.runs += 1;
        let stderr = std::fs::File::create(self.stderr_path(self.runs)).unwrap();
        self.spawn_run(Stdio::from(stderr));
    }

    /// Start `worker run` with its stderr a pipe nobody reads: the read end
    /// is closed at once, as when a supervisor reading it has died.
    fn run_with_a_closed_log(&mut self) {
        self.spawn_run(Stdio::piped());
        drop(self.child.as_mut().unwrap().stderr.take());
    }

    fn spawn_run(&mut self, stderr: Stdio) {
        assert!(self.child.is_none(), "already running");
        let child = self
            .command()
            .args(["worker", "run"])
            .stdout(Stdio::null())
            .stderr(stderr)
            .spawn()
            .expect("`flightdeck worker run` starts");
        self.child = Some(child);
        self.addr = self.wait_listening();
    }

    /// Poll `worker status --json` until the worker answers with an address.
    fn wait_listening(&mut self) -> String {
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(child) = self.child.as_mut() {
                if let Ok(Some(exit)) = child.try_wait() {
                    panic!("the worker exited early ({exit}){}", self.log_tail());
                }
            }
            let out = self.cli(&["worker", "status", "--json"]);
            if out.status.success() {
                if let Ok(status) = serde_json::from_slice::<WorkerStatusReport>(&out.stdout) {
                    let addr = status
                        .listen
                        .as_ref()
                        .and_then(|l| l.addresses.first().cloned());
                    if let (WorkerState::Running, Some(addr)) = (status.state, addr) {
                        return addr;
                    }
                }
            }
            assert!(
                Instant::now() < deadline,
                "the worker never listened{}",
                self.log_tail()
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// SIGTERM the foreground worker and wait for it: a clean stop.
    fn stop(&mut self) -> ExitStatus {
        let mut child = self.child.take().expect("a foreground worker");
        signal(child.id(), libc::SIGTERM);
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                return status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("the worker ignored SIGTERM{}", self.log_tail());
            }
            std::thread::sleep(STEP);
        }
    }

    fn stderr_path(&self, run: u32) -> PathBuf {
        self.home().join(format!("worker-run-{run}.stderr"))
    }

    /// What the worker logged, for a failure message.
    fn log_tail(&self) -> String {
        let mut text = String::new();
        for run in 1..=self.runs {
            if let Ok(log) = std::fs::read_to_string(self.stderr_path(run)) {
                text.push_str(&format!("\n--- worker run {run} stderr ---\n{log}"));
            }
        }
        if let Ok(log) = std::fs::read_to_string(&self.paths().log_file) {
            text.push_str(&format!("\n--- worker.log ---\n{log}"));
        }
        text
    }

    /// Mint a code with `worker pair --json` and exchange it, as a Desktop's
    /// "Connect to remote…" does.
    fn pair(&self, device: &str) -> AccessToken {
        let code: PairingCode = self.cli_json(&["worker", "pair", "--json"]);
        assert!(code.listening, "{code:?}");
        assert_eq!(code.code.len(), 4, "a four-digit code: {code:?}");
        on_runtime(exchange_code(&self.addr, &code.code, &desktop(device)))
            .unwrap_or_else(|e| panic!("the code is exchanged: {e:?}{}", self.log_tail()))
    }

    /// A Desktop window or background link with `token`.
    fn connect(&self, token: &AccessToken, device: &str, kind: ConnectionKind) -> RemoteClient {
        RemoteClient::connect(LinkConfig {
            address: self.addr.clone(),
            token: token.clone(),
            seat: match kind {
                ConnectionKind::Window => SeatRequest::Write,
                ConnectionKind::Background => SeatRequest::Observe,
            },
            user_agent: desktop(device),
            connection: kind,
            retry_cap_ms: (kind == ConnectionKind::Background).then_some(60_000),
        })
    }

    /// Pair `device` and open a live window.
    fn window(&self, device: &str) -> (AccessToken, RemoteClient) {
        let token = self.pair(device);
        let mut client = self.connect(&token, device, ConnectionKind::Window);
        self.wait_live(&mut client, "the window to attach");
        (token, client)
    }

    /// Pump `client` until it is attached and has its snapshot.
    fn wait_live(&self, client: &mut RemoteClient, what: &str) {
        self.wait(what, || {
            client.pump();
            client.state().is_live() && client.workspace().ready
        });
    }

    /// Poll `done` until it holds, failing with the worker's log.
    fn wait(&self, what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + WAIT;
        while !done() {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}{}",
                self.log_tail()
            );
            std::thread::sleep(STEP);
        }
    }

    /// Pump `client` until the ack for `seq` arrives.
    fn ack(&self, client: &mut RemoteClient, seq: Option<u64>, what: &str) -> Ack {
        let seq = seq.unwrap_or_else(|| panic!("{what}: the command did not go out"));
        let mut found = None;
        let mut errors = Vec::new();
        self.wait(&format!("the ack of {what}"), || {
            client.pump();
            errors.extend(client.take_errors());
            found = client.take_results().into_iter().find(|a| a.seq == seq);
            assert!(errors.is_empty(), "{what}: error frames {errors:?}");
            found.is_some()
        });
        found.unwrap()
    }

    /// The ack for `seq`, which must be `Applied`.
    fn applied(&self, client: &mut RemoteClient, seq: Option<u64>, what: &str) -> Ack {
        let ack = self.ack(client, seq, what);
        assert_eq!(
            ack.outcome,
            AckOutcome::Applied,
            "{what}: {:?}{}",
            ack.detail,
            self.log_tail()
        );
        ack
    }

    /// The ack for `seq`, which must be `Rejected`; returns its reason.
    fn rejected(&self, client: &mut RemoteClient, seq: Option<u64>, what: &str) -> String {
        let ack = self.ack(client, seq, what);
        assert_eq!(ack.outcome, AckOutcome::Rejected, "{what}: {ack:?}");
        ack.detail.unwrap_or_default()
    }

    /// Open the repository at `path` from `client` and wait for the project
    /// to appear in its mirror.
    fn open_project(&self, client: &mut RemoteClient, path: &Path) -> ProjectId {
        let seq = client.open_project_path(&path.display().to_string());
        self.applied(client, seq, "open_project_path");
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let mut id = None;
        self.wait("the opened project in the mirror", || {
            client.pump();
            id = project_named(client, &name);
            id.is_some()
        });
        id.unwrap()
    }

    /// Spawn an agent in `project` through the new-agent dialog, as the
    /// palette's "New Agent Session Tab" row does, on branch `branch`.
    fn spawn_agent(
        &self,
        client: &mut RemoteClient,
        project: &ProjectId,
        branch: &str,
    ) -> SessionView {
        let before: Vec<_> = sessions_of(client, project)
            .into_iter()
            .map(|s| s.session_id)
            .collect();
        let seq = client.command_for(
            names::NEW_AGENT_SESSION_TAB,
            &Target::Project(project.clone()),
        );
        self.applied(client, seq, "new_agent_session_tab");
        let mut dialog = None;
        self.wait("the new-agent dialog", || {
            client.pump();
            dialog = client.workspace().dialog.clone();
            dialog.is_some()
        });
        let dialog = dialog.unwrap();
        assert_eq!(dialog.kind, "new_agent", "{dialog:?}");
        let seq = client.command(
            names::DIALOG_CONFIRM,
            Some(serde_json::json!({
                "dialog_id": dialog.dialog_id,
                "choice": "Enter",
                "text": branch,
            })),
        );
        self.applied(client, seq, "dialog_confirm");
        let mut spawned = None;
        self.wait("the agent session to run", || {
            client.pump();
            spawned = sessions_of(client, project)
                .into_iter()
                .find(|s| !before.contains(&s.session_id) && primary_alive(s));
            spawned.is_some()
        });
        spawned.unwrap()
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            signal(child.id(), libc::SIGTERM);
            let deadline = Instant::now() + Duration::from_secs(20);
            while Instant::now() < deadline {
                if let Ok(Some(_)) = child.try_wait() {
                    break;
                }
                std::thread::sleep(STEP);
            }
            let _ = child.kill();
            let _ = child.wait();
        }
        if self.supervised {
            let _ = self.cli(&["worker", "stop"]);
        }
    }
}

/// The global config a test worker reads: the shipping defaults, with no
/// update check, the fake agent as the default, and `~/flightdeck` as the
/// projects root.
fn config_toml() -> String {
    use flightdeck::contracts::domain::AgentDef;
    let mut config = flightdeck::config::schema::default_global_config();
    config.update.check = false;
    config.ui.default_agent = "fake".to_string();
    config.worker.projects_root = "~/flightdeck".to_string();
    config.agents.insert(
        "fake".to_string(),
        AgentDef {
            key: "fake".to_string(),
            display_name: "Fake".to_string(),
            command: Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("scripts/e2e/fake-agent.sh")
                .display()
                .to_string(),
            args: Vec::new(),
            status_patterns: Default::default(),
        },
    );
    flightdeck::config::load::serialize_global_config(&config).expect("the config serializes")
}

/// A git identity and no system or user git config, so commits work and
/// nothing on the machine running the tests leaks in.
fn git_env(home: &Path) -> Vec<(&'static str, String)> {
    vec![
        ("GIT_AUTHOR_NAME", "FlightDeck E2E".to_string()),
        ("GIT_AUTHOR_EMAIL", "e2e@flightdeck.invalid".to_string()),
        ("GIT_COMMITTER_NAME", "FlightDeck E2E".to_string()),
        ("GIT_COMMITTER_EMAIL", "e2e@flightdeck.invalid".to_string()),
        ("GIT_CONFIG_NOSYSTEM", "1".to_string()),
        ("GIT_TERMINAL_PROMPT", "0".to_string()),
        ("HOME", home.display().to_string()),
    ]
}

fn signal(pid: u32, sig: libc::c_int) {
    // SAFETY: kill(2) with a pid this test spawned (or read from the worker's
    // own status) and a constant signal number.
    unsafe {
        libc::kill(pid as libc::pid_t, sig);
    }
}

fn on_runtime<F: std::future::Future>(body: F) -> F::Output {
    runtime::shared().handle().block_on(body)
}

// ---------------------------------------------------------------------------
// Git fixtures
// ---------------------------------------------------------------------------

fn git(home: &Path, dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .envs(git_env(home))
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A repository on `main` with one commit.
fn make_repo(home: &Path, path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    git(home, path, &["init", "-q", "-b", "main"]);
    std::fs::write(path.join("README.md"), "# fixture\n").unwrap();
    git(home, path, &["add", "README.md"]);
    git(home, path, &["commit", "-q", "-m", "Initial commit"]);
}

/// A bare repository at `bare`, cloned from a fresh one beside it.
fn make_bare(home: &Path, bare: &Path) {
    let source = bare.with_extension("src");
    make_repo(home, &source);
    git(
        home,
        bare.parent().unwrap(),
        &[
            "clone",
            "-q",
            "--bare",
            &source.display().to_string(),
            &bare.display().to_string(),
        ],
    );
}

// ---------------------------------------------------------------------------
// Reading the mirror
// ---------------------------------------------------------------------------

fn project_named(client: &RemoteClient, name: &str) -> Option<ProjectId> {
    client
        .workspace()
        .projects
        .iter()
        .find(|p| p.name == name || p.root.ends_with(&format!("/{name}")))
        .map(|p| p.project_id.clone())
}

fn sessions_of(client: &RemoteClient, project: &ProjectId) -> Vec<SessionView> {
    client
        .workspace()
        .projects
        .iter()
        .find(|p| &p.project_id == project)
        .map(|p| p.sessions.clone())
        .unwrap_or_default()
}

fn primary(session: &SessionView) -> &flightdeck::web::protocol::TerminalView {
    session
        .terminals
        .iter()
        .find(|t| t.role == TerminalRole::Primary)
        .expect("a session has a primary terminal")
}

fn primary_alive(session: &SessionView) -> bool {
    session
        .terminals
        .iter()
        .any(|t| t.role == TerminalRole::Primary && t.alive)
}

fn shell_id() -> TerminalId {
    TerminalId::new(WORKER_SHELL_TERMINAL_ID)
}

/// Everything `pty` has been sent since the last read, appended to `into`.
fn read_into(pty: &mut StreamPty, into: &mut Vec<u8>) {
    into.extend(pty.try_read_output().unwrap());
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    String::from_utf8_lossy(haystack).contains(needle)
}

/// Type `line` into the worker shell and wait for `expect` in its output.
fn shell_round_trip(
    worker: &Worker,
    client: &mut RemoteClient,
    pty: &mut StreamPty,
    line: &str,
    expect: &str,
) {
    let mut seen = Vec::new();
    pty.write_input(format!("{line}\r").as_bytes()).unwrap();
    worker.wait(&format!("`{expect}` from the worker shell"), || {
        client.pump();
        read_into(pty, &mut seen);
        contains(&seen, expect)
    });
}

// ---------------------------------------------------------------------------
// 1. Pairing
// ---------------------------------------------------------------------------

/// A code minted on the box by `worker pair` pairs a Desktop, whose window
/// attaches to a worker host that already runs its shell.
#[test]
fn a_cli_minted_code_pairs_and_the_window_attaches_to_a_worker() {
    let worker = Worker::start();
    let (_token, client) = worker.window("laptop");

    assert!(client.is_worker());
    let ws = client.workspace();
    assert_eq!(ws.host_kind, Some(HostKind::Worker));
    assert!(ws.projects.is_empty(), "a fresh worker has no projects");
    let mirror = ws.worker.as_ref().expect("a worker snapshot");
    let shell = mirror.shell.as_ref().expect("the worker shell is running");
    assert_eq!(shell.terminal_id, shell_id());
    assert!(shell.alive, "{shell:?}");
    let home = std::fs::canonicalize(worker.home()).unwrap();
    assert_eq!(
        std::fs::canonicalize(&mirror.home).unwrap(),
        home,
        "the picker's root is the worker's $HOME"
    );

    // The CLI sees the device and its window.
    let devices: Vec<DeviceReport> = worker.cli_json(&["worker", "devices", "--json"]);
    assert_eq!(devices.len(), 1, "{devices:?}");
    assert_eq!(devices[0].label, "laptop");
    let status = worker.status();
    assert_eq!(
        status.attached.as_ref().map(|a| a.label.as_str()),
        Some("laptop"),
        "{status:?}"
    );

    // A code is single use.
    let code: PairingCode = worker.cli_json(&["worker", "pair", "--json"]);
    on_runtime(exchange_code(&worker.addr, &code.code, &desktop("studio")))
        .expect("the first exchange succeeds");
    assert!(
        on_runtime(exchange_code(&worker.addr, &code.code, &desktop("studio"))).is_err(),
        "a used code is refused"
    );
    client.stop();
}

// ---------------------------------------------------------------------------
// 2. Open, scan, browse, and the $HOME boundary
// ---------------------------------------------------------------------------

/// A repository under `$HOME` opens; the projects root is scanned; folders
/// are browsed; nothing outside `$HOME` opens or lists, not even through a
/// link inside it.
#[test]
fn projects_open_scan_and_browse_inside_home_only() {
    let worker = Worker::start();
    let home = worker.home().to_path_buf();
    let alpha = home.join("code").join("alpha");
    make_repo(&home, &alpha);
    let beta = home.join("flightdeck").join("beta");
    make_repo(&home, &beta);
    let outside = tempfile::Builder::new()
        .prefix("fdo")
        .tempdir_in("/tmp")
        .unwrap();
    let foreign = outside.path().join("foreign");
    make_repo(&home, &foreign);
    std::os::unix::fs::symlink(&foreign, home.join("escape")).unwrap();

    let (_token, mut client) = worker.window("laptop");

    // Open.
    let alpha_id = worker.open_project(&mut client, &alpha);
    let status = worker.status();
    assert!(
        status
            .projects
            .iter()
            .any(|p| p.root.ends_with("code/alpha")),
        "{status:?}"
    );

    // Scan: the projects root holds beta, which is not open.
    let seq = client.scan_projects();
    worker.applied(&mut client, seq, "scan_projects");
    let mut scan = None;
    worker.wait("the project scan", || {
        client.pump();
        scan = client.take_project_scan();
        scan.is_some()
    });
    let scan = scan.unwrap();
    assert!(scan.root_exists, "{scan:?}");
    assert!(scan.root.ends_with("/flightdeck"), "{scan:?}");
    let found = scan
        .repos
        .iter()
        .find(|r| r.name == "beta")
        .unwrap_or_else(|| panic!("beta is found: {scan:?}"));
    assert!(!found.is_open_project);

    // Browse: $HOME, then into `code`, where alpha is an open repository.
    let seq = client.browse_dir(None, false);
    worker.applied(&mut client, seq, "browse_dir $HOME");
    let mut listing = None;
    worker.wait("the home listing", || {
        client.pump();
        listing = client.take_dir_listing();
        listing.is_some()
    });
    let listing = listing.unwrap();
    assert!(
        listing.entries.iter().any(|e| e.name == "code"),
        "{listing:?}"
    );
    let code_dir = home.join("code").display().to_string();
    let seq = client.browse_dir(Some(&code_dir), false);
    worker.applied(&mut client, seq, "browse_dir code");
    let mut listing = None;
    worker.wait("the code listing", || {
        client.pump();
        listing = client.take_dir_listing();
        listing.is_some()
    });
    let listing = listing.unwrap();
    let entry = listing
        .entries
        .iter()
        .find(|e| e.name == "alpha")
        .unwrap_or_else(|| panic!("alpha is listed: {listing:?}"));
    assert!(entry.is_git_repo && entry.is_open_project, "{entry:?}");

    // Outside $HOME: refused, by path and through the link.
    let foreign_path = foreign.display().to_string();
    let escape = home.join("escape").display().to_string();
    type Send = fn(&RemoteClient, &str) -> Option<u64>;
    let open: Send = |c, path| c.open_project_path(path);
    let browse: Send = |c, path| c.browse_dir(Some(path), false);
    for (what, send, path) in [
        ("open outside home", open, foreign_path.as_str()),
        ("open through a link", open, escape.as_str()),
        ("browse outside home", browse, "/"),
        ("browse through a link", browse, escape.as_str()),
    ] {
        // One at a time: an ack is matched by its seq, and the rest dropped.
        let seq = send(&client, path);
        let reason = worker.rejected(&mut client, seq, what);
        assert!(
            reason.contains("outside your home folder"),
            "{what}: refused for being outside $HOME: {reason}"
        );
    }
    assert_eq!(
        client.workspace().projects.len(),
        1,
        "only alpha is open: {:?}",
        client.workspace().projects
    );
    assert_eq!(client.workspace().projects[0].project_id, alpha_id);
    client.stop();
}

// ---------------------------------------------------------------------------
// 3. Clone
// ---------------------------------------------------------------------------

/// A clone reports progress, then its result, and opens what it cloned; a
/// failing clone reports git's own words; a name in use is refused up front.
#[test]
fn a_clone_reports_progress_opens_the_project_and_failures_say_why() {
    let worker = Worker::start();
    let home = worker.home().to_path_buf();
    let outside = tempfile::Builder::new()
        .prefix("fdo")
        .tempdir_in("/tmp")
        .unwrap();
    let bare = outside.path().join("origin.git");
    make_bare(&home, &bare);
    let url = format!("file://{}", bare.display());

    let (_token, mut client) = worker.window("laptop");

    let seq = client.clone_project(&url, None);
    let ack = worker.applied(&mut client, seq, "clone_project");
    assert!(
        ack.detail.as_deref().unwrap_or("").contains("origin"),
        "{ack:?}"
    );
    let mut events = Vec::new();
    worker.wait("the clone result", || {
        client.pump();
        events.extend(client.take_clone_events());
        events
            .iter()
            .any(|e| matches!(e, CloneEvent::Result(r) if Some(r.clone_id) == seq))
    });
    let first_progress = events
        .iter()
        .position(|e| matches!(e, CloneEvent::Progress(p) if Some(p.clone_id) == seq));
    let result_at = events
        .iter()
        .position(|e| matches!(e, CloneEvent::Result(_)))
        .unwrap();
    assert!(
        first_progress.is_some_and(|p| p < result_at),
        "progress comes before the result: {events:?}"
    );
    let CloneEvent::Result(result) = &events[result_at] else {
        unreachable!()
    };
    assert!(result.ok, "{result:?}");
    assert!(result.error.is_none(), "{result:?}");
    assert!(
        result
            .path
            .as_deref()
            .is_some_and(|p| p.ends_with("/flightdeck/origin")),
        "cloned into the projects root: {result:?}"
    );
    let project_id = result.project_id.clone().expect("the clone was opened");
    worker.wait("the cloned project in the mirror", || {
        client.pump();
        client
            .workspace()
            .projects
            .iter()
            .any(|p| p.project_id == project_id)
    });
    assert!(home.join("flightdeck/origin/README.md").is_file());

    // A clone of nothing fails with git's own explanation.
    let missing = format!("file://{}", outside.path().join("missing.git").display());
    let seq = client.clone_project(&missing, None);
    worker.applied(&mut client, seq, "clone of a missing repository");
    let mut result = None;
    worker.wait("the failed clone's result", || {
        client.pump();
        for event in client.take_clone_events() {
            if let CloneEvent::Result(r) = event {
                if Some(r.clone_id) == seq {
                    result = Some(r);
                }
            }
        }
        result.is_some()
    });
    let result = result.unwrap();
    assert!(!result.ok, "{result:?}");
    let error = result.error.unwrap_or_default();
    assert!(
        error.contains("missing.git") || error.contains("fatal"),
        "git's text: {error:?}"
    );
    assert!(!home.join("flightdeck/missing").exists());

    // A name that is taken is refused before git runs.
    let seq = client.clone_project(&url, Some("origin"));
    let reason = worker.rejected(&mut client, seq, "clone into a name in use");
    assert!(reason.contains("already exists"), "{reason}");
    client.stop();
}

// ---------------------------------------------------------------------------
// 4–5. Agents and geometry
// ---------------------------------------------------------------------------

/// The new-agent dialog spawns an agent the window can see and talk to, and
/// the window sizes its PTY.
#[test]
fn an_agent_spawns_from_the_window_and_the_window_sizes_its_pty() {
    let worker = Worker::start();
    let repo = worker.home().join("code").join("app");
    make_repo(worker.home(), &repo);
    let (_token, mut client) = worker.window("laptop");
    let project = worker.open_project(&mut client, &repo);

    let session = worker.spawn_agent(&mut client, &project, "e2e-agent");
    let agent_id = primary(&session).terminal_id.clone();
    let mut pty = client.pty(&agent_id);
    let mut seen = Vec::new();
    worker.wait("the agent's banner", || {
        client.pump();
        read_into(&mut pty, &mut seen);
        contains(&seen, "fake-agent: starting")
    });
    worker.wait("the agent working", || {
        client.pump();
        client
            .workspace()
            .session(&session.session_id)
            .is_some_and(|s| s.status.interpreted == InterpretedStatus::Working)
    });
    let status = worker.status();
    let agents: Vec<_> = status.projects.iter().flat_map(|p| &p.agents).collect();
    assert!(
        agents
            .iter()
            .any(|a| a.id == session.session_id.0 && a.running),
        "{status:?}"
    );

    // The window's pane size reaches the PTY and comes back as its geometry.
    let size = PtySize {
        rows: 33,
        cols: 101,
    };
    assert!(client.resize_terminal(&agent_id, size));
    worker.wait("the agent's new geometry", || {
        client.pump();
        client.workspace().terminal(&agent_id).map(|t| t.geometry)
            == Some(Geometry {
                cols: 101,
                rows: 33,
            })
    });

    // The worker shell's PTY too, as the kernel reports it.
    let shell = shell_id();
    let mut shell_pty = client.pty(&shell);
    assert!(client.resize_terminal(&shell, PtySize { rows: 27, cols: 93 }));
    worker.wait("the shell's new geometry", || {
        client.pump();
        client
            .workspace()
            .worker_shell()
            .is_some_and(|s| s.geometry.cols == 93 && s.geometry.rows == 27)
    });
    shell_round_trip(&worker, &mut client, &mut shell_pty, "stty size", "27 93");
    client.stop();
}

// ---------------------------------------------------------------------------
// 6. Takeover
// ---------------------------------------------------------------------------

/// A second window takes the worker over; the first is told who, and its
/// token still works: attaching again takes the worker back.
#[test]
fn a_second_window_takes_over_and_the_first_can_take_it_back() {
    let worker = Worker::start();
    let (laptop_token, mut laptop) = worker.window("laptop");
    let (_studio_token, mut studio) = worker.window("studio");

    worker.wait("the laptop to be taken over", || {
        laptop.pump();
        studio.pump();
        matches!(laptop.state(), LinkState::Ended(LinkEnd::TakenOver { .. }))
    });
    let LinkState::Ended(LinkEnd::TakenOver { by }) = laptop.state().clone() else {
        unreachable!()
    };
    assert_eq!(by, "studio");
    assert!(studio.state().is_live());
    assert_eq!(
        worker.status().attached.map(|a| a.label),
        Some("studio".to_string())
    );
    laptop.stop();

    // The laptop's token is still good.
    let mut again = worker.connect(&laptop_token, "laptop", ConnectionKind::Window);
    worker.wait("the laptop back and the studio taken over", || {
        again.pump();
        studio.pump();
        again.state().is_live()
            && again.workspace().ready
            && matches!(studio.state(), LinkState::Ended(LinkEnd::TakenOver { .. }))
    });
    assert_eq!(
        studio.state(),
        &LinkState::Ended(LinkEnd::TakenOver {
            by: "laptop".to_string()
        })
    );
    assert_eq!(
        worker.status().attached.map(|a| a.label),
        Some("laptop".to_string())
    );
    again.stop();
    studio.stop();
}

// ---------------------------------------------------------------------------
// 7. Background links
// ---------------------------------------------------------------------------

/// A background link hears an agent's status edge and its notification while
/// the window types into it, and is sent not one terminal byte.
#[test]
fn a_background_link_gets_status_and_notifications_but_no_bytes() {
    let worker = Worker::start();
    let repo = worker.home().join("code").join("app");
    make_repo(worker.home(), &repo);
    let (token, mut window) = worker.window("laptop");
    let opened_at = Instant::now();
    let project = worker.open_project(&mut window, &repo);
    let session = worker.spawn_agent(&mut window, &project, "e2e-notify");
    let agent_id = primary(&session).terminal_id.clone();

    let mut background = worker.connect(&token, "laptop", ConnectionKind::Background);
    worker.wait("the background link", || {
        window.pump();
        background.pump();
        background.state().is_live() && background.workspace().ready
    });
    assert_eq!(
        background
            .workspace()
            .worker
            .as_ref()
            .map(|w| w.connections.window.is_some()),
        Some(true),
        "it knows a window is attached"
    );
    let mut background_pty = background.pty(&agent_id);
    let mut window_pty = window.pty(&agent_id);

    // The agent is working; the background link sees that.
    worker.wait("the background link seeing the agent work", || {
        window.pump();
        background.pump();
        background
            .workspace()
            .session(&session.session_id)
            .is_some_and(|s| s.status.interpreted == InterpretedStatus::Working)
    });
    background.take_notifications();

    // Past the startup grace, the window types a turn; the agent finishes it.
    if let Some(left) = NOTIFY_GRACE.checked_sub(opened_at.elapsed()) {
        std::thread::sleep(left);
    }
    window_pty.write_input(b"hello from the window\r").unwrap();
    let mut window_seen = Vec::new();
    let mut leaked = Vec::new();
    let mut notifications = Vec::new();
    worker.wait("the edge and the notification", || {
        window.pump();
        background.pump();
        read_into(&mut window_pty, &mut window_seen);
        read_into(&mut background_pty, &mut leaked);
        notifications.extend(background.take_notifications());
        let idle = background
            .workspace()
            .session(&session.session_id)
            .is_some_and(|s| s.status.interpreted == InterpretedStatus::Idle);
        idle && !notifications.is_empty() && contains(&window_seen, "hello from the window")
    });
    assert!(
        notifications.iter().any(|n| n.body.contains("Fake")),
        "{notifications:?}"
    );

    // Give stray bytes a chance to arrive, then check none did.
    for _ in 0..25 {
        background.pump();
        window.pump();
        read_into(&mut background_pty, &mut leaked);
        std::thread::sleep(STEP);
    }
    assert!(
        leaked.is_empty(),
        "a background link is sent no terminal bytes: {:?}",
        String::from_utf8_lossy(&leaked)
    );
    assert_eq!(worker.status().background_links, 1);
    window.stop();
    background.stop();
}

// ---------------------------------------------------------------------------
// 8. The worker shell
// ---------------------------------------------------------------------------

/// The worker shell answers, runs in `$HOME`, and comes back after `exit`.
#[test]
fn the_worker_shell_round_trips_and_restarts_after_exit() {
    let worker = Worker::start();
    let (_token, mut client) = worker.window("laptop");
    let shell = shell_id();
    let mut pty = client.pty(&shell);

    shell_round_trip(
        &worker,
        &mut client,
        &mut pty,
        "echo fd-e2e-$((6*7))",
        "fd-e2e-42",
    );
    let home = std::fs::canonicalize(worker.home()).unwrap();
    shell_round_trip(
        &worker,
        &mut client,
        &mut pty,
        "pwd -P",
        &home.display().to_string(),
    );

    pty.write_input(b"exit\r").unwrap();
    worker.wait("the shell to exit", || {
        client.pump();
        client.workspace().worker_shell().is_some_and(|s| !s.alive)
    });

    let seq = client.restart_worker_shell();
    worker.applied(&mut client, seq, "restart_worker_shell");
    worker.wait("the shell to run again", || {
        client.pump();
        client.workspace().worker_shell().is_some_and(|s| s.alive)
    });
    let mut pty = client.pty(&shell);
    shell_round_trip(
        &worker,
        &mut client,
        &mut pty,
        "echo fd-again-$((7*6))",
        "fd-again-42",
    );
    client.stop();
}

// ---------------------------------------------------------------------------
// 9. Revoke
// ---------------------------------------------------------------------------

/// `worker revoke` ends a device's window and its background link, and the
/// token stops working.
#[test]
fn revoking_a_device_ends_its_window_and_background_link() {
    let worker = Worker::start();
    let (token, mut window) = worker.window("laptop");
    let mut background = worker.connect(&token, "laptop", ConnectionKind::Background);
    worker.wait("the background link", || {
        window.pump();
        background.pump();
        background.state().is_live() && background.workspace().ready
    });

    let devices: Vec<DeviceReport> = worker.cli_json(&["worker", "devices", "--json"]);
    assert_eq!(devices.len(), 1, "{devices:?}");
    let revoked = devices[0].id.clone();
    let out = worker.cli(&["worker", "revoke", &revoked]);
    assert!(
        out.status.success(),
        "revoke failed: {}{}",
        String::from_utf8_lossy(&out.stderr),
        worker.log_tail()
    );

    worker.wait("both links revoked", || {
        window.pump();
        background.pump();
        window.state() == &LinkState::Ended(LinkEnd::Revoked)
            && background.state() == &LinkState::Ended(LinkEnd::Revoked)
    });

    // And the token no longer attaches.
    let mut retry = worker.connect(&token, "laptop", ConnectionKind::Window);
    worker.wait("the revoked token refused", || {
        retry.pump();
        matches!(
            retry.state(),
            LinkState::Ended(LinkEnd::Revoked | LinkEnd::Unauthorized)
        )
    });
    let devices: Vec<DeviceReport> = worker.cli_json(&["worker", "devices", "--json"]);
    assert!(
        devices.iter().all(|d| d.id != revoked || d.revoked),
        "listed as revoked, or not listed: {devices:?}"
    );
    window.stop();
    background.stop();
    retry.stop();
}

// ---------------------------------------------------------------------------
// 10. Resume after a restart
// ---------------------------------------------------------------------------

/// A worker stopped with SIGTERM and started again reopens its projects and
/// resumes their agents; a paired Desktop's token still works.
#[test]
fn a_restarted_worker_resumes_its_projects_and_agents() {
    let mut worker = Worker::start();
    let repo = worker.home().join("code").join("app");
    make_repo(worker.home(), &repo);
    let (token, mut client) = worker.window("laptop");
    let project = worker.open_project(&mut client, &repo);
    let session = worker.spawn_agent(&mut client, &project, "e2e-resume");
    client.stop();

    let exit = worker.stop();
    assert!(exit.success(), "a clean stop: {exit}{}", worker.log_tail());
    assert!(
        !worker_running(&RealFs, &worker.paths()),
        "the run lock is free"
    );

    worker.run();
    let mut client = worker.connect(&token, "laptop", ConnectionKind::Window);
    worker.wait_live(&mut client, "the window after the restart");
    worker.wait("the project and its agent, running again", || {
        client.pump();
        sessions_of(&client, &project)
            .iter()
            .any(|s| s.session_id == session.session_id && primary_alive(s))
    });
    let status = worker.status();
    assert!(
        status.projects.iter().any(|p| p
            .agents
            .iter()
            .any(|a| a.id == session.session_id.0 && a.running)),
        "{status:?}"
    );
    client.stop();
}

// ---------------------------------------------------------------------------
// 11. The supervisor
// ---------------------------------------------------------------------------

/// `worker start` runs a supervisor that restarts a killed worker, and
/// `worker stop` stops both and frees both locks.
#[test]
fn the_supervisor_restarts_a_killed_worker_and_stop_frees_both_locks() {
    let mut worker = Worker::prepare();
    worker.supervised = true;
    let out = worker.cli(&["worker", "start"]);
    assert!(
        out.status.success(),
        "start failed: {}{}",
        String::from_utf8_lossy(&out.stderr),
        worker.log_tail()
    );
    let status = worker.status();
    assert_eq!(status.state, WorkerState::Running, "{status:?}");
    assert_eq!(status.restarts, 0);
    let first = status.pid.expect("the worker's pid");
    assert!(status.supervisor_pid.is_some(), "{status:?}");

    signal(first, libc::SIGKILL);
    let mut last = None;
    worker.wait("the supervisor to restart the worker", || {
        let status = worker.status();
        let done = status.state == WorkerState::Running
            && status.restarts == 1
            && status.pid.is_some_and(|pid| pid != first);
        last = Some(status);
        done
    });
    let status = last.unwrap();
    assert!(status.last_exit.is_some(), "{status:?}");

    let out = worker.cli(&["worker", "stop"]);
    assert!(
        out.status.success(),
        "stop failed: {}{}",
        String::from_utf8_lossy(&out.stderr),
        worker.log_tail()
    );
    worker.supervised = false;
    assert!(!worker_running(&RealFs, &worker.paths()), "both locks free");
    let paths = worker.paths();
    for lock in [&paths.run_lock, &paths.supervisor_lock] {
        let held = flightdeck::contracts::FileSystem::try_lock_exclusive(&RealFs, lock)
            .expect("the lock file can be probed");
        assert!(held.is_some(), "{} is free", lock.display());
    }
    assert_eq!(worker.status().state, WorkerState::Stopped);
}

// ---------------------------------------------------------------------------
// 12. The local-host lock
// ---------------------------------------------------------------------------

/// While a worker serves this account, the TUI refuses to open — before it
/// touches the terminal — and opens again once the worker stopped.
#[test]
fn the_tui_refuses_to_open_while_a_worker_runs() {
    let mut worker = Worker::start();
    let repo = worker.home().join("code").join("app");
    make_repo(worker.home(), &repo);

    assert!(worker_running(&RealFs, &worker.paths()));
    let out = worker
        .command()
        .current_dir(&repo)
        .output()
        .expect("the TUI binary runs");
    assert!(!out.status.success(), "the TUI refused: {}", out.status);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains(LOCAL_HOST_REFUSAL), "{stderr}");
    assert!(
        !repo.join(".flightdeck").exists(),
        "refused before the project was opened"
    );

    let exit = worker.stop();
    assert!(exit.success(), "{exit}");
    assert!(!worker_running(&RealFs, &worker.paths()));
}

// ---------------------------------------------------------------------------
// A worker whose log reader is gone
// ---------------------------------------------------------------------------

/// A worker writes its log to a pipe its supervisor reads. When that reader
/// is gone (the supervisor was killed), every write fails with `EPIPE`, and
/// the worker must shrug that off rather than panic: opening a project — whose
/// `.gitignore` notice is one such write — and spawning an agent still work.
#[test]
fn a_worker_keeps_working_when_its_log_reader_is_gone() {
    let mut worker = Worker::prepare();
    let repo = worker.home().join("code").join("app");
    make_repo(worker.home(), &repo);
    worker.run_with_a_closed_log();
    let (_token, mut client) = worker.window("laptop");

    let project = worker.open_project(&mut client, &repo);
    worker.spawn_agent(&mut client, &project, "e2e-epipe");
    let alive = worker.child.as_mut().unwrap().try_wait().unwrap();
    assert!(alive.is_none(), "the worker is still running: {alive:?}");
    assert_eq!(worker.status().state, WorkerState::Running);
    client.stop();
}

// ---------------------------------------------------------------------------
// Probes of the locks
// ---------------------------------------------------------------------------

/// `worker status` and a TUI launch find out whether a worker runs by taking
/// its lock for an instant. A worker starting in that instant must wait the
/// probe out, not exit as "already running" — here, while a probe runs in a
/// tight loop, a foreground worker starts and stops three times over.
#[test]
fn a_worker_starts_while_its_lock_is_being_probed() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let mut worker = Worker::prepare();
    let paths = worker.paths();
    let stop = Arc::new(AtomicBool::new(false));
    let probe = {
        let stop = Arc::clone(&stop);
        let paths = paths.clone();
        std::thread::spawn(move || {
            let mut probes = 0u64;
            while !stop.load(Ordering::Relaxed) {
                let _ = worker_running(&RealFs, &paths);
                probes += 1;
            }
            probes
        })
    };
    for _ in 0..3 {
        worker.run();
        let exit = worker.stop();
        assert!(exit.success(), "{exit}{}", worker.log_tail());
    }
    stop.store(true, Ordering::Relaxed);
    assert!(probe.join().unwrap() > 0, "the probe ran");
}
