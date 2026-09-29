//! A generic modal for whatever overlay the host has on screen.
//!
//! Stop-gap until the M3 dialog set: every overlay the TUI can show
//! ([`OverlayView`]) is drawn as one centred card — title, body lines, the text
//! field, the choice list, the buttons — so no flow started from the GUI is
//! dead-ended. Answers go back through the overlay API: a button is
//! `OverlayInput::Choose(id)`, a list row `SelectRow`, a palette row
//! `PaletteRun`, Esc `Cancel`, Enter `Submit`, and any other key a plain
//! [`OverlayKey`] — the same handlers the TUI's keyboard reaches, so typing
//! into a prompt, Tab in the New Agent form or the folder browser's arrows all
//! behave as they do there.

use flightdeck::host::{
    ButtonRole, DialogButton, HostEvent, OverlayInput, OverlayKey, OverlayView,
};
use gpui::prelude::FluentBuilder;
use gpui::{
    div, px, AnyElement, App, Entity, FocusHandle, FontWeight, InteractiveElement, IntoElement,
    KeyDownEvent, ParentElement, StatefulInteractiveElement, Styled,
};
use gpui_component::{h_flex, v_flex};

use std::rc::Rc;

use flightdeck_desktop::overlays::about::about_view;
use flightdeck_desktop::overlays::config::config_view;
use flightdeck_desktop::overlays::help::help_view;
use flightdeck_desktop::overlays::remote::{pairing_view, web_access_view};
use flightdeck_desktop::overlays::Emit;

use crate::fonts::MONO_FAMILY;
use crate::host::HostModel;
use crate::theme::{Hex, Palette};

/// The card's text, reduced from any overlay.
struct Card {
    title: String,
    lines: Vec<String>,
    input: Option<String>,
    /// `(label, selected, input)` per row.
    rows: Vec<(String, bool, OverlayInput)>,
    /// Each button and what pressing it sends.
    buttons: Vec<(DialogButton, OverlayInput)>,
}

fn card(overlay: &OverlayView) -> Card {
    // Overlays without buttons of their own (palette, help, …) close on Esc,
    // which is what this button sends.
    let close = || {
        (
            DialogButton {
                id: "Esc".into(),
                label: "Close".into(),
                role: ButtonRole::Cancel,
                default: false,
            },
            OverlayInput::Cancel,
        )
    };
    match overlay {
        OverlayView::Message(m) => Card {
            title: "FlightDeck".into(),
            lines: m.text.lines().map(str::to_string).collect(),
            input: None,
            rows: Vec::new(),
            buttons: vec![(
                DialogButton {
                    id: "Enter".into(),
                    label: "OK".into(),
                    role: ButtonRole::Primary,
                    default: true,
                },
                OverlayInput::Submit,
            )],
        },
        OverlayView::Dialog(d) => Card {
            title: d.title.clone(),
            lines: d
                .origin_label
                .iter()
                .cloned()
                .chain(d.body.iter().cloned())
                .collect(),
            input: d.input.clone(),
            rows: d
                .list
                .iter()
                .enumerate()
                .map(|(i, r)| (r.label.clone(), r.selected, OverlayInput::SelectRow(i)))
                .collect(),
            buttons: d
                .buttons
                .iter()
                .map(|b| (b.clone(), OverlayInput::Choose(b.id.clone())))
                .collect(),
        },
        OverlayView::Palette(pal) => Card {
            title: "Command palette".into(),
            lines: Vec::new(),
            input: Some(pal.filter.clone()),
            rows: pal
                .entries
                .iter()
                .enumerate()
                .map(|(i, row)| {
                    let label = match &row.keycap {
                        Some(k) => format!("{} › {}    {k}", row.section, row.label),
                        None => format!("{} › {}", row.section, row.label),
                    };
                    (
                        label,
                        i == pal.selected,
                        OverlayInput::PaletteRun(row.action.clone()),
                    )
                })
                .collect(),
            buttons: vec![close()],
        },
        OverlayView::GitStatus(g) => {
            let s = &g.status;
            let mut lines = vec![
                format!("branch   {}", s.branch),
                format!("target   {}", s.base_branch),
                format!(
                    "changes  {} files (+{} −{})",
                    s.changes.total(),
                    s.lines.added,
                    s.lines.removed
                ),
                format!(
                    "upstream {}",
                    s.upstream.clone().unwrap_or_else(|| "none".into())
                ),
                format!("ahead {} · behind {}", s.ahead, s.behind),
            ];
            lines.extend(g.pr_url.iter().map(|u| format!("PR       {u}")));
            Card {
                title: "Git status".into(),
                lines,
                input: None,
                rows: Vec::new(),
                buttons: vec![close()],
            }
        }
        // Drawn by their own views (see [`modal`]); a bare title if one ever
        // reaches here.
        OverlayView::Help(_)
        | OverlayView::About(_)
        | OverlayView::Config(_)
        | OverlayView::WebAccess(_)
        | OverlayView::Pairing(_) => Card {
            title: "FlightDeck".into(),
            lines: Vec::new(),
            input: None,
            rows: Vec::new(),
            buttons: vec![close()],
        },
    }
}

