//! [`AppHost`] driven the way a front-end drives it — `pump`/`publish`/`tick`
//! and [`HostEvent`]s — against the fakes in `crate::testing`, with no terminal
//! anywhere. Each test asserts the state change a front-end would render.

use super::testing::web_surface;
use super::*;
use crate::app::state::{AppState, Services};
use crate::contracts::{AgentDef, Config, FileSystem, InterpretedStatus, StatusPatterns};
use crate::persistence::project_state::default_state;
use crate::testing::{
    FakeClock, FakeCommandRunner, FakeContainerRuntime, FakeFs, FakeGit, FakeNotifier, FakePty,
    FakePtyHandle,
};
use crate::Project;
use flightdeck_remote_protocol::SessionId;
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
    super::testing::project(name, PathBuf::from(root), state, git)
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
fn active_terminal_is_the_selected_agents_focused_terminal() {
    let fakes = Fakes::new();
    let (state, pty) = fakes.state_with_a_tab();
    let mut host = fakes.host(state);
    pty.push_output("drawn by the gui");
    host.pump();
    let term = host.active_terminal().expect("the tab's agent is spawned");
    assert!(term.screen().contents().contains("drawn by the gui"));
    assert!(host.active_terminal_mut().is_some());

    host.active_state_mut().selected_tab = None;
    assert!(
        host.active_terminal().is_none(),
        "no agent selected, no terminal"
    );
}

#[test]
fn tab_terminal_reads_any_projects_tab_not_only_the_active_one() {
    let fakes = Fakes::new();
    let (live, pty) = fakes.state_with_a_tab();
    let live_id = live.tabs[0].meta.id.clone();
    // Project 0 is on screen and its one tab has no process; the live tab
    // sits in background project 1.
    let workspace = Workspace {
        projects: vec![
            project(
                "front",
                "/front",
                super::testing::state_with_tabs("front", &["idle"]),
                Arc::clone(&fakes.git),
            ),
            project("back", "/repo", live, Arc::clone(&fakes.git)),
        ],
        active: 0,
    };
    let mut host = AppHost::from_parts(
        fakes.env(),
        &fakes.notifier,
        workspace,
        web_surface(),
        false,
    );
    host.tick = 1;
    pty.push_output("background output");
    host.pump();

    let term = host
        .tab_terminal(1, &live_id)
        .expect("a background project's spawned tab has a terminal");
    assert!(term.screen().contents().contains("background output"));
    assert_eq!(host.active_project_index(), 0, "reading switched nothing");
    assert!(
        host.active_terminal().is_none(),
        "the active project's own tab is unspawned"
    );
    assert!(
        host.tab_terminal(0, "t0").is_none(),
        "unspawned: no terminal"
    );
    assert!(host.tab_terminal(1, "no-such-tab").is_none());
    assert!(host.tab_terminal(7, &live_id).is_none());
}

#[test]
fn one_terminal_of_a_tab_is_read_and_resized_on_its_own() {
    use crate::view::TerminalRef;
    let fakes = Fakes::new();
    let (state, primary_pty) = fakes.state_with_a_tab();
    let tab_id = state.tabs[0].meta.id.clone();
    let mut host = fakes.host(state);
    let shell_pty = fakes.pty.queue_session();
    host.handle(HostEvent::Command(Command::NewChildTerminal))
        .unwrap();
    shell_pty.push_output("in the shell");
    host.pump();

    // Each terminal by name, not only the focused one.
    let shell = host
        .tab_terminal_at(0, &tab_id, TerminalRef::Child(0))
        .expect("the child is spawned");
    assert!(shell.screen().contents().contains("in the shell"));
    assert!(host
        .tab_terminal_at(0, &tab_id, TerminalRef::Primary)
        .is_some());
    assert!(host
        .tab_terminal_at(0, &tab_id, TerminalRef::Child(1))
        .is_none());
    assert!(host
        .tab_terminal_at(3, &tab_id, TerminalRef::Primary)
        .is_none());

    // Split view's columns: the child alone gets its size.
    let before = primary_pty.resizes().len();
    let column = PtySize { rows: 30, cols: 57 };
    assert!(host.resize_terminal(0, &tab_id, TerminalRef::Child(0), column));
    assert_eq!(shell_pty.resizes().last(), Some(&column));
    assert_eq!(
        host.tab_terminal_at(0, &tab_id, TerminalRef::Child(0))
            .unwrap()
            .screen()
            .size(),
        (30, 57)
    );
    assert_eq!(
        primary_pty.resizes().len(),
        before,
        "the agent kept its size"
    );

    // The same size again is no resize (no spurious SIGWINCH), nor is one
    // below the parser's floor that clamps to the size it already has.
    let resizes = shell_pty.resizes().len();
    assert!(!host.resize_terminal(0, &tab_id, TerminalRef::Child(0), column));
    let tiny = PtySize { rows: 1, cols: 1 };
    assert!(host.resize_terminal(0, &tab_id, TerminalRef::Child(0), tiny));
    assert!(!host.resize_terminal(0, &tab_id, TerminalRef::Child(0), tiny));
    assert_eq!(shell_pty.resizes().len(), resizes + 1);

    // Unknown targets change nothing.
    assert!(!host.resize_terminal(0, &tab_id, TerminalRef::Child(4), column));
    assert!(!host.resize_terminal(0, "no-such-tab", TerminalRef::Primary, column));
    assert!(!host.resize_terminal(9, &tab_id, TerminalRef::Primary, column));
}

#[test]
fn refresh_git_status_runs_for_a_background_project_on_request() {
    let fakes = Fakes::new();
    let projects = vec![
        super::testing::TestProject::new("front", &["f1"]),
        super::testing::TestProject::new("back", &["b1", "b2"]),
    ];
    let mut host = super::testing::host(fakes.env(), &fakes.notifier, projects, 0);
    assert!(!host.workspace.projects[1].status_in_flight);
    host.refresh_git_status(1);
    assert!(
        host.workspace.projects[1].status_in_flight,
        "a worker is out"
    );
    assert!(
        !host.workspace.projects[0].status_in_flight,
        "only the one asked for"
    );
    host.refresh_git_status(1); // in flight: nothing more
    host.refresh_git_status(9); // unknown: nothing at all
                                // The worker answers into the project's channel; a later pump lands it.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while host.workspace.projects[1].status_in_flight {
        assert!(
            std::time::Instant::now() < deadline,
            "the worker never answered"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
        host.pump();
    }
}

#[test]
fn mission_view_spans_every_project() {
    let fakes = Fakes::new();
    let (mut live, _pty) = fakes.state_with_a_tab();
    live.tabs[0].interpreted = Some(InterpretedStatus::WaitingForInput);
    let workspace = Workspace {
        projects: vec![
            project(
                "front",
                "/front",
                super::testing::state_with_tabs("front", &["idle"]),
                Arc::clone(&fakes.git),
            ),
            project("back", "/repo", live, Arc::clone(&fakes.git)),
        ],
        active: 0,
    };
    let host = AppHost::from_parts(
        fakes.env(),
        &fakes.notifier,
        workspace,
        web_surface(),
        false,
    );
    let view = host.mission_view();
    assert_eq!(view.tiles.len(), 1);
    assert_eq!(view.tiles[0].key.project, 1);
    assert_eq!(view.tiles[0].project_name, "back");
    assert_eq!(view.needs_you_count, 1);
    assert_eq!(view.quiet_count, 1, "the unspawned tab never did anything");
}

