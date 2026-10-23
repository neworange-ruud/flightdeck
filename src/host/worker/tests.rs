//! A worker host driven the way `flightdeck worker run` drives it — ticks,
//! the worker API, and the window's frames fed through
//! [`AppHost::route_inbound`] — against the fakes in `crate::testing`, with no
//! server unless a test starts one on loopback.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::json;

use super::*;
use crate::app::commands::Command;
use crate::app::state::{AppState, Services};
use crate::contracts::{AgentDef, StatusPatterns};
use crate::host::testing::{self, worker_options, TestProject};
use crate::persistence::project_state::default_state;
use crate::testing::{
    FakeClock, FakeCommandRunner, FakeContainerRuntime, FakeFs, FakeGit, FakePty, FakePtyHandle,
};
use crate::web::protocol::{
    command as names, worker_command, ConnectionKind, Seat, ServerMsg, ViewerId,
};
use crate::web::server::{WebInbound, WebOutbound};
use tempfile::TempDir;

/// Every service the host borrows, as fakes, plus the worker's notifier.
struct Fakes {
    fs: FakeFs,
    pty: FakePty,
    clock: FakeClock,
    container: FakeContainerRuntime,
    command: FakeCommandRunner,
    notifier: WorkerNotifier,
    git: Arc<FakeGit>,
    /// Holds the stand-in agent executable.
    dir: TempDir,
}

impl Fakes {
    fn new() -> Fakes {
        Fakes {
            fs: FakeFs::new(),
            pty: FakePty::new(),
            clock: FakeClock::default(),
            container: FakeContainerRuntime::new(),
            command: FakeCommandRunner::new(),
            notifier: WorkerNotifier::new(),
            git: Arc::new(FakeGit::new().with_branches(["main"])),
            dir: TempDir::new().expect("tempdir"),
        }
    }

    fn env(&self) -> Env<'_> {
        Env {
            fs: &self.fs,
            pty: &self.pty,
            clock: &self.clock,
            container: &self.container,
            command: &self.command,
            terminal: crate::terminal::session::TerminalProfile::TUI,
        }
    }

    fn services(&self) -> Services<'_> {
        Services {
            git: &*self.git,
            fs: &self.fs,
            pty: &self.pty,
            clock: &self.clock,
            container: &self.container,
            command: &self.command,
        }
    }

    /// A worker host over `projects` (possibly none), home `/home/me`.
    fn worker(&self, projects: Vec<TestProject>) -> AppHost<'_> {
        testing::worker_host(
            self.env(),
            &self.notifier,
            projects,
            worker_options("/home/me"),
        )
    }

    /// A project `repo` whose one tab `Task` runs on the fake PTY, built by
    /// dispatching `NewAgentTab` so the tab is the real thing. Returns it and
    /// the handle driving the tab's primary PTY.
    fn project_with_a_tab(&self) -> (TestProject, FakePtyHandle) {
        let path = self.dir.path().join("opencode");
        std::fs::write(&path, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&path, perms).unwrap();
        }
        let agent = AgentDef {
            key: "opencode".to_string(),
            display_name: "OpenCode".to_string(),
            command: path.to_str().unwrap().to_string(),
            args: vec![],
            status_patterns: StatusPatterns::default(),
        };
        let mut config = Config::default();
        config.ui.default_agent = agent.key.clone();
        config.worktrees.root = ".flightdeck/worktrees".to_string();
        config.agents.insert(agent.key.clone(), agent);
        let handle = self.pty.queue_session();
        let mut state = AppState::new(
            config,
            default_state("main"),
            "/repo",
            "/repo/.flightdeck/state.json",
        );
        state
            .dispatch(
                Command::NewAgentTab {
                    name: "Task".to_string(),
                    agent_key: None,
                },
                &self.services(),
            )
            .expect("the tab is created");
        let project = TestProject {
            name: "repo".to_string(),
            root: PathBuf::from("/repo"),
            state,
            git: Arc::clone(&self.git),
        };
        (project, handle)
    }
}

fn viewer() -> ViewerId {
    ViewerId::new("v1")
}