/// Translate a key pressed with the modal focused into an overlay input.
/// `None` for keys with no overlay meaning (a bare modifier, a Cmd chord).
pub fn overlay_input_for_key(event: &KeyDownEvent) -> Option<OverlayInput> {
    let ks = &event.keystroke;
    if ks.modifiers.platform || ks.modifiers.control {
        return None;
    }
    let key = match ks.key.as_str() {
        "escape" => return Some(OverlayInput::Cancel),
        "enter" => return Some(OverlayInput::Submit),
        "tab" => OverlayKey::Tab,
        "backspace" => OverlayKey::Backspace,
        "up" => OverlayKey::Up,
        "down" => OverlayKey::Down,
        "left" => OverlayKey::Left,
        "right" => OverlayKey::Right,
        _ => {
            let text = ks.key_char.as_deref()?;
            let mut chars = text.chars();
            let c = chars.next()?;
            if chars.next().is_some() || c.is_control() {
                return None;
            }
            OverlayKey::Char(c)
        }
    };
    Some(OverlayInput::Key(key))
}

fn send(host: &Entity<HostModel>, input: OverlayInput, cx: &mut App) {
    host.update(cx, |model, cx| {
        model.dispatch(HostEvent::Overlay(input), cx)
    });
}

/// The modal layer: a scrim over the window and the overlay's card on it.
/// `focus` is the handle the shell keeps focused while an overlay is open.
///
/// Help, About, the configuration manager, web access and phone pairing are
/// drawn by their own views (`flightdeck_desktop::overlays`), answering
/// through the same host model; everything else (messages, prompts, the
/// palette, git status) by the generic card below. The configuration manager
/// tracks `focus` and reads its own keys; for the rest the scrim does.
pub fn modal(
    overlay: &OverlayView,
    host: &Entity<HostModel>,
    focus: &FocusHandle,
    p: &Palette,
    cx: &App,
) -> impl IntoElement {
    let emit: Emit = {
        let host = host.clone();
        Rc::new(move |event, _, cx| host.update(cx, |model, cx| model.dispatch(event, cx)))
    };
    let (content, owns_keys): (AnyElement, bool) = match overlay {
        OverlayView::Help(doc) => (help_view(doc, emit, cx).into_any_element(), false),
        OverlayView::About(doc) => (about_view(doc, emit, cx).into_any_element(), false),
        OverlayView::Config(view) => (config_view(view, emit, focus, cx).into_any_element(), true),
        OverlayView::WebAccess(o) => (web_access_view(o, emit, cx).into_any_element(), false),
        OverlayView::Pairing(v) => (pairing_view(v, emit, cx).into_any_element(), false),
        OverlayView::Message(_)
        | OverlayView::Dialog(_)
        | OverlayView::Palette(_)
        | OverlayView::GitStatus(_) => (generic_card(overlay, host, p), false),
    };

    let key_host = host.clone();
    let paste_host = host.clone();
    let scrim = div()
        .id("modal-scrim")
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .bg(p.scrim.hsla().opacity(0.7));
    let scrim = if owns_keys {
        scrim
    } else {
        scrim
            .track_focus(focus)
            .key_context("Overlay")
            .on_key_down(move |event, _, cx| {
                let ks = &event.keystroke;
                // Paste into a text field: the platform's paste chord.
                let paste = ks.key == "v"
                    && if flightdeck::tui::platform::IS_MACOS {
                        ks.modifiers.platform
                    } else {
                        ks.modifiers.control
                    };
                if paste {
                    if let Some(text) = cx.read_from_clipboard().and_then(|i| i.text()) {
                        paste_host
                            .update(cx, |model, cx| model.dispatch(HostEvent::Paste(text), cx));
                    }
                    cx.stop_propagation();
                    return;
                }
                if let Some(input) = overlay_input_for_key(event) {
                    send(&key_host, input, cx);
                    cx.stop_propagation();
                }
            })
    };
    scrim.child(content)
}

