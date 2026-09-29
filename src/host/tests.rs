//! [`AppHost`] driven the way a front-end drives it — `pump`/`publish`/`tick`
//! and [`HostEvent`]s — against the fakes in `crate::testing`, with no terminal
//! anywhere. Each test asserts the state change a front-end would render.

use super::*;
use crate::app::state::{AppState, Services};
use crate::contracts::{AgentDef, Config, FileSystem, InterpretedStatus, StatusPatterns};
use crate::persistence::project_state::default_state;
use crate::testing::{
    FakeClock, FakeCommandRunner, FakeContainerRuntime, FakeFs, FakeGit, FakeNotifier, FakePty,
    FakePtyHandle,
};
use crate::{Project, WebSurface};
use flightdeck_remote_protocol::SessionId;
use std::sync::Mutex;
use tempfile::TempDir;

/// Every service the host borrows, as fakes. Built first and kept alive for
/// the test, because the host borrows rather than owns them (as it borrows the
/// real ones from `run`).
struct Fakes {
    fs: FakeFs,
    pty: FakePty,
    clock: FakeClock,
    container: FakeContainerRuntime,
    command: FakeCommandRunner,
    notifier: FakeNotifier,
    /// Shared with the host's project, so a test configures the same fake the
    /// host's dispatches read (`Project::git` is behind the trait).
    git: Arc<FakeGit>,
    /// Holds the stand-in agent executable `make_agent` writes.
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
            notifier: FakeNotifier::new(),
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

    /// An agent whose command exists on disk, so a launch resolves it, and
    /// whose basename (`opencode`) the status backend recognises.
    fn make_agent(&self) -> AgentDef {
        let path = self.dir.path().join("opencode");
        std::fs::write(&path, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&path, perms).unwrap();
        }
        AgentDef {
            key: "opencode".to_string(),
            display_name: "OpenCode".to_string(),
            command: path.to_str().unwrap().to_string(),
            args: vec![],
            status_patterns: StatusPatterns::default(),
        }
    }

    /// A project state holding one real, running Agent Session Tab named
    /// `Task`, built by dispatching `NewAgentTab` against [`FakeGit`] so the tab
    /// is the real thing rather than a hand-assembled record. Returns the state
    /// and the handle that drives the tab's primary PTY.
    fn state_with_a_tab(&self) -> (AppState, FakePtyHandle) {
        let agent = self.make_agent();
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
        assert_eq!(state.tabs.len(), 1, "one tab to drive");
        (state, handle)
    }

    /// A host over a one-project workspace holding `state`.
    fn host(&self, state: AppState) -> AppHost<'_> {
        let workspace = Workspace {
            projects: vec![project("proj", "/repo", state, Arc::clone(&self.git))],
            active: 0,
        };
        let mut host =
            AppHost::from_parts(self.env(), &self.notifier, workspace, web_surface(), false);
        // Skip the tick-0 git-status refresh. It would only read the fake, but
        // it does so from a worker thread whose answer lands on a later turn,
        // which would make "a quiet turn asks for no redraw" racy.
        host.tick = 1;
        host
    }
}

/// A [`Project`] with fresh worker channels, as `open_project` builds one.
fn project(name: &str, root: &str, state: AppState, git: Arc<FakeGit>) -> Project {
    let (create_tx, create_rx) = std::sync::mpsc::channel();
    let (status_tx, status_rx) = std::sync::mpsc::channel();
    Project {
        name: name.to_string(),
        git: crate::git::repo::ProjectGit::new(PathBuf::from(root), git),
        state,
        cache: HashMap::new(),
        create_tx,
        create_rx,
        status_tx,
        status_rx,
        status_in_flight: false,
        git_lock: Arc::new(Mutex::new(())),
    }
}

/// A web surface with no server and no real credential file behind it.
fn web_surface() -> WebSurface {
    let store = crate::web::credentials::CredentialStore::open(
        Arc::new(FakeFs::new()),
        Arc::new(FakeClock::default()),
        PathBuf::from("/web.json"),
    );
    let (inbound_tx, inbound_rx) = std::sync::mpsc::channel();
    let (count_tx, count_rx) = std::sync::mpsc::channel();
    WebSurface {
        credentials: Arc::new(Mutex::new(store)),
        streams: crate::web::stream::TerminalStreams::new(1024),
        activity: crate::web::activity::ActivityStore::new(),
        pending_finishes: crate::web::activity::PendingFinishes::new(),
        inbound_tx,
        inbound_rx,
        count_tx,
        count_rx,
        handle: None,
        published: crate::web::server::HostState::default(),
    }
}

