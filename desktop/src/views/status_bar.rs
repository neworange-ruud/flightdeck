//! The 30px status bar (placeholder content).
//!
//! Left: the input-mode pill and the chords that matter in that mode. Right:
//! remote-access state (web viewers, paired phone) and help. M0 shows the
//! TERMINAL pill and static hints; none of it is wired to state yet.

use gpui::{
    div, px, App, FontWeight, IntoElement, ParentElement, Pixels, RenderOnce, Styled, Window,
};
use gpui_component::h_flex;

use crate::theme::Palette;

/// Height from the design brief (A1).
pub const STATUS_BAR_HEIGHT: Pixels = px(30.);

#[derive(IntoElement, Default)]
pub struct StatusBar;

impl RenderOnce for StatusBar {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = Palette::global(cx);
        let hint = |text: &'static str| div().text_color(p.faint.hsla()).child(text);

        h_flex()
            .flex_shrink_0()
            .h(STATUS_BAR_HEIGHT)
            .px_3()
            .gap_4()
            .bg(p.surface_window.hsla())
            .border_t_1()
            .border_color(p.hairline.hsla())
            .text_xs()
            .child(
                div()
                    .px_1p5()
                    .rounded_sm()
                    .bg(p.pill_terminal_bg.hsla())
                    .text_color(p.pill_terminal_ink.hsla())
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("TERMINAL"),
            )
            .child(hint("⌥Esc app mode"))
            .child(hint("⌃B split"))
            .child(hint("⌃T new shell"))
            .child(div().flex_1())
            .child(hint("F1 help"))
    }
}
