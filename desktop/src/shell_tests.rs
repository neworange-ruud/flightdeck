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
use flightdeck::app::modes::InputMode;
use flightdeck::host::testing::{self, TestProject};
use flightdeck::host::{DialogKind, HostEvent, OverlayInput, OverlayView};
use flightdeck::testing::{
    FakeClock, FakeCommandRunner, FakeContainerRuntime, FakeFs, FakeNotifier, FakePty,
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
    }
}

/// Open the app's window over `projects` (the first active), as `app::run`
/// does minus the real services and the tick loop (tests take turns
/// themselves).
fn open(
    app: &mut TestAppContext,
    projects: Vec<TestProject>,
) -> (&'static Fakes, Entity<HostModel>, &mut VisualTestContext) {
    let f = fakes();
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
