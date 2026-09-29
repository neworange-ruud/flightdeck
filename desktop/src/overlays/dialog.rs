//! Dialogs that are not a confirmation or the New Agent form: the rename and
//! status prompts, the backend picker, the base-branch picker and the
//! project-folder browser (beads `remote-control-bmej.4.2`). Also the key
//! rules every dialog shares.
//!
//! A dialog is drawn from its [`DialogView`] alone: the question (and the
//! TUI's key hint, split off as a muted line), the body lines, the text
//! field, the list and the buttons, all verbatim.

use flightdeck::host::{
    ButtonRole, DialogButton, DialogKind, DialogView, HostEvent, OverlayInput, OverlayKey,
};
use gpui::{
    div, prelude::FluentBuilder as _, px, AnyElement, ClickEvent, InteractiveElement as _,
    IntoElement, ParentElement, SharedString, StatefulInteractiveElement as _, Styled,
};

use super::{
    button_row, buttons, choose_folder, dialog_button, dialog_button_with, dialog_card, footer,
    heading, origin_line, overlay, split_hint, text_field, KeyPress, OverlayCx, MONO,
};

/// The debug selector on dialog list row `index`, for tests.
pub fn row_selector(index: usize) -> String {
    format!("dialog-row-{index}")
}

/// Whether the dialog takes typing and list movement, rather than only its
/// buttons' accelerators.
fn is_form(view: &DialogView) -> bool {
    view.input.is_some() || !view.list.is_empty()
}

/// What a key means in any dialog.
///
/// - **Enter** presses the default button — and only when there is one. The
///   confirmations of destructive work (abandon, rebase, merge, quit, …) have
///   none, so Enter never destroys anything: the user presses the button, or
///   its letter.
/// - **Esc** cancels.
/// - In a **form** (a text field or a list), every other key goes to the host
///   as the TUI's key, so typing, `↑`/`↓`, `Tab` (the New Agent target) and
///   `→`/`←` (into / out of a folder) behave as they do there.
/// - Otherwise a typed character presses the button it is the accelerator of
///   (`y`, `n`, `1`, `i`, …), and anything else is ignored — the TUI treats a
///   stray key in some confirmations as "dismiss", which a GUI user reaching
///   for the mouse should not trip over.
pub fn key_events(view: &DialogView, key: &KeyPress) -> Vec<HostEvent> {
    match key {
        KeyPress::Enter => {
            if view.buttons.iter().any(|b| b.default) {
                vec![overlay(OverlayInput::Submit)]
            } else {
                Vec::new()
            }
        }
        KeyPress::Esc => vec![overlay(OverlayInput::Cancel)],
        _ if is_form(view) => match key {
            // A multi-line paste lands in a one-line field as one line, the
            // host's own paste rule (`HostEvent::Paste`).
            KeyPress::Paste(text) => vec![HostEvent::Paste(text.clone())],
            other => super::forward_keys(other),
        },
        KeyPress::Text(text) => {
            let mut chars = text.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => view
                    .buttons
                    .iter()
                    .find(|b| b.id.chars().eq(std::iter::once(c)))
                    .map(|b| vec![overlay(OverlayInput::Choose(b.id.clone()))])
                    .unwrap_or_default(),
                _ => Vec::new(),
            }
        }
        _ => Vec::new(),
    }
}

