//! The configuration manager (SPECS §8), drawn from the host's [`ConfigView`].
//!
//! Every setting the TUI's manager edits is here, because the rows are the
//! host's: this file knows three control shapes (a toggle, a choice, a text
//! field) and nothing about any particular setting. Per the config conventions
//! the layering is shown, not hidden: each row says whether its value is set in
//! the scope being edited, inherited from the global file, or the shipped
//! default, and a row that is set here says what clearing it would fall back
//! to (the host's `inherited` list, index-aligned with `rows`).
//!
//! Edits are staged in the host (`ConfigSet`) and only written by `ConfigSave`;
//! whatever the host says about them (`status`, or a refusal the caller shows)
//! is displayed, never second-guessed here: a value the field does not admit is
//! refused by the manager's own `set_selected`.
//!
//! Inline text editing rides on the manager's own editor: clicking a text row
//! selects it and presses Enter (which starts the edit), and while editing the
//! card forwards keystrokes ([`key_to_input`]). The card tracks the
//! [`FocusHandle`] it is given so those keys arrive; the caller focuses it.

use flightdeck::host::{ConfigView, HostEvent, OverlayInput, OverlayKey};
use flightdeck::tui::config_manager::{ConfigRow, ConfigScope, FieldValue, Origin};
use gpui::prelude::FluentBuilder;
use gpui::{
    div, px, App, Div, FocusHandle, FontWeight, InteractiveElement, IntoElement, KeyDownEvent,
    Keystroke, ParentElement, Pixels, SharedString, StatefulInteractiveElement, Styled,
};
use gpui_component::{h_flex, v_flex};

use super::help::{button, card, danger, ButtonKind, MONO};
use super::Emit;
use crate::theme::Palette;

const CARD_WIDTH: Pixels = px(680.);
const BODY_MAX_HEIGHT: Pixels = px(440.);
const SWITCH_WIDTH: f32 = 36.;
const SWITCH_HEIGHT: f32 = 20.;
const KNOB: f32 = 16.;

/// The card. `focus` is tracked by the card so its keystrokes arrive; the
/// dispatcher focuses it when the overlay opens.
pub fn config_view(
    view: &ConfigView,
    emit: Emit,
    focus: &FocusHandle,
    cx: &App,
) -> impl IntoElement {
    let p = *Palette::global(cx);

    let mut rows = v_flex().gap_px();
    for (i, row) in view.rows.iter().enumerate() {
        rows = rows.child(setting_row(view, i, row, &emit, &p));
    }

    let editing = view.editing;
    let key_emit = emit.clone();
    card(
        "config-overlay",
        format!("Configuration · {}", view.project_name),
        CARD_WIDTH,
        &p,
        &emit,
    )
    .track_focus(focus)
    .on_key_down(move |ev: &KeyDownEvent, window, cx| {
        if let Some(input) = key_to_input(editing, &ev.keystroke) {
            cx.stop_propagation();
            key_emit(HostEvent::Overlay(input), window, cx);
        }
    })
    .child(header(view, &emit, &p))
    .child(
        div()
            .id("config-scroll")
            .debug_selector(|| "config-scroll".into())
            .max_h(BODY_MAX_HEIGHT)
            .overflow_y_scroll()
            .child(rows),
    )
    .child(footer(view, &emit, &p))
}