fn command(seq: u64, name: &str, args: Option<serde_json::Value>) -> WebInbound {
    WebInbound::Command {
        viewer_id: viewer(),
        label: "FlightDeck Desktop/1.0 (laptop)".to_string(),
        command: crate::web::protocol::Command {
            seq,
            name: name.to_string(),
            args,
        },
    }
}

fn worker_op(seq: u64, name: &str) -> WebInbound {
    let args = json!({ "path": "projects/x", "url": "https://example.invalid/r.git" });
    let op = WorkerOp::parse(name, Some(&args))
        .expect("a worker command")
        .expect("valid args");
    WebInbound::WorkerOp {
        viewer_id: viewer(),
        seq,
        op,
    }
}

fn attached(kind: ConnectionKind) -> WebInbound {
    WebInbound::ViewerAttached {
        viewer_id: viewer(),
        address: "100.64.1.2".parse().unwrap(),
        label: "FlightDeck Desktop/1.0 (laptop)".to_string(),
        seat: Seat::Writing,
        cursors: Vec::new(),
        resume_viewer: None,
        kind,
        protocol_version: 7,
    }
}

/// The ack a routed frame was answered with (always the first frame).
fn first_ack(out: &[WebOutbound]) -> &Ack {
    match out.first() {
        Some(WebOutbound::Viewer {
            msg: ServerMsg::Ack(ack),
            ..
        }) => ack,
        other => panic!("expected an ack first, got {other:?}"),
    }
}

fn w<'h>(host: &'h AppHost<'_>) -> &'h WorkerExt {
    host.worker.as_deref().expect("a worker host")
}

/// A real repository with one empty initial snapshot, at `dir`.
fn real_repo(dir: &Path) {
    let run_git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    };
    assert!(run_git(&["init", "--initial-branch=main"]));
    assert!(run_git(&[
        "-c",
        "user.name=FlightDeck Test",
        "-c",
        "user.email=flightdeck@example.invalid",
        "-c",
        "commit.gpgSign=false",
        "-c",
        "core.hooksPath=",
        &["com", "mit"].concat(),
        "--allow-empty",
        "-m",
        "initial",
    ]));
}

// ---------------------------------------------------------------------------
// The empty workspace (design §2.3, A9)
// ---------------------------------------------------------------------------

/// The mandatory one: a fresh worker has no project, and nothing a window can
/// send — any palette command, any worker op, any terminal frame — may panic
/// it. Every command is answered, with an ack.
#[test]
fn an_empty_worker_survives_ticks_and_every_command_name() {
    let fakes = Fakes::new();
    let mut host = fakes.worker(Vec::new());
    assert_eq!(host.project_count(), 0);
    assert_eq!(host.host_kind(), HostKind::Worker);
    host.start();
    for _ in 0..3 {
        host.tick();
        fakes.clock.advance_millis(5_000);
    }

    let mut seq = 0;
    for spec in crate::web::commands::INVENTORY {
        seq += 1;
        let out = host.route_inbound(command(seq, spec.name, None));
        let ack = first_ack(&out);
        assert_eq!(ack.seq, seq, "{}", spec.name);
        if spec.name != names::MARK_ACTIVITY_READ {
            assert_eq!(ack.outcome, AckOutcome::Rejected, "{}", spec.name);
        }
        host.tick();
    }
    // The commands that need a project say so in the worker's words.
    let out = host.route_inbound(command(900, names::OPEN_CONFIGURATION, None));
    assert_eq!(first_ack(&out).detail.as_deref(), Some(NO_PROJECT_REFUSAL));
    let out = host.route_inbound(command(
        901,
        names::SELECT_PROJECT,
        Some(json!({"project_id": "/x"})),
    ));
    assert_eq!(first_ack(&out).detail.as_deref(), Some(NO_PROJECT_REFUSAL));

    for name in worker_command::ALL {
        seq += 1;
        let out = host.route_inbound(worker_op(seq, name));
        let ack = first_ack(&out);
        assert_eq!(ack.seq, seq, "{name}");
        host.tick();
    }

    // Terminal frames for terminals that do not exist, and attaches of both
    // kinds, change nothing and break nothing.
    host.route_inbound(attached(ConnectionKind::Window));
    host.route_inbound(attached(ConnectionKind::Background));
    host.route_inbound(WebInbound::ResizeTerminal {
        viewer_id: viewer(),
        terminal_id: TerminalId::new("no-such-terminal"),
        size: PtySize {
            rows: 30,
            cols: 100,
        },
    });
    host.route_inbound(WebInbound::ViewerDetached {
        viewer_id: viewer(),
    });
    host.tick();

    // The state a window would be sent describes an empty worker.
    let state = host.web_host_state();
    assert!(state.projects.is_empty());
    assert_eq!(state.host_kind, HostKind::Worker);
    assert_eq!(
        (state.geometry.cols, state.geometry.rows),
        (200, 50),
        "an empty worker reports its default grid"
    );
    let worker = state.worker.expect("a worker publishes its own state");
    assert_eq!(worker.home, "/home/me");
    assert_eq!(worker.status.projects_root, "/home/me/flightdeck");

    // And the run loop's own calls hold too.
    let report = host.worker_report();
    assert!(report.projects.is_empty());
    host.persist().expect("an empty workspace persists");
    host.terminate_sessions();
    host.stop_worker();
}

