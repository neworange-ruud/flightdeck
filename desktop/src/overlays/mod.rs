//! Modal overlays: the command palette, dialogs and confirmations the host
//! reports through [`AppHost::overlay`](flightdeck::host::AppHost::overlay),
//! drawn natively (beads `remote-control-bmej.4.1`–`4.3`).
//!
//! ## The rule: a pure function of the view model
//!
//! Nothing here holds dialog state. The host already keeps the TUI's one
//! interactive state (prompt, palette, configuration manager, …) and reads it
//! out as an [`OverlayView`]; this module draws that value and turns clicks
//! and keys into [`HostEvent`]s through an [`Emit`] callback. Filtering,
//! selection, validation and every guard stay in the host, so the GUI cannot
//! drift from the TUI: typing in the palette sends
//! [`OverlayInput::PaletteFilter`] and the next `overlay()` carries the
//! filtered rows; a dialog's `y` is [`OverlayInput::Choose`]`("y")`, the very
//! keypress the TUI's own dialog button synthesizes.
//!
//! ## Mounting (for the shell)
//!
//! ```text
//! let layer = cx.new(|cx| OverlayLayer::new(emit, cx));   // once
//! overlays::register(cx, keymap);                          // once, after keys::register
//! // after every host turn that may have changed it:
//! layer.update(cx, |l, cx| l.set_view(host.overlay(), window, cx));
//! // in the root view's render, as the LAST child of a `.relative()` root:
//! root.child(layer.clone())
//! ```
//!
//! [`OverlayLayer::set_view`] takes keyboard focus when an overlay appears and
//! hands it back to whatever held it when the overlay goes away. The shell's
//! entry points: [`open_palette`] (the titlebar search field, `Ctrl-g`) and
//! [`pick_project_folder`] (the `+` beside the project tabs).
//!
//! ## Key context
//!
//! The layer's root carries the key context [`KEY_CONTEXT`] (`"Overlay"`). It
//! is a sibling of the main area, never an ancestor of a `"Terminal"` element
//! (GPUI matches a context anywhere on the focus path; see [`crate::keys`]),
//! and [`register`] binds every `"Global"` table chord to GPUI's `NoAction`
//! under it: while an overlay is up, `Shift-Left` or `Alt-Up` must not switch
//! projects behind the user's back, exactly as the TUI's modal swallows them.
//!
//! ## Plugging in another overlay
//!
//! Each [`OverlayView`] variant is one arm of [`render_overlay`] and one of
//! [`key_events`]. A module for a variant exposes
//!
//! ```text
//! pub fn render(view: &TheView, cx: &OverlayCx) -> AnyElement;
//! pub fn key_events(view: &TheView, key: &KeyPress) -> Vec<HostEvent>;   // optional
//! ```
//!
//! and replaces its placeholder arm. Without its own `key_events`, a variant
//! gets [`forward_keys`]: every key as the [`OverlayKey`] the TUI's handler
//! expects, which already drives help, the configuration manager and the
//! remote overlays with the TUI's own keys. The shared chrome ([`card`],
//! [`button`], [`keycap`], [`text_field`], [`hint_bar`]) is public so every
//! overlay looks like one family.

pub mod about;
pub mod config;
mod confirm;
mod dialog;
pub mod help;
mod new_agent;
mod palette;
pub mod remote;
pub mod update;

#[cfg(any(test, feature = "spike-snapshot"))]
pub mod fixtures;
#[cfg(test)]
mod tests;

use std::rc::Rc;

use flightdeck::app::keymap::Keymap;
use flightdeck::host::{
    ButtonRole, DialogButton, DialogKind, HostEvent, OverlayInput, OverlayKey, OverlayView,
};
use gpui::{
    div, prelude::FluentBuilder as _, px, AnyElement, App, AppContext as _, Context, Div, Entity,
    FocusHandle, FontWeight, Hsla, InteractiveElement, IntoElement, KeyBinding, KeyDownEvent,
    Keystroke, MouseButton, NoAction, ParentElement, PathPromptOptions, Render, ScrollHandle,
    SharedString, Stateful, StatefulInteractiveElement, Styled, Window,
};
use std::path::PathBuf;

