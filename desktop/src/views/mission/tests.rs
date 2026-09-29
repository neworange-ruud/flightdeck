//! Mission control driven through GPUI's headless test platform against a
//! real [`AppHost`](flightdeck::host::AppHost) over the core's fakes, like
//! `shell_tests`: keys go through the real bindings, key contexts and focus;
//! what happened is read from the host's state and from the exact
//! [`HostEvent`]s the host model received.

use std::path::{Path, PathBuf};

use flightdeck::app::commands::{Command, Selector};
use flightdeck::app::keymap::{Chord, Key, Mods};
use flightdeck::app::modes::InputMode;
use flightdeck::contracts::{InterpretedStatus, PtySize};
use flightdeck::git::status::{LineStats, WorktreeChanges, WorktreeStatus};
use flightdeck::host::testing::{self, TestProject};
use flightdeck::host::HostEvent;
use flightdeck::persistence::workspace::{MainView, MissionScope, RecentScope};
use flightdeck::testing::{
    FakeClock, FakeCommandRunner, FakeContainerRuntime, FakeFs, FakeNotifier, FakePty,
};
use flightdeck::Env;
use gpui::{AppContext, Entity, Keystroke, Modifiers, TestAppContext, VisualTestContext};
use gpui_component::Root;

use crate::host::HostModel;
use crate::shell::FlightDeckWindow;

/// "Now" for every test, in unix seconds.
const NOW: u64 = 2_000_000_000;

struct Fakes {
    fs: FakeFs,
    pty: FakePty,
    clock: FakeClock,
    container: FakeContainerRuntime,
    command: FakeCommandRunner,
    notifier: FakeNotifier,
}

fn fakes() -> &'static Fakes {
    let f = Box::leak(Box::new(Fakes {
        fs: FakeFs::new(),
        pty: FakePty::new(),
        clock: FakeClock::default(),
        container: FakeContainerRuntime::new(),
        command: FakeCommandRunner::new(),
        notifier: FakeNotifier::new(),
    }));
    f.clock.set_unix_secs(NOW);
    f
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

/// A project whose tabs are `(name, status, seconds since last output)`. A
/// tab with a status gets a running agent reporting it; `None` stays
/// unspawned.
fn project(
    f: &'static Fakes,
    name: &str,
    tabs: &[(&str, Option<InterpretedStatus>, u64)],
) -> TestProject {
    let names: Vec<&str> = tabs.iter().map(|t| t.0).collect();
    let mut p = TestProject::new(name, &names);
    for (i, (_, status, ago)) in tabs.iter().enumerate() {
        let tab = &mut p.state.tabs[i];
        if let Some(status) = status {
            let handle = f.pty.queue_session();
            handle.push_output(format!("{} is on it\r\n", tab.meta.name));
            tab.session
                .spawn_primary(&f.pty, "agent", &[], Path::new("/wt"), PtySize::default())
                .unwrap();
            tab.interpreted = Some(*status);
        }
        tab.meta.activity.last_output_at = Some(NOW - ago);
    }
    p
}

/// Two projects, four live sessions (tile order: b-wait, a-attn, a-work,
/// b-work) and one recent idle session (a card), which is alpha's selected
/// tab — so entering Mission control has to pick a tile.
fn workspace(f: &'static Fakes) -> Vec<TestProject> {
    use InterpretedStatus::*;
    let mut alpha = project(
        f,
        "alpha",
        &[
            ("a-work", Some(Working), 30),
            ("a-attn", Some(NeedsAttention), 20),
            ("a-idle", Some(Idle), 3_600),
        ],
    );
    alpha.state.selected_tab = Some(2);
    vec![
        alpha,
        project(
            f,
            "beta",
            &[
                ("b-wait", Some(WaitingForInput), 10),
                ("b-work", Some(Working), 40),
            ],
        ),
    ]
}

/// Dismiss whatever overlay an action opened (a confirmation, a prompt).
fn close_overlays(model: &Entity<HostModel>, cx: &mut VisualTestContext) {
    for _ in 0..4 {
        if model.read_with(cx, |m, _| m.host().overlay().is_none()) {
            break;
        }
        model.update(cx, |m, cx| {
            m.dispatch(
                HostEvent::Overlay(flightdeck::host::OverlayInput::Cancel),
                cx,
            )
        });
    }
    cx.run_until_parked();
}

