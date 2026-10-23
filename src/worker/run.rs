//! `flightdeck worker run`: the worker itself, in the foreground (SPECS §33).
//!
//! The headless driver loop around an [`crate::host::AppHost`] in worker mode:
//! take `run.lock`, load the global config, open the worker workspace, serve
//! the web protocol and the control socket, tick until asked to stop, then
//! tear down as a TUI quit does.
//!
//! Everything it says goes through [`log_line`] to stderr (the supervisor
//! appends it to `worker.log`; a foreground run prints it), never through
//! `eprintln!`, which panics once the supervisor reading the pipe is gone.
//!
//! The control socket is served on its own thread, but the [`AppHost`] is not
//! `Send`: each request crosses to this loop as a [`ControlCall`] and is
//! answered between ticks, so a request never holds up a tick for longer than
//! the worker API call it makes.

#[cfg(unix)]
use std::path::Path;

#[cfg(unix)]
use crate::config::{init::ensure_global_config, load::load_config};
#[cfg(unix)]
use crate::contracts::{Config, FileSystem, WorkerConfig};
#[cfg(unix)]
use crate::host::AppHost;
#[cfg(unix)]
use crate::web::server::ListenPlan;
#[cfg(unix)]
use crate::worker::control::{ControlRequest, ControlResponse, DeviceReport, PairingCode};
use crate::worker::log::log_line;
use crate::worker::paths::WorkerPaths;
#[cfg(unix)]
use crate::worker::status::WorkerRuntimeReport;

/// Exit code of `worker run` when another worker already holds `run.lock`.
/// The supervisor gives up at once on it rather than restarting.
pub const EXIT_ALREADY_RUNNING: i32 = 73;
/// Exit code of `worker run` when the global config is invalid. The
/// supervisor gives up at once on it rather than restarting.
pub const EXIT_CONFIG_INVALID: i32 = 78;

#[cfg(unix)]
/// How often the worker re-reads its interface addresses for Tailscale
/// changes (PRD §4.3).
const LISTEN_POLL: std::time::Duration = std::time::Duration::from_secs(5);
#[cfg(unix)]
/// The loop's pause after a turn that changed something: keep up with a
/// burst of output or a run of control requests.
const BUSY_PAUSE: std::time::Duration = std::time::Duration::from_millis(5);
#[cfg(unix)]
/// The loop's pause after an idle turn. Well under the 50 ms a tick may be
/// apart, so PTY output, web clients and status refreshes keep flowing.
const IDLE_PAUSE: std::time::Duration = std::time::Duration::from_millis(25);

/// Debug builds only: a comma-separated list of `ip:port` to listen on
/// instead of the Tailscale addresses (the integration tests' seam). Absent
/// from release builds, so a shipped worker can never be told to listen on
/// loopback or the LAN (W8).
#[cfg(debug_assertions)]
pub const TEST_LISTEN_ENV: &str = "FLIGHTDECK_WORKER_TEST_LISTEN";

/// Run the worker until it is stopped; returns the process exit code.
pub fn run(paths: &WorkerPaths) -> i32 {
    #[cfg(unix)]
    {
        run_unix(paths)
    }
    #[cfg(not(unix))]
    {
        let _ = paths;
        log_line("error", crate::worker::cli::NOT_SUPPORTED);
        2
    }
}