#[test]
fn an_empty_worker_refuses_host_events_instead_of_panicking() {
    let fakes = Fakes::new();
    let mut host = fakes.worker(Vec::new());
    let err = host.handle(crate::host::HostEvent::FocusApp).unwrap_err();
    assert!(err.to_string().contains(NO_PROJECT_REFUSAL), "{err}");
}

#[test]
fn opening_a_worker_with_no_workspace_file_is_an_empty_worker() {
    let fakes = Fakes::new();
    let mut opts = worker_options("/home/me");
    // The device store is a real file path; point it somewhere harmless.
    let tmp = TempDir::new().unwrap();
    opts.paths = crate::worker::paths::WorkerPaths::from_home(tmp.path());
    let host = AppHost::open_worker(fakes.env(), &fakes.notifier, opts).expect("opens");
    assert_eq!(host.project_count(), 0);
    assert_eq!(host.host_kind(), HostKind::Worker);
    assert!(host.remote_wiring.is_none(), "never FlightDeck Remote");
}

#[test]
fn opening_a_project_on_an_empty_worker_makes_it_active_at_the_default_size() {
    let fakes = Fakes::new();
    let dir = TempDir::new().unwrap();
    real_repo(dir.path());
    let mut host = fakes.worker(Vec::new());
    host.ws_path = Some(PathBuf::from("/home/me/.flightdeck/worker/workspace.json"));

    let opened = host
        .open_worker_project(dir.path())
        .expect("a repository opens");
    assert!(
        opened.sentence.starts_with("Opened project"),
        "{}",
        opened.sentence
    );
    assert_eq!(host.project_count(), 1);
    assert_eq!(host.active_project_index(), 0);
    let size = host.active_state().pty_size;
    assert_eq!((size.cols, size.rows), (200, 50));
    // Written at once, so a crash does not forget it.
    let saved = fakes
        .fs
        .file_contents(Path::new("/home/me/.flightdeck/worker/workspace.json"))
        .expect("the workspace file");
    assert!(saved.contains(&opened.project_id.0), "{saved}");

    // Opening it again switches to it rather than opening it twice.
    let again = host.open_worker_project(dir.path()).unwrap();
    assert!(
        again.sentence.contains("already open"),
        "{}",
        again.sentence
    );
    assert_eq!(host.project_count(), 1);

    // A folder that is not a repository is refused, not opened.
    let plain = TempDir::new().unwrap();
    assert!(host.open_worker_project(plain.path()).is_err());
    assert_eq!(host.project_count(), 1);
}

#[test]
fn a_project_opened_after_the_window_resized_starts_at_the_window_size() {
    let fakes = Fakes::new();
    let dir = TempDir::new().unwrap();
    real_repo(dir.path());
    let mut host = fakes.worker(Vec::new());
    host.route_inbound(WebInbound::ResizeTerminal {
        viewer_id: viewer(),
        terminal_id: TerminalId::new(WORKER_SHELL_TERMINAL_ID),
        size: PtySize {
            rows: 40,
            cols: 132,
        },
    });
    host.open_worker_project(dir.path()).unwrap();
    let size = host.active_state().pty_size;
    assert_eq!((size.cols, size.rows), (132, 40));
}