fn open_host<'a>(
    app: &'a mut TestAppContext,
    host: flightdeck::host::AppHost<'static>,
) -> (Entity<HostModel>, &'a mut VisualTestContext) {
    app.update(|cx| {
        gpui_component::init(cx);
        crate::theme::init(cx);
        flightdeck_desktop::keys::register(cx, crate::commands::keymap());
        flightdeck_desktop::overlays::register(cx, crate::commands::keymap());
    });
    let model = app.new(|_| HostModel::new(host));
    let for_window = model.clone();
    let (_root, cx) = app.add_window_view(|window, cx| {
        let view = cx.new(|cx| FlightDeckWindow::new(for_window, cx));
        Root::new(view, window, cx)
    });
    cx.run_until_parked();
    (model, cx)
}

fn open(app: &mut TestAppContext) -> (&'static Fakes, Entity<HostModel>, &mut VisualTestContext) {
    let f = fakes();
    let host = testing::host(env(f), &f.notifier, workspace(f), 0);
    let (model, cx) = open_host(app, host);
    (f, model, cx)
}

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let bounds = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is not on screen"));
    cx.simulate_click(bounds.center(), Modifiers::none());
    cx.run_until_parked();
}

fn take_dispatched(model: &Entity<HostModel>, cx: &mut VisualTestContext) -> Vec<HostEvent> {
    model.update(cx, |m, _| std::mem::take(&mut m.dispatched))
}

fn view_of(model: &Entity<HostModel>, cx: &mut VisualTestContext) -> MainView {
    model.read_with(cx, |m, _| m.host().workspace_ui().view)
}

fn mode_of(model: &Entity<HostModel>, cx: &mut VisualTestContext) -> InputMode {
    model.read_with(cx, |m, _| m.host().active_state().mode())
}

/// The host's selection as `(project name, tab name)`.
fn selected(model: &Entity<HostModel>, cx: &mut VisualTestContext) -> (String, String) {
    model.read_with(cx, |m, _| {
        let h = m.host();
        let project = h
            .project_name(h.active_project_index())
            .unwrap()
            .to_string();
        let tab = h.active_state().selected().unwrap().meta.name.clone();
        (project, tab)
    })
}

fn sel(project: &str, tab: &str) -> (String, String) {
    (project.to_string(), tab.to_string())
}

/// The GPUI spelling of the table's leave-focus chord on this platform.
fn leave_focus_keys() -> String {
    let entry = crate::commands::keymap().entry("FocusApp").unwrap();
    flightdeck_desktop::keys::chord_to_gpui(entry.triggers[0].chord).unwrap()
}

#[gpui::test]
fn alt_m_switches_to_mission_control_and_back(app: &mut TestAppContext) {
    let (_f, model, cx) = open(app);
    assert_eq!(view_of(&model, cx), MainView::Projects);
    assert!(cx.debug_bounds("agent-row-0").is_some());

    cx.simulate_keystrokes("alt-m");
    assert_eq!(view_of(&model, cx), MainView::Mission);
    for tile in ["mission-tile-0", "mission-tile-3"] {
        assert!(cx.debug_bounds(tile).is_some(), "{tile}");
    }
    assert!(
        cx.debug_bounds("mission-tile-4").is_none(),
        "four live sessions"
    );
    assert!(
        cx.debug_bounds("mission-card-0").is_some(),
        "the idle one is a card"
    );
    assert!(
        cx.debug_bounds("agent-row-0").is_none(),
        "the Projects rail is gone"
    );
    // The first tile (the freshest needs-you) is now the host's selection,
    // through the events the tabs and Alt-N send.
    assert_eq!(selected(&model, cx), sel("beta", "b-wait"));
    assert_eq!(
        take_dispatched(&model, cx),
        [
            HostEvent::SwitchProject(Selector::Index(1)),
            HostEvent::Command(Command::SwitchAgentTab(Selector::Index(0))),
        ]
    );
    assert_eq!(mode_of(&model, cx), InputMode::App, "the grid is APP mode");

    cx.simulate_keystrokes("alt-m");
    assert_eq!(view_of(&model, cx), MainView::Projects);
    assert!(cx.debug_bounds("agent-row-0").is_some());
    assert_eq!(
        selected(&model, cx),
        sel("beta", "b-wait"),
        "selection kept"
    );
}

