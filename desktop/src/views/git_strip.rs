//! The 42px git strip above the terminal (design A1): the selected agent, its
//! branch and target, a changed-files pill and the upstream pill, then Pull
//! base (⌃U), Finish (⌃F) and Push (⌃P, primary).
//!
//! What it shows and which buttons are live comes from
//! [`flightdeck::view::GitStripView`]; a button the view marks unavailable is
//! drawn disabled and has no click handler at all, so it cannot dispatch.
//! Live buttons perform the keymap entry their chord performs.

use flightdeck::view::{GitStripView, UpstreamState};
use gpui::prelude::FluentBuilder;
use gpui::{
    div, px, Entity, FontWeight, InteractiveElement, IntoElement, ParentElement, Pixels,
    StatefulInteractiveElement, Styled,
};
use gpui_component::h_flex;

use crate::commands::{keycap, perform_id};
use crate::fonts::MONO_FAMILY;
use crate::host::HostModel;
use crate::theme::Palette;
use crate::views::icons;

/// Height from the design brief (A1).
pub const GIT_STRIP_HEIGHT: Pixels = px(42.);

/// The strip.
pub fn git_strip(view: &GitStripView, host: &Entity<HostModel>, p: &Palette) -> impl IntoElement {
    let bar = h_flex()
        .flex_shrink_0()
        .h(GIT_STRIP_HEIGHT)
        .pl(px(18.))
        .pr_3()
        .gap(px(14.))
        .border_b_1()
        .border_color(p.hairline.hsla())
        .bg(p.surface_window.hsla())
        .text_size(px(12.5));

    let (left, actions) = match &view.agent {
        None => (
            h_flex()
                .gap(px(14.))
                .child(div().text_color(p.muted.hsla()).child("No agent selected"))
                .child(
                    div()
                        .font_family(MONO_FAMILY)
                        .text_size(px(12.))
                        .text_color(p.muted.hsla())
                        .child(format!("base {}", view.default_branch)),
                ),
            flightdeck::view::GitActions {
                push: false,
                pull_base: false,
                finish: false,
            },
        ),
        Some(agent) => {
            let changed = agent.changes.map(|c| match c.files {
                0 => "clean".to_string(),
                n => format!("{n} changed"),
            });
            let upstream = match &agent.upstream {
                UpstreamState::Unknown => None,
                UpstreamState::None => Some("no upstream".to_string()),
                UpstreamState::Tracking {
                    upstream,
                    ahead,
                    behind,
                } => Some(match (ahead, behind) {
                    (0, 0) => format!("{upstream} · in sync"),
                    _ => format!("{upstream} ↑{ahead} ↓{behind}"),
                }),
            };
            let target_colour = if view.default_branch_valid || !agent.target_is_default {
                p.muted
            } else {
                p.status_attention
            };
            (
                h_flex()
                    .min_w_0()
                    .gap(px(14.))
                    .child(
                        div()
                            .flex_none()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(p.ink.hsla())
                            .child(agent.name.clone()),
                    )
                    .child(div().text_color(p.separator.hsla()).child("/"))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_family(MONO_FAMILY)
                            .text_size(px(12.))
                            .text_color(p.ink_2.hsla())
                            .child(format!("⎇ {}", agent.branch)),
                    )
                    .child(
                        div()
                            .flex_none()
                            .font_family(MONO_FAMILY)
                            .text_size(px(12.))
                            .text_color(target_colour.hsla())
                            .child(format!("→ {}", agent.target_branch)),
                    )
                    .when_some(changed, |row, text| {
                        row.child(icons::pill(text, p.ink_2, p))
                    })
                    .when_some(upstream, |row, text| {
                        row.child(icons::pill(text, p.muted, p))
                    })
                    .when(agent.base_drift > 0, |row| {
                        row.child(icons::pill(
                            format!("target +{}", agent.base_drift),
                            p.status_attention,
                            p,
                        ))
                    }),
                agent.actions,
            )
        }
    };

    bar.child(left)
        .child(div().flex_1())
        .child(button(
            "git-pull-base",
            "Pull base",
            "PullBase",
            actions.pull_base,
            false,
            host,
            p,
        ))
        .child(button(
            "git-finish",
            "Finish",
            "FinishLocalMerge",
            actions.finish,
            false,
            host,
            p,
        ))
        .child(button(
            "git-push",
            "Push",
            "PushBranch",
            actions.push,
            true,
            host,
            p,
        ))
}

/// One action button. `primary` is the light-filled Push.
fn button(
    id: &'static str,
    label: &'static str,
    entry_id: &'static str,
    enabled: bool,
    primary: bool,
    host: &Entity<HostModel>,
    p: &Palette,
) -> impl IntoElement {
    let (bg, ink, hint) = match (primary, enabled) {
        (true, true) => (
            Some(p.button_primary_bg),
            p.button_primary_ink,
            p.button_primary_hint,
        ),
        (false, true) => (None, p.ink_2, p.faint),
        // Disabled: no fill, the least ink, and (below) no handler.
        (_, false) => (None, p.faint, p.separator),
    };
    let host = host.clone();
    h_flex()
        .id(id)
        .debug_selector(move || id.to_string())
        .flex_none()
        .h(px(28.))
        .px(px(if primary { 12. } else { 10. }))
        .gap(px(7.))
        .rounded(px(6.))
        .text_size(px(12.))
        .text_color(ink.hsla())
        .when(primary && enabled, |b| b.font_weight(FontWeight::SEMIBOLD))
        .when_some(bg, |b, bg| b.bg(bg.hsla()))
        .when(bg.is_none(), |b| b.border_1().border_color(p.border.hsla()))
        .child(label)
        .child(icons::keycap(keycap(entry_id).unwrap_or_default(), hint))
        .when(enabled, |b| {
            b.cursor_pointer()
                .on_click(move |_, _, cx| perform_id(entry_id, &host, cx))
        })
}