/// The text on the active tab's primary terminal screen.
fn primary_screen(host: &AppHost) -> String {
    host.active_state().tabs[0]
        .session
        .primary()
        .expect("the tab has a primary terminal")
        .screen()
        .contents()
}

#[test]
fn pump_feeds_pty_output_into_the_session_and_asks_for_a_redraw() {
    let fakes = Fakes::new();
    let (state, pty) = fakes.state_with_a_tab();
    let mut host = fakes.host(state);
    // Settle whatever the launch itself produced, so the next turn's answer is
    // about the output below and nothing else.
    host.pump();
    assert!(!host.pump(), "a quiet turn asks for no redraw");

    pty.push_output("hello from the agent");
    assert!(host.pump(), "PTY output is a reason to redraw");
    assert!(
        primary_screen(&host).contains("hello from the agent"),
        "the bytes reached the session's VT parser: {:?}",
        primary_screen(&host)
    );
    assert!(!host.pump(), "and the next quiet turn is quiet again");
}

#[test]
fn pump_reads_the_clock_once_per_turn() {
    let fakes = Fakes::new();
    let (state, _pty) = fakes.state_with_a_tab();
    let mut host = fakes.host(state);
    fakes.clock.set_millis(1_234);
    host.pump();
    fakes.clock.set_millis(9_999);
    host.publish();
    assert_eq!(
        host.now_ms(),
        1_234,
        "publish and the render share the instant pump read"
    );
}

#[test]
fn deferred_pty_chunks_flush_on_the_tick_they_fall_due() {
    let fakes = Fakes::new();
    let (state, pty) = fakes.state_with_a_tab();
    let tab_id = state.tabs[0].meta.id.clone();
    let mut host = fakes.host(state);
    // A relay-less bridge: the command bridge and its `deferred_pty` queue run
    // exactly as with a phone attached, minus the relay thread.
    host.remote_bridge = Some(RemoteBridge::passthrough(0));
    host.remote_bridge.as_mut().unwrap().enqueue_deferred_pty(
        SessionId::new(tab_id),
        1_000,
        b"\r".to_vec(),
    );

    fakes.clock.set_millis(999);
    host.tick();
    assert!(pty.input().is_empty(), "not due yet, nothing written");

    fakes.clock.set_millis(1_000);
    host.tick();
    assert_eq!(pty.input(), b"\r".to_vec(), "due: written to the primary");

    fakes.clock.set_millis(5_000);
    host.tick();
    assert_eq!(pty.input(), b"\r".to_vec(), "flushed once, never replayed");
}

#[test]
fn a_status_file_edge_reaches_the_notifier_through_a_pump() {
    let fakes = Fakes::new();
    let (mut state, _pty) = fakes.state_with_a_tab();
    let status = PathBuf::from("/status/task");
    state.tabs[0].status_file = Some(status.clone());
    let mut host = fakes.host(state);

    fakes.fs.write(&status, "working\n").unwrap();
    assert!(host.pump(), "a status change is a reason to redraw");
    assert_eq!(
        host.active_state().tabs[0].display_status(0).interpreted,
        InterpretedStatus::Working
    );
    assert!(
        fakes.notifier.posted().is_empty(),
        "working arms, never alerts"
    );

    fakes.fs.write(&status, "working\nidle\n").unwrap();
    host.pump();
    let posted = fakes.notifier.posted();
    assert_eq!(posted.len(), 1, "one finish edge, one notification");
    assert_eq!(
        posted[0].title, "proj: Task",
        "prefixed with the project so several open projects stay legible"
    );
}