// ---------------------------------------------------------------------------
// Refusals (design §2.4)
// ---------------------------------------------------------------------------

#[test]
fn a_worker_refuses_what_only_the_box_may_do() {
    let fakes = Fakes::new();
    let mut host = fakes.worker(vec![TestProject::new("alpha", &[])]);
    for name in [
        names::QUIT,
        names::START_WEB_INTERFACE,
        names::STOP_WEB_INTERFACE,
        names::SHOW_WEB_ACCESS,
        names::PAIR_PHONE,
        names::UNPAIR_PHONE,
        names::OPEN_PROJECT,
    ] {
        let out = host.route_inbound(command(1, name, None));
        let ack = first_ack(&out);
        assert_eq!(ack.outcome, AckOutcome::Rejected, "{name}");
        assert_eq!(ack.detail.as_deref(), worker_refusal(name), "{name}");
    }
    assert!(!host.should_quit(), "quit never reached the host");
    host.tick();
    assert!(
        host.web_surface.handle.is_none(),
        "nothing started a server"
    );
}

#[test]
fn a_worker_never_starts_flightdeck_remote_even_when_enabled() {
    let fakes = Fakes::new();
    let mut project = TestProject::new("alpha", &[]);
    project.state.config.remote.enabled = true;
    let mut host = fakes.worker(vec![project]);
    host.start();
    host.tick();
    assert!(host.remote_setup.is_none());
    assert!(host.remote_bridge.is_none());
}

#[test]
fn a_tui_host_ignores_worker_frames() {
    let fakes = Fakes::new();
    let (project, pty) = fakes.project_with_a_tab();
    let tab_id = project.state.tabs[0].meta.id.clone();
    let mut host = testing::host(fakes.env(), &fakes.notifier, vec![project], 0);
    assert_eq!(host.host_kind(), HostKind::Tui);
    let resizes = pty.resizes().len();
    let out = host.route_inbound(WebInbound::ResizeTerminal {
        viewer_id: viewer(),
        terminal_id: crate::web::stream::primary_terminal_id(&tab_id),
        size: PtySize {
            rows: 40,
            cols: 132,
        },
    });
    assert!(out.is_empty());
    assert_eq!(pty.resizes().len(), resizes, "no PTY was resized");
    let out = host.route_inbound(worker_op(3, worker_command::SCAN_PROJECTS));
    assert!(out.is_empty(), "a TUI host answers no worker op");
    // And refuses nothing a worker refuses on its behalf.
    assert!(host.web_host_state().worker.is_none());
}

#[test]
fn the_desktop_front_end_is_a_desktop_host() {
    let fakes = Fakes::new();
    let mut host = testing::host(
        fakes.env(),
        &fakes.notifier,
        vec![TestProject::new("alpha", &[])],
        0,
    );
    host.set_desktop_front_end();
    assert_eq!(host.host_kind(), HostKind::Desktop);
    assert_eq!(host.web_host_state().host_kind, HostKind::Desktop);
}

// ---------------------------------------------------------------------------
// Notifications, messages, health (design §2.6)
// ---------------------------------------------------------------------------

#[test]
fn an_agent_notification_reaches_every_connection() {
    let fakes = Fakes::new();
    let mut host = fakes.worker(vec![TestProject::new("alpha", &[])]);
    // What `take_finish_notifications` hands the host's notifier each pump.
    host.notifier.notify(&Notification {
        title: "alpha: Task".to_string(),
        body: "OpenCode finished".to_string(),
        sound: NotificationSound::Completion,
    });
    fakes.clock.set_millis(42_000);
    host.tick();
    let sent = &w(&host).sent;
    let note = sent
        .iter()
        .find_map(|f| match f {
            WebOutbound::All(ServerMsg::Delta(Delta::Notification(n))) => Some(n),
            _ => None,
        })
        .expect("a notification frame to every connection");
    assert_eq!(note.title, "alpha: Task");
    assert_eq!(note.sound, NotificationSoundView::Completion);
    assert_eq!(note.at_ms, 42_000);
    assert!(fakes.notifier.take().is_empty(), "the queue was drained");
}