/// The scope switch, the file being edited and the unsaved marker.
fn header(view: &ConfigView, emit: &Emit, p: &Palette) -> Div {
    let scope_button = |scope: ConfigScope, label: SharedString, id: &'static str| {
        let active = view.scope == scope;
        let emit = emit.clone();
        div()
            .id(id)
            .debug_selector(|| id.into())
            .px_3()
            .py_1()
            .rounded_md()
            .text_sm()
            .cursor_pointer()
            .font_weight(if active {
                FontWeight::SEMIBOLD
            } else {
                FontWeight::NORMAL
            })
            .bg(if active {
                p.surface_raised
            } else {
                p.surface_window
            }
            .hsla())
            .text_color(if active { p.ink } else { p.muted }.hsla())
            .on_click(move |_, w, cx| {
                // The host has one "switch scope" gesture (Tab); pressing it
                // when the other scope is showing lands on the one clicked.
                if !active {
                    emit(
                        HostEvent::Overlay(OverlayInput::Key(OverlayKey::Tab)),
                        w,
                        cx,
                    );
                }
            })
            .child(label)
    };

    let path = view
        .path
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "no file (no home directory)".to_string());

    v_flex()
        .gap_2()
        .child(
            h_flex()
                .justify_between()
                .items_center()
                .child(
                    h_flex()
                        .gap_1()
                        .p_0p5()
                        .rounded_lg()
                        .border_1()
                        .border_color(p.border.hsla())
                        .child(scope_button(
                            ConfigScope::Global,
                            "Global".into(),
                            "config-scope-global",
                        ))
                        .child(scope_button(
                            ConfigScope::Project,
                            format!("Project · {}", view.project_name).into(),
                            "config-scope-project",
                        )),
                )
                .child(if view.dirty {
                    div()
                        .text_xs()
                        .text_color(p.status_attention.hsla())
                        .child("● unsaved changes")
                } else {
                    div().text_xs().text_color(p.faint.hsla()).child("saved")
                }),
        )
        .child(
            div()
                .text_xs()
                .font_family(MONO)
                .text_color(p.faint.hsla())
                .child(path),
        )
}

/// One setting: label and key on the left, its control, origin and (for an
/// override) a reset on the right.
fn setting_row(view: &ConfigView, i: usize, row: &ConfigRow, emit: &Emit, p: &Palette) -> Div {
    let selected = view.selected == i;

    let select = {
        let emit = emit.clone();
        move |w: &mut gpui::Window, cx: &mut App| {
            emit(HostEvent::Overlay(OverlayInput::SelectRow(i)), w, cx)
        }
    };

    let label = div()
        .id(SharedString::from(format!("config-label-{i}")))
        .debug_selector(|| format!("config-label-{i}"))
        .flex_1()
        .min_w_0()
        .cursor_pointer()
        .on_click({
            let select = select.clone();
            move |_, w, cx| select(w, cx)
        })
        .child(
            div()
                .text_sm()
                .text_color(p.ink.hsla())
                .child(row.label.clone()),
        )
        .child(
            div()
                .text_xs()
                .font_family(MONO)
                .text_color(p.faint.hsla())
                .child(row.key.clone()),
        );

    let control = control(view, i, row, emit, p);

    let mut right = v_flex().items_end().gap_1().child(
        h_flex()
            .gap_3()
            .items_center()
            .child(origin_tag(row.origin, p))
            .child(control),
    );
    if row.origin == Origin::SetHere {
        let fallback = view
            .inherited
            .get(i)
            .map(|inh| format!("reset → {} ({})", inh.value, inh.origin.label()));
        let emit = emit.clone();
        let key = row.key.clone();
        let scope = view.scope;
        right = right.child(
            h_flex()
                .gap_2()
                .items_center()
                .children(
                    fallback.map(|text| div().text_xs().text_color(p.faint.hsla()).child(text)),
                )
                .child(
                    button(
                        SharedString::from(format!("config-reset-{i}")),
                        "Reset",
                        ButtonKind::Quiet,
                        p,
                        move |_, w, cx| {
                            emit(
                                HostEvent::Overlay(OverlayInput::ConfigSet {
                                    scope,
                                    key: key.clone(),
                                    value: None,
                                }),
                                w,
                                cx,
                            )
                        },
                    )
                    .py_0()
                    .text_xs(),
                ),
        );
    }

    h_flex()
        .gap_4()
        .items_start()
        .px_3()
        .py_2()
        .rounded_md()
        .bg(if selected {
            p.surface_raised
        } else {
            p.surface_window
        }
        .hsla())
        .child(label)
        .child(right)
}

