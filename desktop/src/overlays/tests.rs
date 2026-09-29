//! Overlay tests, in two layers:
//!
//! - **Pure**: what each key means for each view model ([`key_events`]), the
//!   New Agent form's target/agent moves, the title/hint split — no GPUI.
//! - **Through GPUI**: a test window shaped like the app (a `"Global"` root,
//!   an `"App"` pane, the overlay layer beside it) with the real generated
//!   bindings. Keys go through `simulate_keystrokes` (keymap, key contexts,
//!   key-down listeners), clicks through `simulate_click` at the element's
//!   drawn bounds, and each test asserts the [`HostEvent`]s the layer emitted.
//!
//! The view models are [`super::fixtures`], written from the host's own copy
//! (`prompt_dialog` in the core) rather than read from a live host: building
//! an `AppHost` over fakes needs the root crate's `testing` builder, which is
//! not on this branch.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use flightdeck::app::keymap::{Keymap, KeymapOptions};
use flightdeck::host::{
    DialogKind, DialogView, HostEvent, MessageView, NewAgentTarget, OverlayInput, OverlayKey,
    OverlayView,
};
use flightdeck::tui::palette::PaletteAction;
use gpui::{
    div, Context, Entity, FocusHandle, InteractiveElement, IntoElement, Keystroke, Modifiers,
    MouseButton, MouseDownEvent, MouseUpEvent, ParentElement, Render, Styled, TestAppContext,
    VisualTestContext, Window,
};

use super::fixtures::*;
use super::*;
use crate::keys::{key_bindings as table_bindings, KeymapAction};
use crate::theme::Palette;

fn ev(input: OverlayInput) -> HostEvent {
    HostEvent::Overlay(input)
}

fn key(k: OverlayKey) -> HostEvent {
    ev(OverlayInput::Key(k))
}

fn text(s: &str) -> KeyPress {
    KeyPress::Text(s.to_string())
}

// ---------------------------------------------------------------------------
// Pure
// ---------------------------------------------------------------------------

#[test]
fn keystrokes_reduce_to_overlay_presses() {
    // As the platform (and `simulate_keystrokes`) delivers them: with the
    // character the key types.
    let press = |s: &str| key_press(&Keystroke::parse(s).unwrap().with_simulated_ime());
    assert_eq!(press("enter"), Some(KeyPress::Enter));
    assert_eq!(press("escape"), Some(KeyPress::Esc));
    assert_eq!(press("tab"), Some(KeyPress::Tab));
    assert_eq!(press("shift-tab"), None);
    assert_eq!(press("backspace"), Some(KeyPress::Backspace));
    assert_eq!(press("up"), Some(KeyPress::Up));
    assert_eq!(press("right"), Some(KeyPress::Right));
    assert_eq!(press("a"), Some(text("a")));
    assert_eq!(press("shift-a"), Some(text("A")));
    assert_eq!(press("space"), Some(text(" ")));
    // Chords are not typing; paste is the one exception.
    assert_eq!(press("ctrl-a"), None);
    assert_eq!(press("cmd-a"), None);
    assert_eq!(press("alt-a"), None);
    assert_eq!(press("f5"), None);
    assert_eq!(press("cmd-v"), Some(KeyPress::Paste(String::new())));
    assert_eq!(press("ctrl-v"), Some(KeyPress::Paste(String::new())));
}