/// The generic card: title, body lines, text field, choice rows, buttons.
fn generic_card(overlay: &OverlayView, host: &Entity<HostModel>, p: &Palette) -> AnyElement {
    let card = card(overlay);

    let rows = v_flex()
        .id("modal-rows")
        .max_h(px(320.))
        .overflow_y_scroll()
        .gap(px(1.))
        .children(
            card.rows
                .into_iter()
                .enumerate()
                .map(|(i, (label, selected, input))| {
                    let host = host.clone();
                    div()
                        .id(("modal-row", i))
                        .debug_selector(move || format!("modal-row-{i}"))
                        .px_2()
                        .py_1()
                        .rounded(px(5.))
                        .font_family(MONO_FAMILY)
                        .text_size(px(12.))
                        .cursor_pointer()
                        .when(selected, |r| {
                            r.bg(p.surface_raised_nested.hsla())
                                .text_color(p.ink.hsla())
                        })
                        .when(!selected, |r| r.text_color(p.ink_2.hsla()))
                        .hover(|s| s.bg(p.surface_raised_nested.hsla()))
                        .child(label)
                        .on_click(move |_, _, cx| send(&host, input.clone(), cx))
                }),
        );

    let buttons = h_flex().justify_end().gap_2().children(
        card.buttons
            .into_iter()
            .map(|(b, input)| button(b, input, host, p)),
    );

    v_flex()
        .id("modal-card")
        .debug_selector(|| "modal-card".into())
        .w(px(520.))
        .max_h(px(560.))
        .p_4()
        .gap_3()
        .rounded(px(10.))
        .border_1()
        .border_color(p.border_strong.hsla())
        .bg(p.surface_raised.hsla())
        .text_color(p.ink.hsla())
        // Clicks on the card stay on it.
        .on_click(|_, _, cx| cx.stop_propagation())
        .child(
            div()
                .text_size(px(14.))
                .font_weight(FontWeight::SEMIBOLD)
                .child(card.title),
        )
        .when(!card.lines.is_empty(), |c| {
            c.child(
                v_flex()
                    .id("modal-body")
                    .max_h(px(300.))
                    .overflow_y_scroll()
                    .gap_1()
                    .text_size(px(12.5))
                    .text_color(p.ink_2.hsla())
                    .children(card.lines.into_iter().map(|l| {
                        div().whitespace_nowrap().child(if l.is_empty() {
                            " ".to_string()
                        } else {
                            l
                        })
                    })),
            )
        })
        .when_some(card.input, |c, text| {
            c.child(
                div()
                    .px_2()
                    .py_1()
                    .rounded(px(6.))
                    .border_1()
                    .border_color(p.border_strong.hsla())
                    .bg(p.surface_input.hsla())
                    .font_family(MONO_FAMILY)
                    .text_size(px(12.5))
                    .child(format!("{text}▏")),
            )
        })
        .child(rows)
        .child(buttons)
        .into_any_element()
}

fn button(
    b: DialogButton,
    input: OverlayInput,
    host: &Entity<HostModel>,
    p: &Palette,
) -> AnyElement {
    let (bg, ink): (Option<Hex>, Hex) = match b.role {
        ButtonRole::Primary => (Some(p.button_primary_bg), p.button_primary_ink),
        ButtonRole::Destructive => (None, p.status_error),
        ButtonRole::Secondary => (None, p.ink_2),
        ButtonRole::Cancel => (None, p.muted),
    };
    let host = host.clone();
    div()
        .id(gpui::ElementId::Name(
            format!("modal-button-{}", b.id).into(),
        ))
        .debug_selector({
            let id = b.id.clone();
            move || format!("modal-button-{id}")
        })
        .h(px(28.))
        .px_3()
        .flex()
        .items_center()
        .rounded(px(6.))
        .text_size(px(12.))
        .text_color(ink.hsla())
        .when_some(bg, |d, bg| {
            d.bg(bg.hsla()).font_weight(FontWeight::SEMIBOLD)
        })
        .when(bg.is_none(), |d| d.border_1().border_color(p.border.hsla()))
        .when(b.default, |d| d.border_1().border_color(p.accent.hsla()))
        .cursor_pointer()
        .child(b.label)
        .on_click(move |_, _, cx| send(&host, input.clone(), cx))
        .into_any_element()
}
