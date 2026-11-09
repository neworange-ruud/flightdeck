//! Start on boot: `flightdeck worker enable` / `disable` (SPECS §33).
//!
//! `enable` registers the worker with the OS's per-user service manager so it
//! comes back after a reboot without anyone running `flightdeck worker
//! start`:
//!
//! * **macOS:** a launchd agent, `~/Library/LaunchAgents/<LAUNCHD_LABEL>.plist`,
//!   loaded with `launchctl bootstrap gui/<uid>`.
//! * **Linux:** a systemd user unit, `~/.config/systemd/user/<SYSTEMD_UNIT>`,
//!   enabled with `systemctl --user enable`.
//!
//! # Process model
//!
//! The service manager runs **the supervisor, in the foreground**:
//! `flightdeck worker supervise`, exactly what `worker start` spawns in the
//! background. The manager only ever *starts* it; it never restarts it
//! (launchd `KeepAlive` false, systemd `Restart=no`). The supervisor keeps its
//! whole job: crash restarts with backoff, giving up after 5 crashes in 10
//! minutes, `supervisor.json`, the log. A manager that also restarted on exit
//! would fight that: it would undo a give-up and turn a `worker stop` (the
//! supervisor exits 0) into a respawn. So:
//!
//! * `worker stop` signals the supervisor as before. It exits, the service is
//!   inactive, and it **stays stopped** until the next `worker start`, boot or
//!   login. That is the intended meaning of "stop".
//! * `worker start` (and so `restart`) goes **through the manager** when the
//!   service is installed (see [`start`]), so the worker always runs with the
//!   service's environment and, on macOS, in the GUI login session. Where the
//!   manager cannot start it (no GUI session, lingering off, no user bus) it
//!   falls back to the detached spawn it has always used.
//! * Two workers are impossible either way: the supervisor and `worker run`
//!   each take their lock (`lock.rs`), so a second supervisor the manager
//!   started exits at once. `enable` and `start` also never ask the manager to
//!   start one while a worker runs.
//!
//! Neither manager needs forking settings (`Type=forking`, `AbandonProcess
//! Group`): the supervisor is the job's main process for its whole life and
//! always outlives its worker. The worker runs in a session of its own
//! (`daemon::spawn_supervised`), so launchd's process-group kill on the
//! supervisor's exit never reaches it early. On Linux `KillMode=mixed` sends
//! `systemctl stop`'s `SIGTERM` to the supervisor alone, which stops the
//! worker gracefully (its 30 s grace), and only what is left after that is
//! killed; `TimeoutStopSec` and launchd's `ExitTimeOut` leave room for that
//! grace.
//!
//! # The macOS domain
//!
//! The agent goes in the **`gui/<uid>` domain**, not `user/<uid>`. Agents in
//! `~/Library/LaunchAgents` are loaded into `gui/<uid>` when the account logs
//! in at the screen, and a job there runs in that login session, where the
//! login Keychain is unlocked: Claude Code, which keeps its login in the
//! Keychain, works as it does in Terminal. `user/<uid>` is the background
//! domain an SSH session also gets; nothing loads it at boot, and its jobs do
//! not get the GUI session's Keychain. The trade-off: "start on boot" on macOS
//! is **start at login**. An unattended Mac needs automatic login (System
//! Settings > Users & Groups; FileVault prevents it) for the worker to come
//! back after a reboot. Starting at boot without a login would take a
//! LaunchDaemon, which needs root to install and has no unlocked Keychain.
//! Over SSH with nobody logged in at the screen there is no `gui/<uid>`
//! domain: `enable` then writes the plist (it loads at the next login) and
//! `start` falls back to the detached spawn.
//!
//! # Linux lingering
//!
//! Without lingering, systemd starts a user's units at their first login and
//! stops them at their last logout. `enable` checks it (`loginctl show-user`)
//! and prints the `loginctl enable-linger` command when it is off; it never
//! runs sudo itself. While lingering is off `start` keeps the detached spawn,
//! so logging out of SSH does not stop a worker systemd started.
//!
//! # Environment
//!
//! A service does not get the login shell's environment: launchd's `PATH` is
//! `/usr/bin:/bin:/usr/sbin:/sbin`, and the agents, `git`, `node` and Homebrew
//! would not be found. `enable` asks the user's login shell for its `PATH` (as
//! FlightDeck Desktop does at launch) and writes it into the unit, with
//! `SHELL` and `LANG`. Running `enable` again refreshes them. Nothing else
//! from the shell profile is copied: secrets exported there stay out of a
//! world-readable unit file.
//!
//! # Binary path
//!
//! The unit runs the binary `enable` was run with, by the path that survives
//! an update: Homebrew's `bin/flightdeck` link rather than a versioned
//! `Cellar` path `brew upgrade` deletes ([`stable_exe`]). An installer update
//! (`flightdeck update`) replaces the binary in place, so the path stays
//! valid; `worker restart` then runs the new one.
//!
//! Every side effect (files, `launchctl`, `systemctl`, `loginctl`) goes
//! through [`ServiceOs`], so the logic is tested with a fake; the unit text is
//! a pure function.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::worker::paths::WorkerPaths;

