//! The 30px status bar (design A1). Left: the input-mode pill and the hints
//! for that mode. Right: the isolated badge, the remote indicators (web
//! viewers, phone, the input-lock holder — `overlays::remote`'s), and help.
//! The update notice is the banner above the main area
//! (`overlays::update::update_banner`), not a status-bar item.
//!
//! Pill and hints come from [`flightdeck::view::ModeBarView`] (the TUI's
//! status bar model); each hint's keycap and click come from the keymap entry
//! it names, so clicking "app mode" is exactly the leave-focus chord.

use flightdeck::app::modes::InputMode;
use flightdeck::host::{HostNotices, RemoteStatus};
use flightdeck::view::{HintAction, HintView, ModeBarView};
use flightdeck_desktop::overlays::remote::remote_indicator;
use flightdeck_desktop::overlays::update::isolated_badge;
use gpui::{
    div, px, App, Entity, FontWeight, InteractiveElement, IntoElement, ParentElement, Pixels,
    StatefulInteractiveElement, Styled,
};
use gpui_component::h_flex;

use crate::commands::{hint_entry_id, keycap, perform_id};
use crate::fonts::MONO_FAMILY;
use crate::host::HostModel;
use crate::theme::Palette;

/// Height from the design brief (A1).
pub const STATUS_BAR_HEIGHT: Pixels = px(30.);

/// The bar.
pub fn status_bar(
    bar: &ModeBarView,
    remote: &RemoteStatus,
    notices: &HostNotices,
    host: &Entity<HostModel>,
    p: &Palette,
    cx: &App,
) -> impl IntoElement {
    let (pill_bg, pill_ink, pill_word) = match bar.mode {
        InputMode::Terminal => (p.pill_terminal_bg, p.pill_terminal_ink, "TERMINAL"),
        InputMode::App => (p.pill_app_bg, p.pill_app_ink, "APP"),
    };
    let pill_entry = hint_entry_id(bar.pill_action);
    let pill = {
        let host = host.clone();
        div()
            .id("mode-pill")
            .debug_selector(|| "mode-pill".into())
            .flex_none()
            .px_2()
            .py(px(2.))
            .rounded(px(4.))
            .bg(pill_bg.hsla())
            .text_color(pill_ink.hsla())
            .font_family(MONO_FAMILY)
            .text_size(px(10.5))
            .font_weight(FontWeight::MEDIUM)
            .cursor_pointer()
            .child(pill_word)
            .on_click(move |_, _, cx| perform_id(pill_entry, &host, cx))
    };

    let (help, hints): (Vec<&HintView>, Vec<&HintView>) = bar
        .hints
        .iter()
        .partition(|h| h.action == HintAction::OpenHelp);

    h_flex()
        .flex_shrink_0()
        .h(STATUS_BAR_HEIGHT)
        .px(px(14.))
        .gap_4()
        .bg(p.surface_window.hsla())
        .border_t_1()
        .border_color(p.hairline.hsla())
        .text_size(px(11.5))
        .text_color(p.muted.hsla())
        .child(pill)
        .children(hints.into_iter().map(|h| hint(h, host, p)))
        .child(div().flex_1())
        .children(isolated_badge(notices, cx))
        .child(remote_indicator(remote, cx))
        .children(help.into_iter().map(|h| hint(h, host, p)))
}

/// One clickable hint: `⌥Esc app mode`.
fn hint(h: &HintView, host: &Entity<HostModel>, p: &Palette) -> impl IntoElement {
    let entry = hint_entry_id(h.action);
    let host = host.clone();
    h_flex()
        .id(entry)
        .debug_selector(move || format!("hint-{entry}"))
        .flex_none()
        .gap_1()
        .cursor_pointer()
        .hover(|s| s.text_color(p.ink_2.hsla()))
        .child(
            div()
                .font_family(MONO_FAMILY)
                .text_color(p.ink_2.hsla())
                .child(keycap(entry).unwrap_or_else(|| h.key.clone())),
        )
        .child(h.label.clone())
        .on_click(move |_, _, cx| perform_id(entry, &host, cx))
}