#[gpui::test]
fn the_view_switch_segments_and_the_needs_you_badge(app: &mut TestAppContext) {
    let (_f, model, cx) = open(app);
    assert!(
        cx.debug_bounds("view-mission-badge").is_some(),
        "two sessions need you"
    );
    click(cx, "view-mission");
    assert_eq!(view_of(&model, cx), MainView::Mission);
    click(cx, "view-mission");
    assert_eq!(
        view_of(&model, cx),
        MainView::Mission,
        "a segment shows, not toggles"
    );
    click(cx, "view-projects");
    assert_eq!(view_of(&model, cx), MainView::Projects);
}

#[gpui::test]
fn alt_m_with_a_terminal_focused_is_the_terminals(app: &mut TestAppContext) {
    let (_f, model, cx) = open(app);
    model.update(cx, |m, cx| m.dispatch(HostEvent::FocusTerminal, cx));
    cx.run_until_parked();
    assert_eq!(mode_of(&model, cx), InputMode::Terminal);
    cx.simulate_keystrokes("alt-m");
    assert_eq!(
        view_of(&model, cx),
        MainView::Projects,
        "Meta-m is the shell's"
    );
}

#[test]
fn option_m_on_macos_is_the_same_chord() {
    // macOS reports Option+m as key "m", alt, key_char "µ": still Alt-m.
    let keystroke = Keystroke::parse("alt-m->µ").unwrap();
    let chord = flightdeck_desktop::keys::chord_from_keystroke(&keystroke);
    assert_eq!(chord, Some(Chord::new(Key::Char('m'), Mods::ALT)));
    let entry = crate::commands::keymap()
        .lookup(InputMode::App, chord.unwrap())
        .unwrap();
    assert_eq!(entry.id, "ToggleMissionControl");
}

#[gpui::test]
fn arrows_move_the_selected_tile_through_the_grid(app: &mut TestAppContext) {
    // Four tiles, 2×2:  b-wait  a-attn
    //                   a-work  b-work
    let (_f, model, cx) = open(app);
    cx.simulate_keystrokes("alt-m");
    assert_eq!(selected(&model, cx), sel("beta", "b-wait"));
    take_dispatched(&model, cx);

    cx.simulate_keystrokes("right");
    assert_eq!(selected(&model, cx), sel("alpha", "a-attn"));
    assert_eq!(
        take_dispatched(&model, cx),
        [
            HostEvent::SwitchProject(Selector::Index(0)),
            HostEvent::Command(Command::SwitchAgentTab(Selector::Index(1))),
        ]
    );
    cx.simulate_keystrokes("down");
    assert_eq!(selected(&model, cx), sel("beta", "b-work"));
    cx.simulate_keystrokes("left");
    assert_eq!(selected(&model, cx), sel("alpha", "a-work"));
    cx.simulate_keystrokes("up");
    assert_eq!(selected(&model, cx), sel("beta", "b-wait"));
    cx.simulate_keystrokes("up");
    assert_eq!(
        selected(&model, cx),
        sel("beta", "b-wait"),
        "the top edge stays"
    );
    // Alt-arrows move too, and nothing switched terminals.
    cx.simulate_keystrokes("alt-down");
    assert_eq!(selected(&model, cx), sel("alpha", "a-work"));
    assert!(!take_dispatched(&model, cx)
        .iter()
        .any(|e| matches!(e, HostEvent::Command(Command::SwitchChildTerminal(_)))));
    assert_eq!(mode_of(&model, cx), InputMode::App);
}

#[gpui::test]
fn alt_digits_follow_tile_order(app: &mut TestAppContext) {
    let (_f, model, cx) = open(app);
    cx.simulate_keystrokes("alt-m");
    cx.simulate_keystrokes("alt-3");
    assert_eq!(
        selected(&model, cx),
        sel("alpha", "a-work"),
        "tile 3, not tab 3"
    );
    cx.simulate_keystrokes("alt-4");
    assert_eq!(selected(&model, cx), sel("beta", "b-work"));
    cx.simulate_keystrokes("alt-9");
    assert_eq!(selected(&model, cx), sel("beta", "b-work"), "no ninth tile");
}