#[test]
fn workspace_ui_survives_a_reopen() {
    use crate::persistence::workspace::{MainView, MissionScope, RecentScope};
    let fakes = Fakes::new();
    let ws_path = PathBuf::from("/home/u/.flightdeck/workspace.json");
    let projects = || vec![super::testing::TestProject::new("alpha", &["a1"])];
    let mut host = super::testing::host_with_workspace_file(
        fakes.env(),
        &fakes.notifier,
        projects(),
        0,
        ws_path.clone(),
    );
    assert_eq!(host.workspace_ui(), &WorkspaceUi::default(), "no file yet");
    let chosen = WorkspaceUi {
        view: MainView::Mission,
        mission_scope: MissionScope {
            window: RecentScope::Week,
            project: Some("/alpha".to_string()),
        },
    };
    host.set_workspace_ui(chosen.clone());
    let _ = host.persist();
    drop(host);

    let reopened = super::testing::host_with_workspace_file(
        fakes.env(),
        &fakes.notifier,
        projects(),
        0,
        ws_path.clone(),
    );
    assert_eq!(reopened.workspace_ui(), &chosen);
    let saved = load_workspace(&fakes.fs, &ws_path).unwrap();
    assert_eq!(saved.projects, ["/alpha"]);
    assert_eq!(saved.ui, chosen);
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
fn needs_you_counts_agents_waiting_for_the_user() {
    let fakes = Fakes::new();
    let (mut state, _pty) = fakes.state_with_a_tab();
    let status = PathBuf::from("/status/task");
    state.tabs[0].status_file = Some(status.clone());
    let mut host = fakes.host(state);
    assert_eq!(host.needs_you_count(), 0);

    fakes.fs.write(&status, "working\n").unwrap();
    host.pump();
    assert_eq!(
        host.needs_you_count(),
        0,
        "a working agent does not need you"
    );

    fakes.fs.write(&status, "working\nwaiting\n").unwrap();
    host.pump();
    assert_eq!(
        host.active_state().tabs[0].display_status(0).interpreted,
        InterpretedStatus::WaitingForInput
    );
    assert_eq!(host.needs_you_count(), 1);

    fakes.fs.write(&status, "working\nwaiting\nidle\n").unwrap();
    host.pump();
    assert_eq!(host.needs_you_count(), 0, "answered, it stops counting");
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

/// The paste path the TUI and the desktop app share (`HostEvent::Paste`): a
/// bracketed paste cannot smuggle an end marker, so the text after it never
/// reaches the agent as typed input.
#[test]
fn a_bracketed_paste_event_is_stripped_of_embedded_markers() {
    let fakes = Fakes::new();
    let (state, pty) = fakes.state_with_a_tab();
    let mut host = fakes.host(state);
    host.handle(HostEvent::FocusTerminal).unwrap();
    host.active_terminal_mut()
        .expect("a terminal")
        .process_output(b"\x1b[?2004h");

    host.handle(HostEvent::Paste(
        "a\x1b[201~rm -rf ~\x1b[20\x1b[201~1~".to_string(),
    ))
    .unwrap();

    assert_eq!(pty.input(), b"\x1b[200~arm -rf ~\x1b[201~".to_vec());
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

// ---------------------------------------------------------------------------
// The overlay API: every overlay the TUI has, opened through a `HostEvent`,
// read back through `AppHost::overlay`, and answered through
// `HostEvent::Overlay` — asserting the state the TUI's own keys would leave.
// ---------------------------------------------------------------------------

mod overlays {
    use super::*;
    use crate::app::commands::CloseAction;
    use crate::contracts::RebaseOutcome;
    use crate::tui::config_manager::{ConfigScope, FieldValue, Origin};
    use crate::tui::palette::PaletteAction;
    use crate::web::access::{AccessKey, AccessMode};

    fn overlay(host: &AppHost) -> OverlayView {
        host.overlay().expect("an overlay is on screen")
    }

    fn message(host: &AppHost) -> String {
        match overlay(host) {
            OverlayView::Message(m) => m.text,
            other => panic!("expected a message, got {other:?}"),
        }
    }

    fn dialog(host: &AppHost) -> DialogView {
        match overlay(host) {
            OverlayView::Dialog(d) => d,
            other => panic!("expected a dialog, got {other:?}"),
        }
    }

    fn button<'d>(d: &'d DialogView, id: &str) -> &'d DialogButton {
        d.buttons
            .iter()
            .find(|b| b.id == id)
            .unwrap_or_else(|| panic!("no `{id}` button on {d:?}"))
    }

    fn choose(host: &mut AppHost, id: &str) {
        host.handle(HostEvent::Overlay(OverlayInput::Choose(id.to_string())))
            .expect("the button is on the dialog");
    }

    fn command(host: &mut AppHost, cmd: Command) {
        host.handle(HostEvent::Command(cmd)).unwrap();
    }

    fn run(host: &mut AppHost, action: PaletteAction) {
        host.handle(HostEvent::RunPaletteAction(action)).unwrap();
    }

    /// The selected tab's absolute worktree path, as the guards compute it.
    fn worktree(host: &AppHost) -> PathBuf {
        let state = host.active_state();
        crate::fs::paths::to_absolute(
            &state.repo_root,
            Path::new(&state.tabs[0].meta.worktree_path_relative),
        )
    }

    fn branch(host: &AppHost) -> String {
        host.active_state().tabs[0].meta.branch.clone()
    }

    #[test]
    fn nothing_is_on_screen_by_default_and_a_stray_key_reaches_nothing() {
        let fakes = Fakes::new();
        let (state, pty) = fakes.state_with_a_tab();
        let mut host = fakes.host(state);
        host.handle(HostEvent::FocusTerminal).unwrap();
        assert_eq!(host.overlay(), None);

        host.handle(HostEvent::Overlay(OverlayInput::Key(OverlayKey::Char('x'))))
            .unwrap();
        assert!(
            pty.input().is_empty(),
            "an overlay key with no overlay never reaches the terminal"
        );
        assert!(
            host.handle(HostEvent::Overlay(OverlayInput::Choose("y".into())))
                .is_err(),
            "nothing to press"
        );
    }

    #[test]
    fn a_message_reads_out_verbatim_and_its_ok_dismisses_it() {
        let fakes = Fakes::new();
        let (state, _pty) = fakes.state_with_a_tab();
        let mut host = fakes.host(state);
        command(&mut host, Command::ToggleSplitView);
        assert_eq!(message(&host), "Split view on.");

        let refused = host.handle(HostEvent::Overlay(OverlayInput::Choose("y".into())));
        assert!(refused.is_err(), "a notification has only its OK");
        assert_eq!(
            message(&host),
            "Split view on.",
            "and a refusal changes nothing"
        );

        choose(&mut host, "Enter");
        assert_eq!(host.overlay(), None);
    }

    #[test]
    fn the_new_agent_form_offers_the_agent_choice_and_refuses_an_empty_name() {
        let fakes = Fakes::new();
        let (state, _pty) = fakes.state_with_a_tab();
        let mut host = fakes.host(state);
        command(
            &mut host,
            Command::NewAgentTab {
                name: String::new(),
                agent_key: None,
            },
        );
        let d = dialog(&host);
        let DialogKind::NewAgent(form) = &d.kind else {
            panic!("the new-agent form: {d:?}");
        };
        assert_eq!(
            form.agents,
            vec![AgentChoice {
                key: "opencode".into(),
                name: "OpenCode".into()
            }]
        );
        assert_eq!(form.selected_agent, 0, "the configured default");
        assert_eq!(form.target, NewAgentTarget::NewBranch);
        assert_eq!(d.input.as_deref(), Some(""));
        assert_eq!(
            d.list,
            vec![DialogRow {
                label: "(•) OpenCode".into(),
                selected: true
            }]
        );
        let create = button(&d, "Enter");
        assert_eq!(
            (create.label.as_str(), create.role),
            ("Create", ButtonRole::Primary)
        );
        assert!(create.default, "Submit presses Enter");
        assert_eq!(button(&d, "Esc").role, ButtonRole::Cancel);
        assert_eq!(d.origin, crate::web::protocol::DialogOrigin::Desktop);
        assert_eq!(d.origin_label, None);

        // The TUI keeps prompting on an empty name; so does the host.
        host.handle(HostEvent::Overlay(OverlayInput::Submit))
            .unwrap();
        assert_eq!(dialog(&host).id, d.id, "still the same dialog");
        assert_eq!(host.active_state().tabs.len(), 1, "nothing created");

        // Tab cycles the target exactly as the key does.
        host.handle(HostEvent::Overlay(OverlayInput::Key(OverlayKey::Tab)))
            .unwrap();
        let DialogKind::NewAgent(form) = dialog(&host).kind else {
            unreachable!()
        };
        let expected = if form.has_existing_branches {
            NewAgentTarget::ExistingBranch
        } else {
            NewAgentTarget::Base
        };
        assert_eq!(form.target, expected);
        // Back round to a new branch.
        while !matches!(
            dialog(&host).kind,
            DialogKind::NewAgent(NewAgentForm {
                target: NewAgentTarget::NewBranch,
                ..
            })
        ) {
            host.handle(HostEvent::Overlay(OverlayInput::Key(OverlayKey::Tab)))
                .unwrap();
        }

        host.handle(HostEvent::Overlay(OverlayInput::SetText("feature".into())))
            .unwrap();
        assert_eq!(dialog(&host).input.as_deref(), Some("feature"));
        host.handle(HostEvent::Overlay(OverlayInput::Submit))
            .unwrap();
        assert!(
            message(&host).starts_with("Creating worktree for "),
            "the TUI's own progress line: {}",
            message(&host)
        );
        assert_eq!(
            host.active_state().tabs.len(),
            2,
            "the placeholder tab is up"
        );
    }

    #[test]
    fn the_child_agent_picker_is_a_numbered_choice_list() {
        let fakes = Fakes::new();
        let (mut state, _pty) = fakes.state_with_a_tab();
        let mut second = fakes.make_agent();
        second.key = "codex".into();
        second.display_name = "Codex".into();
        let mut config = state.config.clone();
        config.agents.insert(second.key.clone(), second);
        state.reload_config(config);
        let mut host = fakes.host(state);

        run(&mut host, PaletteAction::NewAgentChild);
        let d = dialog(&host);
        let DialogKind::NewAgentChild { agents } = &d.kind else {
            panic!("the backend picker: {d:?}");
        };
        assert_eq!(agents.len(), 2);
        assert_eq!(
            d.buttons.iter().map(|b| b.id.as_str()).collect::<Vec<_>>(),
            vec!["1", "2", "Esc"]
        );
        assert!(
            host.handle(HostEvent::Overlay(OverlayInput::Choose("7".into())))
                .is_err(),
            "only a button the dialog shows"
        );
        choose(&mut host, "Esc");
        assert_eq!(host.overlay(), None);
    }

    #[test]
    fn rename_is_a_text_prompt_that_renames_through_the_tui_path() {
        let fakes = Fakes::new();
        let (state, _pty) = fakes.state_with_a_tab();
        let mut host = fakes.host(state);
        command(
            &mut host,
            Command::RenameAgentTab {
                new_name: String::new(),
            },
        );
        let d = dialog(&host);
        assert_eq!(d.kind, DialogKind::RenameSession);
        assert_eq!(d.title, "Rename this Agent Session Tab");

        host.handle(HostEvent::Overlay(OverlayInput::SetText("   ".into())))
            .unwrap();
        choose(&mut host, "Enter");
        assert_eq!(dialog(&host).id, d.id, "a blank name keeps prompting");

        host.handle(HostEvent::Overlay(OverlayInput::SetText("Renamed".into())))
            .unwrap();
        choose(&mut host, "Enter");
        assert_eq!(host.active_state().tabs[0].meta.name, "Renamed");
        assert_eq!(message(&host), "Renamed tab to 'Renamed'");
    }

    #[test]
    fn set_status_is_a_choice_of_buttons() {
        let fakes = Fakes::new();
        let (state, _pty) = fakes.state_with_a_tab();
        let mut host = fakes.host(state);
        command(&mut host, Command::SetManualStatus(None));
        let d = dialog(&host);
        assert_eq!(d.kind, DialogKind::SetManualStatus);
        assert_eq!(
            d.buttons.iter().map(|b| b.id.as_str()).collect::<Vec<_>>(),
            vec!["i", "w", "b", "d", "c", "Esc"]
        );
        choose(&mut host, "b");
        assert!(
            host.active_state().tabs[0].meta.manual_status.is_some(),
            "dispatched SetManualStatus(Blocked)"
        );
        assert!(!matches!(host.overlay(), Some(OverlayView::Dialog(_))));
    }

    #[test]
    fn close_session_refuses_while_processes_run_and_force_terminate_is_destructive() {
        let fakes = Fakes::new();
        let (state, pty) = fakes.state_with_a_tab();
        let mut host = fakes.host(state);
        command(&mut host, Command::CloseAgentTab { action: None });
        let d = dialog(&host);
        let DialogKind::CloseSession { actions } = &d.kind else {
            panic!("the close menu: {d:?}");
        };
        let id_of = |wanted: CloseAction| {
            (actions.iter().position(|a| *a == wanted).unwrap() + 1).to_string()
        };
        assert_eq!(
            button(&d, &id_of(CloseAction::ForceTerminate)).role,
            ButtonRole::Destructive
        );
        assert_eq!(
            button(&d, &id_of(CloseAction::IfAllStopped)).role,
            ButtonRole::Secondary
        );

        // SPECS §25's refusal path: nothing is closed, the TUI's own sentence.
        choose(&mut host, &id_of(CloseAction::IfAllStopped));
        assert_eq!(
            message(&host),
            "Refused: Processes are still running; tab not closed."
        );
        assert_eq!(host.active_state().tabs.len(), 1);

        command(&mut host, Command::CloseAgentTab { action: None });
        choose(&mut host, &id_of(CloseAction::ForceTerminate));
        assert_eq!(message(&host), "Closed Agent Tab.");
        assert!(host.active_state().tabs.is_empty());
        assert!(pty.terminated());
    }

    /// The sidebar `✕` opens this prompt from a click, which has no command;
    /// open it the way the mouse handler does.
    fn start_close_agent_choice(host: &mut AppHost) {
        let (_, _, ui) = host.tui_parts();
        crate::start_prompt(ui, crate::Prompt::CloseAgentChoice { index: 0 });
    }

    #[test]
    fn close_terminal_and_the_sidebar_close_menu_read_out() {
        let fakes = Fakes::new();
        let (state, _pty) = fakes.state_with_a_tab();
        let mut host = fakes.host(state);
        command(&mut host, Command::CloseChildTerminal);
        assert_eq!(message(&host), "No child terminal selected.");

        start_close_agent_choice(&mut host);
        let d = dialog(&host);
        assert_eq!(d.kind, DialogKind::CloseSessionChoice { index: 0 });
        assert_eq!(
            button(&d, "a").role,
            ButtonRole::Primary,
            "it only asks again"
        );
        assert_eq!(button(&d, "n").role, ButtonRole::Cancel);
        // `a` routes into the abandon confirmation, never straight to removal.
        choose(&mut host, "a");
        assert!(matches!(
            dialog(&host).kind,
            DialogKind::ConfirmAbandon { .. }
        ));
        assert!(fakes.git.removed_worktrees().is_empty());
    }

    #[test]
    fn abandon_confirms_first_warns_when_dirty_and_removes_on_yes() {
        let fakes = Fakes::new();
        let (state, _pty) = fakes.state_with_a_tab();
        let mut host = fakes.host(state);
        let path = worktree(&host);

        command(&mut host, Command::AbandonWorktree { confirm: false });
        let d = dialog(&host);
        assert_eq!(d.kind, DialogKind::ConfirmAbandon { dirty: false });
        assert_eq!(d.title, "Abandon this worktree?");
        assert_eq!(
            (button(&d, "y").label.as_str(), button(&d, "y").role),
            ("Abandon", ButtonRole::Destructive)
        );
        choose(&mut host, "n");
        assert_eq!(host.overlay(), None);
        assert!(
            fakes.git.removed_worktrees().is_empty(),
            "cancel removes nothing"
        );

        fakes.git.set_dirty_at(&path, true);
        command(&mut host, Command::AbandonWorktree { confirm: false });
        let d = dialog(&host);
        assert_eq!(d.kind, DialogKind::ConfirmAbandon { dirty: true });
        assert_eq!(
            d.title,
            "The worktree has uncommitted changes. Discard them and abandon it?"
        );
        assert_eq!(button(&d, "y").label, "Abandon (force)");
        choose(&mut host, "y");
        assert_eq!(message(&host), "Abandoned worktree.");
        assert_eq!(fakes.git.removed_worktrees(), vec![path]);
        assert!(host.active_state().tabs.is_empty());
    }

    #[test]
    fn every_guarded_session_command_refuses_with_no_session_selected() {
        let fakes = Fakes::new();
        let state = AppState::new(
            Config::default(),
            default_state("main"),
            "/repo",
            "/repo/.flightdeck/state.json",
        );
        let mut host = fakes.host(state);
        for cmd in [
            Command::AbandonWorktree { confirm: false },
            Command::FinishLocalMerge { confirm: false },
            Command::RebaseWorktree { confirm: false },
            Command::CloseAgentTab { action: None },
            Command::PushBranch { confirm: None },
        ] {
            command(&mut host, cmd.clone());
            assert_eq!(message(&host), "Error: no tab selected", "{cmd:?}");
            host.handle(HostEvent::Overlay(OverlayInput::Cancel))
                .unwrap();
        }
        assert!(fakes.git.removed_worktrees().is_empty());
        assert!(fakes.git.merges().is_empty());
        assert!(fakes.git.rebases().is_empty());
    }

    #[test]
    fn abandon_refuses_a_tab_that_runs_on_base() {
        let fakes = Fakes::new();
        let (mut state, _pty) = fakes.state_with_a_tab();
        state.tabs[0].meta.runs_on_base = true;
        let mut host = fakes.host(state);
        command(&mut host, Command::AbandonWorktree { confirm: false });
        assert_eq!(
            message(&host),
            "Refused: This tab runs on the base branch in the project root — it has no \
             worktree to abandon. Close the tab instead."
        );
        assert!(fakes.git.removed_worktrees().is_empty());
    }

    #[test]
    fn finish_merge_refuses_every_precondition_then_confirms() {
        let fakes = Fakes::new();
        let (mut state, _pty) = fakes.state_with_a_tab();
        state.tabs[0].meta.runs_on_base = true;
        let mut host = fakes.host(state);
        command(&mut host, Command::FinishLocalMerge { confirm: false });
        assert_eq!(
            message(&host),
            "Refused: This tab already runs on the base branch — there is nothing to merge back."
        );
        host.active_state_mut().tabs[0].meta.runs_on_base = false;

        // SPECS §13: a dirty base disables merge-back entirely.
        fakes.git.set_dirty_at("/repo", true);
        command(&mut host, Command::FinishLocalMerge { confirm: false });
        assert_eq!(
            message(&host),
            "WARNING: Base worktree has uncommitted changes. Local merge is disabled.\n\
             Recommended action: push this branch and create a PR instead."
        );
        fakes.git.set_dirty_at("/repo", false);

        let path = worktree(&host);
        fakes.git.set_dirty_at(&path, true);
        command(&mut host, Command::FinishLocalMerge { confirm: false });
        assert_eq!(
            message(&host),
            format!(
                "Refused: Agent worktree '{}' has uncommitted changes; commit or discard \
                 them before merging.",
                path.display()
            )
        );
        fakes.git.set_dirty_at(&path, false);

        fakes.git.set_current_branch("elsewhere");
        command(&mut host, Command::FinishLocalMerge { confirm: false });
        assert_eq!(
            message(&host),
            "Refused: Base worktree is on 'elsewhere', not the base branch 'main'."
        );
        fakes.git.set_current_branch("main");
        assert!(fakes.git.merges().is_empty(), "no refusal merged anything");

        command(&mut host, Command::FinishLocalMerge { confirm: false });
        let d = dialog(&host);
        let agent_branch = branch(&host);
        assert_eq!(
            d.kind,
            DialogKind::ConfirmMerge {
                agent_branch: agent_branch.clone(),
                base_branch: "main".into(),
                primary_running: true,
            }
        );
        assert_eq!(
            d.title,
            format!(
                "Merge {agent_branch} into main then remove the worktree (stops the running agent)?"
            )
        );
        assert_eq!(button(&d, "y").role, ButtonRole::Destructive);
        choose(&mut host, "n");
        assert!(fakes.git.merges().is_empty(), "cancel merges nothing");

        command(&mut host, Command::FinishLocalMerge { confirm: false });
        choose(&mut host, "y");
        assert_eq!(fakes.git.merges().len(), 1);
        assert_eq!(
            message(&host),
            format!("Merged {agent_branch} into main and removed the worktree.")
        );
    }

    #[test]
    fn rebase_refuses_its_preconditions_keeps_the_guard_copy_and_aborts_on_conflict() {
        let fakes = Fakes::new();
        let (mut state, _pty) = fakes.state_with_a_tab();
        state.tabs[0].meta.runs_on_base = true;
        let mut host = fakes.host(state);
        command(&mut host, Command::RebaseWorktree { confirm: false });
        assert_eq!(
            message(&host),
            "Refused: This tab runs on the base branch — there is nothing to rebase onto."
        );
        host.active_state_mut().tabs[0].meta.runs_on_base = false;

        let path = worktree(&host);
        let agent_branch = branch(&host);
        fakes.git.set_dirty_at(&path, true);
        command(&mut host, Command::RebaseWorktree { confirm: false });
        assert_eq!(
            message(&host),
            format!(
                "Refused: Agent worktree '{}' has uncommitted changes; commit or discard \
                 them before rebasing.",
                path.display()
            )
        );
        fakes.git.set_dirty_at(&path, false);

        // The fake reports `main` checked out everywhere: the wrong branch.
        command(&mut host, Command::RebaseWorktree { confirm: false });
        assert_eq!(
            message(&host),
            format!(
                "Refused: Agent worktree '{}' is on 'main', not the agent branch '{agent_branch}'.",
                path.display()
            )
        );
        assert!(
            fakes.git.rebases().is_empty(),
            "no refusal rebased anything"
        );

        fakes.git.set_current_branch(agent_branch.clone());
        command(&mut host, Command::RebaseWorktree { confirm: false });
        let d = dialog(&host);
        assert!(matches!(d.kind, DialogKind::ConfirmRebase { .. }));
        assert!(
            d.title.ends_with("Rewrites history; aborts on conflict."),
            "SPECS §5's guard copy survives verbatim: {}",
            d.title
        );
        assert_eq!(button(&d, "y").role, ButtonRole::Destructive);
        choose(&mut host, "n");
        assert!(fakes.git.rebases().is_empty(), "cancel rewrites nothing");

        fakes.git.set_rebase_outcome(RebaseOutcome {
            rebased: false,
            conflicted: true,
            message: "conflict in src/lib.rs".into(),
        });
        command(&mut host, Command::RebaseWorktree { confirm: false });
        choose(&mut host, "y");
        let text = message(&host);
        assert!(text.starts_with("Refused: "), "{text}");
        assert!(text.contains("conflict"), "{text}");
        assert_eq!(host.active_state().tabs.len(), 1, "the tab is kept");
    }

    #[test]
    fn pull_base_refuses_off_base_and_on_conflict() {
        let fakes = Fakes::new();
        let (state, _pty) = fakes.state_with_a_tab();
        let mut host = fakes.host(state);

        fakes.git.set_current_branch("feature");
        command(&mut host, Command::PullBase);
        assert_eq!(
            message(&host),
            "Refused: Base folder is on 'feature', not the base branch 'main'."
        );
        assert!(fakes.git.pull_bases().is_empty());

        fakes.git.set_current_branch("main");
        fakes.git.set_pull_base_outcome(RebaseOutcome {
            rebased: false,
            conflicted: true,
            message: "pull hit conflicts; aborted".into(),
        });
        command(&mut host, Command::PullBase);
        assert_eq!(message(&host), "Refused: pull hit conflicts; aborted");
    }

    #[test]
    fn push_confirm_offers_committed_only_or_cancel() {
        let fakes = Fakes::new();
        let (state, _pty) = fakes.state_with_a_tab();
        let mut host = fakes.host(state);
        let path = worktree(&host);
        fakes.git.set_dirty_at(&path, true);
        command(&mut host, Command::PushBranch { confirm: None });
        let d = dialog(&host);
        assert_eq!(d.kind, DialogKind::ConfirmPush);
        assert_eq!(button(&d, "c").role, ButtonRole::Cancel);
        choose(&mut host, "c");
        assert_eq!(message(&host), "Push cancelled.");
        assert!(fakes.git.pushes().is_empty());
    }

    #[test]
    fn quit_confirm_only_quits_on_yes() {
        let fakes = Fakes::new();
        let (state, _pty) = fakes.state_with_a_tab();
        let mut host = fakes.host(state);
        command(&mut host, Command::Quit { confirm: false });
        assert_eq!(dialog(&host).kind, DialogKind::ConfirmQuit);
        choose(&mut host, "n");
        assert!(!host.should_quit());
        command(&mut host, Command::Quit { confirm: false });
        choose(&mut host, "y");
        assert!(host.should_quit());
    }

    #[test]
    fn the_palette_reads_out_filters_and_runs_through_its_own_enter() {
        let fakes = Fakes::new();
        let (state, _pty) = fakes.state_with_a_tab();
        let mut host = fakes.host(state);
        host.handle(HostEvent::OpenPalette).unwrap();
        let OverlayView::Palette(p) = overlay(&host) else {
            panic!("the palette");
        };
        assert_eq!(p.filter, "");
        assert_eq!(p.selected, 0);
        let new_tab = p
            .entries
            .iter()
            .find(|r| r.action == PaletteAction::NewAgentTab)
            .expect("offered outside an isolated run");
        assert_eq!(new_tab.label, "New Agent Session Tab");
        assert!(new_tab.keycap.is_some(), "a key does the same thing");
        assert!(
            !p.entries
                .iter()
                .any(|r| r.action == PaletteAction::UnpairPhone),
            "gated exactly as the TUI's palette: nothing is paired"
        );

        host.handle(HostEvent::Overlay(OverlayInput::PaletteFilter(
            "split".into(),
        )))
        .unwrap();
        let OverlayView::Palette(p) = overlay(&host) else {
            unreachable!()
        };
        assert!(!p.entries.is_empty());
        assert!(p
            .entries
            .iter()
            .all(|r| r.label.to_lowercase().contains("split")));
        host.handle(HostEvent::Overlay(OverlayInput::PaletteFilter(
            String::new(),
        )))
        .unwrap();
        host.handle(HostEvent::Overlay(OverlayInput::SelectRow(2)))
            .unwrap();
        let OverlayView::Palette(p) = overlay(&host) else {
            unreachable!()
        };
        assert_eq!(p.selected, 2, "the palette's own movement got there");
        assert!(host
            .handle(HostEvent::Overlay(OverlayInput::SelectRow(p.entries.len())))
            .is_err());
        host.handle(HostEvent::Overlay(OverlayInput::PaletteFilter(
            "split".into(),
        )))
        .unwrap();

        assert!(
            host.handle(HostEvent::Overlay(OverlayInput::PaletteRun(
                PaletteAction::UnpairPhone
            )))
            .is_err(),
            "a row the palette does not offer is refused"
        );
        assert!(
            matches!(overlay(&host), OverlayView::Palette(_)),
            "still open"
        );

        host.handle(HostEvent::Overlay(OverlayInput::PaletteRun(
            PaletteAction::Dispatch(Command::ToggleSplitView),
        )))
        .unwrap();
        assert!(host.active_state().split_view);
        assert_eq!(message(&host), "Split view on.", "the palette closed first");
    }

    #[test]
    fn a_menu_run_of_a_palette_row_is_gated_like_the_palette() {
        let fakes = Fakes::new();
        let (mut state, _pty) = fakes.state_with_a_tab();
        state.isolated = true;
        let mut host = fakes.host(state);
        assert!(
            host.handle(HostEvent::RunPaletteAction(PaletteAction::OpenProject))
                .is_err(),
            "an isolated run offers no project actions (SPECS §32)"
        );
        assert_eq!(host.overlay(), None);
    }

    /// A front-end row is queued for the desktop to perform, in order, and
    /// taken once; the host does nothing else with it.
    #[test]
    fn a_desktop_front_end_takes_the_front_end_rows_it_chose() {
        use crate::tui::palette::FrontEndAction;
        let fakes = Fakes::new();
        let (state, _pty) = fakes.state_with_a_tab();
        let mut host = fakes.host(state);
        host.set_desktop_front_end();
        host.handle(HostEvent::RunPaletteAction(PaletteAction::FrontEnd(
            FrontEndAction::ConnectToRemote,
        )))
        .unwrap();
        host.handle(HostEvent::OpenPalette).unwrap();
        host.handle(HostEvent::Overlay(OverlayInput::PaletteRun(
            PaletteAction::FrontEnd(FrontEndAction::NewWindow),
        )))
        .unwrap();
        assert_eq!(
            host.take_front_end_actions(),
            [FrontEndAction::ConnectToRemote, FrontEndAction::NewWindow]
        );
        assert!(host.take_front_end_actions().is_empty(), "taken once");
        assert_eq!(host.overlay(), None, "the palette closed");
    }

    /// The TUI never enabled the front-end rows, so it cannot be made to queue
    /// one it has no way to perform.
    #[test]
    fn a_tui_front_end_refuses_the_front_end_rows() {
        use crate::tui::palette::FrontEndAction;
        let fakes = Fakes::new();
        let (state, _pty) = fakes.state_with_a_tab();
        let mut host = fakes.host(state);
        assert!(host
            .handle(HostEvent::RunPaletteAction(PaletteAction::FrontEnd(
                FrontEndAction::NewWindow,
            )))
            .is_err());
        assert!(host.take_front_end_actions().is_empty());
    }

    /// The desktop front-end flag reaches the help overlay; without it the
    /// screen is the TUI's, byte for byte.
    #[test]
    fn a_desktop_front_end_gets_the_desktop_help_document() {
        let fakes = Fakes::new();
        let (state, _pty) = fakes.state_with_a_tab();
        let mut host = fakes.host(state);
        host.set_desktop_front_end();
        host.handle(HostEvent::OpenHelp).unwrap();
        assert_eq!(
            overlay(&host),
            OverlayView::Help(crate::tui::help::help_doc_for(false, false, true))
        );
    }

    #[test]
    fn help_and_about_are_the_tuis_documents() {
        let fakes = Fakes::new();
        let (state, _pty) = fakes.state_with_a_tab();
        let mut host = fakes.host(state);
        host.handle(HostEvent::OpenHelp).unwrap();
        assert_eq!(
            overlay(&host),
            OverlayView::Help(crate::tui::help::help_doc(false, false))
        );
        host.handle(HostEvent::Overlay(OverlayInput::Cancel))
            .unwrap();
        assert_eq!(host.overlay(), None);

        command(&mut host, Command::ShowAbout);
        assert_eq!(
            overlay(&host),
            OverlayView::About(crate::tui::help::about_doc())
        );
        host.handle(HostEvent::Overlay(OverlayInput::Key(OverlayKey::Char('q'))))
            .unwrap();
        assert_eq!(host.overlay(), None, "any key dismisses, as in the TUI");
        assert!(!host.should_quit(), "and it never reached the key map");
    }

    #[test]
    fn the_git_status_panel_reads_out_what_collect_status_found() {
        let fakes = Fakes::new();
        let (state, _pty) = fakes.state_with_a_tab();
        let mut host = fakes.host(state);
        command(&mut host, Command::ShowGitStatus);
        let OverlayView::GitStatus(view) = overlay(&host) else {
            panic!("the status panel: {:?}", host.overlay());
        };
        assert_eq!(view.status.branch, branch(&host));
        assert_eq!(view.status.base_branch, "main");
        host.handle(HostEvent::Overlay(OverlayInput::Submit))
            .unwrap();
        assert_eq!(host.overlay(), None);
    }

    #[test]
    fn the_config_manager_shows_layering_and_saves_a_named_edit() {
        let fakes = Fakes::new();
        let (state, _pty) = fakes.state_with_a_tab();
        let mut host = fakes.host(state);
        run(&mut host, PaletteAction::OpenConfig);
        let OverlayView::Config(view) = overlay(&host) else {
            panic!("the configuration manager");
        };
        assert_eq!(view.project_name, "proj");
        assert_eq!(view.scope, ConfigScope::Project);
        assert_eq!(view.rows.len(), view.inherited.len());
        let row = |v: &ConfigView| {
            v.rows
                .iter()
                .find(|r| r.key == "notifications.enabled")
                .cloned()
                .expect("a curated row")
        };
        assert_ne!(row(&view).origin, Origin::SetHere, "not overridden yet");
        assert!(!view.dirty);

        let refused = host.handle(HostEvent::Overlay(OverlayInput::ConfigSet {
            scope: ConfigScope::Project,
            key: "no.such_key".into(),
            value: Some(FieldValue::Bool(true)),
        }));
        assert!(refused.is_err(), "a key this build lacks is refused");

        host.handle(HostEvent::Overlay(OverlayInput::ConfigSet {
            scope: ConfigScope::Project,
            key: "notifications.enabled".into(),
            value: Some(FieldValue::Bool(false)),
        }))
        .unwrap();
        let OverlayView::Config(view) = overlay(&host) else {
            unreachable!()
        };
        assert_eq!(row(&view).origin, Origin::SetHere);
        assert!(!row(&view).bool_value);
        assert!(view.dirty);

        host.handle(HostEvent::Overlay(OverlayInput::ConfigSave))
            .unwrap();
        let OverlayView::Config(view) = overlay(&host) else {
            unreachable!()
        };
        assert_eq!(view.status.as_deref(), Some("Saved."));
        let written = fakes
            .fs
            .file_contents(Path::new("/repo/.flightdeck/config.toml"))
            .expect("written through the fs seam");
        assert!(written.contains("enabled = false"), "{written}");

        host.handle(HostEvent::Overlay(OverlayInput::Cancel))
            .unwrap();
        assert_eq!(host.overlay(), None);
    }

    #[test]
    fn the_folder_browser_and_the_base_picker_read_out() {
        let fakes = Fakes::new();
        let (state, _pty) = fakes.state_with_a_tab();
        let mut host = fakes.host(state);
        run(&mut host, PaletteAction::OpenProject);
        let d = dialog(&host);
        assert_eq!(
            d.kind,
            DialogKind::OpenProject {
                dir: PathBuf::from("/")
            }
        );
        assert_eq!(d.input.as_deref(), Some(""));
        assert!(!d.list_filter);
        host.handle(HostEvent::Overlay(OverlayInput::Cancel))
            .unwrap();

        run(&mut host, PaletteAction::ChangeProjectBase);
        let d = dialog(&host);
        assert_eq!(d.kind, DialogKind::ChangeProjectBase);
        assert!(d.list_filter, "typing filters the branches");
        assert!(d.list.iter().any(|r| r.label == "main" && r.selected));
        host.handle(HostEvent::Overlay(OverlayInput::SetText("zzz".into())))
            .unwrap();
        assert_eq!(
            dialog(&host).list,
            vec![DialogRow {
                label: "(no matching local branches)".into(),
                selected: false
            }]
        );
        host.handle(HostEvent::Overlay(OverlayInput::Cancel))
            .unwrap();
        assert_eq!(host.overlay(), None);
    }

    #[test]
    fn close_project_refuses_the_last_one_and_closes_another() {
        let fakes = Fakes::new();
        let (state, _pty) = fakes.state_with_a_tab();
        let mut host = fakes.host(state);
        // Refused before it even asks, in the TUI's words.
        run(&mut host, PaletteAction::CloseProject);
        assert_eq!(
            message(&host),
            "Can't close the only project. Use Ctrl-q to quit FlightDeck."
        );
        assert_eq!(host.project_count(), 1);

        let other = AppState::new(
            Config::default(),
            default_state("main"),
            "/repo2",
            "/repo2/state.json",
        );
        host.workspace
            .projects
            .push(project("other", "/repo2", other, Arc::new(FakeGit::new())));
        run(&mut host, PaletteAction::CloseProject);
        let d = dialog(&host);
        assert_eq!(d.kind, DialogKind::CloseProject { index: 0 });
        assert_eq!(button(&d, "y").role, ButtonRole::Destructive);
        choose(&mut host, "n");
        assert_eq!(host.project_count(), 2, "cancel closes nothing");
        run(&mut host, PaletteAction::CloseProject);
        choose(&mut host, "y");
        assert_eq!(message(&host), "Closed project 'proj'.");
        assert_eq!(host.project_count(), 1);
        assert_eq!(host.project_name(0), Some("other"));
    }

    #[test]
    fn unpair_is_offered_only_while_paired_and_confirms_first() {
        let fakes = Fakes::new();
        let (state, _pty) = fakes.state_with_a_tab();
        let mut host = fakes.host(state);
        assert!(
            host.handle(HostEvent::RunPaletteAction(PaletteAction::UnpairPhone))
                .is_err(),
            "nothing to forget"
        );

        host.remote_has_persisted_pairing = true;
        host.pump();
        run(&mut host, PaletteAction::UnpairPhone);
        let d = dialog(&host);
        assert_eq!(d.kind, DialogKind::UnpairPhone);
        assert_eq!(button(&d, "y").role, ButtonRole::Destructive);
        choose(&mut host, "y");
        host.pump();
        assert_eq!(message(&host), "Phone unpaired.");
        assert!(!host.remote_has_persisted_pairing);
    }

    #[test]
    fn pairing_reports_remote_disabled_and_hands_out_the_qr_payload() {
        let fakes = Fakes::new();
        let (state, _pty) = fakes.state_with_a_tab();
        let mut host = fakes.host(state);
        run(&mut host, PaletteAction::PairPhone);
        host.pump();
        assert_eq!(
            message(&host),
            "FlightDeck Remote is disabled — enable it in configuration to pair a phone."
        );
        host.handle(HostEvent::Overlay(OverlayInput::Cancel))
            .unwrap();

        // A relay-less pairing attempt, driven as the relay frames would.
        let mut session = crate::remote::pairing::PairingSession::begin("wss://relay.test");
        session.on_offered(
            flightdeck_remote_protocol::PairingId::new("pair-1"),
            "1234".to_string(),
            i64::MAX,
        );
        host.pairing_session = Some(session);
        host.ui.overlay = crate::tui::render::UiOverlay::Remote(Default::default());
        host.pump();
        let OverlayView::Pairing(view) = overlay(&host) else {
            panic!("the pairing overlay: {:?}", host.overlay());
        };
        assert_eq!(view.code.as_deref(), Some("1234"));
        assert!(
            view.qr_payload
                .as_deref()
                .is_some_and(|p| p.starts_with("fdr1:")),
            "the GUI draws its own QR of the same payload: {:?}",
            view.qr_payload
        );
        assert!(!view.done && !view.failed);

        host.handle(HostEvent::Overlay(OverlayInput::Cancel))
            .unwrap();
        assert_eq!(host.overlay(), None);
        host.pump();
        assert!(
            host.pairing_session.is_none(),
            "dismissing drops the attempt"
        );
    }

    #[test]
    fn the_web_access_overlay_opens_on_start_and_answers_its_own_keys() {
        let fakes = Fakes::new();
        let (mut state, _pty) = fakes.state_with_a_tab();
        state.config.web.bind = "127.0.0.1".into();
        state.config.web.port = 0;
        let mut host = fakes.host(state);
        run(&mut host, PaletteAction::StartWebInterface);
        host.publish();
        let OverlayView::WebAccess(access) = overlay(&host) else {
            panic!("the access overlay: {:?}", host.overlay());
        };
        assert_eq!(access.view.mode, Some(AccessMode::LocalOnly));
        assert_eq!(access.qr_payload, None, "loopback shows no code, so no QR");
        assert!(
            host.handle(HostEvent::Overlay(OverlayInput::Choose("Enter".into())))
                .is_err(),
            "it has keys, not dialog buttons"
        );

        host.handle(HostEvent::Overlay(OverlayInput::WebAccess(AccessKey::Esc)))
            .unwrap();
        assert_eq!(host.overlay(), None);
        host.stop_services();
    }

    #[test]
    fn notices_are_the_status_bar_hints() {
        let fakes = Fakes::new();
        let (mut state, _pty) = fakes.state_with_a_tab();
        state.update_available = Some("9.9.9".into());
        state.isolated = true;
        let host = fakes.host(state);
        let notices = host.notices();
        assert_eq!(
            notices.update.map(|u| u.latest_version).as_deref(),
            Some("9.9.9")
        );
        assert!(notices.isolated);
    }

    #[test]
    fn remote_status_reports_the_web_viewers_and_the_phone() {
        let fakes = Fakes::new();
        let (mut state, _pty) = fakes.state_with_a_tab();
        state.config.web.bind = "127.0.0.1".into();
        state.config.web.port = 0;
        let mut host = fakes.host(state);
        assert_eq!(host.remote_status(), RemoteStatus::default());

        run(&mut host, PaletteAction::StartWebInterface);
        host.publish();
        host.ui.remote_paired = true;
        let status = host.remote_status();
        assert!(status.web_running);
        assert_eq!(status.web_viewers, 0, "listening, nobody attached");
        assert!(status.phone_paired);
        host.stop_services();
    }

    #[test]
    fn the_overlay_api_names_no_terminal_ui_library() {
        for (file, source) in [
            ("overlay.rs", include_str!("overlay.rs")),
            ("mod.rs", include_str!("mod.rs")),
            ("testing.rs", include_str!("testing.rs")),
        ] {
            for library in ["ratatui", "crossterm"] {
                assert!(
                    !source.contains(library),
                    "src/host/{file} names `{library}`"
                );
            }
        }
    }
}

/// The accessors a GUI front-end renders from, and the native folder picker's
/// `OpenProject` event.
mod front_end_reads {
    use super::*;
    use crate::host::testing::{self, TestProject};
    use crate::view::{AgentBadge, ProjectStatus};

    fn host_over<'a>(fakes: &'a Fakes, projects: Vec<TestProject>) -> AppHost<'a> {
        testing::host(fakes.env(), &fakes.notifier, projects, 0)
    }

    fn message(host: &AppHost) -> String {
        match host.overlay() {
            Some(OverlayView::Message(m)) => m.text,
            other => panic!("expected a message, got {other:?}"),
        }
    }

    #[test]
    fn project_tabs_are_one_view_per_project_with_the_active_one_marked() {
        let fakes = Fakes::new();
        let mut host = host_over(
            &fakes,
            vec![
                TestProject::new("alpha", &["a1", "a2"]),
                TestProject::new("beta", &[]),
            ],
        );
        let tabs = host.project_tabs();
        assert_eq!(
            tabs.iter()
                .map(|t| (t.name.as_str(), t.agent_count, t.active))
                .collect::<Vec<_>>(),
            vec![("alpha", 2, true), ("beta", 0, false)]
        );
        assert_eq!(tabs[1].status, ProjectStatus::Idle);

        host.handle(HostEvent::SwitchProject(Selector::Next))
            .unwrap();
        assert!(host.project_tabs()[1].active, "the switch shows on the row");
    }

    #[test]
    fn agent_rows_and_git_strip_read_the_active_project() {
        let fakes = Fakes::new();
        let mut host = host_over(&fakes, vec![TestProject::new("alpha", &["a1", "a2"])]);
        let status = WorktreeStatus {
            branch: "flightdeck/a1".into(),
            base_branch: "main".into(),
            dirty: false,
            changes: crate::git::status::WorktreeChanges::default(),
            lines: crate::git::status::LineStats::default(),
            ahead: 2,
            behind: 0,
            upstream: Some("origin/flightdeck/a1".into()),
            base_drift: 0,
            worktree_path: PathBuf::from("/alpha/.flightdeck/worktrees/a1"),
        };
        testing::set_git_status(&mut host, 0, "t0", status);

        let rows = host.agent_rows();
        assert_eq!(
            rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            ["a1", "a2"]
        );
        assert!(rows[0].selected);
        assert_eq!(rows[0].badge, AgentBadge::Idle);
        assert_eq!(rows[0].alt_index, Some(1));

        let strip = host.git_strip();
        let agent = strip.agent.expect("the first agent is selected");
        assert_eq!(agent.name, "a1");
        assert_eq!(agent.target_branch, "main");
        assert!(agent.actions.push && agent.actions.finish);
    }

    #[test]
    fn mode_bar_follows_the_active_input_mode() {
        let fakes = Fakes::new();
        let mut host = host_over(&fakes, vec![TestProject::new("alpha", &["a1"])]);
        host.handle(HostEvent::FocusTerminal).unwrap();
        assert_eq!(host.mode_bar("Alt+Esc", "F1").pill, "MODE: TERMINAL");
        host.handle(HostEvent::FocusApp).unwrap();
        let bar = host.mode_bar("Alt+Esc", "F1");
        assert_eq!(bar.pill, "MODE: APP");
        assert!(!bar.isolated);
    }

    #[test]
    fn open_project_refuses_a_folder_outside_any_git_repository() {
        let fakes = Fakes::new();
        let mut host = host_over(&fakes, vec![TestProject::new("alpha", &[])]);
        let not_a_repo = TempDir::new().unwrap();
        host.handle(HostEvent::OpenProject(not_a_repo.path().to_path_buf()))
            .unwrap();
        assert!(
            message(&host).starts_with("Could not open project"),
            "the folder browser's own refusal"
        );
        assert_eq!(host.project_count(), 1, "nothing was opened");
    }

    #[test]
    fn open_project_is_refused_in_an_isolated_run() {
        let fakes = Fakes::new();
        let mut project = TestProject::new("alpha", &[]);
        project.state.isolated = true;
        let mut host = host_over(&fakes, vec![project]);
        host.handle(HostEvent::OpenProject(fakes.dir.path().to_path_buf()))
            .unwrap();
        assert!(message(&host).contains("isolated"));
        assert_eq!(host.project_count(), 1);
    }
}

