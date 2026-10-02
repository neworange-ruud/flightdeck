//! The shell, driven through GPUI's headless test platform against a real
//! [`AppHost`](flightdeck::host::AppHost) over the core's fakes
//! (`flightdeck::host::testing`, the root crate's `testing` feature).
//!
//! Clicks land on elements found by their debug selectors; keys go through
//! the real keymap bindings, key contexts and focus. What a control did is
//! read two ways: the host's state after it, and the exact `HostEvent`s the
//! host model received ([`HostModel::dispatched`]) — which is how a button is
//! proven to send what its chord sends.

use flightdeck::app::commands::{Command, Selector};
use std::path::Path;

use flightdeck::app::modes::InputMode;
use flightdeck::contracts::PtySize;
use flightdeck::host::testing::{self, TestProject};
use flightdeck::host::{DialogKind, HostEvent, OverlayInput, OverlayView};
use flightdeck::testing::{
    FakeClock, FakeCommandRunner, FakeContainerRuntime, FakeFs, FakeNotifier, FakePty,
    FakePtyHandle,
};
use flightdeck::Env;
use gpui::{AppContext, Entity, Modifiers, TestAppContext, VisualTestContext};
use gpui_component::Root;

use crate::host::HostModel;
use crate::shell::FlightDeckWindow;

/// The services a test host borrows, leaked to `'static` as the app leaks its
/// real ones (a few bytes per test).
struct Fakes {
    fs: FakeFs,
    pty: FakePty,
    clock: FakeClock,
    container: FakeContainerRuntime,
    command: FakeCommandRunner,
    notifier: FakeNotifier,
}

fn fakes() -> &'static Fakes {
    Box::leak(Box::new(Fakes {
        fs: FakeFs::new(),
        pty: FakePty::new(),
        clock: FakeClock::default(),
        container: FakeContainerRuntime::new(),
        command: FakeCommandRunner::new(),
        notifier: FakeNotifier::new(),
    }))
}

fn env(f: &'static Fakes) -> Env<'static> {
    Env {
        fs: &f.fs,
        pty: &f.pty,
        clock: &f.clock,
        container: &f.container,
        command: &f.command,
        terminal: crate::terminal::desktop_profile(),
    }
}

/// Open the app's window over `projects` (the first active), as `app::run`
/// does minus the real services and the tick loop (tests take turns
/// themselves).
fn open(
    app: &mut TestAppContext,
    projects: Vec<TestProject>,
) -> (&'static Fakes, Entity<HostModel>, &mut VisualTestContext) {
    open_with(app, fakes(), projects)
}

/// [`open`] over fakes the caller already used (to spawn a project's agent).
fn open_with<'a>(
    app: &'a mut TestAppContext,
    f: &'static Fakes,
    projects: Vec<TestProject>,
) -> (&'static Fakes, Entity<HostModel>, &'a mut VisualTestContext) {
    let host = testing::host(env(f), &f.notifier, projects, 0);
    app.update(|cx| {
        gpui_component::init(cx);
        crate::theme::init(cx);
        flightdeck_desktop::keys::register(cx, crate::commands::keymap());
        // Under an open overlay every Global chord is disabled (the modal
        // swallows keys, as in the TUI).
        flightdeck_desktop::overlays::register(cx, crate::commands::keymap());
    });
    let model = app.new(|_| HostModel::new(host));
    let for_window = model.clone();
    let (_root, cx) = app.add_window_view(|window, cx| {
        let view = cx.new(|cx| FlightDeckWindow::new(for_window, cx));
        Root::new(view, window, cx)
    });
    cx.run_until_parked();
    (f, model, cx)
}

fn two_projects() -> Vec<TestProject> {
    vec![
        TestProject::new("alpha", &["a1", "a2"]),
        TestProject::new("beta", &["b1"]),
    ]
}

/// Click the centre of the element tagged `selector`.
fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let bounds = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is not on screen"));
    cx.simulate_click(bounds.center(), Modifiers::none());
    cx.run_until_parked();
}

/// Take (and clear) what the host model received.
fn take_dispatched(model: &Entity<HostModel>, cx: &mut VisualTestContext) -> Vec<HostEvent> {
    model.update(cx, |m, _| std::mem::take(&mut m.dispatched))
}