use crate::keys::binding_specs;
use crate::theme::{Palette, SCRIM_ALPHA};

/// The GPUI key context on the overlay layer's root.
pub const KEY_CONTEXT: &str = "Overlay";

// The chrome the panels (help, about, config, remote) share, reused here so
// every overlay's keycaps, labels and mono text are the same.
pub(crate) use help::{keycap, section_label, MONO};

/// How a view hands the host an answer. A view never touches the host: it calls
/// this with the event a click or key means, and the caller applies it with
/// `AppHost::handle`.
///
/// The caller must not update the [`OverlayLayer`] from inside it (the layer
/// may be the entity being updated when a key arrives); refresh the layer with
/// [`OverlayLayer::set_view`] once the host has handled the event, e.g. from a
/// `cx.defer` or the shell's next render.
pub type Emit = Rc<dyn Fn(HostEvent, &mut Window, &mut App)>;

/// Apply `events` in order through `emit`: a multi-key gesture, such as a
/// click on a New Agent target two `Tab`s away. The host refuses an input that
/// does not fit and changes nothing, so a step that no longer applies is inert.
pub fn emit_all(emit: &Emit, events: Vec<HostEvent>, window: &mut Window, cx: &mut App) {
    for event in events {
        emit(event, window, cx);
    }
}

/// Shorthand for an [`OverlayInput`] event.
pub fn overlay(input: OverlayInput) -> HostEvent {
    HostEvent::Overlay(input)
}

/// Facts the view model does not carry but the chrome shows: the palette's
/// context line (design D, "Quiet rail + palette") names where its commands
/// will act. Set by the shell from its own reads of the host.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OverlayContext {
    /// The active project's name.
    pub project: Option<String>,
    /// The selected Agent Session Tab's name.
    pub agent: Option<String>,
}

/// What every overlay renderer receives.
pub struct OverlayCx<'a> {
    /// The installed palette.
    pub palette: &'a Palette,
    /// Where clicks go.
    pub emit: &'a Emit,
    /// The shell's facts for the chrome.
    pub context: &'a OverlayContext,
    /// The configuration card's own focus: it reads its keys itself
    /// ([`config::key_to_input`]), so the layer focuses this handle instead of
    /// its root while the configuration manager is up.
    pub config_focus: &'a FocusHandle,
    /// Scroll state for the overlay's one scrolling list (palette rows, a
    /// dialog's list), kept across frames by the layer so the highlighted row
    /// can be scrolled into view as the host moves it.
    pub list_scroll: &'a ScrollHandle,
}

impl OverlayCx<'_> {
    /// A click handler that applies `events` in order.
    pub fn on_click_emit(
        &self,
        events: Vec<HostEvent>,
    ) -> impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static {
        let emit = self.emit.clone();
        move |_, window, cx| {
            emit_all(&emit, events.clone(), window, cx);
        }
    }
}

// ---------------------------------------------------------------------------
// The layer
// ---------------------------------------------------------------------------

/// The window's overlay layer: draws the host's current overlay above
/// everything, owns the keyboard while one is up, and is empty otherwise.
pub struct OverlayLayer {
    focus: FocusHandle,
    config_focus: FocusHandle,
    emit: Emit,
    view: Option<OverlayView>,
    context: OverlayContext,
    list_scroll: ScrollHandle,
    /// Who had focus before the overlay took it; given back when it closes.
    restore_focus: Option<FocusHandle>,
}