/// The launchd label, and the plist's file name without `.plist`.
pub const LAUNCHD_LABEL: &str = "agency.neworange.flightdeck.worker";

/// The systemd user unit's name.
pub const SYSTEMD_UNIT: &str = "flightdeck-worker.service";

/// How long the manager waits for the supervisor to exit when it stops the
/// job: the supervisor's own 30 s grace for the worker, plus its teardown.
pub const STOP_TIMEOUT_SECS: u64 = 45;

/// Directories always on the unit's `PATH`, after the login shell's.
const BASE_PATH: &[&str] = &["/usr/local/bin", "/usr/bin", "/bin", "/usr/sbin", "/sbin"];

/// How many times `start` tries `launchctl bootstrap` right after a
/// `bootout`, which can still be finishing.
const BOOTSTRAP_ATTEMPTS: u32 = 3;

/// How long the login shell may take to print its `PATH`.
const LOGIN_SHELL_TIMEOUT: Duration = Duration::from_secs(5);

/// The service manager this OS has.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Manager {
    /// macOS: a per-user launchd agent.
    Launchd,
    /// Linux: a systemd user unit.
    Systemd,
}

impl Manager {
    /// launchd on macOS, systemd on Linux, none elsewhere.
    pub fn for_this_os() -> Option<Manager> {
        use crate::tui::platform::{IS_LINUX, IS_MACOS};
        if IS_MACOS {
            Some(Manager::Launchd)
        } else if IS_LINUX {
            Some(Manager::Systemd)
        } else {
            None
        }
    }

    /// Where the unit file lives under `home`.
    pub fn unit_path(self, home: &Path) -> PathBuf {
        match self {
            Manager::Launchd => home
                .join("Library")
                .join("LaunchAgents")
                .join(format!("{LAUNCHD_LABEL}.plist")),
            Manager::Systemd => home
                .join(".config")
                .join("systemd")
                .join("user")
                .join(SYSTEMD_UNIT),
        }
    }

    /// "launchd agent" / "systemd user unit".
    pub fn describe(self) -> &'static str {
        match self {
            Manager::Launchd => "launchd agent",
            Manager::Systemd => "systemd user unit",
        }
    }

    /// What starting with the OS means here: launchd agents start at login.
    fn when(self) -> &'static str {
        match self {
            Manager::Launchd => "Start at login",
            Manager::Systemd => "Start on boot",
        }
    }
}

/// Who and where: everything about this account the unit depends on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Context {
    pub manager: Manager,
    /// The account's home: the unit's directory, and the worker's.
    pub home: PathBuf,
    /// `~/.flightdeck/logs/worker.log`, where launchd sends the supervisor's
    /// stderr (as `worker start` does).
    pub log_file: PathBuf,
    /// For `gui/<uid>`.
    pub uid: u32,
    /// For `loginctl show-user`; `None` when `$USER` and `$LOGNAME` are unset.
    pub user: Option<String>,
    /// The binary the unit runs ([`stable_exe`]).
    pub exe: PathBuf,
}

impl Context {
    /// This account's context, or `None` on an OS with no supported manager.
    /// Everything hangs off `paths.home`, never `$HOME` directly, so a test
    /// with a throwaway home never touches the real unit.
    pub fn current(paths: &WorkerPaths) -> Option<Context> {
        let manager = Manager::for_this_os()?;
        let exe = std::env::current_exe()
            .map(|exe| stable_exe(&exe, &|p| p.exists()))
            .unwrap_or_else(|_| PathBuf::from("flightdeck"));
        let user = ["USER", "LOGNAME"]
            .iter()
            .filter_map(|k| std::env::var(k).ok())
            .find(|v| !v.is_empty());
        Some(Context {
            manager,
            home: paths.home.clone(),
            log_file: paths.log_file.clone(),
            uid: current_uid(),
            user,
            exe,
        })
    }

    /// The unit file's path.
    pub fn unit_path(&self) -> PathBuf {
        self.manager.unit_path(&self.home)
    }

    /// `gui/<uid>`.
    fn domain(&self) -> String {
        format!("gui/{}", self.uid)
    }

    /// `gui/<uid>/<label>`.
    fn target(&self) -> String {
        format!("gui/{}/{LAUNCHD_LABEL}", self.uid)
    }

    /// The unit for `env`.
    pub fn spec(&self, env: &Environment) -> ServiceSpec {
        let path = service_path(
            env.login_path.as_deref(),
            env.inherited_path.as_deref(),
            self.exe.parent(),
        );
        let mut vars = vec![("PATH".to_string(), path)];
        for (key, value) in [("SHELL", &env.shell), ("LANG", &env.lang)] {
            if let Some(value) = value.as_ref().filter(|v| !v.is_empty()) {
                vars.push((key.to_string(), value.clone()));
            }
        }
        ServiceSpec {
            exe: self.exe.clone(),
            home: self.home.clone(),
            log_file: self.log_file.clone(),
            env: vars,
        }
    }
}

/// The environment `enable` was run in, as far as the unit cares.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Environment {
    /// `PATH` as the login shell sets it ([`probe_login_path`]).
    pub login_path: Option<String>,
    /// This process's own `PATH`, used when the login shell did not answer.
    pub inherited_path: Option<String>,
    pub shell: Option<String>,
    pub lang: Option<String>,
}