/// Dismiss whatever overlay a control opened, so the next one starts clean.
fn close_overlays(model: &Entity<HostModel>, cx: &mut VisualTestContext) {
    for _ in 0..4 {
        let open = model.read_with(cx, |m, _| m.host().overlay().is_some());
        if !open {
            break;
        }
        model.update(cx, |m, cx| {
            m.dispatch(HostEvent::Overlay(OverlayInput::Cancel), cx)
        });
    }
    cx.run_until_parked();
}

fn active_project(model: &Entity<HostModel>, cx: &mut VisualTestContext) -> usize {
    model.read_with(cx, |m, _| m.host().active_project_index())
}

#[gpui::test]
fn one_tab_per_open_project(app: &mut TestAppContext) {
    let (_f, _model, cx) = open(app, two_projects());
    assert!(cx.debug_bounds("project-tab-0").is_some());
    assert!(cx.debug_bounds("project-tab-1").is_some());
    assert!(
        cx.debug_bounds("project-tab-2").is_none(),
        "two projects, two tabs"
    );
    // The active tab carries the close button when another project is open.
    assert!(cx.debug_bounds("project-close-0").is_some());
    assert!(cx.debug_bounds("project-close-1").is_none());
}

#[gpui::test]
fn clicking_a_tab_switches_project(app: &mut TestAppContext) {
    let (_f, model, cx) = open(app, two_projects());
    click(cx, "project-tab-1");
    assert_eq!(active_project(&model, cx), 1);
    assert_eq!(
        take_dispatched(&model, cx),
        [HostEvent::SwitchProject(Selector::Index(1))]
    );
}

#[gpui::test]
fn shift_right_and_left_cycle_projects(app: &mut TestAppContext) {
    let (_f, model, cx) = open(app, two_projects());
    cx.simulate_keystrokes("shift-right");
    assert_eq!(active_project(&model, cx), 1);
    cx.simulate_keystrokes("shift-left");
    assert_eq!(active_project(&model, cx), 0);
    assert_eq!(
        take_dispatched(&model, cx),
        [
            HostEvent::SwitchProject(Selector::Next),
            HostEvent::SwitchProject(Selector::Prev)
        ]
    );
}

#[gpui::test]
fn clicking_a_sidebar_row_selects_that_agent_in_app_mode(app: &mut TestAppContext) {
    let (_f, model, cx) = open(app, two_projects());
    assert!(cx.debug_bounds("agent-row-1").is_some());
    click(cx, "agent-row-1");
    let (selected, mode) = model.read_with(cx, |m, _| {
        let state = m.host().active_state();
        (state.selected_tab, state.mode())
    });
    assert_eq!(selected, Some(1));
    assert_eq!(mode, InputMode::App);
    assert_eq!(
        take_dispatched(&model, cx),
        [
            // Exactly Alt-2's command, then APP mode, as the TUI's click.
            HostEvent::Command(Command::SwitchAgentTab(Selector::Index(1))),
            HostEvent::FocusApp,
        ]
    );
}

#[gpui::test]
fn alt_digit_and_arrows_navigate_agents(app: &mut TestAppContext) {
    let (_f, model, cx) = open(app, two_projects());
    let selected = |model: &Entity<HostModel>, cx: &mut VisualTestContext| {
        model.read_with(cx, |m, _| m.host().active_state().selected_tab)
    };
    cx.simulate_keystrokes("alt-2");
    assert_eq!(selected(&model, cx), Some(1));
    cx.simulate_keystrokes("up");
    assert_eq!(selected(&model, cx), Some(0), "bare Up in APP mode");
    cx.simulate_keystrokes("alt-down");
    assert_eq!(selected(&model, cx), Some(1));
}

#[gpui::test]
fn every_button_dispatches_what_its_chord_dispatches(app: &mut TestAppContext) {
    let (_f, model, cx) = open(app, two_projects());
    // (button, chord): each pair must send the same host event.
    let pairs = [
        ("new-agent", "ctrl-n"),
        ("new-shell", "ctrl-t"),
        ("git-push", "ctrl-p"),
        ("git-pull-base", "ctrl-u"),
        ("git-finish", "ctrl-f"),
        ("command-field", "ctrl-g"),
        ("hint-OpenHelp", "f1"),
    ];
    for (button, chord) in pairs {
        model.update(cx, |m, cx| m.dispatch(HostEvent::FocusApp, cx));
        cx.run_until_parked();
        take_dispatched(&model, cx);

        click(cx, button);
        let by_click = take_dispatched(&model, cx);
        close_overlays(&model, cx);
        take_dispatched(&model, cx);

        cx.simulate_keystrokes(chord);
        let by_chord = take_dispatched(&model, cx);
        close_overlays(&model, cx);

        assert!(!by_click.is_empty(), "{button} dispatched nothing");
        assert_eq!(by_click, by_chord, "{button} vs {chord}");
    }
}