/// The value control for a row's kind.
fn control(view: &ConfigView, i: usize, row: &ConfigRow, emit: &Emit, p: &Palette) -> Div {
    let scope = view.scope;
    let set = {
        let emit = emit.clone();
        let key = row.key.clone();
        move |value: FieldValue, w: &mut gpui::Window, cx: &mut App| {
            emit(
                HostEvent::Overlay(OverlayInput::ConfigSet {
                    scope,
                    key: key.clone(),
                    value: Some(value),
                }),
                w,
                cx,
            )
        }
    };

    if row.is_bool {
        let on = row.bool_value;
        return div().child(
            div()
                .id(SharedString::from(format!("config-toggle-{i}")))
                .debug_selector(|| format!("config-toggle-{i}"))
                .w(px(SWITCH_WIDTH))
                .h(px(SWITCH_HEIGHT))
                .rounded_full()
                .p(px((SWITCH_HEIGHT - KNOB) / 2.))
                .flex()
                .items_center()
                .when(on, |d| d.justify_end())
                .bg(if on {
                    p.accent
                } else {
                    p.surface_raised_nested
                }
                .hsla())
                .cursor_pointer()
                .on_click(move |_, w, cx| set(FieldValue::Bool(!on), w, cx))
                .child(div().size(px(KNOB)).rounded_full().bg(p.ink.hsla())),
        );
    }

    if row.is_text {
        let editing = row.editing;
        let emit = emit.clone();
        let shown = if editing {
            format!("{}▏", row.value)
        } else if row.value.is_empty() {
            "(empty)".to_string()
        } else {
            row.value.clone()
        };
        return div().child(
            div()
                .id(SharedString::from(format!("config-text-{i}")))
                .debug_selector(|| format!("config-text-{i}"))
                .min_w(px(220.))
                .max_w(px(300.))
                .px_2()
                .py_1()
                .rounded_md()
                .border_1()
                .border_color(if editing { p.accent } else { p.border }.hsla())
                .bg(p.surface_input.hsla())
                .font_family(MONO)
                .text_sm()
                .text_color(
                    if row.value.is_empty() && !editing {
                        p.faint
                    } else {
                        p.ink
                    }
                    .hsla(),
                )
                .cursor_text()
                .on_click(move |_, w, cx| {
                    // Select the row, then Enter: the manager's own way into
                    // its inline editor. While already editing, a click in the
                    // field must not press Enter (that would commit).
                    emit(HostEvent::Overlay(OverlayInput::SelectRow(i)), w, cx);
                    if !editing {
                        emit(HostEvent::Overlay(OverlayInput::Submit), w, cx);
                    }
                })
                .child(shown),
        );
    }

    // A choice: one chip per legal value, the current one lit.
    let mut chips = h_flex()
        .gap_px()
        .p_0p5()
        .rounded_md()
        .border_1()
        .border_color(p.border.hsla());
    for (n, choice) in row.choices.iter().enumerate() {
        let active = *choice == row.value;
        let value = choice.clone();
        let set = set.clone();
        chips = chips.child(
            div()
                .id(SharedString::from(format!("config-choice-{i}-{n}")))
                .debug_selector(|| format!("config-choice-{i}-{n}"))
                .px_2()
                .py_0p5()
                .rounded_sm()
                .text_xs()
                .cursor_pointer()
                .bg(if active {
                    p.surface_raised_nested
                } else {
                    p.surface_window
                }
                .hsla())
                .text_color(if active { p.ink } else { p.muted }.hsla())
                .on_click(move |_, w, cx| set(FieldValue::Text(value.clone()), w, cx))
                .child(choice.clone()),
        );
    }
    div().child(chips)
}

/// `set here` in the accent, `from global` muted, `default` faint: colour and
/// wording both, so the layering reads without relying on hue.
fn origin_tag(origin: Origin, p: &Palette) -> Div {
    let colour = match origin {
        Origin::SetHere => p.accent,
        Origin::Global => p.muted,
        Origin::Default => p.faint,
    };
    div()
        .text_xs()
        .text_color(colour.hsla())
        .child(origin.label())
}