impl OverlayLayer {
    /// A layer answering through `emit`, with nothing on screen.
    pub fn new(emit: Emit, cx: &mut Context<Self>) -> Self {
        Self {
            focus: cx.focus_handle(),
            config_focus: cx.focus_handle(),
            emit,
            view: None,
            context: OverlayContext::default(),
            list_scroll: ScrollHandle::new(),
            restore_focus: None,
        }
    }

    /// Show `view` (the host's `overlay()` this turn), or nothing.
    ///
    /// Opening takes keyboard focus, remembering the previous holder; closing
    /// gives it back (if that element still exists — otherwise the shell's own
    /// focus rules apply). The highlighted row of a list is scrolled into view.
    pub fn set_view(
        &mut self,
        view: Option<OverlayView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if view == self.view {
            return;
        }
        let was_open = self.view.is_some();
        let selected_before = self.view.as_ref().and_then(selected_row);
        self.view = view;
        match &self.view {
            Some(open) => {
                if !was_open {
                    self.restore_focus = window
                        .focused(cx)
                        .filter(|f| *f != self.focus && *f != self.config_focus);
                }
                // Still up after a click moved focus, or a different overlay:
                // (re)take the keyboard for whichever element reads it.
                let target = match open {
                    OverlayView::Config(_) => &self.config_focus,
                    _ => &self.focus,
                };
                if !target.is_focused(window) {
                    target.focus(window, cx);
                }
            }
            None => {
                if let Some(previous) = self.restore_focus.take() {
                    previous.focus(window, cx);
                }
            }
        }
        if let Some(selected) = self.view.as_ref().and_then(selected_row) {
            if Some(selected) != selected_before {
                self.list_scroll.scroll_to_item(selected);
            }
        }
        cx.notify();
    }

    /// Replace the chrome's context facts.
    pub fn set_context(&mut self, context: OverlayContext, cx: &mut Context<Self>) {
        if context != self.context {
            self.context = context;
            cx.notify();
        }
    }

    /// The overlay on screen, as last set.
    pub fn view(&self) -> Option<&OverlayView> {
        self.view.as_ref()
    }

    /// Whether an overlay is up (and so owns the keyboard).
    pub fn is_open(&self) -> bool {
        self.view.is_some()
    }

    /// The layer's focus handle, for a shell that re-asserts focus itself.
    /// (While the configuration manager is up its card holds focus instead;
    /// see [`OverlayLayer::keyboard_focused`].)
    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    /// Whether the element that reads the open overlay's keys has focus.
    pub fn keyboard_focused(&self, window: &Window, cx: &App) -> bool {
        self.focus.contains_focused(window, cx)
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(view) = self.view.clone() else {
            return;
        };
        // Modal: nothing typed while an overlay is up reaches anything behind it.
        cx.stop_propagation();
        let press = match key_press(&event.keystroke) {
            Some(KeyPress::Paste(_)) => match cx.read_from_clipboard().and_then(|c| c.text()) {
                Some(text) => KeyPress::Paste(text),
                None => return,
            },
            Some(press) => press,
            None => return,
        };
        let events = key_events(&view, &press);
        emit_all(&self.emit, events, window, cx);
    }
}

impl Render for OverlayLayer {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(view) = &self.view else {
            // Out of layout entirely while nothing is up.
            return div().id("overlay-layer").absolute();
        };
        let p = *Palette::global(cx);
        let mut scrim = p.scrim.hsla();
        scrim.a = SCRIM_ALPHA;
        let ocx = OverlayCx {
            palette: &p,
            emit: &self.emit,
            context: &self.context,
            config_focus: &self.config_focus,
            list_scroll: &self.list_scroll,
        };
        let content = render_overlay(view, &ocx, cx);
        let emit = self.emit.clone();
        div()
            .id("overlay-layer")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key_down))
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .bg(scrim)
            .occlude()
            .flex()
            .flex_col()
            .items_center()
            .map(|this| match view {
                // A palette hangs from the top, where the titlebar field that
                // opens it is; everything else is centred.
                OverlayView::Palette(_) => this.justify_start().pt(px(64.)),
                _ => this.justify_center(),
            })
            // A click on the backdrop is Esc: it dismisses without deciding.
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                emit(overlay(OverlayInput::Cancel), window, cx);
            })
            .child(
                div()
                    // Clicks inside the overlay never reach the backdrop.
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(content),
            )
    }
}