#[test]
fn palette_typing_filters_and_arrows_select_in_the_host() {
    let view = palette("");
    let events = |k: &KeyPress| key_events(&OverlayView::Palette(view.clone()), k);
    assert_eq!(
        events(&text("p")),
        vec![ev(OverlayInput::PaletteFilter("p".into()))]
    );
    assert_eq!(events(&KeyPress::Backspace), vec![], "nothing to erase");
    assert_eq!(events(&KeyPress::Up), vec![], "already at the top");
    assert_eq!(
        events(&KeyPress::Down),
        vec![ev(OverlayInput::SelectRow(1))]
    );
    assert_eq!(
        events(&KeyPress::Enter),
        vec![ev(OverlayInput::PaletteRun(view.entries[0].action.clone()))]
    );
    assert_eq!(events(&KeyPress::Esc), vec![ev(OverlayInput::Cancel)]);
    assert_eq!(events(&KeyPress::Tab), vec![]);

    let filtered = palette("push");
    let events = |k: &KeyPress| key_events(&OverlayView::Palette(filtered.clone()), k);
    assert_eq!(
        events(&KeyPress::Backspace),
        vec![ev(OverlayInput::PaletteFilter("pus".into()))]
    );
    assert_eq!(
        events(&KeyPress::Paste("ed\n".into())),
        vec![ev(OverlayInput::PaletteFilter("pushed".into()))]
    );
    // The last row is a floor, like the top.
    let mut at_end = filtered.clone();
    at_end.selected = at_end.entries.len() - 1;
    assert_eq!(
        key_events(&OverlayView::Palette(at_end), &KeyPress::Down),
        vec![]
    );
    // Nothing matches: Enter runs nothing.
    let empty = palette("zzzzzz");
    assert!(empty.entries.is_empty());
    assert_eq!(
        key_events(&OverlayView::Palette(empty), &KeyPress::Enter),
        vec![]
    );
}

#[test]
fn enter_never_confirms_a_destructive_dialog() {
    for view in [
        abandon(false),
        abandon(true),
        rebase(),
        merge(),
        quit(),
        close_session(),
    ] {
        let overlay_view = OverlayView::Dialog(view.clone());
        assert_eq!(
            key_events(&overlay_view, &KeyPress::Enter),
            vec![],
            "{}",
            view.title
        );
        assert_eq!(
            key_events(&overlay_view, &KeyPress::Esc),
            vec![ev(OverlayInput::Cancel)]
        );
        // Only an accelerator presses anything; a stray key is ignored rather
        // than dismissing (the TUI's Unpair treats any key as "no").
        assert_eq!(key_events(&overlay_view, &text("q")), vec![]);
        assert_eq!(key_events(&overlay_view, &KeyPress::Tab), vec![]);
        assert_eq!(key_events(&overlay_view, &KeyPress::Down), vec![]);
    }
    assert_eq!(
        key_events(&OverlayView::Dialog(abandon(true)), &text("y")),
        vec![ev(OverlayInput::Choose("y".into()))]
    );
    assert_eq!(
        key_events(&OverlayView::Dialog(close_session()), &text("3")),
        vec![ev(OverlayInput::Choose("3".into()))]
    );
}

#[test]
fn form_dialogs_take_the_tui_keys() {
    let view = OverlayView::Dialog(rename());
    assert_eq!(
        key_events(&view, &KeyPress::Enter),
        vec![ev(OverlayInput::Submit)]
    );
    assert_eq!(
        key_events(&view, &text("x")),
        vec![key(OverlayKey::Char('x'))]
    );
    assert_eq!(
        key_events(&view, &KeyPress::Backspace),
        vec![key(OverlayKey::Backspace)]
    );
    assert_eq!(
        key_events(&view, &KeyPress::Paste("a\nb".into())),
        vec![HostEvent::Paste("a\nb".into())]
    );
    let browse = OverlayView::Dialog(open_project());
    assert_eq!(
        key_events(&browse, &KeyPress::Right),
        vec![key(OverlayKey::Right)]
    );
    assert_eq!(
        key_events(&browse, &KeyPress::Left),
        vec![key(OverlayKey::Left)]
    );
    let form = OverlayView::Dialog(new_agent(NewAgentTarget::NewBranch, ""));
    assert_eq!(
        key_events(&form, &KeyPress::Tab),
        vec![key(OverlayKey::Tab)]
    );
    assert_eq!(
        key_events(&form, &KeyPress::Down),
        vec![key(OverlayKey::Down)]
    );
}

#[test]
fn messages_dismiss_on_any_key_and_other_overlays_forward_keys() {
    let message = OverlayView::Message(MessageView {
        text: "Refused: nothing to push.".into(),
    });
    assert_eq!(
        key_events(&message, &text("k")),
        vec![ev(OverlayInput::Submit)]
    );
    assert_eq!(
        key_events(&message, &KeyPress::Esc),
        vec![ev(OverlayInput::Cancel)]
    );
    let help = OverlayView::Help(flightdeck::tui::help::help_doc(false, false));
    assert_eq!(
        key_events(&help, &KeyPress::Down),
        vec![key(OverlayKey::Down)]
    );
    assert_eq!(
        key_events(&help, &text("q")),
        vec![key(OverlayKey::Char('q'))]
    );
    assert_eq!(
        key_events(&help, &KeyPress::Enter),
        vec![ev(OverlayInput::Submit)]
    );
}

