//! Status glyphs and small shared pieces (keycaps, pills) the regions draw.
//!
//! A status is never colour alone: each has its own shape (see
//! [`crate::assets`]), and the working arc spins, so "busy" reads without any
//! colour vision at all. How it spins, cheaply, is [`crate::views::spinner`].

use flightdeck::view::{AgentBadge, ProjectStatus};
use gpui::{div, px, svg, AnyElement, ElementId, IntoElement, ParentElement, Pixels, Styled};

use crate::assets::icon;
use crate::fonts::MONO_FAMILY;
use crate::theme::{Hex, Palette};

/// An icon from the embedded set, tinted `colour`.
pub fn icon(path: &'static str, size: Pixels, colour: Hex) -> gpui::Svg {
    svg()
        .path(path)
        .size(size)
        .flex_none()
        .text_color(colour.hsla())
}

/// The working arc, turning in steps with the app's spinner clock
/// ([`crate::views::spinner`]). `id` must be unique among its siblings (it
/// keys the arc's own entity).
pub fn spinner(id: impl Into<ElementId>, size: Pixels, colour: Hex) -> AnyElement {
    crate::views::spinner::spinner(id, size, colour).into_any_element()
}

/// An agent's status glyph: arc (working, spinning), filled triangle
/// (waiting for you), hollow ring (idle), check (done), cross (error).
pub fn agent_glyph(
    id: impl Into<ElementId>,
    badge: AgentBadge,
    size: Pixels,
    p: &Palette,
) -> AnyElement {
    match badge {
        AgentBadge::Working => spinner(id, size, p.status_working),
        AgentBadge::WaitingAttention => {
            icon(icon::STATUS_ATTENTION, size, p.status_attention).into_any_element()
        }
        AgentBadge::Idle => icon(icon::STATUS_IDLE, size, p.status_idle).into_any_element(),
        AgentBadge::Done => icon(icon::STATUS_DONE, size, p.status_done).into_any_element(),
        AgentBadge::Error => icon(icon::STATUS_ERROR, size, p.status_error).into_any_element(),
    }
}

/// A project tab's status: a small solid dot when idle (the mockup's quiet
/// state), the spinning arc when an agent works, the triangle when one needs
/// you.
pub fn project_glyph(id: impl Into<ElementId>, status: ProjectStatus, p: &Palette) -> AnyElement {
    match status {
        ProjectStatus::Idle => div()
            .flex_none()
            .size(px(7.))
            .rounded_full()
            .bg(p.status_idle.hsla())
            .into_any_element(),
        ProjectStatus::Working => spinner(id, px(12.), p.status_working),
        ProjectStatus::NeedsAttention => {
            icon(icon::STATUS_ATTENTION, px(11.), p.status_attention).into_any_element()
        }
    }
}

/// The colour a badge's words are drawn in.
pub fn badge_colour(badge: AgentBadge, p: &Palette) -> Hex {
    match badge {
        AgentBadge::Working => p.status_working,
        AgentBadge::WaitingAttention => p.status_attention,
        AgentBadge::Idle => p.muted,
        AgentBadge::Done => p.status_done,
        AgentBadge::Error => p.status_error,
    }
}

/// A keycap hint in mono (`⌃N`), coloured `colour`.
pub fn keycap(text: impl Into<gpui::SharedString>, colour: Hex) -> gpui::Div {
    div()
        .flex_none()
        .font_family(MONO_FAMILY)
        .text_size(px(10.5))
        .text_color(colour.hsla())
        .child(text.into())
}

/// A plain-text tooltip builder, for `.tooltip(..)` on a stateful element.
pub fn tooltip(
    text: impl Into<gpui::SharedString>,
) -> impl Fn(&mut gpui::Window, &mut gpui::App) -> gpui::AnyView + 'static {
    let text: gpui::SharedString = text.into();
    move |window, cx| gpui_component::tooltip::Tooltip::new(text.clone()).build(window, cx)
}

/// A rounded pill (`6 changed`, `no upstream`).
pub fn pill(text: impl Into<gpui::SharedString>, ink: Hex, p: &Palette) -> gpui::Div {
    div()
        .flex_none()
        .px_2()
        .py(px(2.))
        .rounded(px(10.))
        .bg(p.surface_raised.hsla())
        .text_size(px(11.5))
        .text_color(ink.hsla())
        .child(text.into())
}