impl Environment {
    /// Read from this process, asking `$SHELL` for its login `PATH`.
    pub fn current() -> Environment {
        let shell = std::env::var("SHELL").ok().filter(|s| !s.is_empty());
        let home = crate::app::state::user_home();
        let login_path = shell
            .as_deref()
            .and_then(|s| probe_login_path(Path::new(s), home.as_deref(), LOGIN_SHELL_TIMEOUT));
        Environment {
            login_path,
            inherited_path: std::env::var("PATH").ok(),
            shell,
            lang: std::env::var("LANG").ok(),
        }
    }
}

/// What the unit runs, and with which environment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceSpec {
    pub exe: PathBuf,
    pub home: PathBuf,
    pub log_file: PathBuf,
    /// Set in the unit, in this order (`PATH` first).
    pub env: Vec<(String, String)>,
}

impl ServiceSpec {
    /// The `PATH` the unit sets.
    pub fn path(&self) -> &str {
        self.env
            .iter()
            .find(|(k, _)| k == "PATH")
            .map(|(_, v)| v.as_str())
            .unwrap_or("")
    }
}

// ---------------------------------------------------------------------------
// The unit text (pure)
// ---------------------------------------------------------------------------

/// The unit file for `manager`.
pub fn render(manager: Manager, spec: &ServiceSpec) -> String {
    match manager {
        Manager::Launchd => launchd_plist(spec),
        Manager::Systemd => systemd_unit(spec),
    }
}

/// The launchd agent's plist.
///
/// `KeepAlive` is false: launchd starts the supervisor (at load, so at login)
/// and never restarts it; the supervisor restarts the worker. `ProcessType`
/// `Interactive` keeps launchd from throttling the CPU and I/O of the agents'
/// builds, as it does for an unspecified type.
pub fn launchd_plist(spec: &ServiceSpec) -> String {
    let s = |v: &str| format!("<string>{}</string>", xml_escape(v));
    let path = |p: &Path| s(&p.to_string_lossy());
    let mut env = String::new();
    for (key, value) in &spec.env {
        env.push_str(&format!(
            "\t\t<key>{}</key>\n\t\t{}\n",
            xml_escape(key),
            s(value)
        ));
    }
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<!-- Written by `flightdeck worker enable`; `flightdeck worker disable` removes it.
     Run `flightdeck worker enable` again to refresh the binary path and PATH. -->
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{label}</string>
	<key>ProgramArguments</key>
	<array>
		{exe}
		<string>worker</string>
		<string>supervise</string>
	</array>
	<key>WorkingDirectory</key>
	{home}
	<key>EnvironmentVariables</key>
	<dict>
{env}	</dict>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<false/>
	<key>LimitLoadToSessionType</key>
	<string>Aqua</string>
	<key>ProcessType</key>
	<string>Interactive</string>
	<key>ExitTimeOut</key>
	<integer>{timeout}</integer>
	<key>StandardErrorPath</key>
	{log}
</dict>
</plist>
"#,
        label = LAUNCHD_LABEL,
        exe = path(&spec.exe),
        home = path(&spec.home),
        log = path(&spec.log_file),
        timeout = STOP_TIMEOUT_SECS,
    )
}

/// The systemd user unit.
pub fn systemd_unit(spec: &ServiceSpec) -> String {
    let mut env = String::new();
    for (key, value) in &spec.env {
        env.push_str(&format!(
            "Environment={}\n",
            systemd_quote(&format!("{key}={value}"))
        ));
    }
    format!(
        "# Written by `flightdeck worker enable`; `flightdeck worker disable` removes it.
# Run `flightdeck worker enable` again to refresh the binary path and PATH.
[Unit]
Description=FlightDeck worker

[Service]
Type=simple
ExecStart={exe} worker supervise
WorkingDirectory=~
{env}# The supervisor restarts the worker after a crash and gives up after
# repeated ones; systemd only starts it, so a `flightdeck worker stop` stays
# stopped.
Restart=no
# SIGTERM to the supervisor alone: it stops the worker gracefully.
KillMode=mixed
TimeoutStopSec={timeout}

[Install]
WantedBy=default.target
",
        exe = systemd_quote(&spec.exe.to_string_lossy()),
        timeout = STOP_TIMEOUT_SECS,
    )
}

/// `s` as XML character data: `&`, `<`, `>`, and both quotes escaped.
pub fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