#[cfg(unix)]
fn run_unix(paths: &WorkerPaths) -> i32 {
    use std::sync::atomic::Ordering;

    use crate::contracts::{RealClock, RealFs, SystemCommandRunner};
    use crate::host::worker::{
        clone::RealGitCloner, health::RealHealthProbe, WorkerNotifier, WorkerOptions,
    };
    use crate::worker::control;

    install_panic_logger();

    if let Err(e) = paths.ensure_dirs() {
        log_line(
            "error",
            &format!("could not create {}: {e}", paths.dir.display()),
        );
        return 1;
    }
    let fs = RealFs;
    // Held for the whole run: dropped (and released) when this returns.
    let _run_lock = match crate::worker::lock::acquire(&fs, &paths.run_lock) {
        Ok(Some(lock)) => lock,
        Ok(None) => {
            log_line(
                "error",
                &format!(
                    "another worker is already running for this account ({} is held)",
                    paths.run_lock.display()
                ),
            );
            return EXIT_ALREADY_RUNNING;
        }
        Err(e) => {
            log_line(
                "error",
                &format!("could not take {}: {e}", paths.run_lock.display()),
            );
            return 1;
        }
    };

    let config = match load_global_config(&fs, &paths.home) {
        Ok(config) => config,
        Err(ConfigProblem::Invalid(message)) => {
            // Logged before exiting: the supervisor's give-up reason sends
            // the user to `worker logs` for this line.
            log_line("error", &message);
            return EXIT_CONFIG_INVALID;
        }
        Err(ConfigProblem::Io(message)) => {
            log_line("error", &message);
            return 1;
        }
    };
    let listen = match listen_plan(&config.worker) {
        Ok(listen) => listen,
        Err(message) => {
            log_line("error", &message);
            return EXIT_CONFIG_INVALID;
        }
    };
    // Fail before resuming any agent when the socket could never be bound.
    if let Err(e) = control::check_socket_path(&paths.control_socket) {
        log_line("error", &e.to_string());
        return 1;
    }

    let pid = std::process::id();
    if let Err(e) = std::fs::write(&paths.worker_pid, format!("{pid}\n")) {
        log_line(
            "warn",
            &format!("could not write {}: {e}", paths.worker_pid.display()),
        );
    }
    let shutdown = crate::signals::install_shutdown_flag();

    let pty = crate::terminal::pty::PortablePtyBackend;
    let clock = RealClock;
    let container = crate::runtime::PodmanCli;
    let command = SystemCommandRunner;
    let env = crate::Env {
        fs: &fs,
        pty: &pty,
        clock: &clock,
        container: &container,
        command: &command,
        terminal: crate::terminal::session::TerminalProfile::TUI,
    };
    let notifier = WorkerNotifier::new();
    log_line(
        "info",
        &format!(
            "starting FlightDeck {} worker (pid {pid}, listen plan {listen:?})",
            crate::VERSION
        ),
    );
    // integrate: add `listen,` to this literal once C adds
    // `WorkerOptions.listen: ListenPlan`, then drop the `drop(listen)` below.
    let opts = WorkerOptions {
        paths: paths.clone(),
        home: paths.home.clone(),
        config: config.worker.clone(),
        global_config: config,
        health: Box::new(RealHealthProbe),
        cloner: std::sync::Arc::new(RealGitCloner),
    };
    // integrate: remove once `listen` moves into `opts` above.
    drop(listen);
    let mut host = match AppHost::open_worker(env, &notifier, opts) {
        Ok(host) => host,
        Err(e) => {
            log_line(
                "error",
                &format!("could not open the worker workspace: {e}"),
            );
            remove_quietly(&paths.worker_pid);
            return 1;
        }
    };
    host.resume_all_projects();
    host.start_worker();

    let (server, calls) = match control::serve(&paths.control_socket) {
        Ok(served) => served,
        Err(e) => {
            log_line(
                "error",
                &format!(
                    "could not serve the control socket {}: {e}",
                    paths.control_socket.display()
                ),
            );
            let _ = teardown(&mut host, paths);
            return 1;
        }
    };
    if let Some(requested) = take_restart_marker(&paths.restart_marker) {
        log_line("info", &restart_message(&requested));
    }
    log_line("info", "the worker is running");

    while !shutdown.load(Ordering::Relaxed) {
        let mut changed = host.tick();
        while let Ok(call) = calls.try_recv() {
            let (response, line) = answer(&mut host, &call.request);
            call.reply(response);
            if let Some(line) = line {
                log_line("info", &line);
            }
            changed = true;
        }
        for line in host.take_log_lines() {
            log_line("info", &line);
        }
        std::thread::sleep(if changed { BUSY_PAUSE } else { IDLE_PAUSE });
    }

    log_line("info", "stopping the worker");
    // Stop answering first: a request arriving during teardown gets "not
    // running" at once rather than waiting out the reply timeout. Requests
    // already queued are answered "shutting down".
    drop(server);
    drop(calls);
    teardown(&mut host, paths)
}

