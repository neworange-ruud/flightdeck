//! The command palette (`Ctrl-g`; beads `remote-control-bmej.4.1`), design D
//! "Quiet rail + palette": a filter field, a context line naming where the
//! commands act, the rows grouped under their section headings with each
//! row's keycap, and a hint bar.
//!
//! The host filters and selects (the TUI's `CommandPalette`); this draws the
//! [`PaletteView`] it reports and answers with [`OverlayInput::PaletteFilter`],
//! [`OverlayInput::SelectRow`] and [`OverlayInput::PaletteRun`].

use flightdeck::host::{HostEvent, OverlayInput, PaletteView};
use gpui::{
    div, prelude::FluentBuilder as _, px, AnyElement, FontWeight, InteractiveElement as _,
    IntoElement, ParentElement, SharedString, StatefulInteractiveElement as _, Styled,
};

use super::{dialog_card, hint_bar, keycap, overlay, KeyPress, OverlayCx, MONO};

/// The debug selector on palette row `index`, for tests.
pub fn row_selector(index: usize) -> String {
    format!("palette-row-{index}")
}

/// What a key means in the palette.
pub fn key_events(view: &PaletteView, key: &KeyPress) -> Vec<HostEvent> {
    let one = |input| vec![overlay(input)];
    match key {
        KeyPress::Text(text) | KeyPress::Paste(text) => {
            // A pasted newline would be a control character to the filter.
            let typed: String = text.chars().filter(|c| !c.is_control()).collect();
            if typed.is_empty() {
                return Vec::new();
            }
            one(OverlayInput::PaletteFilter(format!(
                "{}{typed}",
                view.filter
            )))
        }
        KeyPress::Backspace => {
            if view.filter.is_empty() {
                return Vec::new();
            }
            let mut filter = view.filter.clone();
            filter.pop();
            one(OverlayInput::PaletteFilter(filter))
        }
        KeyPress::Up if view.selected > 0 => one(OverlayInput::SelectRow(view.selected - 1)),
        KeyPress::Down if view.selected + 1 < view.entries.len() => {
            one(OverlayInput::SelectRow(view.selected + 1))
        }
        // Run the highlighted row by name, so a row the host no longer offers
        // is refused rather than something else run in its place.
        KeyPress::Enter => view
            .entries
            .get(view.selected)
            .map(|row| one(OverlayInput::PaletteRun(row.action.clone())))
            .unwrap_or_default(),
        KeyPress::Esc => one(OverlayInput::Cancel),
        _ => Vec::new(),
    }
}

/// Draw the palette.
pub fn render(view: &PaletteView, cx: &OverlayCx) -> AnyElement {
    let p = cx.palette;

    let field = div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(10.))
        .h(px(52.))
        .px(px(16.))
        .child(
            div()
                .text_size(px(15.))
                .text_color(p.muted.hsla())
                .child("›"),
        )
        .child(
            div()
                .flex_1()
                .flex()
                .flex_row()
                .items_center()
                .text_size(px(15.))
                .map(|d| {
                    let caret = div().w(px(1.5)).h(px(18.)).bg(p.accent.hsla());
                    if view.filter.is_empty() {
                        d.child(caret).child(
                            div()
                                .pl(px(2.))
                                .text_color(p.faint.hsla())
                                .child("Type a command…"),
                        )
                    } else {
                        d.child(div().text_color(p.ink.hsla()).child(view.filter.clone()))
                            .child(caret)
                    }
                }),
        )
        .child(keycap("esc", p));

    let context = context_line(cx);

    let mut rows = Vec::with_capacity(view.entries.len());
    let mut section = None;
    for (i, row) in view.entries.iter().enumerate() {
        let header = (section != Some(row.section)).then(|| {
            section = Some(row.section);
            div()
                .pt(px(if i == 0 { 4. } else { 10. }))
                .pb(px(4.))
                .px(px(10.))
                .text_size(px(10.5))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(p.muted.hsla())
                .child(row.section.to_uppercase())
        });
        let selected = i == view.selected;
        let selector = row_selector(i);
        let line = div()
            .id(SharedString::from(selector.clone()))
            .debug_selector(move || selector)
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.))
            .h(px(32.))
            .px(px(10.))
            .rounded(px(6.))
            .cursor_pointer()
            .map(|d| {
                if selected {
                    d.bg(p.surface_raised.hsla())
                } else {
                    d.hover(|s| s.bg(p.surface_raised.hsla()))
                }
            })
            .on_click(cx.on_click_emit(vec![overlay(OverlayInput::PaletteRun(row.action.clone()))]))
            .child(
                div()
                    .w(px(2.))
                    .h(px(14.))
                    .rounded(px(1.))
                    .when(selected, |d| d.bg(p.accent.hsla())),
            )
            .child(
                div()
                    .flex_1()
                    .text_size(px(13.))
                    .text_color(if selected {
                        p.ink.hsla()
                    } else {
                        p.ink_2.hsla()
                    })
                    .child(row.label),
            )
            .children(row.keycap.clone().map(|k| keycap(k, p)));
        // Header and row are one child, so the scroll container's item index
        // is the row index the host selects.
        rows.push(div().children(header).child(line));
    }

    let list = div()
        .id("palette-rows")
        .track_scroll(cx.list_scroll)
        .overflow_y_scroll()
        .max_h(px(400.))
        .px(px(8.))
        .py(px(6.))
        .children(rows)
        .when(view.entries.is_empty(), |d| {
            d.child(
                div()
                    .px(px(10.))
                    .py(px(18.))
                    .text_color(p.faint.hsla())
                    .child("No matching commands"),
            )
        });

    let count = match view.entries.len() {
        1 => "1 command".to_string(),
        n => format!("{n} commands"),
    };
    let footer = div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .px(px(16.))
        .py(px(9.))
        .bg(p.surface_window.hsla())
        .border_t_1()
        .border_color(p.hairline.hsla())
        .child(hint_bar(
            p,
            &[("↑↓", "select"), ("⏎", "run"), ("esc", "close")],
        ))
        .child(
            div()
                .text_size(px(11.5))
                .text_color(p.faint.hsla())
                .child(count),
        );

    dialog_card(p, 620.)
        .child(field)
        .children(context)
        .child(div().h(px(1.)).bg(p.hairline.hsla()))
        .child(list)
        .child(footer)
        .into_any_element()
}

/// `flightdeck › ui-redesign`: the project and agent the commands act on, or
/// nothing when the shell has not said.
fn context_line(cx: &OverlayCx) -> Option<gpui::Div> {
    let p = cx.palette;
    let parts: Vec<String> = [&cx.context.project, &cx.context.agent]
        .into_iter()
        .flatten()
        .cloned()
        .collect();
    if parts.is_empty() {
        return None;
    }
    Some(
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.))
            .px(px(16.))
            .pb(px(10.))
            .mt(px(-6.))
            .text_size(px(11.5))
            .text_color(p.muted.hsla())
            .child("in")
            .children(parts.into_iter().enumerate().flat_map(|(i, part)| {
                let sep = (i > 0).then(|| div().text_color(p.separator.hsla()).child("›"));
                sep.into_iter().chain(std::iter::once(
                    div()
                        .font_family(MONO)
                        .text_color(p.ink_2.hsla())
                        .child(part),
                ))
            })),
    )
}