#[gpui::test]
fn the_mode_pill_leaves_the_current_mode(app: &mut TestAppContext) {
    let (_f, model, cx) = open(app, two_projects());
    model.update(cx, |m, cx| m.dispatch(HostEvent::FocusApp, cx));
    cx.run_until_parked();
    take_dispatched(&model, cx);
    click(cx, "mode-pill");
    assert_eq!(take_dispatched(&model, cx), [HostEvent::FocusTerminal]);
}

#[gpui::test]
fn a_disabled_git_button_dispatches_nothing(app: &mut TestAppContext) {
    // An agent running on the base branch has nothing to merge back: Finish
    // is disabled, while Push stays live.
    let mut project = TestProject::new("alpha", &["on-base"]);
    project.state.tabs[0].meta.runs_on_base = true;
    let (_f, model, cx) = open(app, vec![project]);
    take_dispatched(&model, cx);

    click(cx, "git-finish");
    assert_eq!(take_dispatched(&model, cx), [], "disabled Finish is inert");

    click(cx, "git-push");
    assert_eq!(
        take_dispatched(&model, cx),
        [HostEvent::Command(Command::PushBranch { confirm: None })]
    );
}

#[gpui::test]
fn with_no_agent_every_git_button_is_disabled(app: &mut TestAppContext) {
    let (_f, model, cx) = open(app, vec![TestProject::new("empty", &[])]);
    take_dispatched(&model, cx);
    for button in ["git-pull-base", "git-finish", "git-push"] {
        click(cx, button);
        assert_eq!(take_dispatched(&model, cx), [], "{button}");
    }
}

#[gpui::test]
fn the_overlay_layer_answers_the_live_host(app: &mut TestAppContext) {
    let (_f, model, cx) = open(app, two_projects());

    // End to end: Ctrl-g opens the palette, typing filters it in the host,
    // Enter runs the highlighted row.
    cx.simulate_keystrokes("ctrl-g");
    assert!(matches!(
        model.read_with(cx, |m, _| m.host().overlay()),
        Some(OverlayView::Palette(_))
    ));
    assert!(
        cx.debug_bounds("palette-row-0").is_some(),
        "the layer draws the palette"
    );
    cx.simulate_keystrokes("a b o u t");
    let filtered = match model.read_with(cx, |m, _| m.host().overlay()) {
        Some(OverlayView::Palette(p)) => p,
        other => panic!("expected the palette, got {other:?}"),
    };
    assert_eq!(filtered.filter, "about");
    assert_eq!(filtered.entries[0].label, "About FlightDeck");
    cx.simulate_keystrokes("enter");
    assert!(matches!(
        model.read_with(cx, |m, _| m.host().overlay()),
        Some(OverlayView::About(_))
    ));
    take_dispatched(&model, cx);
    cx.simulate_keystrokes("escape");
    assert_eq!(
        take_dispatched(&model, cx),
        [HostEvent::Overlay(OverlayInput::Cancel)]
    );
    assert_eq!(model.read_with(cx, |m, _| m.host().overlay()), None);

    // Help is drawn by its own overlay view, and Esc still closes it.
    cx.simulate_keystrokes("f1");
    assert!(matches!(
        model.read_with(cx, |m, _| m.host().overlay()),
        Some(OverlayView::Help(_))
    ));
    cx.simulate_keystrokes("escape");
    assert_eq!(model.read_with(cx, |m, _| m.host().overlay()), None);

    // While an overlay is open a table chord is not performed: Shift-Right
    // stays with the overlay instead of switching project.
    cx.simulate_keystrokes("ctrl-g");
    cx.simulate_keystrokes("shift-right");
    assert_eq!(active_project(&model, cx), 0);
    close_overlays(&model, cx);

    // Typing reaches a text prompt key by key, and Enter submits it.
    model.update(cx, |m, cx| {
        m.dispatch(
            HostEvent::Command(Command::RenameAgentTab {
                new_name: String::new(),
            }),
            cx,
        )
    });
    cx.run_until_parked();
    cx.simulate_keystrokes("r e n a m e d enter");
    let name = model.read_with(cx, |m, _| m.host().active_state().tabs[0].meta.name.clone());
    assert_eq!(name, "renamed");
    close_overlays(&model, cx);

    // A dialog button is `Choose(id)`: the manual-status prompt's first one.
    cx.simulate_keystrokes("ctrl-s");
    let dialog = match model.read_with(cx, |m, _| m.host().overlay()) {
        Some(OverlayView::Dialog(d)) => d,
        other => panic!("expected the manual status prompt, got {other:?}"),
    };
    assert_eq!(dialog.kind, DialogKind::SetManualStatus);
    let first = dialog.buttons[0].id.clone();
    let selector: &'static str = Box::leak(format!("overlay-button-{first}").into_boxed_str());
    take_dispatched(&model, cx);
    click(cx, selector);
    assert_eq!(
        take_dispatched(&model, cx),
        [HostEvent::Overlay(OverlayInput::Choose(first))]
    );
}

