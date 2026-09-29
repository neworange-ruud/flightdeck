//! The 272px agents sidebar (placeholder content).
//!
//! Final shape (design brief A1): an "AGENTS" header with a status summary,
//! one row per agent with nested terminal rows under the selected one, and a
//! "New agent" footer. M0 renders the header, an empty state and the footer so
//! the column's surfaces, widths and type scale can be judged in the real
//! window; rows arrive with the agent model.

use gpui::{
    div, px, App, FontWeight, IntoElement, ParentElement, Pixels, RenderOnce, Styled, Window,
};
use gpui_component::{h_flex, v_flex};

use crate::theme::Palette;

/// Width from the design brief (A1).
pub const SIDEBAR_WIDTH: Pixels = px(272.);

#[derive(IntoElement, Default)]
pub struct Sidebar;

impl RenderOnce for Sidebar {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let p = Palette::global(cx);

        v_flex()
            .flex_shrink_0()
            .w(SIDEBAR_WIDTH)
            .h_full()
            .bg(p.surface_sidebar.hsla())
            .border_r_1()
            .border_color(p.hairline.hsla())
            // Header: "AGENTS" + the live summary ("1 working · 1 waiting").
            .child(
                h_flex()
                    .px_4()
                    .pt_4()
                    .pb_2()
                    .justify_between()
                    .text_xs()
                    .child(
                        div()
                            .text_color(p.muted.hsla())
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("AGENTS"),
                    )
                    .child(div().text_color(p.faint.hsla()).child("none running")),
            )
            // Body: empty state until agent rows exist.
            .child(
                v_flex()
                    .flex_1()
                    .px_4()
                    .pt_2()
                    .gap_1()
                    .text_sm()
                    .child(div().text_color(p.ink_2.hsla()).child("No agents yet"))
                    .child(
                        div()
                            .text_xs()
                            .text_color(p.faint.hsla())
                            .child("Start one to see it here."),
                    ),
            )
            // Footer: the "New agent ⌃N" button (display only in M0).
            .child(
                h_flex()
                    .p_3()
                    .border_t_1()
                    .border_color(p.hairline.hsla())
                    .child(
                        h_flex()
                            .flex_1()
                            .h(px(30.))
                            .px_3()
                            .justify_between()
                            .rounded_md()
                            .bg(p.surface_raised.hsla())
                            .text_sm()
                            .text_color(p.ink.hsla())
                            .child("New agent")
                            .child(div().text_xs().text_color(p.faint.hsla()).child("⌃N")),
                    ),
            )
    }
}