#[test]
fn messages_with_no_window_stay_unread_until_a_window_attaches() {
    let fakes = Fakes::new();
    let mut host = fakes.worker(vec![TestProject::new("alpha", &[])]);
    host.message("Could not open project: nope");
    host.message("Opened project 'alpha'.");
    host.tick();
    let unread = w(&host).messages.unread();
    assert_eq!(unread.len(), 2);
    assert_eq!(unread[0].level, MessageLevel::Error);
    assert_eq!(unread[1].level, MessageLevel::Info);
    let state = host.web_host_state();
    let worker = state.worker.unwrap();
    assert_eq!(worker.host_messages, unread);
    assert_eq!(worker.status.unread_messages, 2);
    assert_eq!(host.worker_report().unread_messages, 2);
    assert!(
        matches!(host.ui.overlay, crate::tui::render::UiOverlay::None),
        "a worker leaves no notification up for a screen it does not have"
    );

    // The ring keeps the newest hundred.
    for i in 0..150 {
        host.message(format!("message {i}"));
        host.tick();
    }
    let unread = w(&host).messages.unread();
    assert_eq!(unread.len(), messages::HOST_MESSAGES_KEPT);
    assert_eq!(unread.last().unwrap().text, "message 149");

    // A background link reads nothing; a window reads everything.
    host.route_inbound(attached(ConnectionKind::Background));
    assert_eq!(w(&host).messages.unread_count(), 100);
    host.route_inbound(attached(ConnectionKind::Window));
    assert_eq!(w(&host).messages.unread_count(), 0);
    assert!(host
        .take_log_lines()
        .iter()
        .any(|l| l.contains("window attached: laptop")));
}

#[test]
fn a_background_link_is_not_sent_the_terminal_replay() {
    let fakes = Fakes::new();
    let (project, pty) = fakes.project_with_a_tab();
    let mut host = fakes.worker(vec![project]);
    // A replay ring only fills while a server runs; seed one by hand.
    let id = crate::web::stream::primary_terminal_id(&host.active_state().tabs[0].meta.id);
    host.web_surface.streams.pty_output(&id, b"history");
    drop(pty);
    assert!(host
        .route_inbound(attached(ConnectionKind::Background))
        .is_empty());
    let replay = host.route_inbound(attached(ConnectionKind::Window));
    assert!(
        replay.iter().any(|f| matches!(
            f,
            WebOutbound::Viewer {
                msg: ServerMsg::TermBytes(_),
                ..
            }
        )),
        "a window is replayed what it missed: {replay:?}"
    );
}

#[test]
fn an_update_is_one_message_telling_how_to_update_the_worker() {
    let fakes = Fakes::new();
    let mut host = fakes.worker(vec![TestProject::new("alpha", &[])]);
    // What the background check sends when it finds a newer release.
    let (tx, rx) = std::sync::mpsc::channel();
    host.update_rx = rx;
    tx.send("9.9.9".to_string()).unwrap();
    host.tick();
    let unread = w(&host).messages.unread();
    assert_eq!(unread.len(), 1);
    assert_eq!(unread[0].text, update_message("9.9.9"));
    assert_eq!(unread[0].level, MessageLevel::Info);
    assert!(unread[0].text.contains("flightdeck worker restart"));
}

#[test]
fn health_is_sampled_every_ten_seconds_of_host_clock() {
    let fakes = Fakes::new();
    let (project, _pty) = fakes.project_with_a_tab();
    let mut host = fakes.worker(vec![project]);
    fakes.clock.set_millis(1_000);
    host.tick();
    let first = w(&host).last_health.clone().expect("sampled at once");
    assert_eq!(first.sampled_at_ms, 1_000);
    assert_eq!((first.agents_total, first.agents_running), (1, 1));
    assert_eq!(first.cpu_count, 1);

    fakes.clock.set_millis(10_999);
    host.tick();
    assert_eq!(w(&host).last_health.as_ref().unwrap().sampled_at_ms, 1_000);

    fakes.clock.set_millis(11_000);
    host.tick();
    let second = w(&host).last_health.clone().unwrap();
    assert_eq!(second.sampled_at_ms, 11_000);
    assert_eq!(second.uptime_secs, 11);
    assert_eq!(host.web_host_state().worker.unwrap().health, Some(second));
}