/// The highlighted row of the overlay's scrolling list, if it has one.
fn selected_row(view: &OverlayView) -> Option<usize> {
    match view {
        OverlayView::Palette(p) => (!p.entries.is_empty()).then_some(p.selected),
        OverlayView::Dialog(d) => d.list.iter().position(|r| r.selected),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Dispatch: one arm per overlay
// ---------------------------------------------------------------------------

/// Draw `view`. One arm per [`OverlayView`] variant; the git status panel,
/// which has no view of its own yet, draws [`placeholder`], the generic
/// title/body/buttons card.
pub fn render_overlay(view: &OverlayView, cx: &OverlayCx, app: &App) -> AnyElement {
    let emit = || cx.emit.clone();
    match view {
        OverlayView::Palette(v) => palette::render(v, cx),
        OverlayView::Dialog(v) => match &v.kind {
            DialogKind::NewAgent(form) => new_agent::render(v, form, cx),
            kind if confirm::handles(kind) => confirm::render(v, cx),
            _ => dialog::render(v, cx),
        },
        OverlayView::Message(m) => message(&m.text, cx),
        OverlayView::Help(doc) => help::help_view(doc, emit(), app).into_any_element(),
        OverlayView::About(doc) => about::about_view(doc, emit(), app).into_any_element(),
        OverlayView::Config(v) => {
            config::config_view(v, emit(), cx.config_focus, app).into_any_element()
        }
        OverlayView::WebAccess(v) => remote::web_access_view(v, emit(), app).into_any_element(),
        OverlayView::Pairing(v) => remote::pairing_view(v, emit(), app).into_any_element(),
        OverlayView::GitStatus(status) => {
            placeholder("Git status", status.pr_url.iter().cloned().collect(), cx)
        }
    }
}

/// What a key press means for `view`, as the events to emit (possibly none).
pub fn key_events(view: &OverlayView, key: &KeyPress) -> Vec<HostEvent> {
    match view {
        OverlayView::Palette(v) => palette::key_events(v, key),
        OverlayView::Dialog(v) => dialog::key_events(v, key),
        // SPECS §22: any input dismisses a notification. Esc is still Cancel,
        // so a front-end that treats the two differently later can.
        OverlayView::Message(_) => match key {
            KeyPress::Esc => vec![overlay(OverlayInput::Cancel)],
            _ => vec![overlay(OverlayInput::Submit)],
        },
        // The configuration card reads its own keys (`config::key_to_input`,
        // the TUI manager's alphabet) and stops the ones it takes; what
        // bubbles past it is not the manager's.
        OverlayView::Config(_) => Vec::new(),
        OverlayView::Help(_)
        | OverlayView::About(_)
        | OverlayView::GitStatus(_)
        | OverlayView::WebAccess(_)
        | OverlayView::Pairing(_) => forward_keys(key),
    }
}

/// Every key as the plain [`OverlayKey`] the TUI's handler for that overlay
/// reads (Enter is [`OverlayInput::Submit`], Esc [`OverlayInput::Cancel`]).
/// Parity by construction for an overlay whose keys are its whole interface.
pub fn forward_keys(key: &KeyPress) -> Vec<HostEvent> {
    let k = |key| vec![overlay(OverlayInput::Key(key))];
    match key {
        KeyPress::Enter => vec![overlay(OverlayInput::Submit)],
        KeyPress::Esc => vec![overlay(OverlayInput::Cancel)],
        KeyPress::Tab => k(OverlayKey::Tab),
        KeyPress::Backspace => k(OverlayKey::Backspace),
        KeyPress::Up => k(OverlayKey::Up),
        KeyPress::Down => k(OverlayKey::Down),
        KeyPress::Left => k(OverlayKey::Left),
        KeyPress::Right => k(OverlayKey::Right),
        KeyPress::Text(text) | KeyPress::Paste(text) => text
            .chars()
            .map(|c| overlay(OverlayInput::Key(OverlayKey::Char(c))))
            .collect(),
    }
}

// ---------------------------------------------------------------------------
// Keys
// ---------------------------------------------------------------------------

/// A key press, reduced to what an overlay distinguishes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyPress {
    Enter,
    Esc,
    Tab,
    Backspace,
    Up,
    Down,
    Left,
    Right,
    /// Printable text the key typed (usually one character).
    Text(String),
    /// A paste. [`key_press`] returns it with empty text; the layer fills it
    /// from the clipboard before mapping it.
    Paste(String),
}

/// The overlay meaning of a keystroke, or `None` for keys an overlay ignores
/// (function keys, modifier-only presses, `Cmd`/`Ctrl` chords other than
/// paste, Alt-typed symbols).
pub fn key_press(keystroke: &Keystroke) -> Option<KeyPress> {
    let m = &keystroke.modifiers;
    if keystroke.key == "v" && (m.platform || m.control) && !m.alt {
        return Some(KeyPress::Paste(String::new()));
    }
    if m.platform || m.control || m.function {
        return None;
    }
    let named = match keystroke.key.as_str() {
        "enter" => Some(KeyPress::Enter),
        "escape" => Some(KeyPress::Esc),
        // Shift-Tab has no TUI meaning in any overlay.
        "tab" if !m.shift => Some(KeyPress::Tab),
        "tab" => None,
        "backspace" => Some(KeyPress::Backspace),
        "up" => Some(KeyPress::Up),
        "down" => Some(KeyPress::Down),
        "left" => Some(KeyPress::Left),
        "right" => Some(KeyPress::Right),
        _ => None,
    };
    if named.is_some() || m.alt {
        return named;
    }
    keystroke
        .key_char
        .as_ref()
        .filter(|t| !t.is_empty() && !t.chars().any(char::is_control))
        .map(|t| KeyPress::Text(t.clone()))
}

/// GPUI bindings for the overlay context: every `"Global"` chord of `keymap`
/// disabled under [`KEY_CONTEXT`], so a table chord pressed with an overlay
/// up reaches the overlay's key handler (which ignores it) instead of acting
/// behind the modal. Deeper contexts win in GPUI, so this beats the
/// `"Global"` binding on the window root.
pub fn key_bindings(keymap: &Keymap) -> Vec<KeyBinding> {
    binding_specs(keymap)
        .into_iter()
        .filter(|spec| spec.context == "Global")
        .map(|spec| KeyBinding::new(&spec.keystroke, NoAction {}, Some(KEY_CONTEXT)))
        .collect()
}

/// Bind [`key_bindings`]. Call once at start-up, after [`crate::keys::register`].
pub fn register(cx: &mut App, keymap: &Keymap) {
    cx.bind_keys(key_bindings(keymap));
}

// ---------------------------------------------------------------------------
// Entry points for the shell
// ---------------------------------------------------------------------------

/// Open the command palette: the titlebar search field's click (its `Ctrl-g`
/// already reaches the host through the keymap table).
pub fn open_palette(emit: &Emit, window: &mut Window, cx: &mut App) {
    emit(HostEvent::OpenPalette, window, cx);
}

/// Pick a project folder with the platform's folder picker and open it: the
/// titlebar `+`. The chosen path is `HostEvent::OpenProject`, which runs the
/// host's Open Project path (the TUI folder browser's Enter), so it gets
/// exactly the TUI's checks — outside a git repository it is refused with the
/// TUI's `Could not open project: …` notification, an already-open project is
/// switched to, and an isolated run refuses Open Project outright. A
/// cancelled picker does nothing.
pub fn pick_project_folder(emit: &Emit, window: &mut Window, cx: &mut App) {
    choose_folder(emit.clone(), true, window, cx);
}

/// Answer an open folder-browser dialog with the platform picker's choice.
fn choose_folder(emit: Emit, open_prompt_first: bool, window: &mut Window, cx: &mut App) {
    let chosen = cx.prompt_for_paths(PathPromptOptions {
        files: false,
        directories: true,
        multiple: false,
        prompt: Some("Open Project".into()),
    });
    window
        .spawn(cx, async move |cx| {
            let Ok(Ok(Some(paths))) = chosen.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let events = folder_answer(path, open_prompt_first);
            // The window may have closed while the picker was up.
            cx.update(|window, cx| emit_all(&emit, events, window, cx))
                .ok();
        })
        .detach();
}

/// The events that open `path`. With no prompt open, the host's own
/// `OpenProject` event. Answering the folder-browser prompt already on screen
/// (its "Choose…" button), type the path as the prompt's (a typed path wins
/// over the highlighted folder, SPECS §22) and press Open.
fn folder_answer(path: PathBuf, open_prompt_first: bool) -> Vec<HostEvent> {
    if open_prompt_first {
        return vec![HostEvent::OpenProject(path)];
    }
    vec![
        overlay(OverlayInput::SetText(path.to_string_lossy().into_owned())),
        overlay(OverlayInput::Submit),
    ]
}

// ---------------------------------------------------------------------------
// Shared chrome
// ---------------------------------------------------------------------------

/// The card a dialog sits in: raised above the scrim, `width` wide, with no
/// title row of its own — a dialog's heading is its question, and its answers
/// are in a [`footer`]. (The panels use [`help::card`], which has a title and
/// a close button; a question with Cancel beside its answers needs neither.)
pub fn dialog_card(p: &Palette, width: f32) -> Div {
    div()
        .flex()
        .flex_col()
        .w(px(width))
        .max_w(px(width))
        .bg(p.surface_sidebar.hsla())
        .border_1()
        .border_color(p.border_strong.hsla())
        .rounded(px(12.))
        .shadow_lg()
        .overflow_hidden()
        .text_color(p.ink.hsla())
        .text_size(px(13.))
}

/// The card's footer: the button row on the window surface, under a hairline.
pub fn footer(p: &Palette) -> Div {
    div()
        .flex()
        .flex_row()
        // Wrap rather than push the leftmost buttons out of a right-aligned
        // row: the dismissal is the first one, and must stay reachable.
        .flex_wrap()
        .items_center()
        .gap(px(8.))
        .px(px(16.))
        .py(px(12.))
        .bg(p.surface_window.hsla())
        .border_t_1()
        .border_color(p.hairline.hsla())
}

/// How a dialog button id (a TUI accelerator) is drawn as a keycap.
pub fn accel_keycap(id: &str) -> String {
    match id {
        "Enter" => "⏎".to_string(),
        "Esc" => "esc".to_string(),
        "Tab" => "tab".to_string(),
        other => other.to_string(),
    }
}

/// The debug selector a dialog button carries, for tests to find it.
pub fn button_selector(id: &str) -> String {
    format!("overlay-button-{id}")
}

/// One dialog button, styled by its [`ButtonRole`] (which
/// [`help::button`]'s kinds do not cover: a destructive answer is drawn in the
/// danger colours); the host's default (Enter) button is
/// the solid one, and the button's accelerator is drawn beside its label.
/// `on_click` is what it does.
pub fn dialog_button(
    p: &Palette,
    b: &DialogButton,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    dialog_button_with(p, b, Some(accel_keycap(&b.id)), on_click)
}

/// [`button`] with an explicit keycap (or none), for a GUI-only button that
/// has no TUI accelerator.
pub fn dialog_button_with(
    p: &Palette,
    b: &DialogButton,
    keycap: Option<String>,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let (bg, ink, border, hover): (Hsla, Hsla, Hsla, Hsla) = match b.role {
        ButtonRole::Destructive => (
            p.danger_bg.hsla(),
            p.danger.hsla(),
            p.danger_border.hsla(),
            p.danger_border.hsla(),
        ),
        _ if b.default => (
            p.ink.hsla(),
            p.surface_window.hsla(),
            p.ink.hsla(),
            p.ink_2.hsla(),
        ),
        ButtonRole::Primary | ButtonRole::Secondary => (
            p.surface_raised.hsla(),
            p.ink.hsla(),
            p.border_strong.hsla(),
            p.surface_raised_nested.hsla(),
        ),
        ButtonRole::Cancel => (
            p.surface_window.hsla(),
            p.ink_2.hsla(),
            p.border.hsla(),
            p.surface_raised.hsla(),
        ),
    };
    let selector = button_selector(&b.id);
    div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector)
        .flex()
        .flex_row()
        .flex_shrink_0()
        .items_center()
        .gap(px(8.))
        .h(px(30.))
        .px(px(12.))
        .rounded(px(7.))
        .border_1()
        .border_color(border)
        .bg(bg)
        .text_color(ink)
        .text_size(px(12.5))
        .font_weight(FontWeight::MEDIUM)
        .cursor_pointer()
        .hover(move |s| s.bg(hover))
        .on_click(on_click)
        .child(b.label.clone())
        .children(keycap.map(|k| {
            div()
                .font_family(MONO)
                .text_size(px(10.5))
                .opacity(0.7)
                .child(k)
        }))
}

