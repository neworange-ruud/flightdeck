//! The platform layer, driven through GPUI's headless test platform: menu
//! items against chords, an isolated run's refusals, and the launch with no
//! repository. The window is the app's real root ([`AppRoot`]) over a live
//! [`AppHost`] built on the core's fakes, like `shell_tests`.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use flightdeck::app::commands::Command;
use flightdeck::app::keymap::{Context, Keymap};
use flightdeck::host::testing::{self, TestProject};
use flightdeck::host::{AppHost, HostEvent, OverlayInput, OverlayView};
use flightdeck::testing::{
    FakeClock, FakeCommandRunner, FakeContainerRuntime, FakeFs, FakeNotifier, FakePty,
};
use flightdeck::Env;
use flightdeck_desktop::keys::{chord_to_gpui, KeymapAction};
use gpui::{AppContext, Entity, Modifiers, TestAppContext, VisualTestContext};
use gpui_component::Root;

use crate::commands::{intent_for, keymap, Intent};
use crate::host::HostModel;
use crate::menus::{self, AppCommand, ItemModel, Target};
use crate::root::{AppRoot, Opener, RootHandle};

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

fn two_projects() -> Vec<TestProject> {
    vec![
        TestProject::new("alpha", &["a1", "a2"]),
        TestProject::new("beta", &["b1"]),
    ]
}

/// An opener that must not be used (a launch inside a repository).
fn no_opener() -> Opener {
    Rc::new(|_| Err("no opener in this test".to_string()))
}

/// Open the app's window root over `initial` (or the launcher), with the
/// bindings and handlers `app::run` registers.
fn open_root<'a>(
    app: &'a mut TestAppContext,
    initial: Option<AppHost<'static>>,
    opener: Opener,
    recent: Vec<PathBuf>,
) -> (Entity<AppRoot>, &'a mut VisualTestContext) {
    app.update(|cx| {
        gpui_component::init(cx);
        crate::theme::init(cx);
        flightdeck_desktop::keys::register(cx, keymap());
        flightdeck_desktop::overlays::register(cx, keymap());
        menus::register_actions(cx);
    });
    let slot: Rc<RefCell<Option<Entity<AppRoot>>>> = Rc::default();
    let for_window = slot.clone();
    let (_root, cx) = app.add_window_view(move |window, cx| {
        let root = cx.new(|cx| AppRoot::new(initial, opener, recent, None, false, cx));
        cx.set_global(RootHandle(root.clone()));
        *for_window.borrow_mut() = Some(root.clone());
        Root::new(root, window, cx)
    });
    cx.run_until_parked();
    let root = slot.borrow().clone().expect("the window built its root");
    (root, cx)
}

