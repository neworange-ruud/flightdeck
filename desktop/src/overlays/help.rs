//! The help overlay, drawn from the [`HelpDoc`] the host builds, plus the small
//! pieces of overlay chrome (card, keycap, button) the sibling modules share.
//!
//! The doc is `flightdeck::tui::help::help_doc`, which is itself generated from
//! the keymap table (`Keymap::help_sections`), so this screen lists exactly the
//! chords that are bound, with the leave-focus key this platform and config
//! actually have. Nothing here restates a shortcut. The isolated-run note
//! (SPECS §32) is a [`HelpNote`] the host puts first in the doc when the run is
//! isolated; it is drawn above the shortcuts, before any of them.

use flightdeck::host::{HostEvent, OverlayInput};
use flightdeck::tui::help::{HelpDoc, HelpNote, HelpRow, HelpSection};
use gpui::{
    div, px, App, ClickEvent, Div, ElementId, FontWeight, InteractiveElement, IntoElement,
    ParentElement, Pixels, SharedString, Stateful, StatefulInteractiveElement, Styled, Window,
};
use gpui_component::{h_flex, v_flex};

use super::Emit;
use crate::theme::Palette;

/// The mono family for keycaps and meta text, as the terminal element picks it
/// per OS (bundled Geist Mono replaces these later).
#[cfg(target_os = "macos")]
pub(crate) const MONO: &str = "Menlo";
#[cfg(target_os = "windows")]
pub(crate) const MONO: &str = "Consolas";
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub(crate) const MONO: &str = "DejaVu Sans Mono";

/// Width of the shortcut-key column, wide enough for `Ctrl+Shift+Left` style
/// chords without wrapping.
const KEY_COLUMN: Pixels = px(190.);
/// The tallest the scrolling body grows before it scrolls; the card is a modal
/// on a ~900px window, so the header and footer stay on screen.
const BODY_MAX_HEIGHT: Pixels = px(560.);

/// What a button means, which sets its fill.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ButtonKind {
    /// The one main action of a card: light fill on the dark surface (the
    /// brief's "Push" button).
    Primary,
    /// An ordinary action: raised fill with a border.
    Secondary,
    /// A low-emphasis action: no fill until hovered.
    Quiet,
}

/// A clickable button. `on_click` runs on a completed primary click.
pub(crate) fn button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    kind: ButtonKind,
    p: &Palette,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let id = id.into();
    let (fill, ink, border) = match kind {
        ButtonKind::Primary => (p.ink, p.surface_window, p.ink),
        ButtonKind::Secondary => (p.surface_raised, p.ink, p.border),
        ButtonKind::Quiet => (p.surface_window, p.muted, p.surface_window),
    };
    div()
        .id(id.clone())
        .debug_selector(|| id.to_string())
        .px_3()
        .py_1()
        .rounded_md()
        .border_1()
        .border_color(border.hsla())
        .bg(fill.hsla())
        .text_color(ink.hsla())
        .text_sm()
        .font_weight(FontWeight::MEDIUM)
        .cursor_pointer()
        .hover(|s| s.border_color(p.border_strong.hsla()))
        .on_click(on_click)
        .child(label.into())
}

/// The colour for a refusal or an error: the palette's danger role, the one
/// a destructive dialog button is drawn in.
pub(crate) fn danger(p: &Palette) -> gpui::Hsla {
    p.danger.hsla()
}

/// A key or chord drawn as a keycap: mono, on a raised chip.
pub(crate) fn keycap(text: impl Into<SharedString>, p: &Palette) -> Div {
    div()
        .px_1p5()
        .rounded_sm()
        .border_1()
        .border_color(p.border.hsla())
        .bg(p.surface_raised.hsla())
        .text_color(p.ink_2.hsla())
        .text_xs()
        .font_family(MONO)
        .child(text.into())
}

/// An upper-case, muted group heading.
pub(crate) fn section_label(text: impl Into<SharedString>, p: &Palette) -> Div {
    div()
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(p.muted.hsla())
        .child(text.into().to_uppercase())
}

