use super::*;

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, VecDeque};

/// An in-memory [`ServiceOs`]: files in a map, every command recorded (as
/// [`display_argv`] prints it) and answered from a script, success by
/// default.
#[derive(Default)]
struct FakeOs {
    files: RefCell<BTreeMap<PathBuf, String>>,
    ran: RefCell<Vec<String>>,
    /// Per command, the answers still to give; the last one repeats.
    answers: RefCell<HashMap<String, VecDeque<Ran>>>,
    pauses: Cell<u32>,
    /// Every path written or removed, in order.
    touched: RefCell<Vec<PathBuf>>,
}

impl FakeOs {
    fn with_file(self, path: &Path, contents: &str) -> Self {
        self.files
            .borrow_mut()
            .insert(path.to_path_buf(), contents.to_string());
        self
    }

    fn answer(&self, command: &str, ran: Ran) {
        self.answers
            .borrow_mut()
            .entry(command.to_string())
            .or_default()
            .push_back(ran);
    }

    fn says(&self, command: &str, stdout: &str) {
        self.answer(
            command,
            Ran {
                ok: true,
                code: Some(0),
                stdout: stdout.to_string(),
                stderr: String::new(),
            },
        );
    }

    fn fails(&self, command: &str, stderr: &str) {
        self.answer(
            command,
            Ran {
                ok: false,
                code: Some(5),
                stdout: String::new(),
                stderr: stderr.to_string(),
            },
        );
    }

    fn ran(&self) -> Vec<String> {
        self.ran.borrow().clone()
    }

    fn file(&self, path: &Path) -> Option<String> {
        self.files.borrow().get(path).cloned()
    }
}

impl ServiceOs for FakeOs {
    fn read(&self, path: &Path) -> Option<String> {
        self.file(path)
    }

    fn write(&self, path: &Path, contents: &str) -> std::io::Result<()> {
        self.touched.borrow_mut().push(path.to_path_buf());
        self.files
            .borrow_mut()
            .insert(path.to_path_buf(), contents.to_string());
        Ok(())
    }

    fn remove(&self, path: &Path) -> std::io::Result<()> {
        self.touched.borrow_mut().push(path.to_path_buf());
        self.files.borrow_mut().remove(path);
        Ok(())
    }

    fn run(&self, argv: &[String]) -> Ran {
        let command = display_argv(argv);
        self.ran.borrow_mut().push(command.clone());
        let mut answers = self.answers.borrow_mut();
        match answers.get_mut(&command) {
            Some(queue) if queue.len() > 1 => queue.pop_front().unwrap(),
            Some(queue) if !queue.is_empty() => queue[0].clone(),
            _ => Ran {
                ok: true,
                code: Some(0),
                ..Ran::default()
            },
        }
    }

    fn pause(&self) {
        self.pauses.set(self.pauses.get() + 1);
    }
}

fn mac() -> Context {
    Context {
        manager: Manager::Launchd,
        home: PathBuf::from("/Users/u"),
        log_file: PathBuf::from("/Users/u/.flightdeck/logs/worker.log"),
        uid: 501,
        user: Some("u".to_string()),
        exe: PathBuf::from("/opt/homebrew/bin/flightdeck"),
    }
}

fn linux() -> Context {
    Context {
        manager: Manager::Systemd,
        home: PathBuf::from("/home/u"),
        log_file: PathBuf::from("/home/u/.flightdeck/logs/worker.log"),
        uid: 1000,
        user: Some("u".to_string()),
        exe: PathBuf::from("/home/u/.local/bin/flightdeck"),
    }
}

fn env() -> Environment {
    Environment {
        login_path: Some("/opt/homebrew/bin:/Users/u/.local/bin:/usr/bin:/bin".to_string()),
        inherited_path: Some("/usr/bin:/bin".to_string()),
        shell: Some("/bin/zsh".to_string()),
        lang: Some("en_US.UTF-8".to_string()),
    }
}

fn spec(home: &str, exe: &str, path: &str) -> ServiceSpec {
    ServiceSpec {
        exe: PathBuf::from(exe),
        home: PathBuf::from(home),
        log_file: PathBuf::from(format!("{home}/.flightdeck/logs/worker.log")),
        env: vec![
            ("PATH".to_string(), path.to_string()),
            ("SHELL".to_string(), "/bin/zsh".to_string()),
        ],
    }
}