/// A dialog's buttons in the platform order: dismissals left of the
/// decisions. Each presses its button in the host.
pub fn buttons(p: &Palette, buttons: &[DialogButton], cx: &OverlayCx) -> Vec<Stateful<Div>> {
    let (cancels, decisions): (Vec<&DialogButton>, Vec<&DialogButton>) =
        buttons.iter().partition(|b| b.role == ButtonRole::Cancel);
    cancels
        .into_iter()
        .chain(decisions)
        .map(|b| {
            dialog_button(
                p,
                b,
                cx.on_click_emit(vec![overlay(OverlayInput::Choose(b.id.clone()))]),
            )
        })
        .collect()
}

/// [`buttons`] in a right-aligned [`footer`].
pub fn button_row(p: &Palette, dialog_buttons: &[DialogButton], cx: &OverlayCx) -> Div {
    footer(p)
        .justify_end()
        .children(buttons(p, dialog_buttons, cx))
}

/// A read-only picture of a text field: its text (or a faint placeholder) and
/// a caret. Typing goes to the host as key presses; the next view shows it.
pub fn text_field(p: &Palette, text: &str, placeholder: &str, mono: bool) -> Div {
    let caret = div().w(px(1.5)).h(px(16.)).bg(p.accent.hsla());
    div()
        .flex()
        .flex_row()
        .items_center()
        .h(px(34.))
        .px(px(10.))
        .rounded(px(7.))
        .border_1()
        .border_color(p.border_strong.hsla())
        .bg(p.surface_input.hsla())
        .when(mono, |d| d.font_family(MONO).text_size(px(12.5)))
        .map(|d| {
            if text.is_empty() {
                d.child(caret).child(
                    div()
                        .pl(px(2.))
                        .text_color(p.faint.hsla())
                        .child(placeholder.to_string()),
                )
            } else {
                d.child(div().text_color(p.ink.hsla()).child(text.to_string()))
                    .child(caret)
            }
        })
}