#[test]
fn new_agent_targets_follow_the_tui_tab_cycle() {
    use super::new_agent::{arrows_to_agent, tabs_to};
    let form = |view: DialogView| match view.kind {
        DialogKind::NewAgent(f) => f,
        _ => unreachable!(),
    };
    let tab = || key(OverlayKey::Tab);
    let new = form(new_agent(NewAgentTarget::NewBranch, ""));
    assert_eq!(tabs_to(&new, NewAgentTarget::NewBranch), Some(vec![]));
    assert_eq!(
        tabs_to(&new, NewAgentTarget::ExistingBranch),
        Some(vec![tab()])
    );
    assert_eq!(
        tabs_to(&new, NewAgentTarget::Base),
        Some(vec![tab(), tab()])
    );
    let base = form(new_agent(NewAgentTarget::Base, ""));
    assert_eq!(tabs_to(&base, NewAgentTarget::NewBranch), Some(vec![tab()]));
    assert_eq!(
        tabs_to(&base, NewAgentTarget::ExistingBranch),
        Some(vec![tab(), tab()])
    );
    // No other local branch: Tab skips "existing", so it cannot be reached.
    let mut lonely = new.clone();
    lonely.has_existing_branches = false;
    assert_eq!(tabs_to(&lonely, NewAgentTarget::ExistingBranch), None);
    assert_eq!(tabs_to(&lonely, NewAgentTarget::Base), Some(vec![tab()]));

    assert_eq!(
        arrows_to_agent(&new, 2),
        Some(vec![key(OverlayKey::Down), key(OverlayKey::Down)])
    );
    let mut on_codex = new.clone();
    on_codex.selected_agent = 1;
    assert_eq!(
        arrows_to_agent(&on_codex, 0),
        Some(vec![key(OverlayKey::Up)])
    );
    assert_eq!(arrows_to_agent(&new, 9), None);
    // Existing-branch mode: the arrows move the branch list, not the agent.
    let existing = form(new_agent(NewAgentTarget::ExistingBranch, ""));
    assert_eq!(arrows_to_agent(&existing, 1), None);
}

#[test]
fn titles_split_into_question_and_key_hint_verbatim() {
    assert_eq!(
        split_hint("Change project default base   (type to filter · ↑/↓ select · Enter apply)"),
        (
            "Change project default base",
            Some("type to filter · ↑/↓ select · Enter apply")
        )
    );
    // SPECS §5's guard copy has a parenthesis but no three-space hint.
    let rebase = rebase().title;
    assert_eq!(split_hint(&rebase), (rebase.as_str(), None));
}

#[test]
fn a_picked_folder_goes_through_the_open_project_prompt() {
    assert_eq!(
        folder_answer("/work/repo", true),
        vec![
            HostEvent::RunPaletteAction(PaletteAction::OpenProject),
            ev(OverlayInput::SetText("/work/repo".into())),
            ev(OverlayInput::Submit),
        ]
    );
    assert_eq!(
        folder_answer("/work/repo", false),
        vec![
            ev(OverlayInput::SetText("/work/repo".into())),
            ev(OverlayInput::Submit),
        ]
    );
}

#[test]
fn every_global_chord_is_disabled_under_the_overlay_context() {
    let keymap = Keymap::new(KeymapOptions {
        use_f2_to_leave_focus: false,
        leave_focus_uses_shift: false,
        command_v_pastes: true,
    });
    let globals = crate::keys::binding_specs(&keymap)
        .into_iter()
        .filter(|s| s.context == "Global")
        .count();
    let bindings = key_bindings(&keymap);
    assert_eq!(bindings.len(), globals);
    assert!(bindings.iter().all(|b| gpui::is_no_action(b.action())));
}

// ---------------------------------------------------------------------------
// Through GPUI
// ---------------------------------------------------------------------------