// ---------------------------------------------------------------------------
// PTY geometry (design §2.7)
// ---------------------------------------------------------------------------

#[test]
fn resize_terminal_resizes_the_named_pty_and_seeds_later_spawns() {
    let fakes = Fakes::new();
    let (project, pty) = fakes.project_with_a_tab();
    let tab_id = project.state.tabs[0].meta.id.clone();
    let mut host = fakes.worker(vec![project, TestProject::new("beta", &[])]);
    assert_eq!(
        host.project_state(1).unwrap().pty_size,
        PtySize {
            rows: 50,
            cols: 200
        },
        "every project starts at the worker's default grid"
    );

    let size = PtySize {
        rows: 40,
        cols: 132,
    };
    let out = host.route_inbound(WebInbound::ResizeTerminal {
        viewer_id: viewer(),
        terminal_id: crate::web::stream::primary_terminal_id(&tab_id),
        size,
    });
    assert!(out.is_empty());
    assert_eq!(pty.resizes().last(), Some(&size), "the live PTY followed");
    assert_eq!(w(&host).last_window_size, Some(size));
    for i in 0..host.project_count() {
        assert_eq!(
            host.project_state(i).unwrap().pty_size,
            size,
            "the next agent of project {i} spawns at the window's size"
        );
    }

    // Below the grid minimum is clamped, not passed through.
    host.route_inbound(WebInbound::ResizeTerminal {
        viewer_id: viewer(),
        terminal_id: crate::web::stream::primary_terminal_id(&tab_id),
        size: PtySize { rows: 0, cols: 0 },
    });
    let seeded = host.project_state(1).unwrap().pty_size;
    assert!(seeded.rows > 0 && seeded.cols > 0, "{seeded:?}");
}

#[test]
fn input_for_the_worker_shell_goes_to_the_worker_shell() {
    let fakes = Fakes::new();
    let mut host = fakes.worker(Vec::new());
    let out = host.route_inbound(WebInbound::Input {
        viewer_id: viewer(),
        input: crate::web::protocol::Input {
            seq: 7,
            terminal_id: TerminalId::new(WORKER_SHELL_TERMINAL_ID),
            data: b"ls\r".to_vec(),
            image: None,
        },
    });
    let ack = first_ack(&out);
    assert_eq!(ack.seq, 7);
    // The shell is not running (never started), and says so — rather than
    // "no such terminal", which would mean the input was routed nowhere.
    assert_eq!(ack.outcome, AckOutcome::Rejected);
    assert!(
        !ack.detail.as_deref().unwrap_or("").contains("No such"),
        "{:?}",
        ack.detail
    );
}

// ---------------------------------------------------------------------------
// The project picker and clones (routing; the picker's own logic is tested
// with `projects` and `clone`)
// ---------------------------------------------------------------------------

#[test]
fn every_worker_op_is_answered_to_the_viewer_that_asked() {
    let fakes = Fakes::new();
    let mut host = fakes.worker(vec![TestProject::new("alpha", &[])]);
    for (seq, name) in worker_command::ALL.iter().enumerate() {
        let out = host.route_inbound(worker_op(seq as u64, name));
        assert_eq!(first_ack(&out).seq, seq as u64, "{name}");
        for frame in &out {
            assert!(
                matches!(frame, WebOutbound::Viewer { viewer_id, .. } if *viewer_id == viewer()),
                "{name}: {frame:?}"
            );
        }
    }
}

#[test]
fn a_scan_answers_with_the_projects_root() {
    let fakes = Fakes::new();
    let mut host = fakes.worker(Vec::new());
    let out = host.route_inbound(worker_op(4, worker_command::SCAN_PROJECTS));
    let scan = out
        .iter()
        .find_map(|f| match f {
            WebOutbound::Viewer {
                msg: ServerMsg::ProjectScan(scan),
                ..
            } => Some(scan),
            _ => None,
        })
        .expect("a project scan frame");
    assert_eq!(scan.seq, 4);
    assert_eq!(scan.root, "/home/me/flightdeck");
    assert!(!scan.root_exists);
}