/// The last action's outcome, "Edit raw file" and Save.
fn footer(view: &ConfigView, emit: &Emit, p: &Palette) -> Div {
    let status = view.status.clone().map(|text| {
        let refused = text.starts_with("Refused") || text.starts_with("Error");
        div()
            .text_xs()
            .text_color(if refused { danger(p) } else { p.muted.hsla() })
            .child(text)
    });

    let raw = {
        let emit = emit.clone();
        button(
            "config-edit-raw",
            "Edit raw file",
            ButtonKind::Secondary,
            p,
            move |_, w, cx| emit(HostEvent::Overlay(OverlayInput::ConfigEditRaw), w, cx),
        )
    };

    // Saving with nothing staged would only re-write the files; the button is
    // drawn inert instead and does not emit.
    let dirty = view.dirty;
    let save = {
        let emit = emit.clone();
        button(
            "config-save",
            "Save",
            if dirty {
                ButtonKind::Primary
            } else {
                ButtonKind::Quiet
            },
            p,
            move |_, w, cx| {
                if dirty {
                    emit(HostEvent::Overlay(OverlayInput::ConfigSave), w, cx)
                }
            },
        )
    };

    h_flex()
        .justify_between()
        .items_center()
        .gap_3()
        .child(div().flex_1().children(status))
        .child(h_flex().gap_2().child(raw).child(save))
}