#[gpui::test]
fn a_chord_acts_on_the_selected_tile(app: &mut TestAppContext) {
    let (_f, model, cx) = open(app);
    cx.simulate_keystrokes("alt-m");
    cx.simulate_keystrokes("right");
    take_dispatched(&model, cx);
    // Ctrl-p is the Projects view's Push, unchanged, for a-attn.
    cx.simulate_keystrokes("ctrl-p");
    assert_eq!(
        take_dispatched(&model, cx),
        [HostEvent::Command(Command::PushBranch { confirm: None })]
    );
    assert_eq!(selected(&model, cx), sel("alpha", "a-attn"));
}

#[gpui::test]
fn enter_opens_the_tile_full_size_and_alt_esc_goes_back(app: &mut TestAppContext) {
    let (_f, model, cx) = open(app);
    cx.simulate_keystrokes("alt-m");
    cx.simulate_keystrokes("down");
    take_dispatched(&model, cx);

    cx.simulate_keystrokes("enter");
    assert_eq!(
        mode_of(&model, cx),
        InputMode::Terminal,
        "its terminal is focused"
    );
    assert_eq!(take_dispatched(&model, cx), [HostEvent::FocusTerminal]);
    assert_eq!(selected(&model, cx), sel("alpha", "a-work"));
    assert!(cx.debug_bounds("mission-tile-0").is_none(), "no grid");
    for strip in ["mission-strip-0", "mission-strip-3"] {
        assert!(
            cx.debug_bounds(strip).is_some(),
            "{strip}: the others stay visible"
        );
    }
    assert!(cx.debug_bounds("mission-show-in-projects").is_some());
    // Typing reaches the opened session's terminal.
    cx.simulate_input("y");
    assert!(take_dispatched(&model, cx).contains(&HostEvent::TerminalInput(b"y".to_vec())));

    // Alt-1 swaps the open session and stays full size.
    cx.simulate_keystrokes("alt-1");
    assert_eq!(selected(&model, cx), sel("beta", "b-wait"));
    assert_eq!(mode_of(&model, cx), InputMode::Terminal);

    let leave = leave_focus_keys();
    cx.simulate_keystrokes(&leave);
    assert_eq!(mode_of(&model, cx), InputMode::App);
    assert!(
        cx.debug_bounds("mission-tile-0").is_some(),
        "back to the grid"
    );
    assert_eq!(view_of(&model, cx), MainView::Mission);
}

#[gpui::test]
fn enter_with_no_live_session_does_nothing(app: &mut TestAppContext) {
    let f = fakes();
    let quiet = vec![project(f, "alpha", &[("idle", None, 60)])];
    let host = testing::host(env(f), &f.notifier, quiet, 0);
    let (model, cx) = open_host(app, host);
    cx.simulate_keystrokes("alt-m");
    take_dispatched(&model, cx);
    cx.simulate_keystrokes("enter");
    assert_eq!(take_dispatched(&model, cx), []);
    assert_eq!(mode_of(&model, cx), InputMode::App);
}

#[gpui::test]
fn show_in_projects_opens_that_session_there(app: &mut TestAppContext) {
    let (_f, model, cx) = open(app);
    cx.simulate_keystrokes("alt-m");
    cx.simulate_keystrokes("alt-2");
    cx.simulate_keystrokes("enter");
    click(cx, "mission-show-in-projects");
    assert_eq!(view_of(&model, cx), MainView::Projects);
    assert_eq!(selected(&model, cx), sel("alpha", "a-attn"));
    assert_eq!(
        mode_of(&model, cx),
        InputMode::Terminal,
        "still typing into it"
    );
    // The grid's quiet link goes back to Projects too.
    click(cx, "view-mission");
    assert_eq!(mode_of(&model, cx), InputMode::App);
    click(cx, "mission-see-all");
    assert_eq!(view_of(&model, cx), MainView::Projects);
}