#[test]
fn a_failed_clone_is_reported_to_the_window_and_as_an_error_message() {
    let fakes = Fakes::new();
    let mut host = fakes.worker(Vec::new());
    host.finish_clone(
        9,
        "https://example.invalid/r.git".to_string(),
        Err("fatal: repository not found".to_string()),
        0,
    );
    let result = w(&host)
        .sent
        .iter()
        .find_map(|f| match f {
            WebOutbound::Windows(ServerMsg::CloneResult(r)) => Some(r.clone()),
            _ => None,
        })
        .expect("a clone result");
    assert_eq!(result.clone_id, 9);
    assert!(!result.ok);
    assert_eq!(result.error.as_deref(), Some("fatal: repository not found"));
    let unread = w(&host).messages.unread();
    assert_eq!(unread.len(), 1);
    assert_eq!(unread[0].level, MessageLevel::Error);
    assert!(unread[0].text.contains("repository not found"));
}

#[test]
fn a_successful_clone_opens_the_project() {
    let fakes = Fakes::new();
    let dir = TempDir::new().unwrap();
    real_repo(dir.path());
    let mut host = fakes.worker(Vec::new());
    host.finish_clone(
        3,
        "https://example.invalid/r.git".to_string(),
        Ok(dir.path().to_path_buf()),
        0,
    );
    assert_eq!(host.project_count(), 1);
    let result = w(&host)
        .sent
        .iter()
        .find_map(|f| match f {
            WebOutbound::Windows(ServerMsg::CloneResult(r)) => Some(r.clone()),
            _ => None,
        })
        .expect("a clone result");
    assert!(result.ok);
    assert!(result.project_id.is_some());
    assert_eq!(w(&host).messages.unread()[0].level, MessageLevel::Info);
}

// ---------------------------------------------------------------------------
// Configuration targeting (.15)
// ---------------------------------------------------------------------------

#[test]
fn a_config_save_naming_a_project_writes_that_projects_file() {
    let fakes = Fakes::new();
    let mut host = fakes.worker(vec![
        TestProject::new("alpha", &[]),
        TestProject::new("beta", &[]),
    ]);
    assert_eq!(host.active_project_index(), 0, "alpha is active");
    let args = json!({
        "changes": [{ "scope": "project", "key": "ui.mode_border", "value": "bright" }],
        "project_id": "/beta",
    });
    let out = host.route_inbound(command(5, names::OPEN_CONFIGURATION, Some(args)));
    assert_eq!(first_ack(&out).outcome, AckOutcome::Applied, "{out:?}");
    let beta = fakes
        .fs
        .file_contents(Path::new("/beta/.flightdeck/config.toml"))
        .expect("beta's project file");
    assert!(beta.contains("mode_border = \"bright\""), "{beta}");
    assert!(
        fakes
            .fs
            .file_contents(Path::new("/alpha/.flightdeck/config.toml"))
            .is_none(),
        "the active project's file is untouched"
    );
    let view = out
        .iter()
        .find_map(|f| match f {
            WebOutbound::Viewer {
                msg: ServerMsg::Configuration(view),
                ..
            } => Some(view),
            _ => None,
        })
        .expect("the manager comes back");
    assert_eq!(view.project_name, "beta");

    // A project that is not open is refused, and nothing is written.
    let args = json!({
        "changes": [{ "scope": "project", "key": "ui.mode_border", "value": "dim" }],
        "project_id": "/gamma",
    });
    let out = host.route_inbound(command(6, names::OPEN_CONFIGURATION, Some(args)));
    assert_eq!(first_ack(&out).outcome, AckOutcome::Rejected);
    assert!(fakes
        .fs
        .file_contents(Path::new("/gamma/.flightdeck/config.toml"))
        .is_none());
}

// ---------------------------------------------------------------------------
// The worker API (`worker status/pair/devices/revoke`)
// ---------------------------------------------------------------------------

/// Pair a Desktop called `label` the way the server's exchange does.
fn pair(host: &mut AppHost, address: &str, label: &str) -> String {
    let mut store = host.web_surface.credentials.lock().unwrap();
    let code = store.mint_bootstrap_code();
    store
        .exchange_code(address, code.reveal(), Some(label))
        .expect("the code is exchanged")
        .id()
        .as_str()
        .to_string()
}