#[gpui::test]
fn a_turn_redraws_and_teardown_saves_once(app: &mut TestAppContext) {
    let (f, model, cx) = open(app, two_projects());
    model.update(cx, |m, cx| {
        m.turn(cx);
    });
    model.update(cx, |m, _| {
        m.teardown();
        // Idempotent: the quit hook and a signal may both ask.
        m.teardown();
    });
    let saved =
        f.fs.file_contents(std::path::Path::new("/alpha/.flightdeck/state.json"))
            .expect("the active project's state is persisted");
    assert!(saved.contains("a1"), "{saved}");
    assert!(f
        .fs
        .file_contents(std::path::Path::new("/beta/.flightdeck/state.json"))
        .is_some());
}

#[gpui::test]
fn plus_opens_the_picked_folder_through_the_host(app: &mut TestAppContext) {
    let (_f, model, cx) = open(app, two_projects());
    take_dispatched(&model, cx);
    click(cx, "open-project");
    let picked = tempfile::TempDir::new().expect("tempdir");
    let answer = picked.path().to_path_buf();
    let reply = answer.clone();
    cx.simulate_path_prompt_response(move |_| Some(vec![reply.clone()]));
    cx.run_until_parked();
    assert_eq!(
        take_dispatched(&model, cx),
        [HostEvent::OpenProject(answer)]
    );
    // An empty folder is no repository: the host refuses it with the TUI's
    // folder-browser message, and nothing opens.
    match model.read_with(cx, |m, _| m.host().overlay()) {
        Some(OverlayView::Message(m)) => {
            assert!(m.text.starts_with("Could not open project"), "{}", m.text)
        }
        other => panic!("expected the refusal, got {other:?}"),
    }
    assert_eq!(model.read_with(cx, |m, _| m.host().project_count()), 2);
}

#[gpui::test]
fn the_hosts_terminals_are_alacritty_answering_with_the_theme_colours(app: &mut TestAppContext) {
    let (_f, model, cx) = open(app, two_projects());
    let profiles: Vec<_> = model.read_with(cx, |m, _| {
        (0..m.host().project_count())
            .map(|i| m.host().project_state(i).unwrap().terminal_profile)
            .collect()
    });
    let palette = crate::theme::Palette::dark();
    let channels = crate::terminal::view::channels;
    for profile in profiles {
        // The desktop's choice; the TUI keeps vt100 (`TerminalProfile::TUI`).
        assert_eq!(
            profile.emulator,
            flightdeck::terminal::grid::Emulator::Alacritty
        );
        assert_eq!(
            profile.default_colors,
            Some((
                channels(palette.terminal_ink),
                channels(palette.surface_terminal)
            ))
        );
    }
}

#[gpui::test]
fn an_idle_turn_asks_for_no_redraw(app: &mut TestAppContext) {
    let (_f, model, cx) = open(app, two_projects());
    // The first turns settle whatever start-up changed.
    model.update(cx, |m, cx| {
        m.turn(cx);
        m.turn(cx);
    });
    let redrew = model.update(cx, |m, cx| m.turn(cx));
    assert!(!redrew, "nothing happened, so nothing is drawn");
}