#[test]
fn a_command_event_changes_the_active_project_state() {
    let fakes = Fakes::new();
    let (state, _pty) = fakes.state_with_a_tab();
    let mut host = fakes.host(state);
    assert!(!host.active_state().split_view);

    host.handle(HostEvent::Command(Command::ToggleSplitView))
        .unwrap();

    assert!(host.active_state().split_view, "dispatched into AppState");
    assert!(
        matches!(&host.ui.overlay, crate::tui::render::UiOverlay::Dialog(d) if d.title == "Split view on."),
        "and the effect surfaced as the same message the key binding shows"
    );
}

#[test]
fn terminal_input_reaches_the_focused_pty_only_in_terminal_focus() {
    let fakes = Fakes::new();
    let (state, pty) = fakes.state_with_a_tab();
    let mut host = fakes.host(state);

    host.handle(HostEvent::FocusTerminal).unwrap();
    assert!(host.terminal_focused());
    host.handle(HostEvent::TerminalInput(b"ls\r".to_vec()))
        .unwrap();
    assert_eq!(
        pty.input(),
        b"ls\r".to_vec(),
        "no web server, so no lock to contest"
    );

    // A paste outside terminal focus is a no-op, as it is from the keyboard.
    host.handle(HostEvent::FocusApp).unwrap();
    host.handle(HostEvent::Paste("more".to_string())).unwrap();
    assert_eq!(pty.input(), b"ls\r".to_vec(), "App mode swallows the paste");
}

#[test]
fn resize_event_resizes_every_session_and_records_the_size() {
    let fakes = Fakes::new();
    let (state, pty) = fakes.state_with_a_tab();
    let mut host = fakes.host(state);
    let size = PtySize {
        rows: 40,
        cols: 120,
    };

    host.handle(HostEvent::Resize(size)).unwrap();

    assert_eq!(host.active_state().pty_size, size);
    assert_eq!(pty.resizes().last(), Some(&size), "the live PTY followed");
}

#[test]
fn switch_project_event_moves_the_active_project() {
    let fakes = Fakes::new();
    let (state, _pty) = fakes.state_with_a_tab();
    let mut host = fakes.host(state);
    let other = AppState::new(
        Config::default(),
        default_state("main"),
        "/repo2",
        "/repo2/state.json",
    );
    host.workspace
        .projects
        .push(project("other", "/repo2", other, Arc::new(FakeGit::new())));

    host.handle(HostEvent::SwitchProject(Selector::Next))
        .unwrap();

    assert_eq!(host.active_project_index(), 1);
    assert_eq!(host.project_name(1), Some("other"));
}

#[test]
fn quit_event_is_what_should_quit_reports() {
    let fakes = Fakes::new();
    let (state, _pty) = fakes.state_with_a_tab();
    let mut host = fakes.host(state);
    assert!(!host.should_quit());
    host.handle(HostEvent::Quit).unwrap();
    assert!(host.should_quit());
}

#[test]
fn persist_writes_each_project_state_through_the_fs_seam() {
    let fakes = Fakes::new();
    let (state, _pty) = fakes.state_with_a_tab();
    let host = fakes.host(state);

    host.persist().unwrap();

    let saved = fakes
        .fs
        .file_contents(Path::new("/repo/.flightdeck/state.json"))
        .expect("state.json written through FileSystem");
    assert!(saved.contains("Task"), "the tab is in it: {saved}");
}

#[test]
fn terminate_sessions_kills_every_agent() {
    let fakes = Fakes::new();
    let (state, pty) = fakes.state_with_a_tab();
    let mut host = fakes.host(state);
    host.terminate_sessions();
    assert!(pty.terminated(), "no orphaned child left behind");
}

#[test]
fn start_and_stop_services_are_quiet_with_remote_and_web_off() {
    let fakes = Fakes::new();
    let (mut state, _pty) = fakes.state_with_a_tab();
    // The update check is a real network fetch plus a cache write under the
    // real `$HOME` (SPECS §30) — off, so this test stays hermetic.
    state.config.update.check = false;
    let mut host = fakes.host(state);

    host.start();
    assert!(
        host.update_tx.is_none(),
        "the checker was handed its sender"
    );
    assert!(host.remote_setup.is_none(), "remote off: no relay thread");
    assert!(host.remote_bridge.is_none(), "and no bridge to feed");
    assert!(!host.web_surface.running(), "web off: no listener");
    host.tick();
    host.stop_services();
    host.stop_services();
}