/// Close every connection, persist, terminate every agent (as a TUI quit
/// does), and remove the socket and pid files. The exit code: 0, or 1 when
/// the state could not be saved.
#[cfg(unix)]
fn teardown(host: &mut AppHost<'_>, paths: &WorkerPaths) -> i32 {
    host.stop_worker();
    // Persist before terminating, as the TUI does: a session that is still
    // alive is what the next start resumes.
    let persisted = host.persist();
    host.terminate_sessions();
    for line in host.take_log_lines() {
        log_line("info", &line);
    }
    remove_quietly(&paths.control_socket);
    remove_quietly(&paths.worker_pid);
    match persisted {
        Ok(()) => {
            log_line("info", "the worker stopped");
            0
        }
        Err(e) => {
            log_line(
                "error",
                &format!("the worker stopped, but saving state failed: {e}"),
            );
            1
        }
    }
}

#[cfg(unix)]
fn remove_quietly(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// Log a panic as one worker log line, with the backtrace when
/// `RUST_BACKTRACE` asks for one. Panics the VT parser recovers from are not
/// logged (the TUI's hook skips them the same way).
#[cfg(unix)]
fn install_panic_logger() {
    std::panic::set_hook(Box::new(|info| {
        if crate::terminal::session::parser_panic_expected() {
            return;
        }
        let backtrace = std::backtrace::Backtrace::capture();
        let trace = match backtrace.status() {
            std::backtrace::BacktraceStatus::Captured => format!("\n{backtrace}"),
            _ => String::new(),
        };
        log_line("error", &format!("panicked: {info}{trace}"));
    }));
}

#[cfg(unix)]
/// Why the global config could not be used.
#[derive(Debug, PartialEq, Eq)]
enum ConfigProblem {
    /// It is invalid: exit [`EXIT_CONFIG_INVALID`], which the supervisor
    /// does not retry.
    Invalid(String),
    /// It could not be written or read: an ordinary failure.
    Io(String),
}

#[cfg(unix)]
/// The global `~/.flightdeck/config.toml` under `home`, written with the
/// documented defaults first if missing. A worker reads only the global file
/// (SPECS §33): it serves every project, so no project's `[worker]` applies.
fn load_global_config(fs: &dyn FileSystem, home: &Path) -> Result<Config, ConfigProblem> {
    let path = home.join(".flightdeck").join("config.toml");
    ensure_global_config(fs, &path)
        .map_err(|e| ConfigProblem::Io(format!("could not write {}: {e}", path.display())))?;
    load_config(fs, &path).map_err(|e| match e {
        crate::contracts::FlightDeckError::Io(_) => {
            ConfigProblem::Io(format!("could not read {}: {e}", path.display()))
        }
        _ => ConfigProblem::Invalid(format!("invalid global config {}: {e}", path.display())),
    })
}

#[cfg(unix)]
/// Where to listen: this machine's Tailscale addresses, on `[worker] port`.
///
/// Debug builds also honour [`TEST_LISTEN_ENV`] (fixed addresses, loopback
/// allowed) for the integration tests; a value that does not parse is an
/// error rather than a silent fallback to Tailscale.
fn listen_plan(worker: &WorkerConfig) -> Result<ListenPlan, String> {
    #[cfg(debug_assertions)]
    {
        if let Ok(value) = std::env::var(TEST_LISTEN_ENV) {
            return parse_test_listen(&value)
                .map(ListenPlan::Fixed)
                .map_err(|e| format!("{TEST_LISTEN_ENV}: {e}"));
        }
    }
    Ok(tailscale_plan(worker.port))
}

#[cfg(unix)]
/// The release listen plan.
fn tailscale_plan(port: u16) -> ListenPlan {
    use crate::remote::direct::net::{CliTailscaleProbe, RealAddressSource};
    ListenPlan::Tailscale {
        port,
        source: std::sync::Arc::new(RealAddressSource),
        probe: std::sync::Arc::new(CliTailscaleProbe),
        poll: LISTEN_POLL,
    }
}

/// `127.0.0.1:0, [::1]:7420` → the addresses. Empty is an error: a worker
/// told to listen nowhere is a broken test.
#[cfg(all(unix, debug_assertions))]
fn parse_test_listen(value: &str) -> Result<Vec<std::net::SocketAddr>, String> {
    let addrs = value
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(|part| {
            part.parse::<std::net::SocketAddr>()
                .map_err(|e| format!("'{part}' is not an ip:port ({e})"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if addrs.is_empty() {
        return Err("no address given".to_string());
    }
    Ok(addrs)
}

#[cfg(unix)]
/// Read and delete the marker `worker restart` leaves for the next worker:
/// its contents (the unix second the restart was asked for), or `None` when
/// there is no marker. Deleted even when unreadable, so it is read once.
fn take_restart_marker(path: &Path) -> Option<String> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(_) => String::new(),
    };
    let _ = std::fs::remove_file(path);
    Some(contents.trim().to_string())
}

#[cfg(unix)]
/// The log line for a start that `worker restart` asked for.
fn restart_message(requested: &str) -> String {
    match requested.parse::<u64>() {
        Ok(at) => format!("started by `flightdeck worker restart` (requested at unix {at})"),
        Err(_) => "started by `flightdeck worker restart`".to_string(),
    }
}

#[cfg(unix)]
/// The worker API the control socket reaches. [`AppHost`] in worker mode is
/// the real one; the tests drive [`answer`] with a fake.
trait ControlTarget {
    fn report(&self) -> WorkerRuntimeReport;
    fn pair(&mut self) -> PairingCode;
    fn paired_devices(&self) -> Vec<DeviceReport>;
    fn revoke(&mut self, selector: &str) -> Result<(DeviceReport, u32), String>;
    fn prepare_shutdown(&mut self, restarting: bool);
}

#[cfg(unix)]
impl ControlTarget for AppHost<'_> {
    fn report(&self) -> WorkerRuntimeReport {
        self.worker_report()
    }
    fn pair(&mut self) -> PairingCode {
        self.mint_pairing_code()
    }
    fn paired_devices(&self) -> Vec<DeviceReport> {
        self.devices()
    }
    fn revoke(&mut self, selector: &str) -> Result<(DeviceReport, u32), String> {
        self.revoke_device(selector)
    }
    fn prepare_shutdown(&mut self, restarting: bool) {
        self.set_shutdown_intent(restarting)
    }
}

#[cfg(unix)]
/// Answer one control request, and the line to log about it, if any. A
/// pairing code itself is never logged.
fn answer(
    target: &mut dyn ControlTarget,
    request: &ControlRequest,
) -> (ControlResponse, Option<String>) {
    match request {
        ControlRequest::Ping => (
            ControlResponse::Pong {
                pid: std::process::id(),
                version: crate::VERSION.to_string(),
            },
            None,
        ),
        ControlRequest::Status => (ControlResponse::Status(Box::new(target.report())), None),
        ControlRequest::Pair => {
            let code = target.pair();
            let line = format!(
                "minted a pairing code (valid {} s, single use)",
                code.expires_in_secs
            );
            (ControlResponse::Paired(code), Some(line))
        }
        ControlRequest::Devices => (
            ControlResponse::Devices {
                devices: target.paired_devices(),
            },
            None,
        ),
        ControlRequest::Revoke { device } => match target.revoke(device) {
            Ok((device, closed_connections)) => {
                let line = format!(
                    "revoked {} [{}]: closed {closed_connections} connection(s)",
                    device.label, device.id
                );
                (
                    ControlResponse::Revoked {
                        device,
                        closed_connections,
                    },
                    Some(line),
                )
            }
            Err(message) => (ControlResponse::Error { message }, None),
        },
        ControlRequest::PrepareShutdown { restarting } => {
            target.prepare_shutdown(*restarting);
            let line = if *restarting {
                "the next stop is a restart"
            } else {
                "the next stop is a stop"
            };
            (ControlResponse::Ok, Some(line.to_string()))
        }
    }
}

#[cfg(all(test, unix))]
mod tests;