// --- Terminals within a session, split view, spinners ------------------------

/// The PTY size agents spawn at in these tests.
const SPAWN: PtySize = crate::shell::NOMINAL_PTY_SIZE;

/// A project `alpha` whose one agent `a1` is running, and its PTY.
fn live_agent(f: &'static Fakes) -> (Vec<TestProject>, FakePtyHandle) {
    let mut project = TestProject::new("alpha", &["a1"]);
    let handle = f.pty.queue_session();
    let tab = &mut project.state.tabs[0];
    tab.session.set_profile(crate::terminal::desktop_profile());
    tab.session
        .spawn_primary(&f.pty, "claude", &[], Path::new("/alpha"), SPAWN)
        .expect("fake spawn");
    (vec![project], handle)
}

/// A debug selector `prefix-i`.
fn sel(prefix: &str, i: usize) -> &'static str {
    Box::leak(format!("{prefix}-{i}").into_boxed_str())
}

fn selected_child(model: &Entity<HostModel>, cx: &mut VisualTestContext) -> Option<usize> {
    model.read_with(cx, |m, _| {
        m.host()
            .active_state()
            .selected()
            .and_then(|t| t.session.selected_child())
    })
}

fn child_count(model: &Entity<HostModel>, cx: &mut VisualTestContext) -> usize {
    model.read_with(cx, |m, _| {
        m.host()
            .active_state()
            .selected()
            .map_or(0, |t| t.session.child_count())
    })
}

fn mode(model: &Entity<HostModel>, cx: &mut VisualTestContext) -> InputMode {
    model.read_with(cx, |m, _| m.host().active_state().mode())
}

fn dispatch(model: &Entity<HostModel>, event: HostEvent, cx: &mut VisualTestContext) {
    model.update(cx, |m, cx| m.dispatch(event, cx));
    cx.run_until_parked();
}

/// Ctrl-t (APP mode), with the PTY the new shell gets.
fn new_shell(f: &'static Fakes, cx: &mut VisualTestContext) -> FakePtyHandle {
    let handle = f.pty.queue_session();
    cx.simulate_keystrokes("ctrl-t");
    handle
}

/// The size each of the selected agent's terminals has now, agent first.
fn grid_sizes(model: &Entity<HostModel>, cx: &mut VisualTestContext) -> Vec<(u16, u16)> {
    model.read_with(cx, |m, _| {
        let host = m.host();
        let tab = host.active_state().selected().expect("an agent");
        flightdeck::view::terminal_views(tab)
            .iter()
            .map(|t| {
                host.tab_terminal_at(0, &tab.meta.id, t.target)
                    .expect("spawned")
                    .screen()
                    .size()
            })
            .collect()
    })
}

/// Draw, then take a turn: what a measured pane size needs to reach its PTY.
fn draw_and_turn(model: &Entity<HostModel>, cx: &mut VisualTestContext) {
    cx.run_until_parked();
    model.update(cx, |m, cx| {
        m.turn(cx);
    });
    cx.run_until_parked();
}