/// `s` as one double-quoted systemd word: `\` and `"` backslash-escaped, a
/// newline as `\n`, `%` (a specifier) doubled. For an `Environment=`
/// assignment and for `ExecStart=`'s executable, which is all this quotes:
/// `$` is left alone, as neither expands variables (only `ExecStart=`'s
/// arguments do, and ours are fixed words). Checked with `systemd-analyze
/// verify` (systemd 252) on a path with spaces, `%`, `$`, `"` and `\`.
pub fn systemd_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '%' => out.push_str("%%"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The path the unit should run for the binary at `current`.
///
/// * Linux reports a binary replaced in place as `… (deleted)`: the suffix
///   is dropped, as the path now holds the new binary.
/// * A binary inside a Homebrew `Cellar` (`<prefix>/Cellar/<formula>/<version>
///   /bin/<name>`, as Linux reports a Homebrew install) becomes
///   `<prefix>/bin/<name>` when that exists: `brew upgrade` deletes the
///   versioned directory but keeps the link pointing at the new one. macOS
///   reports the path the binary was started by, usually that link already.
pub fn stable_exe(current: &Path, exists: &dyn Fn(&Path) -> bool) -> PathBuf {
    let text = current.to_string_lossy();
    let current = match text.strip_suffix(" (deleted)") {
        Some(live) => PathBuf::from(live),
        None => current.to_path_buf(),
    };
    let parts: Vec<_> = current.components().collect();
    let cellar = parts.iter().rposition(|c| c.as_os_str() == "Cellar");
    if let (Some(at), Some(name)) = (cellar, current.file_name()) {
        // Cellar / formula / version / bin / name
        if parts.len() == at + 5 && parts[at + 3].as_os_str() == "bin" {
            let prefix: PathBuf = parts[..at].iter().collect();
            let link = prefix.join("bin").join(name);
            if exists(&link) {
                return link;
            }
        }
    }
    current
}

/// The unit's `PATH`: the login shell's when it answered, else this
/// process's; then the binary's own directory and [`BASE_PATH`] when missing.
/// Empty and relative entries are dropped (a service must not resolve
/// commands against its working directory), and duplicates keep their first
/// place.
pub fn service_path(
    login: Option<&str>,
    inherited: Option<&str>,
    exe_dir: Option<&Path>,
) -> String {
    let first = login
        .filter(|p| !p.trim().is_empty())
        .or(inherited)
        .unwrap_or("");
    let exe_dir = exe_dir.map(|d| d.to_string_lossy().into_owned());
    let mut out: Vec<String> = Vec::new();
    let candidates = first
        .split(':')
        .map(str::to_string)
        .chain(exe_dir)
        .chain(BASE_PATH.iter().map(|s| s.to_string()));
    for dir in candidates {
        if dir.starts_with('/') && !out.contains(&dir) {
            out.push(dir);
        }
    }
    out.join(":")
}

// ---------------------------------------------------------------------------
// The seam
// ---------------------------------------------------------------------------

/// How a command ended.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ran {
    /// Exit code 0.
    pub ok: bool,
    pub code: Option<i32>,
    pub stdout: String,
    /// Its stderr, or why it could not be started.
    pub stderr: String,
}

impl Ran {
    /// The most useful line to show for a failure.
    fn reason(&self) -> String {
        let text = if self.stderr.trim().is_empty() {
            &self.stdout
        } else {
            &self.stderr
        };
        match text.trim() {
            "" => match self.code {
                Some(code) => format!("exit code {code}"),
                None => "no exit code".to_string(),
            },
            t => t.lines().last().unwrap_or(t).trim().to_string(),
        }
    }
}

/// Every side effect of start on boot: the unit file and the manager's
/// commands.
pub trait ServiceOs {
    /// A file's contents, `None` when it does not exist (or cannot be read).
    fn read(&self, path: &Path) -> Option<String>;
    /// Write `contents` (parents created, readable by all, written by the
    /// owner).
    fn write(&self, path: &Path, contents: &str) -> std::io::Result<()>;
    /// Remove a file; one that does not exist is not an error.
    fn remove(&self, path: &Path) -> std::io::Result<()>;
    /// Run `argv` (no shell) with stdin null and capture its output. A
    /// program that could not be started is a failed [`Ran`].
    fn run(&self, argv: &[String]) -> Ran;
    /// Wait a moment before trying again.
    fn pause(&self);
}

/// The real [`ServiceOs`].
#[derive(Clone, Copy, Debug, Default)]
pub struct RealServiceOs;

impl ServiceOs for RealServiceOs {
    fn read(&self, path: &Path) -> Option<String> {
        std::fs::read_to_string(path).ok()
    }

    fn write(&self, path: &Path, contents: &str) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let tmp = path.with_file_name(format!(".{name}.fdtmp"));
        std::fs::write(&tmp, contents)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o644))?;
        }
        std::fs::rename(&tmp, path).inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
    }

    fn remove(&self, path: &Path) -> std::io::Result<()> {
        match std::fs::remove_file(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    }

    fn run(&self, argv: &[String]) -> Ran {
        let Some((program, args)) = argv.split_first() else {
            return Ran::default();
        };
        match std::process::Command::new(program)
            .args(args)
            .stdin(std::process::Stdio::null())
            .output()
        {
            Ok(output) => Ran {
                ok: output.status.success(),
                code: output.status.code(),
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            },
            Err(e) => Ran {
                ok: false,
                code: None,
                stdout: String::new(),
                stderr: format!("could not run {program}: {e}"),
            },
        }
    }

    fn pause(&self) {
        std::thread::sleep(Duration::from_millis(500));
    }
}

#[cfg(unix)]
fn current_uid() -> u32 {
    // SAFETY: getuid(2) takes nothing and cannot fail.
    unsafe { libc::getuid() }
}