/// The app's shape: a `"Global"` root that performs table actions, an `"App"`
/// pane that holds focus before an overlay opens, and the layer beside it
/// (never around it).
struct Shell {
    layer: Entity<OverlayLayer>,
    app_focus: FocusHandle,
    performed: Rc<RefCell<Vec<&'static str>>>,
}

impl Render for Shell {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let performed = self.performed.clone();
        div()
            .key_context("Global")
            .on_action(move |action: &KeymapAction, _, _| {
                if let Some(entry) = action.entry(Keymap::for_this_platform(false)) {
                    performed.borrow_mut().push(entry.id);
                }
            })
            .relative()
            .size_full()
            .child(
                div()
                    .key_context("App")
                    .track_focus(&self.app_focus)
                    .size_full(),
            )
            .child(self.layer.clone())
    }
}

/// A test window and the events its layer emitted.
struct Harness<'a> {
    shell: Entity<Shell>,
    cx: &'a mut VisualTestContext,
    events: Rc<RefCell<Vec<HostEvent>>>,
}

fn open(app: &mut TestAppContext) -> Harness<'_> {
    let keymap = Keymap::for_this_platform(false);
    app.update(|cx| {
        cx.set_global(Palette::dark());
        cx.bind_keys(table_bindings(keymap));
        register(cx, keymap);
    });
    let events: Rc<RefCell<Vec<HostEvent>>> = Rc::default();
    let sink = events.clone();
    let (shell, cx) = app.add_window_view(move |_, cx| Shell {
        layer: new_layer(move |event, _, _| sink.borrow_mut().push(event), cx),
        app_focus: cx.focus_handle(),
        performed: Rc::default(),
    });
    // The app pane holds focus before any overlay opens.
    cx.update(|window, cx| {
        let focus = shell.read(cx).app_focus.clone();
        focus.focus(window, cx);
    });
    cx.run_until_parked();
    Harness { shell, cx, events }
}

impl Harness<'_> {
    fn layer(&mut self) -> Entity<OverlayLayer> {
        self.shell.read_with(self.cx, |s, _| s.layer.clone())
    }

    fn show(&mut self, view: Option<OverlayView>) {
        let layer = self.layer();
        self.cx.update(|window, cx| {
            layer.update(cx, |l, cx| l.set_view(view, window, cx));
        });
        self.cx.run_until_parked();
    }

    fn take(&mut self) -> Vec<HostEvent> {
        std::mem::take(&mut *self.events.borrow_mut())
    }

    fn performed(&mut self) -> Vec<&'static str> {
        self.shell.read_with(self.cx, |s, _| {
            std::mem::take(&mut *s.performed.borrow_mut())
        })
    }

    fn keys(&mut self, keystrokes: &str) -> Vec<HostEvent> {
        self.take();
        self.cx.simulate_keystrokes(keystrokes);
        self.cx.run_until_parked();
        self.take()
    }

    fn click(&mut self, selector: &'static str, clicks: usize) -> Vec<HostEvent> {
        self.take();
        let bounds = self
            .cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} was not drawn"));
        let position = bounds.center();
        for n in 1..=clicks {
            self.cx.simulate_event(MouseDownEvent {
                button: MouseButton::Left,
                position,
                modifiers: Modifiers::none(),
                click_count: n,
                first_mouse: false,
            });
            self.cx.simulate_event(MouseUpEvent {
                button: MouseButton::Left,
                position,
                modifiers: Modifiers::none(),
                click_count: n,
            });
        }
        self.cx.run_until_parked();
        self.take()
    }

    fn layer_focused(&mut self) -> bool {
        let layer = self.layer();
        self.cx
            .update(|window, cx| layer.read(cx).focus_handle().is_focused(window))
    }

    fn keyboard_focused(&mut self) -> bool {
        let layer = self.layer();
        self.cx
            .update(|window, cx| layer.read(cx).keyboard_focused(window, cx))
    }

    fn app_focused(&mut self) -> bool {
        let shell = self.shell.clone();
        self.cx
            .update(|window, cx| shell.read(cx).app_focus.is_focused(window))
    }
}