#[gpui::test]
fn left_right_and_alt_arrows_cycle_the_agents_terminals(app: &mut TestAppContext) {
    let f = fakes();
    let (projects, agent) = live_agent(f);
    let (_f, model, cx) = open_with(app, f, projects);
    dispatch(&model, HostEvent::FocusApp, cx);
    let _s1 = new_shell(f, cx);
    let _s2 = new_shell(f, cx);
    assert_eq!(child_count(&model, cx), 2);
    assert_eq!(selected_child(&model, cx), Some(1), "a new shell is active");
    // The sidebar nests all three under the agent: those rows are the tabs.
    for i in 0..3 {
        assert!(cx.debug_bounds(sel("terminal-row", i)).is_some(), "row {i}");
    }
    assert!(cx.debug_bounds(sel("terminal-row", 3)).is_none());

    // APP mode: bare Left / Right cycle agent → shell 1 → shell 2 → agent.
    cx.simulate_keystrokes("left");
    assert_eq!(selected_child(&model, cx), Some(0));
    cx.simulate_keystrokes("left");
    assert_eq!(selected_child(&model, cx), None, "the agent");
    cx.simulate_keystrokes("left");
    assert_eq!(selected_child(&model, cx), Some(1), "wraps");
    cx.simulate_keystrokes("right");
    assert_eq!(selected_child(&model, cx), None, "and back");
    // The main area is the active terminal.
    let on_screen = model.read_with(cx, |m, _| {
        m.host().active_terminal().map(|t| t.title.clone())
    });
    assert_eq!(on_screen.as_deref(), Some("claude"));

    // TERMINAL mode: Alt-Left / Alt-Right, and the mode stays.
    dispatch(&model, HostEvent::FocusTerminal, cx);
    cx.simulate_keystrokes("alt-right");
    assert_eq!(selected_child(&model, cx), Some(0));
    cx.simulate_keystrokes("alt-right");
    assert_eq!(selected_child(&model, cx), Some(1));
    cx.simulate_keystrokes("alt-left");
    assert_eq!(selected_child(&model, cx), Some(0));
    assert_eq!(mode(&model, cx), InputMode::Terminal);
    // Bare Left in TERMINAL mode is the program's, not a switch.
    cx.simulate_keystrokes("alt-left");
    cx.simulate_keystrokes("left");
    assert_eq!(selected_child(&model, cx), None);
    assert!(
        agent.input().ends_with(b"\x1b[D"),
        "{:?}",
        String::from_utf8_lossy(&agent.input())
    );
    // Alt-arrows work in APP mode too.
    dispatch(&model, HostEvent::FocusApp, cx);
    cx.simulate_keystrokes("alt-left");
    assert_eq!(selected_child(&model, cx), Some(1));
}

#[gpui::test]
fn clicking_a_nested_terminal_row_selects_and_focuses_it(app: &mut TestAppContext) {
    let f = fakes();
    let (projects, _agent) = live_agent(f);
    let (_f, model, cx) = open_with(app, f, projects);
    dispatch(&model, HostEvent::FocusApp, cx);
    let _s1 = new_shell(f, cx);
    let _s2 = new_shell(f, cx);
    dispatch(&model, HostEvent::FocusApp, cx);
    take_dispatched(&model, cx);

    click(cx, "terminal-row-1");
    assert_eq!(selected_child(&model, cx), Some(0), "shell 1");
    assert_eq!(
        mode(&model, cx),
        InputMode::Terminal,
        "and typing goes there"
    );
    assert_eq!(
        take_dispatched(&model, cx),
        [
            HostEvent::Command(Command::SwitchChildTerminal(Selector::Index(0))),
            HostEvent::FocusTerminal,
        ]
    );
    dispatch(&model, HostEvent::FocusApp, cx);
    click(cx, "terminal-row-0");
    assert_eq!(selected_child(&model, cx), None, "the agent");
    assert_eq!(mode(&model, cx), InputMode::Terminal);
}

#[gpui::test]
fn ctrl_t_adds_and_ctrl_w_closes_a_terminal_by_the_tuis_rules(app: &mut TestAppContext) {
    let f = fakes();
    let (projects, agent) = live_agent(f);
    let (_f, model, cx) = open_with(app, f, projects);
    dispatch(&model, HostEvent::FocusApp, cx);
    let shell = new_shell(f, cx);
    assert_eq!(child_count(&model, cx), 1);
    assert_eq!(selected_child(&model, cx), Some(0));
    assert!(
        cx.debug_bounds("terminal-row-1").is_some(),
        "its nested row"
    );

    // Ctrl-w asks first, as the TUI does; No keeps the shell.
    let confirm = |model: &Entity<HostModel>, cx: &mut VisualTestContext| match model
        .read_with(cx, |m, _| m.host().overlay())
    {
        Some(OverlayView::Dialog(d)) => {
            assert_eq!(
                d.kind,
                DialogKind::CloseTerminal {
                    label: "shell 1".to_string()
                }
            );
        }
        other => panic!("expected the close confirmation, got {other:?}"),
    };
    cx.simulate_keystrokes("ctrl-w");
    confirm(&model, cx);
    cx.simulate_keystrokes("n");
    assert_eq!(model.read_with(cx, |m, _| m.host().overlay()), None);
    assert_eq!(child_count(&model, cx), 1);
    assert!(!shell.terminated());

    // Yes closes the active shell (its process tree is ended) and the agent
    // is active again.
    cx.simulate_keystrokes("ctrl-w");
    confirm(&model, cx);
    cx.simulate_keystrokes("y");
    assert_eq!(child_count(&model, cx), 0);
    assert!(shell.terminated());
    assert_eq!(selected_child(&model, cx), None);
    assert!(cx.debug_bounds("terminal-row-1").is_none());

    // On the agent itself Ctrl-w is refused with the TUI's message (the agent
    // closes with its session, Ctrl-k, behind a confirmation).
    cx.simulate_keystrokes("ctrl-w");
    match model.read_with(cx, |m, _| m.host().overlay()) {
        Some(OverlayView::Message(m)) => {
            assert_eq!(m.text, "No child terminal selected.")
        }
        other => panic!("expected the refusal, got {other:?}"),
    }
    assert!(!agent.terminated());
    close_overlays(&model, cx);
    cx.simulate_keystrokes("ctrl-k");
    assert!(
        matches!(
            model.read_with(cx, |m, _| m.host().overlay()),
            Some(OverlayView::Dialog(_))
        ),
        "closing the agent asks first"
    );
    assert!(!agent.terminated());
}