mod prompts;

/// FlightDeck Desktop as a remote control, end to end
/// (`specs/DESKTOP_REMOTE_CONTROL_PLAN.md`): a real [`AppHost`] with its
/// embedded web server started the way the palette starts it, and the core's
/// native client ([`crate::web::client::RemoteClient`]) attached to it over a
/// real socket. Nothing between them is faked — the host's own tick serves
/// the client, and the client's frames run the host's own dispatch path.
mod native_remote_client {
    use super::*;
    use crate::contracts::{PtySession, TabId};
    use crate::tui::palette::PaletteAction;
    use crate::web::access::AccessKey;
    use crate::web::client::views::{dialog_input, DialogDraft};
    use crate::web::client::{exchange_code, LinkConfig, RemoteClient, Target};
    use crate::web::protocol::{command as names, SeatRequest};
    use std::time::{Duration, Instant};

    fn until(
        host: &mut AppHost,
        client: &mut RemoteClient,
        what: &str,
        mut done: impl FnMut(&mut AppHost, &mut RemoteClient) -> bool,
    ) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            host.tick();
            client.pump();
            if done(host, client) {
                return;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn a_native_client_browses_and_drives_a_tab_the_host_is_not_showing() {
        let fakes = Fakes::new();
        let (mut state, task_pty) = fakes.state_with_a_tab();
        let second_pty = fakes.pty.queue_session();
        state
            .dispatch(
                Command::NewAgentTab {
                    name: "Second".to_string(),
                    agent_key: None,
                },
                &fakes.services(),
            )
            .unwrap();
        assert_eq!(state.selected_tab, Some(1), "the host shows Second");
        state.config.web.bind = "127.0.0.1".into();
        state.config.web.port = 0;
        let mut host = fakes.host(state);
        host.handle(HostEvent::RunPaletteAction(
            PaletteAction::StartWebInterface,
        ))
        .unwrap();
        host.publish();
        host.handle(HostEvent::Overlay(OverlayInput::WebAccess(AccessKey::Esc)))
            .unwrap();

        // Pair with a code, as the connect window does.
        let address = host
            .web_surface
            .handle
            .as_ref()
            .expect("listening")
            .bound_addr()
            .to_string();
        let code = host
            .web_surface
            .credentials
            .lock()
            .unwrap()
            .mint_bootstrap_code()
            .reveal()
            .to_string();
        let token = crate::remote::runtime::shared()
            .handle()
            .block_on(exchange_code(&address, &code, "FlightDeck Desktop/test"))
            .expect("the code is exchanged");
        let mut client = RemoteClient::connect(LinkConfig {
            address,
            token,
            seat: SeatRequest::Write,
            user_agent: "FlightDeck Desktop/test".to_string(),
        });
        until(&mut host, &mut client, "the snapshot", |_, c| {
            c.state().is_live() && c.workspace().ready
        });

        // Browse to Task, here only (R2).
        let task = TabId(host.active_state().tabs[0].meta.id.clone());
        assert!(client.workspace_mut().select_session(&task));
        let task_terminal = client
            .workspace()
            .selected_terminal()
            .expect("Task's agent terminal is mirrored")
            .terminal_id
            .clone();

        // A targeted command opens the shared dialog; answering it acts on Task.
        client
            .command_for(names::SET_MANUAL_STATUS, &Target::Session(task.clone()))
            .expect("live");
        until(&mut host, &mut client, "the shared dialog", |_, c| {
            c.workspace().dialog.is_some()
        });
        let view = client.workspace().dialog.clone().unwrap();
        let mut draft = DialogDraft::new(&view);
        let reply = dialog_input(&view, &mut draft, &OverlayInput::Choose("b".into()))
            .expect("Blocked answers it");
        client.command(reply.name, Some(reply.args)).expect("live");
        until(&mut host, &mut client, "Task blocked", |h, _| {
            h.active_state().tabs[0].meta.manual_status.as_deref() == Some("blocked")
        });
        assert_eq!(host.active_state().tabs[1].meta.manual_status, None);
        assert_eq!(
            host.active_state().selected_tab,
            Some(1),
            "the host is still showing Second"
        );

        // Keystrokes reach Task's PTY and no other.
        let mut pty = client.pty(&task_terminal);
        pty.write_input(b"echo hi\r").unwrap();
        until(&mut host, &mut client, "the keystrokes", |_, _| {
            task_pty.input().ends_with(b"echo hi\r")
        });
        assert!(!second_pty.input().ends_with(b"echo hi\r"));

        // And Task's output reaches the client although the host is not
        // showing it.
        task_pty.push_output("output from task");
        let mut seen = Vec::new();
        until(&mut host, &mut client, "Task's bytes", |_, _| {
            seen.extend(pty.try_read_output().unwrap());
            String::from_utf8_lossy(&seen).contains("output from task")
        });

        client.stop();
        host.stop_services();
    }
}