fn model_of(root: &Entity<AppRoot>, cx: &mut VisualTestContext) -> Entity<HostModel> {
    root.read_with(cx, |root, _| root.model().cloned())
        .expect("a project is open")
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

fn close_overlays(model: &Entity<HostModel>, cx: &mut VisualTestContext) {
    for _ in 0..4 {
        if !model.read_with(cx, |m, _| m.host().overlay().is_some()) {
            break;
        }
        model.update(cx, |m, cx| {
            m.dispatch(HostEvent::Overlay(OverlayInput::Cancel), cx)
        });
    }
    cx.run_until_parked();
}

/// APP mode, nothing recorded: the state each comparison starts from.
fn reset(model: &Entity<HostModel>, cx: &mut VisualTestContext) {
    close_overlays(model, cx);
    model.update(cx, |m, cx| m.dispatch(HostEvent::FocusApp, cx));
    cx.run_until_parked();
    take_dispatched(model, cx);
}

// --- menus ---------------------------------------------------------------------

#[gpui::test]
fn every_menu_item_dispatches_what_its_chord_dispatches(app: &mut TestAppContext) {
    let f = fakes();
    let host = testing::host(env(f), &f.notifier, two_projects(), 0);
    let (root, cx) = open_root(app, Some(host), no_opener(), Vec::new());
    let model = model_of(&root, cx);

    let ids = menus::entry_ids(&menus::menu_model(keymap(), false));
    assert!(ids.len() > 20, "the menu lists the table: {ids:?}");
    for id in ids {
        // Quit ends the app; its meaning is pinned by the `commands` tests.
        if id == "Quit" {
            continue;
        }
        let entry = keymap().entry(id).unwrap();
        let expected = match intent_for(&entry.action) {
            Intent::Host(event) => event,
            Intent::PasteClipboard => panic!("{id}: no menu item pastes"),
            // The view switch is the window's own state, not a host event:
            // the menu item and Alt-m must both flip it.
            Intent::ToggleMainView => {
                let view = |model: &Entity<HostModel>, cx: &mut VisualTestContext| {
                    model.read_with(cx, |m, _| m.host().workspace_ui().view)
                };
                for how in ["menu", "chord"] {
                    reset(&model, cx);
                    let before = view(&model, cx);
                    if how == "menu" {
                        cx.dispatch_action(KeymapAction::for_entry(entry));
                    } else {
                        cx.simulate_keystrokes("alt-m");
                    }
                    cx.run_until_parked();
                    assert_eq!(view(&model, cx), before.toggled(), "{id} from its {how}");
                    cx.update(|_, cx| crate::views::mission::toggle_view(&model, cx));
                    cx.run_until_parked();
                }
                continue;
            }
        };

        reset(&model, cx);
        // A menu click dispatches the item's action to the focused window.
        cx.dispatch_action(KeymapAction::for_entry(entry));
        cx.run_until_parked();
        let by_menu = take_dispatched(&model, cx);
        assert_eq!(by_menu, [expected], "{id} from its menu item");

        // The same event as the chord, when the entry has one a focused
        // window can receive (Leave terminal focus is a Terminal-mode chord).
        if let Some(trigger) = entry
            .triggers
            .iter()
            .find(|t| t.context != Context::Terminal)
        {
            reset(&model, cx);
            cx.simulate_keystrokes(&chord_to_gpui(trigger.chord).unwrap());
            let by_chord = take_dispatched(&model, cx);
            assert_eq!(by_chord, by_menu, "{id}: {}", trigger.chord);
        }
    }
}

#[gpui::test]
fn about_and_settings_ask_the_host_for_the_palettes_rows(app: &mut TestAppContext) {
    let f = fakes();
    let host = testing::host(env(f), &f.notifier, two_projects(), 0);
    let (root, cx) = open_root(app, Some(host), no_opener(), Vec::new());
    let model = model_of(&root, cx);

    reset(&model, cx);
    cx.dispatch_action(menus::About);
    cx.run_until_parked();
    assert_eq!(
        take_dispatched(&model, cx),
        [HostEvent::Command(Command::ShowAbout)]
    );
    assert!(matches!(
        model.read_with(cx, |m, _| m.host().overlay()),
        Some(OverlayView::About(_))
    ));

    reset(&model, cx);
    cx.dispatch_action(menus::OpenSettings);
    cx.run_until_parked();
    assert_eq!(
        take_dispatched(&model, cx),
        [HostEvent::RunPaletteAction(
            flightdeck::tui::palette::PaletteAction::OpenConfig
        )]
    );
    assert!(
        model.read_with(cx, |m, _| m.host().overlay().is_some()),
        "the configuration manager opened"
    );
}

#[gpui::test]
fn the_open_project_item_answers_a_folder_picker_through_the_host(app: &mut TestAppContext) {
    let f = fakes();
    let host = testing::host(env(f), &f.notifier, two_projects(), 0);
    let (root, cx) = open_root(app, Some(host), no_opener(), Vec::new());
    let model = model_of(&root, cx);
    reset(&model, cx);

    cx.dispatch_action(menus::OpenProjectFolder);
    cx.run_until_parked();
    cx.simulate_path_prompt_response(|_| Some(vec![PathBuf::from("/work/other")]));
    cx.run_until_parked();
    assert_eq!(
        take_dispatched(&model, cx),
        [HostEvent::OpenProject(PathBuf::from("/work/other"))]
    );
}

#[test]
fn macos_shortcuts_are_platform_conventions_and_cmd_w_stays_free() {
    let bindings = menus::platform_bindings(keymap());
    if flightdeck::tui::platform::IS_MACOS {
        assert_eq!(bindings.len(), 3, "Cmd-, Cmd-O Cmd-Q");
    } else {
        assert!(bindings.is_empty());
    }
    for binding in &bindings {
        for stroke in binding.keystrokes() {
            let key = stroke.unparse();
            assert_ne!(key, "cmd-w", "Cmd-W must never close a session");
        }
    }
}

#[test]
fn the_f2_setting_rebinds_only_the_leave_focus_key() {
    use flightdeck_desktop::keys::binding_specs;
    let stroke_of = |keymap: &Keymap| -> Vec<String> {
        binding_specs(keymap)
            .into_iter()
            .filter(|b| b.entry_id == "FocusApp")
            .map(|b| b.keystroke)
            .collect()
    };
    assert_eq!(stroke_of(crate::commands::keymap_for(true)), ["f2"]);
    let default = stroke_of(crate::commands::keymap_for(false));
    assert_eq!(default.len(), 1);
    assert!(default[0].ends_with("escape"), "{default:?}");
}

// --- isolated runs ---------------------------------------------------------------

#[gpui::test]
fn an_isolated_run_disables_and_refuses_what_it_cannot_do(app: &mut TestAppContext) {
    let f = fakes();
    let host = testing::isolated_host(env(f), &f.notifier, TestProject::new("solo", &["only"]));
    let (root, cx) = open_root(app, Some(host), no_opener(), Vec::new());
    let model = model_of(&root, cx);
    reset(&model, cx);

    // The run says what it is, and offers no way to add a project.
    assert!(cx.debug_bounds("isolated-badge").is_some());
    assert!(cx.debug_bounds("open-project").is_none());

    // The button is drawn disabled and sends nothing.
    click(cx, "new-agent");
    assert_eq!(take_dispatched(&model, cx), [], "New agent is inert");

    // The menu draws New agent, the project switches and Open project…
    // disabled, and nothing else.
    let mut disabled = Vec::new();
    for menu in menus::menu_model(keymap(), true) {
        fn walk(items: &[ItemModel], out: &mut Vec<Target>) {
            for item in items {
                match item {
                    ItemModel::Item {
                        target,
                        disabled: true,
                        ..
                    } => out.push(*target),
                    ItemModel::Submenu { items, .. } => walk(items, out),
                    _ => {}
                }
            }
        }
        walk(&menu.items, &mut disabled);
    }
    assert_eq!(
        disabled,
        [
            Target::Entry("NewAgentTab"),
            Target::App(AppCommand::OpenProject),
            Target::Entry("SwitchProjectPrev"),
            Target::Entry("SwitchProjectNext"),
        ]
    );

    // The chords reach the host and are refused there with the TUI's message;
    // so is a menu action that slips through, and Open project….
    let refused = |model: &Entity<HostModel>, cx: &mut VisualTestContext| {
        match model.read_with(cx, |m, _| m.host().overlay()) {
            Some(OverlayView::Message(m)) => {
                assert!(m.text.contains("isolated run"), "{}", m.text)
            }
            other => panic!("expected the isolated refusal, got {other:?}"),
        }
        assert_eq!(model.read_with(cx, |m, _| m.host().project_count()), 1);
        assert_eq!(
            model.read_with(cx, |m, _| m.host().active_state().tabs.len()),
            1,
            "still the one session"
        );
    };
    for chord in ["ctrl-n", "shift-right", "shift-left"] {
        reset(&model, cx);
        cx.simulate_keystrokes(chord);
        assert!(
            !take_dispatched(&model, cx).is_empty(),
            "{chord} reached the host"
        );
        refused(&model, cx);
    }
    for id in ["NewAgentTab", "SwitchProjectNext"] {
        reset(&model, cx);
        cx.dispatch_action(KeymapAction::for_entry(keymap().entry(id).unwrap()));
        cx.run_until_parked();
        refused(&model, cx);
    }
    reset(&model, cx);
    cx.dispatch_action(menus::OpenProjectFolder);
    cx.run_until_parked();
    cx.simulate_path_prompt_response(|_| Some(vec![PathBuf::from("/work/other")]));
    cx.run_until_parked();
    refused(&model, cx);

    // Nothing is saved on the way out.
    reset(&model, cx);
    model.update(cx, |m, _| m.teardown());
    assert!(f
        .fs
        .file_contents(Path::new("/solo/.flightdeck/state.json"))
        .is_none());
}

// --- launching without a repository ------------------------------------------------

/// An opener over the fakes that records the folders it was asked for.
fn recording_opener(f: &'static Fakes, seen: Rc<RefCell<Vec<PathBuf>>>) -> Opener {
    Rc::new(move |path| {
        seen.borrow_mut().push(path.to_path_buf());
        Ok(testing::host(
            env(f),
            &f.notifier,
            vec![TestProject::new("picked", &["p1"])],
            0,
        ))
    })
}

#[gpui::test]
fn a_launch_outside_a_repository_shows_the_empty_state(app: &mut TestAppContext) {
    let f = fakes();
    let recent = vec![PathBuf::from("/work/one"), PathBuf::from("/work/two")];
    let seen = Rc::default();
    let (root, cx) = open_root(app, None, recording_opener(f, seen), recent);
    assert!(root.read_with(cx, |r, _| r.is_launcher()));
    assert!(cx.debug_bounds("launcher").is_some());
    assert!(cx.debug_bounds("launcher-open").is_some(), "Open project…");
    assert!(cx.debug_bounds("recent-0").is_some());
    assert!(cx.debug_bounds("recent-1").is_some());
    assert!(cx.debug_bounds("recent-2").is_none());
    // No shell behind it.
    assert!(cx.debug_bounds("new-agent").is_none());
    assert!(cx.debug_bounds("command-field").is_none());
}

#[gpui::test]
fn picking_a_folder_in_the_empty_state_opens_it(app: &mut TestAppContext) {
    let f = fakes();
    let seen: Rc<RefCell<Vec<PathBuf>>> = Rc::default();
    let (root, cx) = open_root(app, None, recording_opener(f, seen.clone()), Vec::new());

    click(cx, "launcher-open");
    cx.simulate_path_prompt_response(|_| Some(vec![PathBuf::from("/work/picked")]));
    cx.run_until_parked();

    assert_eq!(*seen.borrow(), [PathBuf::from("/work/picked")]);
    assert!(
        !root.read_with(cx, |r, _| r.is_launcher()),
        "the shell replaced it"
    );
    assert!(cx.debug_bounds("launcher-open").is_none());
    assert!(cx.debug_bounds("new-agent").is_some(), "the shell is up");
    assert_eq!(
        model_of(&root, cx).read_with(cx, |m, _| m.host().project_name(0).map(str::to_string)),
        Some("picked".to_string())
    );
}

#[gpui::test]
fn a_cancelled_picker_leaves_the_empty_state(app: &mut TestAppContext) {
    let f = fakes();
    let seen: Rc<RefCell<Vec<PathBuf>>> = Rc::default();
    let (root, cx) = open_root(app, None, recording_opener(f, seen.clone()), Vec::new());
    click(cx, "launcher-open");
    cx.simulate_path_prompt_response(|_| None);
    cx.run_until_parked();
    assert!(seen.borrow().is_empty());
    assert!(root.read_with(cx, |r, _| r.is_launcher()));
}

#[gpui::test]
fn a_recent_project_opens_on_click(app: &mut TestAppContext) {
    let f = fakes();
    let seen: Rc<RefCell<Vec<PathBuf>>> = Rc::default();
    let recent = vec![PathBuf::from("/work/one"), PathBuf::from("/work/two")];
    let (root, cx) = open_root(app, None, recording_opener(f, seen.clone()), recent);
    click(cx, "recent-1");
    assert_eq!(*seen.borrow(), [PathBuf::from("/work/two")]);
    assert!(!root.read_with(cx, |r, _| r.is_launcher()));
}

#[gpui::test]
fn a_folder_that_cannot_open_keeps_the_empty_state_and_says_why(app: &mut TestAppContext) {
    let opener: Opener = Rc::new(|_| Err("not inside a Git repository".to_string()));
    let (root, cx) = open_root(app, None, opener, Vec::new());
    click(cx, "launcher-open");
    cx.simulate_path_prompt_response(|_| Some(vec![PathBuf::from("/tmp/plain")]));
    cx.run_until_parked();
    assert!(root.read_with(cx, |r, _| r.is_launcher()));
    assert_eq!(
        root.read_with(cx, |r, _| r.error().map(str::to_string)),
        Some("not inside a Git repository".to_string())
    );
}