#[gpui::test]
fn ctrl_b_lays_the_terminals_side_by_side_each_at_its_own_size(app: &mut TestAppContext) {
    let f = fakes();
    let (projects, _agent) = live_agent(f);
    let (_f, model, cx) = open_with(app, f, projects);
    dispatch(&model, HostEvent::FocusApp, cx);
    draw_and_turn(&model, cx);
    let single = grid_sizes(&model, cx)[0];

    // Two terminals, split: two panes, each with its header.
    let _s1 = new_shell(f, cx);
    assert!(cx.debug_bounds("split-pane-0").is_none(), "off by default");
    cx.simulate_keystrokes("ctrl-b");
    assert!(model.read_with(cx, |m, _| m.host().active_state().split_view));
    // The TUI's own confirmation of the toggle.
    match model.read_with(cx, |m, _| m.host().overlay()) {
        Some(OverlayView::Message(m)) => assert_eq!(m.text, "Split view on."),
        other => panic!("expected the toggle's message, got {other:?}"),
    }
    close_overlays(&model, cx);
    let bounds = |cx: &mut VisualTestContext, n: usize| -> Vec<gpui::Bounds<gpui::Pixels>> {
        (0..n)
            .map(|i| cx.debug_bounds(sel("split-pane", i)).expect("a pane"))
            .collect()
    };
    let two = bounds(cx, 2);
    assert!(cx.debug_bounds("split-pane-2").is_none());
    assert!(cx.debug_bounds("split-header-1").is_some());
    assert!(two[0].origin.x < two[1].origin.x, "left to right");
    assert_eq!(two[0].origin.y, two[1].origin.y);
    assert!((two[0].size.width - two[1].size.width).abs() <= gpui::px(1.));

    draw_and_turn(&model, cx);
    let halves = grid_sizes(&model, cx);
    assert_eq!(halves[0].0, halves[1].0, "same height");
    assert!(
        halves[0].0 < single.0,
        "a header row less: {halves:?} vs {single:?}"
    );
    assert!(halves[0].1.abs_diff(halves[1].1) <= 1, "{halves:?}");
    assert!(halves[0].1 < single.1 / 2 + 1, "{halves:?} vs {single:?}");

    // A third terminal: three panes, every column narrower.
    let _s2 = new_shell(f, cx);
    let three = bounds(cx, 3);
    assert!(three[2].origin.x > three[1].origin.x);
    draw_and_turn(&model, cx);
    let thirds = grid_sizes(&model, cx);
    assert_eq!(thirds.len(), 3);
    assert!(thirds.iter().all(|s| s.1 < halves[1].1), "{thirds:?}");
    let widest = thirds.iter().map(|s| s.1).max().unwrap();
    let narrowest = thirds.iter().map(|s| s.1).min().unwrap();
    assert!(widest - narrowest <= 1, "{thirds:?}");

    // Off again: every terminal back to the one viewport.
    cx.simulate_keystrokes("ctrl-b");
    close_overlays(&model, cx);
    assert!(cx.debug_bounds("split-pane-0").is_none());
    draw_and_turn(&model, cx);
    assert_eq!(grid_sizes(&model, cx), vec![single; 3]);
}