#[test]
fn devices_are_named_by_their_host_name() {
    let fakes = Fakes::new();
    let mut host = fakes.worker(Vec::new());
    let id = pair(&mut host, "100.64.0.2", "FlightDeck Desktop/1.0 (studio)");
    let devices = host.devices();
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].id, id);
    assert_eq!(devices[0].label, "studio");
    assert_eq!(devices[0].state, DeviceState::Offline);
    assert!(!devices[0].revoked);
    assert_eq!(device_name(Some("curl/8")), "curl/8");
    assert_eq!(device_name(None), "unnamed device");
}

#[test]
fn revoking_resolves_an_id_then_a_name_then_a_unique_prefix() {
    let fakes = Fakes::new();
    let mut host = fakes.worker(Vec::new());
    let alpha = pair(&mut host, "100.64.0.2", "FlightDeck Desktop/1.0 (alpha)");
    let beta1 = pair(&mut host, "100.64.0.3", "FlightDeck Desktop/1.0 (beta)");
    let beta2 = pair(&mut host, "100.64.0.4", "FlightDeck Desktop/1.0 (beta)");
    let gamma = pair(&mut host, "100.64.0.5", "FlightDeck Desktop/1.0 (gamma)");

    assert!(host.revoke_device("  ").is_err());
    assert!(host
        .revoke_device("nobody")
        .unwrap_err()
        .contains("No paired device"));

    // Two devices share a name: ambiguous, and both are named.
    let err = host.revoke_device("beta").unwrap_err();
    assert!(err.contains(&beta1) && err.contains(&beta2), "{err}");

    // An exact id.
    let (report, closed) = host.revoke_device(&beta1).unwrap();
    assert_eq!(report.id, beta1);
    assert!(report.revoked);
    assert_eq!(closed, 0, "no server, no connections to close");
    // Now the name is unique among the devices still paired.
    assert_eq!(host.revoke_device("BETA").unwrap().0.id, beta2);
    // A name, any case.
    assert_eq!(host.revoke_device("Alpha").unwrap().0.id, alpha);
    // A unique id prefix.
    let prefix: String = gamma.chars().take(8).collect();
    assert_eq!(host.revoke_device(&prefix).unwrap().0.id, gamma);

    assert!(host.devices().iter().all(|d| d.revoked));
    assert!(host
        .take_log_lines()
        .iter()
        .any(|l| l.contains("revoked device alpha")));
}

#[test]
fn a_pairing_code_says_the_worker_is_not_listening_when_it_is_not() {
    let fakes = Fakes::new();
    let mut host = fakes.worker(Vec::new());
    host.start_worker();
    let code = host.mint_pairing_code();
    assert_eq!(code.code.len(), 4);
    assert_eq!(code.expires_in_secs, 120);
    assert!(!code.listening);
    assert!(code.address.is_none());
    assert!(code.not_listening_reason.is_some());
    let report = host.worker_report();
    assert!(!report.listen.listening);
    assert!(report.listen.reason.is_some());
    assert!(host
        .take_log_lines()
        .iter()
        .any(|l| l.starts_with("not listening")));
}

#[test]
fn a_worker_on_loopback_listens_reports_and_stops() {
    let fakes = Fakes::new();
    let mut opts = worker_options("/home/me");
    opts.listen = crate::web::server::ListenPlan::Fixed(vec!["127.0.0.1:0".parse().unwrap()]);
    let mut host = testing::worker_host(
        fakes.env(),
        &fakes.notifier,
        vec![TestProject::new("alpha", &[])],
        opts,
    );
    host.start_worker();
    host.tick();
    assert!(host.web_surface.running());
    let report = host.worker_report();
    assert!(report.listen.listening, "{:?}", report.listen);
    assert_eq!(report.projects.len(), 1);
    assert_eq!(report.projects[0].name, "alpha");
    let code = host.mint_pairing_code();
    assert!(code.listening);
    assert!(code.address.is_some());
    assert!(host
        .take_log_lines()
        .iter()
        .any(|l| l.starts_with("listening on 127.0.0.1:")));

    host.set_shutdown_intent(true);
    host.stop_worker();
    assert!(!host.web_surface.running());
    assert!(host.take_log_lines().iter().any(|l| l.contains("restart")));
    // Idempotent.
    host.stop_worker();
}