#[gpui::test]
fn the_layer_takes_focus_and_gives_it_back(app: &mut TestAppContext) {
    let mut h = open(app);
    assert!(h.app_focused());
    h.show(Some(OverlayView::Dialog(quit())));
    assert!(h.layer_focused(), "an open overlay owns the keyboard");
    // Replaced by another overlay: still focused.
    h.show(Some(OverlayView::Palette(palette(""))));
    assert!(h.layer_focused());
    h.show(None);
    assert!(h.app_focused(), "focus returns to where it was");
    // Closed: keys are not the layer's.
    assert_eq!(h.keys("escape"), vec![]);
}

#[gpui::test]
fn table_chords_do_not_act_behind_an_overlay(app: &mut TestAppContext) {
    let mut h = open(app);
    // Sanity: with no overlay the Global chords perform.
    h.keys("shift-left f1");
    assert_eq!(h.performed(), vec!["SwitchProjectPrev", "OpenHelp"]);

    h.show(Some(OverlayView::Palette(palette(""))));
    let events = h.keys("shift-left alt-up ctrl-g f1 alt-1 ctrl-q");
    assert_eq!(
        h.performed(),
        Vec::<&str>::new(),
        "nothing behind the modal"
    );
    assert_eq!(events, vec![], "and the palette ignores chords");
}

#[gpui::test]
fn palette_keys_filter_select_and_run(app: &mut TestAppContext) {
    let mut h = open(app);
    let view = palette("");
    h.show(Some(OverlayView::Palette(view.clone())));
    assert_eq!(
        h.keys("p"),
        vec![ev(OverlayInput::PaletteFilter("p".into()))]
    );
    assert_eq!(h.keys("down"), vec![ev(OverlayInput::SelectRow(1))]);
    assert_eq!(
        h.keys("enter"),
        vec![ev(OverlayInput::PaletteRun(view.entries[0].action.clone()))]
    );
    assert_eq!(h.keys("escape"), vec![ev(OverlayInput::Cancel)]);

    // The host filtered and moved the selection: the view follows it.
    let mut filtered = palette("pu");
    filtered.selected = 1.min(filtered.entries.len() - 1);
    h.show(Some(OverlayView::Palette(filtered.clone())));
    assert_eq!(
        h.keys("s"),
        vec![ev(OverlayInput::PaletteFilter("pus".into()))]
    );
    assert_eq!(
        h.keys("backspace"),
        vec![ev(OverlayInput::PaletteFilter("p".into()))]
    );
    assert_eq!(
        h.keys("enter"),
        vec![ev(OverlayInput::PaletteRun(
            filtered.entries[filtered.selected].action.clone()
        ))]
    );
}

#[gpui::test]
fn palette_rows_run_on_click(app: &mut TestAppContext) {
    let mut h = open(app);
    let view = palette("");
    h.show(Some(OverlayView::Palette(view.clone())));
    assert_eq!(
        h.click("palette-row-2", 1),
        vec![ev(OverlayInput::PaletteRun(view.entries[2].action.clone()))]
    );
    // The backdrop (above the card, which hangs 64px from the top) is Esc.
    h.take();
    h.cx.simulate_click(gpui::point(gpui::px(20.), gpui::px(20.)), Modifiers::none());
    h.cx.run_until_parked();
    assert_eq!(h.take(), vec![ev(OverlayInput::Cancel)]);
}

#[gpui::test]
fn confirmations_press_their_buttons_and_never_confirm_on_enter(app: &mut TestAppContext) {
    let mut h = open(app);
    h.show(Some(OverlayView::Dialog(abandon(true))));
    assert_eq!(h.keys("enter"), vec![], "no default button, no Enter");
    assert_eq!(h.keys("x"), vec![], "a stray key does nothing");
    assert_eq!(h.keys("y"), vec![ev(OverlayInput::Choose("y".into()))]);
    assert_eq!(h.keys("escape"), vec![ev(OverlayInput::Cancel)]);
    assert_eq!(
        h.click("overlay-button-y", 1),
        vec![ev(OverlayInput::Choose("y".into()))]
    );
    assert_eq!(
        h.click("overlay-button-n", 1),
        vec![ev(OverlayInput::Choose("n".into()))]
    );

    h.show(Some(OverlayView::Dialog(close_session())));
    assert_eq!(h.keys("3"), vec![ev(OverlayInput::Choose("3".into()))]);
    assert_eq!(
        h.click("overlay-button-3", 1),
        vec![ev(OverlayInput::Choose("3".into()))]
    );
    assert_eq!(
        h.click("overlay-button-Esc", 1),
        vec![ev(OverlayInput::Choose("Esc".into()))]
    );
}