/// The modal card every overlay sits in: a title row with a close button
/// (which answers [`OverlayInput::Cancel`], the same as Esc), then the body the
/// caller appends. It does not position itself or draw a scrim; the overlay
/// dispatcher decides where a card goes.
pub(crate) fn card(
    id: impl Into<ElementId>,
    title: impl Into<SharedString>,
    width: Pixels,
    p: &Palette,
    emit: &Emit,
) -> Stateful<Div> {
    let close = {
        let emit = emit.clone();
        button(
            "overlay-close",
            "✕",
            ButtonKind::Quiet,
            p,
            move |_, w, cx| emit(HostEvent::Overlay(OverlayInput::Cancel), w, cx),
        )
        .px_2()
    };
    v_flex()
        .id(id)
        .w(width)
        .max_w_full()
        .gap_3()
        .p_4()
        .rounded_lg()
        .border_1()
        .border_color(p.border_strong.hsla())
        .bg(p.surface_window.hsla())
        .text_color(p.ink.hsla())
        .child(
            h_flex()
                .justify_between()
                .items_center()
                .child(
                    div()
                        .text_base()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(title.into()),
                )
                .child(close),
        )
}

/// The help screen. `doc` is `AppHost::overlay()`'s `Help(doc)`.
pub fn help_view(doc: &HelpDoc, emit: Emit, cx: &App) -> impl IntoElement {
    let p = *Palette::global(cx);
    let mut body = v_flex().gap_4().pb_1();
    for note in &doc.notes {
        body = body.child(note_block(note, &p));
    }
    for (si, section) in doc.sections.iter().enumerate() {
        body = body.child(section_block(si, section, &p));
    }
    card("help-overlay", doc.title.clone(), px(620.), &p, &emit).child(
        div()
            .id("help-scroll")
            .max_h(BODY_MAX_HEIGHT)
            .overflow_y_scroll()
            .child(body),
    )
}

/// A note that must be read before the shortcuts (the isolated-run note): in
/// the attention colour on its tinted fill, so it is not mistaken for a row.
fn note_block(note: &HelpNote, p: &Palette) -> Div {
    let mut lines = v_flex().gap_0p5();
    for line in &note.lines {
        lines = lines.child(
            div()
                .text_sm()
                .text_color(p.ink_2.hsla())
                .child(line.clone()),
        );
    }
    v_flex()
        .gap_1()
        .p_3()
        .rounded_md()
        .bg(p.status_attention_bg.hsla())
        .border_1()
        .border_color(p.status_attention.hsla())
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(p.status_attention.hsla())
                .child(note.title.clone()),
        )
        .child(lines)
}

fn section_block(si: usize, section: &HelpSection, p: &Palette) -> Div {
    let mut rows = v_flex().gap_1p5();
    for (ri, row) in section.rows.iter().enumerate() {
        rows = rows.child(help_row(si, ri, row, p));
    }
    v_flex()
        .gap_2()
        .child(section_label(section.title.clone(), p))
        .child(rows)
}

fn help_row(si: usize, ri: usize, row: &HelpRow, p: &Palette) -> Stateful<Div> {
    h_flex()
        .id(SharedString::from(format!("help-row-{si}-{ri}")))
        .debug_selector(|| format!("help-row-{si}-{ri}"))
        .gap_3()
        .items_start()
        .child(
            div()
                .w(KEY_COLUMN)
                .flex_shrink_0()
                // An h_flex parent so the chip hugs its text instead of
                // stretching to the whole column.
                .child(h_flex().child(keycap(row.keys.clone(), p).flex_shrink_0())),
        )
        .child(
            div()
                .flex_1()
                .text_sm()
                .text_color(p.ink_2.hsla())
                .child(row.description.clone()),
        )
}

/// Shared harness for the overlay views' tests: mount a view in a headless
/// window, click elements by their debug selector, and read back what the view
/// emitted.
#[cfg(test)]
pub(crate) mod testkit {
    use std::cell::RefCell;
    use std::rc::Rc;

    use flightdeck::host::HostEvent;
    use gpui::{
        point, px, size, AnyElement, App, Context, IntoElement, Modifiers, Render, ScrollDelta,
        ScrollWheelEvent, TestAppContext, TouchPhase, VisualTestContext, Window,
    };

    use super::super::Emit;
    use crate::theme::Palette;

    /// Builds the element under test each frame.
    type Build = Box<dyn Fn(&mut Window, &App) -> AnyElement>;

    /// A window whose whole content is one view function's output.
    pub struct Harness {
        build: Build,
    }