/// What a keystroke means to the configuration manager, as the overlay input
/// the TUI's own `handle_config_key` would receive.
///
/// While a text field is being edited every printable key is text and only
/// Enter (commit), Esc (cancel the edit) and Backspace do anything else, as in
/// the TUI. Otherwise the keys are the manager's: up/down, tab (scope), enter
/// or space (toggle), and the letters `c` (clear), `s` (save), `e` (edit raw)
/// passed through for the host to interpret. Chords with ctrl/alt/cmd are not
/// the manager's.
pub fn key_to_input(editing: bool, keystroke: &Keystroke) -> Option<OverlayInput> {
    let m = &keystroke.modifiers;
    if m.control || m.alt || m.platform || m.function {
        return None;
    }
    let typed = || {
        // A platform key-down carries the character it types; a bare
        // `Keystroke::parse` does not, so a one-character key name stands in.
        let text = keystroke
            .key_char
            .as_deref()
            .or_else(|| (keystroke.key.chars().count() == 1).then_some(keystroke.key.as_str()))?;
        let mut chars = text.chars();
        let c = chars.next()?;
        (chars.next().is_none() && !c.is_control()).then_some(c)
    };
    match keystroke.key.as_str() {
        "enter" => Some(OverlayInput::Submit),
        "escape" => Some(OverlayInput::Cancel),
        "backspace" if editing => Some(OverlayInput::Key(OverlayKey::Backspace)),
        "up" if !editing => Some(OverlayInput::Key(OverlayKey::Up)),
        "down" if !editing => Some(OverlayInput::Key(OverlayKey::Down)),
        "tab" if !editing => Some(OverlayInput::Key(OverlayKey::Tab)),
        _ if editing => typed().map(|c| OverlayInput::Key(OverlayKey::Char(c))),
        "space" => Some(OverlayInput::Key(OverlayKey::Char(' '))),
        "c" | "s" | "e" => typed().map(|c| OverlayInput::Key(OverlayKey::Char(c))),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use flightdeck::tui::config_manager::ConfigManager;
    use gpui::{Keystroke, TestAppContext, VisualTestContext};

    use super::super::help::testkit::{click, mount, recorder, scroll_down, take, Events};
    use super::*;

    /// The real manager, read out the way `overlay_bridge::config_view` does,
    /// so the fixture has every row the TUI edits.
    fn view_of(cm: &ConfigManager) -> ConfigView {
        ConfigView {
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

    fn manager() -> ConfigManager {
        ConfigManager::new(
            "demo",
            Some(PathBuf::from("/home/u/.flightdeck/config.toml")),
            "/repo/.flightdeck/config.toml",
            toml::Table::new(),
            toml::Table::new(),
            vec!["claude".into(), "codex".into()],
        )
    }

    fn first(view: &ConfigView, pick: impl Fn(&ConfigRow) -> bool) -> (usize, ConfigRow) {
        view.rows
            .iter()
            .enumerate()
            .find(|(_, r)| pick(r))
            .map(|(i, r)| (i, r.clone()))
            .expect("the manager has a row of this kind")
    }

    fn draw(cx: &mut TestAppContext, view: ConfigView) -> (&mut VisualTestContext, Events) {
        let (emit, events) = recorder();
        let focus = cx.update(|cx| cx.focus_handle());
        let cx = mount(cx, move |_, cx| {
            config_view(&view, emit.clone(), &focus, cx).into_any_element()
        });
        (cx, events)
    }

    fn leak(s: String) -> &'static str {
        Box::leak(s.into_boxed_str())
    }

    /// Every row the manager has is a toggle, a choice or a text field, so the
    /// three controls cover every setting the TUI edits, and each is drawn.
    #[gpui::test]
    fn every_setting_has_a_control(cx: &mut TestAppContext) {
        let view = view_of(&manager());
        assert!(view.rows.len() > 5, "a real manager has many rows");
        for row in &view.rows {
            assert!(
                row.is_bool || row.is_text || !row.choices.is_empty(),
                "`{}` fits no control",
                row.key
            );
        }
        let rows = view.rows.clone();
        let (cx, _) = draw(cx, view);
        for (i, row) in rows.iter().enumerate() {
            assert!(
                cx.debug_bounds(leak(format!("config-label-{i}"))).is_some(),
                "row {} not drawn",
                row.key
            );
        }
    }

    #[gpui::test]
    fn a_toggle_sets_the_opposite_bool_in_the_shown_scope(cx: &mut TestAppContext) {
        let view = view_of(&manager());
        let (i, row) = first(&view, |r| r.is_bool);
        let (cx, events) = draw(cx, view);
        click(cx, leak(format!("config-toggle-{i}")));
        assert_eq!(
            take(&events),
            vec![HostEvent::Overlay(OverlayInput::ConfigSet {
                scope: ConfigScope::Project,
                key: row.key,
                value: Some(FieldValue::Bool(!row.bool_value)),
            })]
        );
    }

    #[gpui::test]
    fn a_choice_chip_sets_that_choice(cx: &mut TestAppContext) {
        let view = view_of(&manager());
        let (i, row) = first(&view, |r| !r.choices.is_empty());
        let last = row.choices.len() - 1;
        let (cx, events) = draw(cx, view);
        click(cx, leak(format!("config-choice-{i}-{last}")));
        assert_eq!(
            take(&events),
            vec![HostEvent::Overlay(OverlayInput::ConfigSet {
                scope: ConfigScope::Project,
                key: row.key,
                value: Some(FieldValue::Text(row.choices[last].clone())),
            })]
        );
    }

    /// A text row is edited by the manager's own editor: select it, press
    /// Enter. Clicking again while editing must not press Enter (a commit).
    #[gpui::test]
    fn a_text_field_opens_the_managers_inline_editor(cx: &mut TestAppContext) {
        let mut cm = manager();
        let view = view_of(&cm);
        let (i, text_row) = first(&view, |r| r.is_text);
        let (c, events) = draw(cx, view);
        scroll_down(c, "config-scroll");
        click(c, leak(format!("config-text-{i}")));
        assert_eq!(
            take(&events),
            vec![
                HostEvent::Overlay(OverlayInput::SelectRow(i)),
                HostEvent::Overlay(OverlayInput::Submit)
            ]
        );

        cm.select_key(&text_row.key);
        cm.toggle_selected();
        let editing = view_of(&cm);
        assert!(editing.editing);
        let (c, events) = draw(cx, editing);
        scroll_down(c, "config-scroll");
        click(c, leak(format!("config-text-{i}")));
        assert_eq!(
            take(&events),
            vec![HostEvent::Overlay(OverlayInput::SelectRow(i))]
        );
    }

    /// Layering: only a row set in this scope offers Reset, and it says what
    /// clearing falls back to.
    #[gpui::test]
    fn reset_is_offered_only_for_an_override_and_clears_it(cx: &mut TestAppContext) {
        let mut cm = manager();
        let (i, row) = first(&view_of(&cm), |r| r.is_bool);
        cm.select_key(&row.key);
        cm.set_selected(FieldValue::Bool(!row.bool_value)).unwrap();
        let view = view_of(&cm);
        assert_eq!(view.rows[i].origin, Origin::SetHere);
        assert_eq!(view.inherited[i].value, row.value, "clearing restores it");
        let untouched = (0..view.rows.len())
            .find(|n| view.rows[*n].origin != Origin::SetHere)
            .unwrap();

        let (cx, events) = draw(cx, view);
        assert!(cx
            .debug_bounds(leak(format!("config-reset-{untouched}")))
            .is_none());
        click(cx, leak(format!("config-reset-{i}")));
        assert_eq!(
            take(&events),
            vec![HostEvent::Overlay(OverlayInput::ConfigSet {
                scope: ConfigScope::Project,
                key: row.key,
                value: None,
            })]
        );
    }

    #[gpui::test]
    fn the_scope_switch_only_moves_to_the_other_scope(cx: &mut TestAppContext) {
        let view = view_of(&manager());
        assert_eq!(view.scope, ConfigScope::Project);
        let (cx, events) = draw(cx, view);
        click(cx, "config-scope-project");
        assert!(take(&events).is_empty(), "already showing Project");
        click(cx, "config-scope-global");
        assert_eq!(
            take(&events),
            vec![HostEvent::Overlay(OverlayInput::Key(OverlayKey::Tab))]
        );
    }

    #[gpui::test]
    fn save_needs_unsaved_edits_and_raw_edit_is_always_there(cx: &mut TestAppContext) {
        let mut cm = manager();
        let (cx1, events) = draw(cx, view_of(&cm));
        click(cx1, "config-save");
        click(cx1, "config-edit-raw");
        assert_eq!(
            take(&events),
            vec![HostEvent::Overlay(OverlayInput::ConfigEditRaw)],
            "a clean manager has nothing to save"
        );

        let (_, row) = first(&view_of(&cm), |r| r.is_bool);
        cm.select_key(&row.key);
        cm.set_selected(FieldValue::Bool(!row.bool_value)).unwrap();
        assert!(view_of(&cm).dirty);
        let (cx2, events) = draw(cx, view_of(&cm));
        click(cx2, "config-save");
        assert_eq!(
            take(&events),
            vec![HostEvent::Overlay(OverlayInput::ConfigSave)]
        );
    }

    /// The card tracks the focus handle it is given, and a keystroke into it is
    /// forwarded to the host as the TUI's key.
    #[gpui::test]
    fn the_focused_card_forwards_keystrokes(cx: &mut TestAppContext) {
        let view = view_of(&manager());
        let (emit, events) = recorder();
        let focus = cx.update(|cx| cx.focus_handle());
        let handle = focus.clone();
        let cx = mount(cx, move |_, cx| {
            config_view(&view, emit.clone(), &handle, cx).into_any_element()
        });
        cx.update(|window, cx| window.focus(&focus, cx));
        cx.simulate_keystrokes("down");
        assert_eq!(
            take(&events),
            vec![HostEvent::Overlay(OverlayInput::Key(OverlayKey::Down))]
        );
    }

    #[test]
    fn keystrokes_map_to_the_tuis_config_keys() {
        let key = |s: &str| Keystroke::parse(s).unwrap();
        let ki = |k: OverlayKey| Some(OverlayInput::Key(k));
        // Browsing.
        assert_eq!(key_to_input(false, &key("down")), ki(OverlayKey::Down));
        assert_eq!(key_to_input(false, &key("up")), ki(OverlayKey::Up));
        assert_eq!(key_to_input(false, &key("tab")), ki(OverlayKey::Tab));
        assert_eq!(
            key_to_input(false, &key("enter")),
            Some(OverlayInput::Submit)
        );
        assert_eq!(
            key_to_input(false, &key("escape")),
            Some(OverlayInput::Cancel)
        );
        assert_eq!(key_to_input(false, &key("s")), ki(OverlayKey::Char('s')));
        assert_eq!(key_to_input(false, &key("x")), None);
        assert_eq!(key_to_input(false, &key("ctrl-s")), None);
        // Editing: printable keys are text, including the ones that mean
        // something when browsing.
        assert_eq!(key_to_input(true, &key("s")), ki(OverlayKey::Char('s')));
        assert_eq!(key_to_input(true, &key("x")), ki(OverlayKey::Char('x')));
        assert_eq!(
            key_to_input(true, &key("backspace")),
            ki(OverlayKey::Backspace)
        );
        assert_eq!(key_to_input(true, &key("down")), None);
        assert_eq!(
            key_to_input(true, &key("enter")),
            Some(OverlayInput::Submit)
        );
    }
}