#[gpui::test]
fn text_dialogs_type_through_the_host(app: &mut TestAppContext) {
    let mut h = open(app);
    h.show(Some(OverlayView::Dialog(rename())));
    assert_eq!(
        h.keys("a shift-b"),
        vec![key(OverlayKey::Char('a')), key(OverlayKey::Char('B'))]
    );
    assert_eq!(h.keys("backspace"), vec![key(OverlayKey::Backspace)]);
    assert_eq!(h.keys("enter"), vec![ev(OverlayInput::Submit)]);
    assert_eq!(
        h.click("overlay-button-Enter", 1),
        vec![ev(OverlayInput::Choose("Enter".into()))]
    );
}

#[gpui::test]
fn the_new_agent_form_answers_with_the_tui_keys(app: &mut TestAppContext) {
    let mut h = open(app);
    h.show(Some(OverlayView::Dialog(new_agent(
        NewAgentTarget::NewBranch,
        "fix",
    ))));
    // Target segments are Tab presses along the TUI's cycle.
    assert_eq!(
        h.click("new-agent-target-existing", 1),
        vec![key(OverlayKey::Tab)]
    );
    assert_eq!(
        h.click("new-agent-target-base", 1),
        vec![key(OverlayKey::Tab), key(OverlayKey::Tab)]
    );
    assert_eq!(h.click("new-agent-target-new", 1), vec![], "already there");
    // Agent chips are arrow presses to that agent.
    assert_eq!(
        h.click("new-agent-agent-3", 1),
        vec![
            key(OverlayKey::Down),
            key(OverlayKey::Down),
            key(OverlayKey::Down)
        ]
    );
    assert_eq!(h.keys("x"), vec![key(OverlayKey::Char('x'))]);
    assert_eq!(h.keys("tab"), vec![key(OverlayKey::Tab)]);
    assert_eq!(h.keys("enter"), vec![ev(OverlayInput::Submit)]);
    assert_eq!(
        h.click("overlay-button-Enter", 1),
        vec![ev(OverlayInput::Choose("Enter".into()))]
    );
    assert!(
        h.cx.debug_bounds("overlay-button-Tab").is_none(),
        "the target button is the segmented control"
    );

    // Existing branch: rows select, agents are fixed.
    h.show(Some(OverlayView::Dialog(new_agent(
        NewAgentTarget::ExistingBranch,
        "flightdeck",
    ))));
    assert_eq!(
        h.click("dialog-row-1", 1),
        vec![ev(OverlayInput::SelectRow(1))]
    );
    assert_eq!(h.click("new-agent-agent-2", 1), vec![]);
    assert_eq!(h.keys("down"), vec![key(OverlayKey::Down)]);
}

#[gpui::test]
fn the_folder_browser_navigates_and_takes_the_native_picker(app: &mut TestAppContext) {
    let mut h = open(app);
    h.show(Some(OverlayView::Dialog(open_project())));
    assert_eq!(
        h.click("dialog-row-2", 1),
        vec![ev(OverlayInput::SelectRow(2))]
    );
    assert_eq!(
        h.click("dialog-row-1", 2),
        vec![
            ev(OverlayInput::SelectRow(1)),
            ev(OverlayInput::SelectRow(1)),
            key(OverlayKey::Right)
        ],
        "a double click opens the folder"
    );
    assert_eq!(h.click("overlay-button-←", 1), vec![key(OverlayKey::Left)]);
    assert_eq!(h.keys("right"), vec![key(OverlayKey::Right)]);

    assert_eq!(h.click("overlay-button-choose-folder", 1), vec![]);
    assert!(h.cx.did_prompt_for_paths());
    h.cx.simulate_path_prompt_response(|options| {
        assert!(options.directories && !options.files && !options.multiple);
        Some(vec![PathBuf::from("/work/repo")])
    });
    h.cx.run_until_parked();
    assert_eq!(
        h.take(),
        vec![
            ev(OverlayInput::SetText("/work/repo".into())),
            ev(OverlayInput::Submit)
        ]
    );
}