/// A row of keyboard hints (`↑↓ select  ⏎ run  esc close`).
pub fn hint_bar(p: &Palette, hints: &[(&str, &str)]) -> Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(12.))
        .text_size(px(11.5))
        .text_color(p.muted.hsla())
        .children(hints.iter().map(|(key, what)| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(5.))
                .child(keycap(key.to_string(), p))
                .child(what.to_string())
        }))
}

/// Split a TUI dialog title into its question and its trailing key hint: the
/// TUI writes `Question   (↑/↓ select · Enter apply)`, three spaces then a
/// parenthesis. The GUI shows the question as the heading and the hint as a
/// muted line, both verbatim.
pub fn split_hint(title: &str) -> (&str, Option<&str>) {
    if let Some(at) = title.find("   (") {
        let hint = title[at..].trim();
        if let Some(inner) = hint.strip_prefix('(').and_then(|h| h.strip_suffix(')')) {
            return (title[..at].trim_end(), Some(inner));
        }
    }
    (title, None)
}

/// The heading and body block every dialog opens with.
pub fn heading(p: &Palette, title: &str, body: &[String]) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(
            div()
                .text_size(px(14.5))
                .line_height(px(21.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(p.ink.hsla())
                .child(title.to_string()),
        )
        .children(body.iter().map(|line| {
            div()
                .text_size(px(13.))
                .line_height(px(19.))
                .text_color(p.ink_2.hsla())
                .child(line.clone())
        }))
}

/// A browser-opened dialog's origin line (D13: `opened from browser · …`).
pub fn origin_line(p: &Palette, label: Option<&str>) -> Option<Div> {
    label.map(|l| {
        div()
            .text_size(px(11.5))
            .text_color(p.status_attention.hsla())
            .child(l.to_string())
    })
}

/// A notification (SPECS §22): its text, verbatim, and OK. Refusals, errors
/// and warnings carry their `Refused:`/`Error:`/`WARNING:` prefix in the text.
fn message(text: &str, cx: &OverlayCx) -> AnyElement {
    let p = cx.palette;
    let ok = DialogButton {
        id: "Enter".to_string(),
        label: "OK".to_string(),
        role: ButtonRole::Primary,
        default: true,
    };
    let lines: Vec<String> = text.lines().map(str::to_string).collect();
    dialog_card(p, 440.)
        .child(
            div()
                .p(px(20.))
                .flex()
                .flex_col()
                .gap(px(4.))
                .children(lines.iter().map(|line| {
                    div()
                        .text_size(px(13.5))
                        .line_height(px(20.))
                        .text_color(p.ink.hsla())
                        .child(line.clone())
                })),
        )
        .child(footer(p).justify_end().child(dialog_button(
            p,
            &ok,
            cx.on_click_emit(vec![overlay(OverlayInput::Submit)]),
        )))
        .into_any_element()
}

/// The generic card for an overlay whose own view has not been written yet:
/// its title, its text lines, and Close (Esc). Keys still reach the host as
/// the TUI's own ([`forward_keys`]), so the overlay is usable meanwhile.
pub fn placeholder(title: &str, lines: Vec<String>, cx: &OverlayCx) -> AnyElement {
    let p = cx.palette;
    let close = DialogButton {
        id: "Esc".to_string(),
        label: "Close".to_string(),
        role: ButtonRole::Cancel,
        default: false,
    };
    dialog_card(p, 560.)
        .child(
            div()
                .id("overlay-placeholder-body")
                .max_h(px(480.))
                .overflow_y_scroll()
                .p(px(20.))
                .child(heading(p, title, &lines)),
        )
        .child(footer(p).justify_end().child(dialog_button(
            p,
            &close,
            cx.on_click_emit(vec![overlay(OverlayInput::Cancel)]),
        )))
        .into_any_element()
}

/// A convenience for a shell or test that builds a layer from a closure.
pub fn new_layer(
    emit: impl Fn(HostEvent, &mut Window, &mut App) + 'static,
    cx: &mut App,
) -> Entity<OverlayLayer> {
    let emit: Emit = Rc::new(emit);
    cx.new(|cx| OverlayLayer::new(emit, cx))
}