/// Run `f` with captured output: (result, out, err).
fn capture<T>(f: impl FnOnce(&mut dyn Write, &mut dyn Write) -> T) -> (T, String, String) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let result = f(&mut out, &mut err);
    (
        result,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

// ---------------------------------------------------------------------------
// The unit text
// ---------------------------------------------------------------------------

#[test]
fn the_plist_is_pinned() {
    let plist = launchd_plist(&spec(
        "/Users/u",
        "/opt/homebrew/bin/flightdeck",
        "/opt/homebrew/bin:/usr/bin",
    ));
    let expected = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<!-- Written by `flightdeck worker enable`; `flightdeck worker disable` removes it.
     Run `flightdeck worker enable` again to refresh the binary path and PATH. -->
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>agency.neworange.flightdeck.worker</string>
	<key>ProgramArguments</key>
	<array>
		<string>/opt/homebrew/bin/flightdeck</string>
		<string>worker</string>
		<string>supervise</string>
	</array>
	<key>WorkingDirectory</key>
	<string>/Users/u</string>
	<key>EnvironmentVariables</key>
	<dict>
		<key>PATH</key>
		<string>/opt/homebrew/bin:/usr/bin</string>
		<key>SHELL</key>
		<string>/bin/zsh</string>
	</dict>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<false/>
	<key>LimitLoadToSessionType</key>
	<string>Aqua</string>
	<key>ProcessType</key>
	<string>Interactive</string>
	<key>ExitTimeOut</key>
	<integer>45</integer>
	<key>StandardErrorPath</key>
	<string>/Users/u/.flightdeck/logs/worker.log</string>
</dict>
</plist>
"#;
    assert_eq!(plist, expected);
}

#[test]
fn the_plist_escapes_paths_with_spaces_and_markup() {
    let plist = launchd_plist(&spec(
        "/Users/Ann & Bob",
        "/Users/Ann & Bob/My <Tools>/flightdeck",
        "/Users/Ann & Bob/bin:/usr/bin",
    ));
    assert!(
        plist.contains("<string>/Users/Ann &amp; Bob/My &lt;Tools&gt;/flightdeck</string>"),
        "{plist}"
    );
    assert!(plist.contains("<string>/Users/Ann &amp; Bob</string>"));
    assert!(plist.contains("<string>/Users/Ann &amp; Bob/bin:/usr/bin</string>"));
    assert!(plist.contains("<string>/Users/Ann &amp; Bob/.flightdeck/logs/worker.log</string>"));
    assert!(
        !plist.contains("Ann & Bob"),
        "an unescaped & breaks the XML"
    );
    assert_eq!(xml_escape(r#"a"b'c"#), "a&quot;b&apos;c");
}

#[test]
fn the_systemd_unit_is_pinned() {
    let unit = systemd_unit(&spec(
        "/home/u",
        "/home/u/.local/bin/flightdeck",
        "/home/u/.local/bin:/usr/bin",
    ));
    let expected =
        "# Written by `flightdeck worker enable`; `flightdeck worker disable` removes it.
# Run `flightdeck worker enable` again to refresh the binary path and PATH.
[Unit]
Description=FlightDeck worker

[Service]
Type=simple
ExecStart=\"/home/u/.local/bin/flightdeck\" worker supervise
WorkingDirectory=~
Environment=\"PATH=/home/u/.local/bin:/usr/bin\"
Environment=\"SHELL=/bin/zsh\"
# The supervisor restarts the worker after a crash and gives up after
# repeated ones; systemd only starts it, so a `flightdeck worker stop` stays
# stopped.
Restart=no
# SIGTERM to the supervisor alone: it stops the worker gracefully.
KillMode=mixed
TimeoutStopSec=45

[Install]
WantedBy=default.target
";
    assert_eq!(unit, expected);
}

/// What `systemd-analyze verify` (systemd 252) accepted and parsed back to
/// the original path and value.
#[test]
fn the_systemd_unit_quotes_spaces_specifiers_quotes_and_backslashes() {
    let unit = systemd_unit(&spec(
        "/home/u",
        "/home/u/my tools/100%/$bin/flightdeck",
        "/home/u/my tools:/opt/a\"b\\c",
    ));
    assert!(
        unit.contains("ExecStart=\"/home/u/my tools/100%%/$bin/flightdeck\" worker supervise\n"),
        "{unit}"
    );
    assert!(
        unit.contains("Environment=\"PATH=/home/u/my tools:/opt/a\\\"b\\\\c\"\n"),
        "{unit}"
    );
    // Neither the executable nor an assignment expands `$`.
    assert_eq!(systemd_quote("a$b"), "\"a$b\"");
    assert_eq!(systemd_quote("a\nb"), "\"a\\nb\"");
}

#[test]
fn render_picks_the_manager_s_format() {
    let s = spec("/home/u", "/x/flightdeck", "/usr/bin");
    assert!(render(Manager::Launchd, &s).starts_with("<?xml"));
    assert!(render(Manager::Systemd, &s).contains("[Service]"));
}

#[test]
fn unit_paths() {
    assert_eq!(
        Manager::Launchd.unit_path(Path::new("/Users/u")),
        PathBuf::from("/Users/u/Library/LaunchAgents/agency.neworange.flightdeck.worker.plist")
    );
    assert_eq!(
        Manager::Systemd.unit_path(Path::new("/home/u")),
        PathBuf::from("/home/u/.config/systemd/user/flightdeck-worker.service")
    );
}

#[test]
fn this_os_has_the_expected_manager() {
    let expected = if cfg!(target_os = "macos") {
        Some(Manager::Launchd)
    } else if cfg!(target_os = "linux") {
        Some(Manager::Systemd)
    } else {
        None
    };
    assert_eq!(Manager::for_this_os(), expected);
}

// ---------------------------------------------------------------------------
// Binary path and PATH
// ---------------------------------------------------------------------------

#[test]
fn a_homebrew_cellar_binary_becomes_the_prefix_link() {
    let exists = |p: &Path| p == Path::new("/home/linuxbrew/.linuxbrew/bin/flightdeck");
    assert_eq!(
        stable_exe(
            Path::new("/home/linuxbrew/.linuxbrew/Cellar/flightdeck/1.32.0/bin/flightdeck"),
            &exists
        ),
        PathBuf::from("/home/linuxbrew/.linuxbrew/bin/flightdeck")
    );
    let exists = |p: &Path| p == Path::new("/opt/homebrew/bin/flightdeck");
    assert_eq!(
        stable_exe(
            Path::new("/opt/homebrew/Cellar/flightdeck/1.32.0/bin/flightdeck"),
            &exists
        ),
        PathBuf::from("/opt/homebrew/bin/flightdeck")
    );
}

#[test]
fn a_cellar_binary_without_a_link_stays_put() {
    let cellar = Path::new("/opt/homebrew/Cellar/flightdeck/1.32.0/bin/flightdeck");
    assert_eq!(stable_exe(cellar, &|_| false), cellar);
    // Not the Cellar layout: left alone even if a link would exist.
    let odd = Path::new("/opt/homebrew/Cellar/flightdeck/bin/flightdeck");
    assert_eq!(stable_exe(odd, &|_| true), odd);
}

#[test]
fn other_binaries_are_used_as_they_are() {
    for path in [
        "/opt/homebrew/bin/flightdeck",
        "/Users/u/.cargo/bin/flightdeck",
        "/home/u/.local/bin/flightdeck",
    ] {
        assert_eq!(stable_exe(Path::new(path), &|_| true), PathBuf::from(path));
    }
}

#[test]
fn a_binary_replaced_in_place_loses_the_deleted_suffix() {
    assert_eq!(
        stable_exe(
            Path::new("/home/u/.local/bin/flightdeck (deleted)"),
            &|_| false
        ),
        PathBuf::from("/home/u/.local/bin/flightdeck")
    );
}

#[test]
fn the_login_shell_path_comes_first_then_the_binary_and_the_base() {
    assert_eq!(
        service_path(
            Some("/opt/homebrew/bin:/usr/bin:/bin"),
            Some("/ignored"),
            Some(Path::new("/Users/u/.cargo/bin"))
        ),
        "/opt/homebrew/bin:/usr/bin:/bin:/Users/u/.cargo/bin:/usr/local/bin:/usr/sbin:/sbin"
    );
}

#[test]
fn without_a_login_shell_the_inherited_path_is_used() {
    assert_eq!(
        service_path(None, Some("/a:/b"), None),
        "/a:/b:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"
    );
    assert_eq!(
        service_path(Some("  "), Some("/a"), None),
        "/a:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin",
        "a blank answer counts as none"
    );
    assert_eq!(
        service_path(None, None, None),
        "/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"
    );
}

#[test]
fn relative_empty_and_repeated_path_entries_are_dropped() {
    assert_eq!(
        service_path(Some(".:/a::bin:/a:/usr/bin"), None, Some(Path::new("/a"))),
        "/a:/usr/bin:/usr/local/bin:/bin:/usr/sbin:/sbin"
    );
}

#[test]
fn the_spec_carries_path_shell_and_lang() {
    let s = mac().spec(&env());
    assert_eq!(s.exe, PathBuf::from("/opt/homebrew/bin/flightdeck"));
    assert_eq!(
        s.env,
        vec![
            (
                "PATH".to_string(),
                "/opt/homebrew/bin:/Users/u/.local/bin:/usr/bin:/bin:/usr/local/bin:/usr/sbin:/sbin"
                    .to_string()
            ),
            ("SHELL".to_string(), "/bin/zsh".to_string()),
            ("LANG".to_string(), "en_US.UTF-8".to_string()),
        ]
    );
    let bare = mac().spec(&Environment::default());
    assert_eq!(bare.env.len(), 1, "unset SHELL and LANG are left out");
}

#[cfg(unix)]
#[test]
fn the_login_shell_is_asked_for_its_path() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let shell = dir.path().join("fake-shell");
    std::fs::write(
        &shell,
        "#!/bin/sh\nprintf '/from/profile:/usr/bin' > \"$FLIGHTDECK_PATH_OUT\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        probe_login_path(&shell, Some(dir.path()), Duration::from_secs(5)).as_deref(),
        Some("/from/profile:/usr/bin")
    );
}

#[cfg(unix)]
#[test]
fn a_failing_or_hanging_login_shell_gives_no_path() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let script = |name: &str, body: &str| {
        let path = dir.path().join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    };
    let failing = script("failing", "exit 3");
    assert_eq!(
        probe_login_path(&failing, None, Duration::from_secs(5)),
        None
    );
    let hanging = script("hanging", "exec sleep 30");
    let started = std::time::Instant::now();
    assert_eq!(
        probe_login_path(&hanging, None, Duration::from_millis(200)),
        None
    );
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(
        probe_login_path(
            Path::new("/nonexistent/shell"),
            None,
            Duration::from_secs(1)
        ),
        None
    );
}

#[cfg(unix)]
#[test]
fn a_real_login_shell_reports_a_path() {
    let path = probe_login_path(Path::new("/bin/sh"), None, Duration::from_secs(10));
    assert!(path.is_some_and(|p| p.contains('/')));
}

// ---------------------------------------------------------------------------
// Parsing the managers' answers
// ---------------------------------------------------------------------------

#[test]
fn launchctl_print_running_or_not() {
    let running = "gui/501/agency.neworange.flightdeck.worker = {\n\tactive count = 1\n\tstate = running\n\tpid = 4100\n}\n";
    assert!(launchd_running(running));
    let waiting = "gui/501/agency.neworange.flightdeck.worker = {\n\tstate = not running\n}\n";
    assert!(!launchd_running(waiting));
}

#[test]
fn launchctl_print_disabled_in_both_formats() {
    let label = LAUNCHD_LABEL;
    let new = format!(
        "disabled services = {{\n\t\"com.apple.x\" => enabled\n\t\"{label}\" => disabled\n}}\n"
    );
    let old = format!("disabled services = {{\n\t\"{label}\" => true\n}}\n");
    let enabled = format!("disabled services = {{\n\t\"{label}\" => enabled\n}}\n");
    let other =
        "disabled services = {\n\t\"agency.neworange.flightdeck.worker.other\" => disabled\n}\n";
    assert!(launchd_disabled(&new, label));
    assert!(launchd_disabled(&old, label));
    assert!(!launchd_disabled(&enabled, label));
    assert!(!launchd_disabled(other, label));
}

#[test]
fn key_value_properties() {
    assert_eq!(
        parse_properties("LoadState=loaded\nUnitFileState=enabled\nnoise\nLinger=yes\n"),
        vec![
            ("LoadState".to_string(), "loaded".to_string()),
            ("UnitFileState".to_string(), "enabled".to_string()),
            ("Linger".to_string(), "yes".to_string()),
        ]
    );
}

#[test]
fn display_quotes_only_what_needs_it() {
    let argv = vec![
        "launchctl".to_string(),
        "bootstrap".to_string(),
        "gui/501".to_string(),
        "/Users/Ann Bob/it's.plist".to_string(),
    ];
    assert_eq!(
        display_argv(&argv),
        r"launchctl bootstrap gui/501 '/Users/Ann Bob/it'\''s.plist'"
    );
}

// ---------------------------------------------------------------------------
// query
// ---------------------------------------------------------------------------

const MAC_PLIST: &str = "/Users/u/Library/LaunchAgents/agency.neworange.flightdeck.worker.plist";
const LINUX_UNIT: &str = "/home/u/.config/systemd/user/flightdeck-worker.service";
const PRINT: &str = "launchctl print gui/501/agency.neworange.flightdeck.worker";
const PRINT_DISABLED: &str = "launchctl print-disabled gui/501";
const SHOW: &str = "systemctl --user show flightdeck-worker.service --property=LoadState,UnitFileState,ActiveState";
const LINGER: &str = "loginctl show-user u --property=Linger";

fn mac_installed() -> FakeOs {
    let plist = launchd_plist(&mac().spec(&env()));
    FakeOs::default().with_file(Path::new(MAC_PLIST), &plist)
}

fn linux_installed() -> FakeOs {
    let unit = systemd_unit(&linux().spec(&env()));
    FakeOs::default().with_file(Path::new(LINUX_UNIT), &unit)
}

#[test]
fn nothing_installed_asks_no_manager() {
    for ctx in [mac(), linux()] {
        let os = FakeOs::default();
        let r = query(&os, &ctx);
        assert!(!r.installed && !r.enabled && !r.loaded && !r.active);
        assert_eq!(r.unit, ctx.unit_path().to_string_lossy());
        assert!(os.ran().is_empty(), "{:?}", os.ran());
    }
}

#[test]
fn a_loaded_running_launchd_agent() {
    let os = mac_installed();
    os.says(PRINT, "\tstate = running\n");
    os.says(PRINT_DISABLED, "disabled services = {\n}\n");
    let r = query(&os, &mac());
    assert!(r.installed && r.enabled && r.loaded && r.active);
    assert!(!r.other_binary);
    assert_eq!(r.linger, None);
    assert_eq!(os.ran(), vec![PRINT, PRINT_DISABLED]);
}

#[test]
fn an_unloaded_or_disabled_launchd_agent() {
    let os = mac_installed();
    os.fails(PRINT, "Could not find service");
    os.says(
        PRINT_DISABLED,
        &format!("\t\"{LAUNCHD_LABEL}\" => disabled\n"),
    );
    let r = query(&os, &mac());
    assert!(r.installed && !r.loaded && !r.active && !r.enabled);
}

#[test]
fn a_unit_for_another_binary_is_flagged() {
    let os = mac_installed();
    let mut ctx = mac();
    ctx.exe = PathBuf::from("/Users/u/.cargo/bin/flightdeck");
    assert!(query(&os, &ctx).other_binary);
    let os = linux_installed();
    let mut ctx = linux();
    ctx.exe = PathBuf::from("/usr/local/bin/flightdeck");
    assert!(query(&os, &ctx).other_binary);
    assert!(!query(&linux_installed(), &linux()).other_binary);
}

#[test]
fn a_systemd_unit_with_lingering() {
    let os = linux_installed();
    os.says(
        SHOW,
        "LoadState=loaded\nUnitFileState=enabled\nActiveState=active\n",
    );
    os.says(LINGER, "Linger=yes\n");
    let r = query(&os, &linux());
    assert!(r.installed && r.enabled && r.loaded && r.active);
    assert_eq!(r.linger, Some(true));
    assert_eq!(r.error, None);
    assert_eq!(os.ran(), vec![SHOW, LINGER]);
}

#[test]
fn systemd_without_a_bus_is_reported() {
    let os = linux_installed();
    os.fails(SHOW, "Failed to connect to bus: No medium found");
    os.says(LINGER, "Linger=no\n");
    let r = query(&os, &linux());
    assert!(r.installed && !r.enabled && !r.active);
    assert_eq!(
        r.error.as_deref(),
        Some("systemctl --user: Failed to connect to bus: No medium found")
    );
    assert_eq!(r.linger, Some(false));
}

#[test]
fn lingering_is_unknown_without_a_user_or_loginctl() {
    let os = linux_installed();
    os.fails(LINGER, "loginctl: command not found");
    assert_eq!(query(&os, &linux()).linger, None);
    let os = linux_installed();
    let mut ctx = linux();
    ctx.user = None;
    assert_eq!(query(&os, &ctx).linger, None);
    assert!(!os.ran().iter().any(|c| c.starts_with("loginctl")));
}

// ---------------------------------------------------------------------------
// enable
// ---------------------------------------------------------------------------

const ENABLE: &str = "launchctl enable gui/501/agency.neworange.flightdeck.worker";
const BOOTOUT: &str = "launchctl bootout gui/501/agency.neworange.flightdeck.worker";
const BOOTSTRAP: &str = "launchctl bootstrap gui/501 /Users/u/Library/LaunchAgents/agency.neworange.flightdeck.worker.plist";

#[test]
fn enable_on_macos_writes_loads_and_starts() {
    let os = FakeOs::default();
    let ctx = mac();
    let s = ctx.spec(&env());
    let (result, out, err) = capture(|o, e| enable(&os, &ctx, &s, false, false, o, e));
    assert_eq!(result, Ok(Enabled { started: true }), "{err}");
    assert_eq!(os.file(Path::new(MAC_PLIST)), Some(launchd_plist(&s)));
    assert_eq!(os.ran(), vec![ENABLE, BOOTOUT, BOOTSTRAP]);
    assert!(out.starts_with(&format!(
        "Start at login is on: launchd agent {MAC_PLIST}.\n  Runs: /opt/homebrew/bin/flightdeck worker supervise\n  PATH: /opt/homebrew/bin:"
    )), "{out}");
    assert!(err.is_empty());
}

#[test]
fn enable_twice_rewrites_the_same_unit() {
    let os = FakeOs::default();
    let ctx = mac();
    let s = ctx.spec(&env());
    let _ = capture(|o, e| enable(&os, &ctx, &s, false, false, o, e));
    let first = os.file(Path::new(MAC_PLIST));
    let (result, _, _) = capture(|o, e| enable(&os, &ctx, &s, false, false, o, e));
    assert_eq!(result, Ok(Enabled { started: true }));
    assert_eq!(os.file(Path::new(MAC_PLIST)), first);
    assert_eq!(os.ran().len(), 6, "the same three steps again");
}

#[test]
fn enable_on_macos_leaves_a_running_worker_alone() {
    let os = FakeOs::default();
    let ctx = mac();
    let s = ctx.spec(&env());
    let (result, out, _) = capture(|o, e| enable(&os, &ctx, &s, true, false, o, e));
    assert_eq!(result, Ok(Enabled { started: false }));
    assert!(
        os.file(Path::new(MAC_PLIST)).is_some(),
        "the plist is updated"
    );
    assert_eq!(os.ran(), vec![ENABLE], "nothing loaded or started");
    assert!(out.contains("is left alone: run `flightdeck worker restart`"), "{out}");
}

#[test]
fn enable_on_macos_without_a_gui_session_still_installs() {
    let os = FakeOs::default();
    os.fails(
        BOOTSTRAP,
        "Bootstrap failed: 125: Domain does not support specified action",
    );
    let ctx = mac();
    let s = ctx.spec(&env());
    let (result, out, err) = capture(|o, e| enable(&os, &ctx, &s, false, false, o, e));
    assert_eq!(result, Ok(Enabled { started: false }), "{err}");
    assert_eq!(os.pauses.get(), BOOTSTRAP_ATTEMPTS - 1, "retried");
    assert_eq!(
        os.ran().iter().filter(|c| *c == BOOTSTRAP).count() as u32,
        BOOTSTRAP_ATTEMPTS
    );
    assert!(os.file(Path::new(MAC_PLIST)).is_some());
    assert!(out.contains("It loads at the next login"), "{out}");
    assert!(
        out.contains("Domain does not support specified action"),
        "{out}"
    );
}

#[test]
fn a_bootstrap_that_succeeds_on_retry_starts_the_worker() {
    let os = FakeOs::default();
    os.fails(BOOTSTRAP, "Bootstrap failed: 5: Input/output error");
    os.says(BOOTSTRAP, "");
    let ctx = mac();
    let s = ctx.spec(&env());
    let (result, out, _) = capture(|o, e| enable(&os, &ctx, &s, false, false, o, e));
    assert_eq!(result, Ok(Enabled { started: true }));
    assert_eq!(os.pauses.get(), 1);
    assert!(!out.contains("Input/output error"), "{out}");
}

#[test]
fn a_dry_run_changes_nothing_and_prints_the_file_and_commands() {
    let os = FakeOs::default();
    let ctx = mac();
    let s = ctx.spec(&env());
    let (result, out, _) = capture(|o, e| enable(&os, &ctx, &s, false, true, o, e));
    assert_eq!(result, Ok(Enabled { started: false }));
    assert!(os.touched.borrow().is_empty());
    assert!(os.ran().is_empty(), "{:?}", os.ran());
    assert!(
        out.starts_with(&format!("Would write {MAC_PLIST}:\n\n    <?xml")),
        "{out}"
    );
    assert!(out.contains("    \t<key>RunAtLoad</key>\n"), "{out}");
    assert!(out.contains(&format!("Would run:\n    {ENABLE}   # a failure here is expected and ignored\n    {BOOTOUT}   # a failure here is expected and ignored\n    {BOOTSTRAP}\n")), "{out}");
}

#[test]
fn enable_on_linux_with_lingering_enables_and_starts() {
    let os = FakeOs::default();
    os.says(LINGER, "Linger=yes\n");
    let ctx = linux();
    let s = ctx.spec(&env());
    let (result, out, err) = capture(|o, e| enable(&os, &ctx, &s, false, false, o, e));
    assert_eq!(result, Ok(Enabled { started: true }), "{err}");
    assert_eq!(os.file(Path::new(LINUX_UNIT)), Some(systemd_unit(&s)));
    assert_eq!(
        os.ran(),
        vec![
            LINGER,
            "systemctl --user daemon-reload",
            "systemctl --user enable flightdeck-worker.service",
            "systemctl --user start flightdeck-worker.service",
        ]
    );
    assert!(
        out.starts_with("Start on boot is on: systemd user unit "),
        "{out}"
    );
    assert!(!out.contains("linger"), "{out}");
}

#[test]
fn enable_on_linux_without_lingering_says_how_and_does_not_start() {
    let os = FakeOs::default();
    os.says(LINGER, "Linger=no\n");
    let ctx = linux();
    let s = ctx.spec(&env());
    let (result, out, _) = capture(|o, e| enable(&os, &ctx, &s, false, false, o, e));
    assert_eq!(result, Ok(Enabled { started: false }));
    assert!(
        !os.ran().iter().any(|c| c.contains(" start ")),
        "{:?}",
        os.ran()
    );
    assert!(out.contains("Lingering is off for your account"), "{out}");
    assert!(
        out.contains("\n    sudo loginctl enable-linger u\n"),
        "{out}"
    );
    assert!(out.contains("Start the worker now with `flightdeck worker start`."));
    assert!(
        !os.ran().iter().any(|c| c.contains("enable-linger")),
        "never runs sudo or enable-linger itself"
    );
}

#[test]
fn enable_on_linux_with_unknown_lingering_warns() {
    let os = FakeOs::default();
    os.fails(LINGER, "Failed to get user: No such user");
    let ctx = linux();
    let s = ctx.spec(&env());
    let (result, out, _) = capture(|o, e| enable(&os, &ctx, &s, false, false, o, e));
    assert_eq!(result, Ok(Enabled { started: false }));
    assert!(
        out.contains("Could not tell whether lingering is on"),
        "{out}"
    );
}

#[test]
fn enable_on_linux_leaves_a_running_worker_alone() {
    let os = FakeOs::default();
    os.says(LINGER, "Linger=yes\n");
    let ctx = linux();
    let s = ctx.spec(&env());
    let (result, out, _) = capture(|o, e| enable(&os, &ctx, &s, true, false, o, e));
    assert_eq!(result, Ok(Enabled { started: false }));
    assert!(!os.ran().iter().any(|c| c.contains(" start ")));
    assert!(out.contains("restart it under systemd"), "{out}");
}

#[test]
fn enable_on_linux_fails_clearly_without_a_user_bus() {
    let os = FakeOs::default();
    os.says(LINGER, "Linger=yes\n");
    os.fails(
        "systemctl --user daemon-reload",
        "Failed to connect to bus: No medium found",
    );
    let ctx = linux();
    let s = ctx.spec(&env());
    let (result, _, err) = capture(|o, e| enable(&os, &ctx, &s, false, false, o, e));
    assert_eq!(result, Err(1));
    assert!(err.contains("Failed to connect to bus"), "{err}");
    assert!(err.contains("The unit is written"), "{err}");
    assert!(
        !os.ran().iter().any(|c| c.contains(" enable ")),
        "stopped there"
    );
}

#[test]
fn a_dry_run_on_linux_only_asks() {
    let os = FakeOs::default();
    os.says(LINGER, "Linger=yes\n");
    let ctx = linux();
    let s = ctx.spec(&env());
    let (_, out, _) = capture(|o, e| enable(&os, &ctx, &s, false, true, o, e));
    assert_eq!(os.ran(), vec![LINGER], "a read-only question");
    assert!(os.touched.borrow().is_empty());
    assert!(
        out.contains("    systemctl --user start flightdeck-worker.service\n"),
        "{out}"
    );
    assert!(out.contains("    [Service]\n"), "{out}");
}

// ---------------------------------------------------------------------------
// disable
// ---------------------------------------------------------------------------

#[test]
fn disable_on_macos_unloads_and_removes() {
    let os = mac_installed();
    let ctx = mac();
    let report = query(&os, &ctx);
    os.ran.borrow_mut().clear();
    let (result, out, _) = capture(|o, e| disable(&os, &ctx, &report, false, o, e));
    assert_eq!(result, Ok(()));
    assert_eq!(os.ran(), vec![BOOTOUT]);
    assert_eq!(os.file(Path::new(MAC_PLIST)), None);
    assert_eq!(
        out,
        format!("Start at login is off: removed {MAC_PLIST}.\n")
    );
}

#[test]
fn disable_on_linux_disables_removes_and_reloads() {
    let os = linux_installed();
    let ctx = linux();
    let report = query(&os, &ctx);
    os.ran.borrow_mut().clear();
    let (result, _, _) = capture(|o, e| disable(&os, &ctx, &report, false, o, e));
    assert_eq!(result, Ok(()));
    assert_eq!(
        os.ran(),
        vec![
            "systemctl --user disable flightdeck-worker.service",
            "systemctl --user daemon-reload",
            "systemctl --user reset-failed flightdeck-worker.service",
        ]
    );
    assert_eq!(os.file(Path::new(LINUX_UNIT)), None);
}

#[test]
fn disable_with_nothing_set_up_does_nothing() {
    let os = FakeOs::default();
    let ctx = linux();
    let report = query(&os, &ctx);
    let (result, out, _) = capture(|o, e| disable(&os, &ctx, &report, false, o, e));
    assert_eq!(result, Ok(()));
    assert!(os.ran().is_empty());
    assert!(os.touched.borrow().is_empty());
    assert_eq!(out, "Start on boot is not set up.\n");
}

#[test]
fn a_dry_run_disable_changes_nothing() {
    let os = mac_installed();
    let ctx = mac();
    let report = query(&os, &ctx);
    os.ran.borrow_mut().clear();
    let (_, out, _) = capture(|o, e| disable(&os, &ctx, &report, true, o, e));
    assert!(os.ran().is_empty());
    assert!(os.file(Path::new(MAC_PLIST)).is_some());
    assert!(out.contains(&format!("Would remove {MAC_PLIST}")), "{out}");
    assert!(out.contains(BOOTOUT), "{out}");
}

// ---------------------------------------------------------------------------
// start
// ---------------------------------------------------------------------------

#[test]
fn start_without_a_unit_is_not_installed() {
    for ctx in [mac(), linux()] {
        let os = FakeOs::default();
        let report = query(&os, &ctx);
        assert_eq!(start(&os, &ctx, &report), ServiceStart::NotInstalled);
        assert!(os.ran().is_empty());
    }
}

#[test]
fn start_on_macos_reloads_the_plist() {
    let os = mac_installed();
    os.says(PRINT, "\tstate = not running\n");
    let ctx = mac();
    let report = query(&os, &ctx);
    os.ran.borrow_mut().clear();
    assert_eq!(start(&os, &ctx, &report), ServiceStart::Started);
    assert_eq!(os.ran(), vec![BOOTOUT, BOOTSTRAP]);
}

#[test]
fn start_on_macos_bootstraps_an_unloaded_agent() {
    let os = mac_installed();
    os.fails(PRINT, "Could not find service");
    let ctx = mac();
    let report = query(&os, &ctx);
    os.ran.borrow_mut().clear();
    assert_eq!(start(&os, &ctx, &report), ServiceStart::Started);
    assert_eq!(
        os.ran(),
        vec![BOOTSTRAP],
        "no bootout of what is not loaded"
    );
}

#[test]
fn start_on_macos_without_a_gui_session_falls_back() {
    let os = mac_installed();
    os.fails(PRINT, "Could not find service");
    os.fails(
        BOOTSTRAP,
        "Bootstrap failed: 125: Domain does not support specified action",
    );
    let ctx = mac();
    let report = query(&os, &ctx);
    match start(&os, &ctx, &report) {
        ServiceStart::Failed(why) => assert!(why.contains("no GUI session"), "{why}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn start_on_macos_respects_launchctl_disable() {
    let os = mac_installed();
    os.says(
        PRINT_DISABLED,
        &format!("\t\"{LAUNCHD_LABEL}\" => disabled\n"),
    );
    let ctx = mac();
    let report = query(&os, &ctx);
    os.ran.borrow_mut().clear();
    assert!(matches!(
        start(&os, &ctx, &report),
        ServiceStart::Skipped(_)
    ));
    assert!(os.ran().is_empty());
}

#[test]
fn start_on_linux_needs_lingering() {
    let os = linux_installed();
    os.says(
        SHOW,
        "LoadState=loaded\nUnitFileState=enabled\nActiveState=inactive\n",
    );
    os.says(LINGER, "Linger=no\n");
    let ctx = linux();
    let report = query(&os, &ctx);
    os.ran.borrow_mut().clear();
    match start(&os, &ctx, &report) {
        ServiceStart::Skipped(why) => {
            assert!(why.contains("sudo loginctl enable-linger u"), "{why}")
        }
        other => panic!("{other:?}"),
    }
    assert!(os.ran().is_empty(), "a detached worker survives logout");
}

#[test]
fn start_on_linux_with_lingering_uses_systemd() {
    let os = linux_installed();
    os.says(
        SHOW,
        "LoadState=loaded\nUnitFileState=enabled\nActiveState=failed\n",
    );
    os.says(LINGER, "Linger=yes\n");
    let ctx = linux();
    let report = query(&os, &ctx);
    os.ran.borrow_mut().clear();
    assert_eq!(start(&os, &ctx, &report), ServiceStart::Started);
    assert_eq!(
        os.ran(),
        vec!["systemctl --user start flightdeck-worker.service"]
    );
}

#[test]
fn start_on_linux_skips_a_disabled_unit_or_a_missing_bus() {
    let os = linux_installed();
    os.says(
        SHOW,
        "LoadState=loaded\nUnitFileState=disabled\nActiveState=inactive\n",
    );
    os.says(LINGER, "Linger=yes\n");
    let report = query(&os, &linux());
    assert!(matches!(
        start(&os, &linux(), &report),
        ServiceStart::Skipped(_)
    ));

    let os = linux_installed();
    os.fails(SHOW, "Failed to connect to bus");
    os.says(LINGER, "Linger=yes\n");
    let report = query(&os, &linux());
    assert!(matches!(
        start(&os, &linux(), &report),
        ServiceStart::Skipped(_)
    ));
}

// ---------------------------------------------------------------------------
// The status lines
// ---------------------------------------------------------------------------

fn report(manager: Manager) -> BootReport {
    BootReport {
        manager,
        unit: "/x".to_string(),
        installed: true,
        enabled: true,
        loaded: true,
        active: true,
        linger: (manager == Manager::Systemd).then_some(true),
        other_binary: false,
        error: None,
    }
}

#[test]
fn status_lines() {
    let lines = |r: &BootReport| render_status(r, Some("u")).join("\n");
    let mut r = report(Manager::Launchd);
    assert_eq!(lines(&r), "Start at login: on (launchd agent, loaded)");
    r.loaded = false;
    assert_eq!(
        lines(&r),
        "Start at login: on (launchd agent; it loads at your next login)"
    );
    r.enabled = false;
    assert!(lines(&r).contains("disabled with `launchctl disable`"));
    r.installed = false;
    assert_eq!(
        lines(&r),
        "Start at login: off (`flightdeck worker enable` sets it up)"
    );

    let mut r = report(Manager::Systemd);
    assert_eq!(
        lines(&r),
        "Start on boot: on (systemd user unit, lingering on)"
    );
    r.linger = Some(false);
    assert_eq!(
        lines(&r),
        "Start on boot: only after you log in: lingering is off (run `sudo loginctl enable-linger u`)"
    );
    assert!(render_status(&r, None)[0].contains("enable-linger $USER"));
    r.enabled = false;
    assert!(lines(&r).contains("not enabled"));
    r.error = Some("systemctl --user: no bus".to_string());
    assert_eq!(
        lines(&r),
        "Start on boot: installed (systemd user unit), but systemd could not be asked\n  systemctl --user: no bus"
    );
    r.installed = false;
    assert_eq!(
        lines(&r),
        "Start on boot: off (`flightdeck worker enable` sets it up)"
    );
}

#[test]
fn status_flags_another_binary() {
    let mut r = report(Manager::Launchd);
    r.other_binary = true;
    let lines = render_status(&r, None);
    assert_eq!(lines.len(), 2);
    assert!(lines[1].contains("another flightdeck binary"), "{lines:?}");
}