#[gpui::test]
fn in_split_view_focus_moves_between_panes_and_only_the_active_one_gets_input(
    app: &mut TestAppContext,
) {
    let f = fakes();
    let (projects, agent) = live_agent(f);
    let (_f, model, cx) = open_with(app, f, projects);
    dispatch(&model, HostEvent::FocusApp, cx);
    let s1 = new_shell(f, cx);
    let s2 = new_shell(f, cx);
    cx.simulate_keystrokes("ctrl-b");
    close_overlays(&model, cx);
    let input = |h: &FakePtyHandle| String::from_utf8_lossy(&h.input()).to_string();

    // Shell 2 is active: typing reaches it alone.
    dispatch(&model, HostEvent::FocusTerminal, cx);
    cx.simulate_keystrokes("x");
    assert_eq!(input(&s2), "x");
    assert_eq!((input(&agent), input(&s1)), (String::new(), String::new()));

    // Alt-Left moves the focus one pane left; typing follows it.
    cx.simulate_keystrokes("alt-left");
    assert_eq!(selected_child(&model, cx), Some(0));
    cx.simulate_keystrokes("y");
    assert_eq!((input(&s1), input(&s2)), ("y".to_string(), "x".to_string()));

    // A click in another pane's body makes it active and focuses it.
    dispatch(&model, HostEvent::FocusApp, cx);
    click(cx, "split-pane-0");
    assert_eq!(selected_child(&model, cx), None, "the agent's pane");
    assert_eq!(mode(&model, cx), InputMode::Terminal);
    cx.simulate_keystrokes("z");
    assert_eq!(input(&agent), "z");
    assert_eq!((input(&s1), input(&s2)), ("y".to_string(), "x".to_string()));

    // A header click does the same.
    click(cx, "split-header-2");
    assert_eq!(selected_child(&model, cx), Some(1));
    // APP-mode Right from the last pane wraps to the first, as the tabs do.
    dispatch(&model, HostEvent::FocusApp, cx);
    cx.simulate_keystrokes("right");
    assert_eq!(selected_child(&model, cx), None);
}

#[gpui::test]
fn the_spinner_clock_holds_still_while_the_window_is_inactive_or_hidden(app: &mut TestAppContext) {
    use crate::views::spinner::{SpinnerClock, STEP};
    let f = fakes();
    let (mut projects, _agent) = live_agent(f);
    projects[0].state.tabs[0].interpreted = Some(flightdeck::contracts::InterpretedStatus::Working);
    let (_f, _model, cx) = open_with(app, f, projects);
    // The test platform opens its window in the background; bring it to the
    // front as the app's launch does.
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    let schedule = cx.update(|_, cx| SpinnerClock::schedule(cx));
    assert!(
        schedule.spinners >= 2,
        "the sidebar row and the project tab: {schedule:?}"
    );
    assert!(schedule.running());
    let steps = |cx: &mut VisualTestContext| cx.update(|_, cx| SpinnerClock::steps(cx));
    let advance = |cx: &mut VisualTestContext, n: u32| {
        cx.executor().advance_clock(STEP * n);
        cx.run_until_parked();
    };

    let before = steps(cx);
    advance(cx, 4);
    assert!(steps(cx) >= before + 4, "8 fps while shown");

    // Another app is active: the arcs stop, and so does the timer.
    cx.deactivate_window();
    assert!(!cx.update(|_, cx| SpinnerClock::schedule(cx)).running());
    advance(cx, 1);
    let paused = steps(cx);
    advance(cx, 8);
    assert_eq!(steps(cx), paused, "no steps while inactive");
    assert!(!cx.update(|_, cx| SpinnerClock::is_ticking(cx)));

    // Back to the front: they turn again.
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    advance(cx, 2);
    assert!(steps(cx) > paused);

    // Covered, minimised or on another Space: still again.
    cx.simulate_visibility_change(gpui::WindowVisibility::Hidden);
    cx.run_until_parked();
    advance(cx, 1);
    let hidden = steps(cx);
    advance(cx, 8);
    assert_eq!(steps(cx), hidden, "no steps while hidden");
    cx.simulate_visibility_change(gpui::WindowVisibility::Visible);
    cx.run_until_parked();
    advance(cx, 2);
    assert!(steps(cx) > hidden);
}