#[gpui::test]
fn clicking_a_tile_selects_it_and_a_double_click_opens_it(app: &mut TestAppContext) {
    let (_f, model, cx) = open(app);
    cx.simulate_keystrokes("alt-m");
    click(cx, "mission-tile-3");
    assert_eq!(selected(&model, cx), sel("beta", "b-work"));
    assert_eq!(mode_of(&model, cx), InputMode::App);
    let bounds = cx.debug_bounds("mission-tile-2").unwrap();
    cx.simulate_event(gpui::MouseDownEvent {
        position: bounds.center(),
        modifiers: Modifiers::none(),
        button: gpui::MouseButton::Left,
        click_count: 2,
        first_mouse: false,
    });
    cx.simulate_event(gpui::MouseUpEvent {
        position: bounds.center(),
        modifiers: Modifiers::none(),
        button: gpui::MouseButton::Left,
        click_count: 2,
    });
    cx.run_until_parked();
    assert_eq!(selected(&model, cx), sel("alpha", "a-work"));
    assert_eq!(mode_of(&model, cx), InputMode::Terminal);
}

fn dirty(changes: u32) -> WorktreeStatus {
    WorktreeStatus {
        branch: "flightdeck/a-idle".to_string(),
        base_branch: "main".to_string(),
        dirty: true,
        changes: WorktreeChanges {
            modified: changes,
            ..Default::default()
        },
        lines: LineStats {
            added: 31,
            removed: 4,
        },
        ahead: 0,
        behind: 0,
        upstream: None,
        base_drift: 0,
        worktree_path: PathBuf::from("/alpha/.flightdeck/worktrees/a-idle"),
    }
}

#[gpui::test]
fn a_card_action_dispatches_its_chords_command_for_that_tab(app: &mut TestAppContext) {
    let f = fakes();
    let mut host = testing::host(env(f), &f.notifier, workspace(f), 1);
    testing::set_git_status(&mut host, 0, "t2", dirty(2));
    let (model, cx) = open_host(app, host);
    cx.simulate_keystrokes("alt-m");
    take_dispatched(&model, cx);

    // "a-idle · 2 files changed, not pushed": its action is Push.
    click(cx, "mission-card-action-0");
    let by_card = take_dispatched(&model, cx);
    assert_eq!(selected(&model, cx), sel("alpha", "a-idle"));
    assert_eq!(
        by_card.last(),
        Some(&HostEvent::Command(Command::PushBranch { confirm: None }))
    );
    assert_eq!(
        &by_card[..by_card.len() - 1],
        [
            HostEvent::SwitchProject(Selector::Index(0)),
            HostEvent::Command(Command::SwitchAgentTab(Selector::Index(2))),
        ],
        "select the card's session first"
    );
    // Exactly what Ctrl-p sends for it (Push asks first: dirty tree).
    close_overlays(&model, cx);
    take_dispatched(&model, cx);
    cx.simulate_keystrokes("ctrl-p");
    assert_eq!(
        take_dispatched(&model, cx),
        [HostEvent::Command(Command::PushBranch { confirm: None })]
    );
}

#[gpui::test]
fn scope_and_view_persist_across_a_reopen(app: &mut TestAppContext) {
    let f = fakes();
    let ws_path = PathBuf::from("/home/u/.flightdeck/workspace.json");
    let first =
        testing::host_with_workspace_file(env(f), &f.notifier, workspace(f), 0, ws_path.clone());
    let (model, cx) = open_host(app, first);
    cx.simulate_keystrokes("alt-m");
    let week_beta = MissionScope {
        window: RecentScope::Week,
        project: Some("/beta".to_string()),
    };
    // What the scope menu's items do.
    cx.update(|_, cx| super::set_scope(&model, week_beta.clone(), cx));
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("mission-card-0").is_none(),
        "beta only: alpha's card is filtered"
    );
    assert!(cx.debug_bounds("mission-tile-1").is_some());
    assert!(
        cx.debug_bounds("mission-tile-2").is_none(),
        "beta's two tiles"
    );
    model.update(cx, |m, _| m.teardown());

    let again =
        testing::host_with_workspace_file(env(f), &f.notifier, workspace(f), 0, ws_path.clone());
    assert_eq!(again.workspace_ui().mission_scope, week_beta);
    assert_eq!(again.workspace_ui().view, MainView::Mission);
    let reopened = app.new(|_| HostModel::new(again));
    let (_root, cx) = app.add_window_view(|window, cx| {
        let view = cx.new(|cx| FlightDeckWindow::new(reopened.clone(), cx));
        Root::new(view, window, cx)
    });
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("mission-tile-1").is_some(),
        "it opens on Mission control"
    );
    assert!(
        cx.debug_bounds("mission-tile-2").is_none(),
        "still beta only"
    );
}