    impl Render for Harness {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            (self.build)(window, cx)
        }
    }

    /// The events a view emitted, in order.
    pub type Events = Rc<RefCell<Vec<HostEvent>>>;

    /// An [`Emit`] that records into a fresh [`Events`].
    pub fn recorder() -> (Emit, Events) {
        let events: Events = Rc::default();
        let sink = events.clone();
        let emit: Emit = Rc::new(move |event, _, _| sink.borrow_mut().push(event));
        (emit, events)
    }

    /// Mount `build`'s element in a tall window (so nothing is scrolled out of
    /// layout) and draw it.
    pub fn mount(
        cx: &mut TestAppContext,
        build: impl Fn(&mut Window, &App) -> AnyElement + 'static,
    ) -> &mut VisualTestContext {
        cx.update(|cx| cx.set_global(Palette::dark()));
        let (_, vcx) = cx.add_window_view(|_, _| Harness {
            build: Box::new(build),
        });
        vcx.simulate_resize(size(px(1280.), px(2400.)));
        vcx.run_until_parked();
        vcx.update(|window, _| window.refresh());
        vcx.run_until_parked();
        vcx
    }

    /// Click the centre of the element registered under `selector`.
    pub fn click(cx: &mut VisualTestContext, selector: &'static str) {
        let bounds = cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("no element `{selector}` was drawn"));
        cx.simulate_click(
            point(
                bounds.origin.x + bounds.size.width / 2.,
                bounds.origin.y + bounds.size.height / 2.,
            ),
            Modifiers::none(),
        );
    }

    /// Wheel the scroll area registered under `selector` all the way down.
    pub fn scroll_down(cx: &mut VisualTestContext, selector: &'static str) {
        let bounds = cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("no element `{selector}` was drawn"));
        cx.simulate_event(ScrollWheelEvent {
            position: bounds.center(),
            delta: ScrollDelta::Pixels(point(px(0.), px(-10_000.))),
            modifiers: Modifiers::none(),
            touch_phase: TouchPhase::Moved,
        });
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
    }

    /// Drain and return what was emitted so far.
    pub fn take(events: &Events) -> Vec<HostEvent> {
        std::mem::take(&mut *events.borrow_mut())
    }
}

#[cfg(test)]
mod tests {
    use flightdeck::app::keymap::Keymap;
    use flightdeck::tui::help::help_doc;
    use gpui::{IntoElement as _, TestAppContext};

    use super::testkit::{click, mount, recorder, take};
    use super::*;

    /// Every row of the doc gets a row on screen, in order, and the doc itself
    /// lists exactly the chords the keymap table binds: the screen cannot show a
    /// key the table does not have, nor miss one it does.
    #[gpui::test]
    fn every_help_row_is_drawn_and_the_doc_is_the_keymap(cx: &mut TestAppContext) {
        let doc = help_doc(false, false);
        let table: Vec<(String, String)> = Keymap::for_this_platform(false)
            .help_sections()
            .iter()
            .flat_map(|s| {
                s.rows
                    .iter()
                    .map(|r| (r.keys.clone(), r.description.to_string()))
            })
            .collect();
        let shown: Vec<(String, String)> = doc
            .sections
            .iter()
            .flat_map(|s| {
                s.rows
                    .iter()
                    .map(|r| (r.keys.clone(), r.description.clone()))
            })
            .collect();
        assert_eq!(shown, table, "the doc is the keymap table's help rows");

        let (emit, _) = recorder();
        let drawn = doc.clone();
        let cx = mount(cx, move |_, cx| {
            help_view(&drawn, emit.clone(), cx).into_any_element()
        });
        for (si, section) in doc.sections.iter().enumerate() {
            for ri in 0..section.rows.len() {
                let id: &'static str = Box::leak(format!("help-row-{si}-{ri}").into_boxed_str());
                assert!(cx.debug_bounds(id).is_some(), "row {id} was not drawn");
            }
        }
    }

    /// The isolated-run note is a note above the sections and is drawn only
    /// when the host put it in the doc.
    #[test]
    fn the_isolated_note_comes_from_the_doc() {
        assert!(help_doc(false, false).notes.is_empty());
        let isolated = help_doc(false, true);
        assert_eq!(isolated.notes[0].title, "Isolated run (--isolated)");
    }

    #[gpui::test]
    fn the_close_button_answers_cancel(cx: &mut TestAppContext) {
        let doc = help_doc(false, true);
        let (emit, events) = recorder();
        let cx = mount(cx, move |_, cx| {
            help_view(&doc, emit.clone(), cx).into_any_element()
        });
        click(cx, "overlay-close");
        assert_eq!(
            take(&events),
            vec![HostEvent::Overlay(OverlayInput::Cancel)]
        );
    }
}
