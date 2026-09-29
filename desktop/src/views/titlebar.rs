//! The 44px window titlebar (placeholder content).
//!
//! On macOS the native titlebar is transparent and this view IS the titlebar:
//! it draws under the inset traffic lights and moves the window itself (see
//! [`crate::app::window_options`], which sets `app_owns_titlebar_drag`). On
//! Windows and Linux the OS draws its own decorations above the window, and
//! this row is a plain toolbar — no drag or double-click handling, because the
//! native title bar above it already does both.
//!
//! M0 content is static: the view switcher, one project tab and the command
//! field are shapes with labels, not controls.

#[cfg(target_os = "macos")]
use gpui::MouseButton;
use gpui::{
    div, px, Context, FontWeight, InteractiveElement, IntoElement, ParentElement, Pixels, Render,
    Styled, Window,
};
use gpui_component::h_flex;

use crate::theme::Palette;

/// Height from the design brief (A1).
pub const TITLEBAR_HEIGHT: Pixels = px(44.);

/// Room left of the first control for the macOS traffic lights (three 12px
/// buttons plus AppKit's spacing, inset by `crate::app::TRAFFIC_LIGHT_INSET`).
#[cfg(target_os = "macos")]
const LEADING_INSET: Pixels = px(84.);
#[cfg(not(target_os = "macos"))]
const LEADING_INSET: Pixels = px(12.);

#[derive(Default)]
pub struct TitleBar {
    /// Set on mouse-down, consumed on the first mouse-move: a press that then
    /// moves is a window drag, one that does not is a click. Mirrors
    /// gpui-component's `TitleBar`, which we do not use because it draws its
    /// own min/max/close on Windows/Linux and the brief keeps native ones.
    #[cfg(target_os = "macos")]
    drag_pending: bool,
}

impl Render for TitleBar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = *Palette::global(cx);

        let bar = h_flex()
            .id("titlebar")
            .flex_shrink_0()
            .h(TITLEBAR_HEIGHT)
            .pl(LEADING_INSET)
            .pr_3()
            .gap_3()
            .bg(p.surface_window.hsla())
            .border_b_1()
            .border_color(p.hairline.hsla())
            .child(view_switcher(&p))
            .child(div().w(px(1.)).h(px(18.)).bg(p.hairline.hsla()))
            .child(project_tab(&p, "flightdeck"))
            .child(div().flex_1())
            .child(command_field(&p));

        #[cfg(target_os = "macos")]
        let bar = bar
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &gpui::MouseDownEvent, window, _| {
                    if event.click_count >= 2 {
                        // Honour the user's System Settings choice
                        // (zoom / minimise / nothing) like a native titlebar.
                        window.titlebar_double_click();
                    } else {
                        this.drag_pending = true;
                    }
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.drag_pending = false),
            )
            .on_mouse_move(cx.listener(|this, _, window, _| {
                if std::mem::take(&mut this.drag_pending) {
                    window.start_window_move();
                }
            }));
        bar
    }
}

/// `[ Projects | Mission control ]` segmented control, Projects selected.
fn view_switcher(p: &Palette) -> impl IntoElement {
    let segment = |label: &'static str, selected: bool| {
        let base = div().px_2p5().py_0p5().rounded_md().text_xs();
        if selected {
            base.bg(p.surface_raised.hsla())
                .text_color(p.ink.hsla())
                .font_weight(FontWeight::MEDIUM)
        } else {
            base.text_color(p.muted.hsla())
        }
        .child(label)
    };
    h_flex()
        .p_0p5()
        .gap_0p5()
        .rounded_lg()
        .border_1()
        .border_color(p.hairline.hsla())
        .child(segment("Projects", true))
        .child(segment("Mission control", false))
}

/// One project tab: status dot + name, raised because it is the active one.
fn project_tab(p: &Palette, name: &'static str) -> impl IntoElement {
    h_flex()
        .gap_1p5()
        .px_2p5()
        .py_1()
        .rounded_md()
        .bg(p.surface_raised.hsla())
        .text_xs()
        .text_color(p.ink.hsla())
        .child(div().size(px(7.)).rounded_full().bg(p.status_idle.hsla()))
        .child(name)
}

/// The 200px "Command… ⌃G" field (display only in M0).
fn command_field(p: &Palette) -> impl IntoElement {
    h_flex()
        .w(px(200.))
        .h(px(26.))
        .px_2()
        .justify_between()
        .rounded_md()
        .border_1()
        .border_color(p.border.hsla())
        .bg(p.surface_input.hsla())
        .text_xs()
        .child(div().text_color(p.faint.hsla()).child("Command…"))
        .child(div().text_color(p.faint.hsla()).child("⌃G"))
}