#[cfg(not(unix))]
fn current_uid() -> u32 {
    0
}

/// Ask `shell` as a login, interactive shell for its `PATH`, as FlightDeck
/// Desktop does for its environment (`desktop/src/shell_env.rs`): written to a
/// temporary file, not a pipe an rc-started daemon could hold open, with all
/// stdio null and a deadline. `None` when it fails, hangs or prints nothing.
pub fn probe_login_path(shell: &Path, home: Option<&Path>, timeout: Duration) -> Option<String> {
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Instant;
    const OUT_VAR: &str = "FLIGHTDECK_PATH_OUT";
    static PROBES: AtomicU32 = AtomicU32::new(0);
    let out = std::env::temp_dir().join(format!(
        "flightdeck-login-path-{}-{}",
        std::process::id(),
        PROBES.fetch_add(1, Ordering::Relaxed)
    ));
    let mut command = Command::new(shell);
    command
        .args([
            "-l",
            "-i",
            "-c",
            &format!("command printf '%s' \"$PATH\" > \"${OUT_VAR}\""),
        ])
        .env(OUT_VAR, &out)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(home) = home {
        command.current_dir(home);
    }
    let mut child = command.spawn().ok()?;
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let text = std::fs::read_to_string(&out).ok();
    let _ = std::fs::remove_file(&out);
    status.filter(|s| s.success())?;
    text.map(|t| t.trim().to_string()).filter(|t| !t.is_empty())
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

fn launchctl(args: &[&str]) -> Vec<String> {
    let mut v = argv(&["launchctl"]);
    v.extend(argv(args));
    v
}

fn systemctl(args: &[&str]) -> Vec<String> {
    let mut v = argv(&["systemctl", "--user"]);
    v.extend(argv(args));
    v
}

/// `argv` as a person would type it: an argument with anything but safe
/// characters goes in single quotes.
pub fn display_argv(argv: &[String]) -> String {
    argv.iter()
        .map(|a| {
            let safe = !a.is_empty()
                && a.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_./=:@%+,".contains(c));
            if safe {
                a.clone()
            } else {
                format!("'{}'", a.replace('\'', r"'\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The `loginctl enable-linger` line to print.
fn linger_command(user: Option<&str>) -> String {
    format!("sudo loginctl enable-linger {}", user.unwrap_or("$USER"))
}

// ---------------------------------------------------------------------------
// Status
// ---------------------------------------------------------------------------

/// Whether start on boot is set up: `worker status` text and the
/// `start_on_boot` field of `worker status --json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BootReport {
    pub manager: Manager,
    /// The unit file's path.
    pub unit: String,
    /// The unit file exists.
    pub installed: bool,
    /// It will start with the OS: systemd `is-enabled`; launchd, installed
    /// and not disabled with `launchctl disable`.
    pub enabled: bool,
    /// The manager has it loaded (launchd: in `gui/<uid>`; systemd: the unit
    /// is known).
    pub loaded: bool,
    /// The manager is running it now.
    pub active: bool,
    /// systemd only: lingering is on, so it starts at boot rather than at
    /// login. `None` on macOS, or when it could not be told.
    pub linger: Option<bool>,
    /// The unit runs a different `flightdeck` binary than this one.
    pub other_binary: bool,
    /// Why the manager could not be asked (`systemctl --user` with no bus).
    pub error: Option<String>,
}

/// Ask the manager about the worker's unit. Runs nothing while no unit file
/// exists.
pub fn query(os: &dyn ServiceOs, ctx: &Context) -> BootReport {
    let unit = ctx.unit_path();
    let file = os.read(&unit);
    let mut report = BootReport {
        manager: ctx.manager,
        unit: unit.to_string_lossy().into_owned(),
        installed: file.is_some(),
        enabled: false,
        loaded: false,
        active: false,
        linger: None,
        other_binary: false,
        error: None,
    };
    let Some(file) = file else {
        return report;
    };
    let exe = ctx.exe.to_string_lossy();
    report.other_binary = !match ctx.manager {
        Manager::Launchd => file.contains(&format!("<string>{}</string>", xml_escape(&exe))),
        Manager::Systemd => file.contains(&format!("ExecStart={} ", systemd_quote(&exe))),
    };
    match ctx.manager {
        Manager::Launchd => {
            let print = os.run(&launchctl(&["print", &ctx.target()]));
            report.loaded = print.ok;
            report.active = print.ok && launchd_running(&print.stdout);
            let disabled = os.run(&launchctl(&["print-disabled", &ctx.domain()]));
            report.enabled = !(disabled.ok && launchd_disabled(&disabled.stdout, LAUNCHD_LABEL));
        }
        Manager::Systemd => {
            let show = os.run(&systemctl(&[
                "show",
                SYSTEMD_UNIT,
                "--property=LoadState,UnitFileState,ActiveState",
            ]));
            if show.ok {
                let props = parse_properties(&show.stdout);
                let get = |k: &str| {
                    props
                        .iter()
                        .find(|(key, _)| key == k)
                        .map(|(_, v)| v.as_str())
                };
                report.loaded = get("LoadState") == Some("loaded");
                report.enabled = get("UnitFileState") == Some("enabled");
                report.active = matches!(get("ActiveState"), Some("active" | "activating"));
            } else {
                report.error = Some(format!("systemctl --user: {}", show.reason()));
            }
            report.linger = query_linger(os, ctx);
        }
    }
    report
}

/// `loginctl show-user <user> --property=Linger`: `Some(true)` for `yes`.
fn query_linger(os: &dyn ServiceOs, ctx: &Context) -> Option<bool> {
    let user = ctx.user.as_deref()?;
    let ran = os.run(&argv(&["loginctl", "show-user", user, "--property=Linger"]));
    if !ran.ok {
        return None;
    }
    parse_properties(&ran.stdout)
        .into_iter()
        .find(|(k, _)| k == "Linger")
        .map(|(_, v)| v == "yes")
}

/// `KEY=value` lines (`systemctl show`, `loginctl show-user`).
pub fn parse_properties(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let (k, v) = line.split_once('=')?;
            Some((k.trim().to_string(), v.trim().to_string()))
        })
        .collect()
}

/// Whether `launchctl print` says the job runs (`state = running`).
pub fn launchd_running(print: &str) -> bool {
    print
        .lines()
        .any(|l| l.trim().replace(' ', "") == "state=running")
}

/// Whether `launchctl print-disabled` lists `label` as disabled: `"label" =>
/// disabled` (macOS 13 and later) or `"label" => true` (earlier).
pub fn launchd_disabled(print: &str, label: &str) -> bool {
    let quoted = format!("\"{label}\"");
    print.lines().any(|l| {
        let l = l.trim();
        l.starts_with(&quoted)
            && l.split("=>")
                .nth(1)
                .is_some_and(|v| matches!(v.trim(), "disabled" | "true"))
    })
}

/// `worker status`'s lines about start on boot.
pub fn render_status(r: &BootReport, user: Option<&str>) -> Vec<String> {
    let when = r.manager.when();
    if !r.installed {
        return vec![format!(
            "{when}: off (`flightdeck worker enable` sets it up)"
        )];
    }
    let mut lines = Vec::new();
    let head = match r.manager {
        Manager::Launchd if !r.enabled => {
            "installed, but disabled with `launchctl disable` (run `flightdeck worker enable`)"
                .to_string()
        }
        Manager::Launchd if r.loaded => format!("on ({}, loaded)", r.manager.describe()),
        Manager::Launchd => format!("on ({}; it loads at your next login)", r.manager.describe()),
        Manager::Systemd if r.error.is_some() => {
            format!(
                "installed ({}), but systemd could not be asked",
                r.manager.describe()
            )
        }
        Manager::Systemd if !r.enabled => {
            "installed, but not enabled (run `flightdeck worker enable`)".to_string()
        }
        Manager::Systemd if r.linger == Some(true) => {
            format!("on ({}, lingering on)", r.manager.describe())
        }
        Manager::Systemd => format!(
            "only after you log in: lingering is off (run `{}`)",
            linger_command(user)
        ),
    };
    lines.push(format!("{when}: {head}"));
    if let Some(error) = &r.error {
        lines.push(format!("  {error}"));
    }
    if r.other_binary {
        lines.push(
            "  It runs another flightdeck binary: run `flightdeck worker enable` to point it \
             at this one."
                .to_string(),
        );
    }
    lines
}

// ---------------------------------------------------------------------------
// enable / disable / start
// ---------------------------------------------------------------------------

/// A step `enable` or `disable` takes.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Action {
    Write { path: PathBuf, contents: String },
    Remove { path: PathBuf },
    Run(Vec<String>),
}

/// What a failed step means.
#[derive(Clone, Debug, PartialEq, Eq)]
enum OnFailure {
    /// Expected at times (`bootout` of a job that is not loaded): go on.
    Ignore,
    /// Print this, then go on.
    Note(String),
    /// Stop here and fail.
    Abort,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Step {
    action: Action,
    on_failure: OnFailure,
    attempts: u32,
}

impl Step {
    fn new(action: Action, on_failure: OnFailure) -> Step {
        Step {
            action,
            on_failure,
            attempts: 1,
        }
    }

    fn run(argv: Vec<String>, on_failure: OnFailure) -> Step {
        Step::new(Action::Run(argv), on_failure)
    }
}

/// How a run of steps went.
struct Executed {
    /// Every step that mattered succeeded (none was noted).
    clean: bool,
}

/// Print the steps a `--dry-run` would take.
fn print_plan(steps: &[Step], out: &mut dyn Write) {
    let mut runs = Vec::new();
    for step in steps {
        match &step.action {
            Action::Write { path, contents } => {
                let _ = writeln!(out, "Would write {}:\n", path.display());
                for line in contents.lines() {
                    let _ = writeln!(out, "    {line}");
                }
                let _ = writeln!(out);
            }
            Action::Remove { path } => {
                let _ = writeln!(out, "Would remove {}", path.display());
            }
            Action::Run(argv) => {
                let tail = if step.on_failure == OnFailure::Ignore {
                    "   # a failure here is expected and ignored"
                } else {
                    ""
                };
                runs.push(format!("    {}{tail}", display_argv(argv)));
            }
        }
    }
    if !runs.is_empty() {
        let _ = writeln!(out, "Would run:");
        for line in runs {
            let _ = writeln!(out, "{line}");
        }
    }
}

/// Take `steps` in order. `Err(1)` once an [`OnFailure::Abort`] step fails,
/// after saying why.
fn execute(
    os: &dyn ServiceOs,
    steps: &[Step],
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<Executed, i32> {
    let mut clean = true;
    for step in steps {
        let failure = match &step.action {
            Action::Write { path, contents } => os
                .write(path, contents)
                .err()
                .map(|e| format!("could not write {}: {e}", path.display())),
            Action::Remove { path } => os
                .remove(path)
                .err()
                .map(|e| format!("could not remove {}: {e}", path.display())),
            Action::Run(argv) => {
                let mut last = None;
                for attempt in 0..step.attempts.max(1) {
                    if attempt > 0 {
                        os.pause();
                    }
                    let ran = os.run(argv);
                    if ran.ok {
                        last = None;
                        break;
                    }
                    last = Some(format!("`{}` failed: {}", display_argv(argv), ran.reason()));
                }
                last
            }
        };
        let Some(failure) = failure else { continue };
        match &step.on_failure {
            OnFailure::Ignore => {}
            OnFailure::Note(note) => {
                clean = false;
                let _ = writeln!(out, "{note}\n  ({failure})");
            }
            OnFailure::Abort => {
                let _ = writeln!(err, "flightdeck worker: {failure}");
                return Err(1);
            }
        }
    }
    Ok(Executed { clean })
}

/// What `enable` did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Enabled {
    /// The manager was asked to start the worker, and agreed: the caller
    /// waits for it to answer.
    pub started: bool,
}

/// `flightdeck worker enable [--dry-run]`: write the unit for `spec`, register
/// it, and start the worker through the manager unless one already runs.
pub fn enable(
    os: &dyn ServiceOs,
    ctx: &Context,
    spec: &ServiceSpec,
    worker_running: bool,
    dry_run: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<Enabled, i32> {
    let unit = ctx.unit_path();
    let write = Step::new(
        Action::Write {
            path: unit.clone(),
            contents: render(ctx.manager, spec),
        },
        OnFailure::Abort,
    );
    let mut steps = vec![write];
    let mut start = false;
    let mut notes = Vec::new();
    match ctx.manager {
        Manager::Launchd => {
            // Clears a `launchctl disable` left from before, which would make
            // the bootstrap fail.
            steps.push(Step::run(
                launchctl(&["enable", &ctx.target()]),
                OnFailure::Ignore,
            ));
            if worker_running {
                notes.push(
                    "A worker is running and is left alone: run `flightdeck worker restart` \
                     to restart it under launchd with these settings."
                        .to_string(),
                );
            } else {
                steps.push(Step::run(
                    launchctl(&["bootout", &ctx.target()]),
                    OnFailure::Ignore,
                ));
                let mut bootstrap = Step::run(
                    launchctl(&["bootstrap", &ctx.domain(), &unit.to_string_lossy()]),
                    OnFailure::Note(
                        "launchd could not load it now; that takes a login at the screen \
                         (over SSH with nobody logged in there is no GUI session). It loads at \
                         the next login; until then, start the worker with `flightdeck worker \
                         start`."
                            .to_string(),
                    ),
                );
                bootstrap.attempts = BOOTSTRAP_ATTEMPTS;
                steps.push(bootstrap);
                start = true;
            }
        }
        Manager::Systemd => {
            steps.push(Step::run(systemctl(&["daemon-reload"]), OnFailure::Abort));
            steps.push(Step::run(
                systemctl(&["enable", SYSTEMD_UNIT]),
                OnFailure::Abort,
            ));
            let linger = query_linger(os, ctx);
            if linger != Some(true) {
                let state = if linger == Some(false) {
                    "Lingering is off"
                } else {
                    "Could not tell whether lingering is on"
                };
                notes.push(format!(
                    "{state} for your account, so systemd starts the worker only once you log \
                     in, and stops it when you log out. To start it at boot and keep it \
                     running, run (it may ask for your password):\n\n    {}\n",
                    linger_command(ctx.user.as_deref())
                ));
            }
            if worker_running {
                notes.push(
                    "A worker is running and is left alone: run `flightdeck worker restart` \
                     to restart it under systemd with these settings."
                        .to_string(),
                );
            } else if linger == Some(true) {
                steps.push(Step::run(
                    systemctl(&["start", SYSTEMD_UNIT]),
                    OnFailure::Note(
                        "systemd could not start the worker; see `flightdeck worker logs` \
                         and `journalctl --user -u flightdeck-worker`."
                            .to_string(),
                    ),
                ));
                start = true;
            } else {
                notes.push("Start the worker now with `flightdeck worker start`.".to_string());
            }
        }
    }

    if dry_run {
        print_plan(&steps, out);
        for note in &notes {
            let _ = writeln!(out, "\n{note}");
        }
        return Ok(Enabled { started: false });
    }

    let executed = match execute(os, &steps, out, err) {
        Ok(executed) => executed,
        Err(code) => {
            if ctx.manager == Manager::Systemd && os.read(&unit).is_some() {
                let _ = writeln!(
                    err,
                    "The unit is written ({}), but not enabled. `systemctl --user` needs your \
                     user manager and its D-Bus in this session (over SSH, a normal login \
                     session has them); then run `flightdeck worker enable` again.",
                    unit.display()
                );
            }
            return Err(code);
        }
    };
    let _ = writeln!(
        out,
        "{} is on: {} {}.",
        ctx.manager.when(),
        ctx.manager.describe(),
        unit.display()
    );
    let _ = writeln!(out, "  Runs: {} worker supervise", spec.exe.display());
    let _ = writeln!(out, "  PATH: {}", spec.path());
    let _ = writeln!(
        out,
        "  Run `flightdeck worker enable` again after changing your PATH, to refresh it."
    );
    for note in &notes {
        let _ = writeln!(out, "\n{note}");
    }
    Ok(Enabled {
        started: start && executed.clean,
    })
}

/// `flightdeck worker disable [--dry-run]`: unregister and remove the unit.
/// The caller has already stopped a worker the manager was running
/// (`report.active`), gracefully, so unloading it kills nothing.
pub fn disable(
    os: &dyn ServiceOs,
    ctx: &Context,
    report: &BootReport,
    dry_run: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<(), i32> {
    let unit = ctx.unit_path();
    if !report.installed && !report.loaded {
        let _ = writeln!(out, "{} is not set up.", ctx.manager.when());
        return Ok(());
    }
    let remove = Step::new(Action::Remove { path: unit.clone() }, OnFailure::Abort);
    let steps = match ctx.manager {
        Manager::Launchd => vec![
            Step::run(launchctl(&["bootout", &ctx.target()]), OnFailure::Ignore),
            remove,
        ],
        Manager::Systemd => vec![
            Step::run(
                systemctl(&["disable", SYSTEMD_UNIT]),
                OnFailure::Note("systemd could not disable the unit.".to_string()),
            ),
            remove,
            Step::run(systemctl(&["daemon-reload"]), OnFailure::Ignore),
            Step::run(
                systemctl(&["reset-failed", SYSTEMD_UNIT]),
                OnFailure::Ignore,
            ),
        ],
    };
    if dry_run {
        print_plan(&steps, out);
        return Ok(());
    }
    execute(os, &steps, out, err)?;
    let _ = writeln!(
        out,
        "{} is off: removed {}.",
        ctx.manager.when(),
        unit.display()
    );
    Ok(())
}

/// How [`start`] went.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServiceStart {
    /// No unit: start the worker the usual way.
    NotInstalled,
    /// The manager started it.
    Started,
    /// The manager is not used this time, for this reason: start it the
    /// usual way.
    Skipped(String),
    /// The manager refused, for this reason: start it the usual way.
    Failed(String),
}

/// Start the supervisor through the manager, for `worker start` while no
/// worker runs. launchd reloads the plist first (`bootout`, then
/// `bootstrap`, whose `RunAtLoad` starts it), so a plist `enable` rewrote
/// while a worker ran takes effect now.
pub fn start(os: &dyn ServiceOs, ctx: &Context, report: &BootReport) -> ServiceStart {
    if !report.installed {
        return ServiceStart::NotInstalled;
    }
    match ctx.manager {
        Manager::Launchd => {
            if !report.enabled {
                return ServiceStart::Skipped(
                    "the launchd agent is disabled with `launchctl disable`".to_string(),
                );
            }
            if report.loaded {
                os.run(&launchctl(&["bootout", &ctx.target()]));
            }
            let bootstrap = launchctl(&[
                "bootstrap",
                &ctx.domain(),
                &ctx.unit_path().to_string_lossy(),
            ]);
            let mut last = String::new();
            for attempt in 0..BOOTSTRAP_ATTEMPTS {
                if attempt > 0 {
                    os.pause();
                }
                let ran = os.run(&bootstrap);
                if ran.ok {
                    return ServiceStart::Started;
                }
                last = ran.reason();
            }
            ServiceStart::Failed(format!(
                "launchd could not load it ({last}); with nobody logged in at the screen there \
                 is no GUI session"
            ))
        }
        Manager::Systemd => {
            if let Some(error) = &report.error {
                return ServiceStart::Skipped(error.clone());
            }
            if !report.enabled {
                return ServiceStart::Skipped("the systemd unit is not enabled".to_string());
            }
            if report.linger != Some(true) {
                return ServiceStart::Skipped(format!(
                    "lingering is off, and a worker systemd started would stop when you log \
                     out (run `{}`)",
                    linger_command(ctx.user.as_deref())
                ));
            }
            let ran = os.run(&systemctl(&["start", SYSTEMD_UNIT]));
            if ran.ok {
                ServiceStart::Started
            } else {
                ServiceStart::Failed(format!("systemd could not start it ({})", ran.reason()))
            }
        }
    }
}

// launchd and systemd only: the paths and commands the tests expect are Unix
// ones, and `flightdeck worker` refuses to run on Windows.
#[cfg(all(test, unix))]
mod tests;