/// Draw a dialog.
pub fn render(view: &DialogView, cx: &OverlayCx) -> AnyElement {
    let p = cx.palette;
    let (question, hint) = split_hint(&view.title);
    let browsing = matches!(view.kind, DialogKind::OpenProject { .. });
    let mono_list = matches!(
        view.kind,
        DialogKind::OpenProject { .. } | DialogKind::ChangeProjectBase
    );

    let field = view.input.as_deref().map(|text| {
        let placeholder = match view.kind {
            DialogKind::OpenProject { .. } => "or type a path",
            DialogKind::ChangeProjectBase => "filter branches",
            _ => "",
        };
        text_field(p, text, placeholder, mono_list)
    });

    let list = (!view.list.is_empty()).then(|| render_rows(view, mono_list, browsing, cx));

    let body = div()
        .flex()
        .flex_col()
        .gap(px(14.))
        .p(px(20.))
        .child(heading(p, question, &view.body))
        .children(origin_line(p, view.origin_label.as_deref()))
        .children(field)
        .children(list)
        .children(hint.map(|h| {
            div()
                .text_size(px(11.5))
                .text_color(p.muted.hsla())
                .child(h.to_string())
        }));

    let footer = if browsing {
        // The folder browser's own moves as buttons on the left, plus the
        // platform picker feeding the same prompt (so its checks still run).
        let emit = cx.emit.clone();
        let parent = DialogButton {
            id: "←".to_string(),
            label: "Parent folder".to_string(),
            role: ButtonRole::Secondary,
            default: false,
        };
        let native = DialogButton {
            id: "choose-folder".to_string(),
            label: "Choose folder…".to_string(),
            role: ButtonRole::Secondary,
            default: false,
        };
        footer(p)
            .child(dialog_button(
                p,
                &parent,
                cx.on_click_emit(vec![overlay(OverlayInput::Key(OverlayKey::Left))]),
            ))
            .child(dialog_button_with(
                p,
                &native,
                None,
                move |_: &ClickEvent, window, cx| choose_folder(emit.clone(), false, window, cx),
            ))
            .child(div().flex_1())
            .children(buttons(p, &view.buttons, cx))
    } else {
        button_row(p, &view.buttons, cx)
    };

    dialog_card(p, 520.)
        .child(body)
        .child(footer)
        .into_any_element()
}

/// The dialog's list: click to highlight (the host's `SelectRow`); in the
/// folder browser a double click opens the folder (`→`), elsewhere it
/// highlights and presses the default button.
pub fn render_rows(
    view: &DialogView,
    mono: bool,
    browsing: bool,
    cx: &OverlayCx,
) -> gpui::Stateful<gpui::Div> {
    let p = cx.palette;
    let has_default = view.buttons.iter().any(|b| b.default);
    div()
        .id("dialog-rows")
        .track_scroll(cx.list_scroll)
        .overflow_y_scroll()
        .max_h(px(240.))
        .p(px(4.))
        .rounded(px(8.))
        .border_1()
        .border_color(p.border.hsla())
        .bg(p.surface_terminal.hsla())
        .children(view.list.iter().enumerate().map(|(i, row)| {
            let selector = row_selector(i);
            let emit = cx.emit.clone();
            div()
                .id(SharedString::from(selector.clone()))
                .debug_selector(move || selector)
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.))
                .h(px(28.))
                .px(px(10.))
                .rounded(px(5.))
                .cursor_pointer()
                .text_size(px(if mono { 12. } else { 13. }))
                .when(mono, |d| d.font_family(MONO))
                .map(|d| {
                    if row.selected {
                        d.bg(p.surface_raised_nested.hsla())
                            .text_color(p.ink.hsla())
                    } else {
                        d.text_color(p.ink_2.hsla())
                            .hover(|s| s.bg(p.surface_raised.hsla()))
                    }
                })
                .on_click(move |event: &ClickEvent, window, cx| {
                    let mut events = vec![overlay(OverlayInput::SelectRow(i))];
                    if event.click_count() >= 2 {
                        if browsing {
                            events.push(overlay(OverlayInput::Key(OverlayKey::Right)));
                        } else if has_default {
                            events.push(overlay(OverlayInput::Submit));
                        }
                    }
                    super::emit_all(&emit, events, window, cx);
                })
                .when(browsing, |d| {
                    d.child(div().text_color(p.faint.hsla()).child("▸"))
                })
                .child(row.label.clone())
        }))
}