#[gpui::test]
fn the_shell_entry_points_open_the_palette_and_a_picked_project(app: &mut TestAppContext) {
    let h = open(app);
    let recorded: Rc<RefCell<Vec<HostEvent>>> = Rc::default();
    let sink = recorded.clone();
    let emit: Emit = Rc::new(move |event, _, _| sink.borrow_mut().push(event));
    h.cx.update(|window, cx| {
        open_palette(&emit, window, cx);
        pick_project_folder(&emit, window, cx);
    });
    h.cx.run_until_parked();
    h.cx.simulate_path_prompt_response(|_| Some(vec![PathBuf::from("/work/other")]));
    h.cx.run_until_parked();
    assert_eq!(
        recorded.borrow().clone(),
        vec![
            HostEvent::OpenPalette,
            HostEvent::RunPaletteAction(PaletteAction::OpenProject),
            ev(OverlayInput::SetText("/work/other".into())),
            ev(OverlayInput::Submit),
        ]
    );

    // A cancelled picker does nothing.
    recorded.borrow_mut().clear();
    h.cx.update(|window, cx| pick_project_folder(&emit, window, cx));
    h.cx.run_until_parked();
    h.cx.simulate_path_prompt_response(|_| None);
    h.cx.run_until_parked();
    assert!(recorded.borrow().is_empty());
}

#[gpui::test]
fn messages_and_panels_answer_the_host(app: &mut TestAppContext) {
    let mut h = open(app);
    h.show(Some(OverlayView::Message(MessageView {
        text: "Refused: nothing to push.".into(),
    })));
    assert_eq!(h.keys("k"), vec![ev(OverlayInput::Submit)]);
    assert_eq!(
        h.click("overlay-button-Enter", 1),
        vec![ev(OverlayInput::Submit)]
    );
    h.show(Some(OverlayView::Help(flightdeck::tui::help::help_doc(
        false, false,
    ))));
    // The help card reads no keys itself: the TUI's help keys reach the host.
    assert_eq!(h.keys("down"), vec![key(OverlayKey::Down)]);
    assert_eq!(h.click("overlay-close", 1), vec![ev(OverlayInput::Cancel)]);
}

/// The configuration manager as the host reads it out (`overlay_bridge`),
/// from the real manager over empty config files.
fn config() -> flightdeck::host::ConfigView {
    let cm = flightdeck::tui::config_manager::ConfigManager::new(
        "demo",
        Some(PathBuf::from("/home/u/.flightdeck/config.toml")),
        "/repo/.flightdeck/config.toml",
        toml::Table::new(),
        toml::Table::new(),
        vec!["claude".into(), "codex".into()],
    );
    flightdeck::host::ConfigView {
        project_name: cm.project_name().to_string(),
        scope: cm.scope(),
        path: cm.current_path(),
        rows: cm.rows(),
        inherited: cm.inherited_rows(),
        selected: cm.selected_index(),
        editing: cm.is_editing(),
        dirty: cm.dirty(),
        status: cm.status().map(str::to_string),
    }
}

#[gpui::test]
fn the_config_card_reads_its_own_keys(app: &mut TestAppContext) {
    let mut h = open(app);
    h.show(Some(OverlayView::Config(config())));
    assert!(
        !h.layer_focused(),
        "the card, not the layer root, has focus"
    );
    assert!(h.keyboard_focused());
    // The manager's alphabet, once each (the layer adds nothing on top) ...
    assert_eq!(h.keys("down"), vec![key(OverlayKey::Down)]);
    assert_eq!(h.keys("s"), vec![key(OverlayKey::Char('s'))]);
    // ... and what it does not take goes nowhere, behind the modal included.
    assert_eq!(h.keys("x shift-left"), vec![]);
    assert_eq!(h.performed(), Vec::<&str>::new());
    // Another overlay replaces it: the layer takes the keyboard back.
    h.show(Some(OverlayView::Dialog(quit())));
    assert!(h.layer_focused());
    h.show(None);
    assert!(h.app_focused());
}
