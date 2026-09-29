//! The 44px window titlebar: view switcher, one tab per open project, the
//! `+` that opens a project, and the command field (design A1).
//!
//! On macOS the native titlebar is transparent and this view IS the titlebar:
//! it draws under the inset traffic lights and moves the window itself (see
//! [`crate::app::window_options`], which sets `app_owns_titlebar_drag`). On
//! Windows and Linux the OS draws its own decorations above the window, and
//! this row is a plain toolbar — no drag or double-click handling, because the
//! native title bar above it already does both.
//!
//! Every control does what its chord does: a tab click is
//! `SwitchProject(Index)` (Shift-Left/Right cycle through the same event),
//! `+` answers a native folder picker with `HostEvent::OpenProject` (the host
//! validates it as the TUI's folder browser does), `×` is the palette's Close
//! Project, and the command field is Ctrl-g.

use flightdeck::app::commands::Selector;
use flightdeck::host::HostEvent;
use flightdeck::tui::palette::PaletteAction;
use flightdeck::view::{AgentBadge, ProjectStatus, ProjectTabView};
use gpui::prelude::FluentBuilder;
#[cfg(target_os = "macos")]
use gpui::MouseButton;
use gpui::{
    div, px, App, Context, Entity, FontWeight, InteractiveElement, IntoElement, ParentElement,
    PathPromptOptions, Pixels, Render, StatefulInteractiveElement, Styled, Window,
};
use gpui_component::h_flex;

use crate::assets::icon;
use crate::commands::{keycap, perform_id};
use crate::fonts::MONO_FAMILY;
use crate::host::HostModel;
use crate::theme::Palette;
use crate::views::icons;

/// Height from the design brief (A1).
pub const TITLEBAR_HEIGHT: Pixels = px(44.);

/// Room left of the first control for the macOS traffic lights (three 12px
/// buttons plus AppKit's spacing, inset by `crate::app::TRAFFIC_LIGHT_INSET`).
#[cfg(target_os = "macos")]
const LEADING_INSET: Pixels = px(84.);
#[cfg(not(target_os = "macos"))]
const LEADING_INSET: Pixels = px(12.);

/// A project name longer than this is truncated with an ellipsis, so one long
/// name cannot push the others off the bar.
const TAB_NAME_MAX: Pixels = px(160.);

pub struct TitleBar {
    host: Entity<HostModel>,
    /// Set on mouse-down, consumed on the first mouse-move: a press that then
    /// moves is a window drag, one that does not is a click. Mirrors
    /// gpui-component's `TitleBar`, which we do not use because it draws its
    /// own min/max/close on Windows/Linux and the brief keeps native ones.
    #[cfg(target_os = "macos")]
    drag_pending: bool,
}

impl TitleBar {
    pub fn new(host: Entity<HostModel>, cx: &mut Context<Self>) -> Self {
        cx.observe(&host, |_, _, cx| cx.notify()).detach();
        Self {
            host,
            #[cfg(target_os = "macos")]
            drag_pending: false,
        }
    }
}

impl Render for TitleBar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = *Palette::global(cx);
        let model = self.host.read(cx);
        let tabs = model.host().project_tabs();
        let isolated = model.host().is_isolated();
        let needs_you = agents_needing_you(model);

        let bar = h_flex()
            .id("titlebar")
            .flex_shrink_0()
            .h(TITLEBAR_HEIGHT)
            .pl(LEADING_INSET)
            .pr_3()
            .gap(px(14.))
            .bg(p.surface_window.hsla())
            .border_b_1()
            .border_color(p.hairline.hsla())
            .child(view_switcher(&p, needs_you))
            .child(div().flex_none().w(px(1.)).h(px(20.)).bg(p.border.hsla()))
            .child(project_tabs(&p, &tabs, &self.host, isolated))
            .child(command_field(&p, &self.host));

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

/// How many agents across every open project are waiting for the user: the
/// Mission control badge.
fn agents_needing_you(model: &HostModel) -> usize {
    let host = model.host();
    (0..host.project_count())
        .filter_map(|i| Some((host.project_state(i)?, host.git_status(i)?)))
        .flat_map(|(state, git)| {
            flightdeck::view::agent_row_views(state, git, host.now_ms(), host.now_unix_secs())
        })
        .filter(|row| row.badge == AgentBadge::WaitingAttention)
        .count()
}

/// `[ Projects | Mission control (n) ]`, Projects selected. Mission control
/// is milestone M5; its segment shows the needs-you count already.
fn view_switcher(p: &Palette, needs_you: usize) -> impl IntoElement {
    let segment = |path: &'static str, label: &'static str, selected: bool| {
        let base = h_flex()
            .h(px(24.))
            .px(px(10.))
            .gap(px(6.))
            .rounded(px(5.))
            .text_size(px(12.));
        if selected {
            base.bg(p.surface_raised_nested.hsla())
                .text_color(p.ink.hsla())
                .font_weight(FontWeight::MEDIUM)
                .child(icons::icon(path, px(12.), p.ink))
        } else {
            base.text_color(p.muted.hsla())
                .child(icons::icon(path, px(12.), p.muted))
        }
        .child(label)
    };
    h_flex()
        .flex_none()
        .p(px(3.))
        .gap(px(2.))
        .rounded(px(8.))
        .bg(p.surface_input.hsla())
        .border_1()
        .border_color(p.border.hsla())
        .child(segment(icon::PROJECTS, "Projects", true))
        .child(
            segment(icon::MISSION, "Mission control", false).when(needs_you > 0, |s| {
                s.child(
                    div()
                        .size(px(16.))
                        .rounded_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(p.status_attention_bg.hsla())
                        .text_color(p.status_attention.hsla())
                        .text_size(px(10.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(needs_you.to_string()),
                )
            }),
        )
}

/// The project tabs, then `+`. The row takes the free width and scrolls
/// sideways when there are more tabs than fit; names are truncated.
fn project_tabs(
    p: &Palette,
    tabs: &[ProjectTabView],
    host: &Entity<HostModel>,
    isolated: bool,
) -> impl IntoElement {
    h_flex()
        .id("project-tabs")
        .flex_1()
        .min_w_0()
        .overflow_x_scroll()
        .text_size(px(12.5))
        .children(
            tabs.iter()
                .enumerate()
                .map(|(i, tab)| project_tab(p, i, tab, host, tabs.len() > 1)),
        )
        .when(!isolated, |row| row.child(open_project_button(p, host)))
}

/// One project tab: status glyph + name; the active one is raised and carries
/// its agent count and a close button.
fn project_tab(
    p: &Palette,
    index: usize,
    tab: &ProjectTabView,
    host: &Entity<HostModel>,
    closable: bool,
) -> impl IntoElement {
    let host_click = host.clone();
    let base = h_flex()
        .id(("project-tab", index))
        .debug_selector(|| format!("project-tab-{index}"))
        .flex_none()
        .h(px(30.))
        .px(px(10.))
        .gap(px(8.))
        .rounded(px(7.))
        .cursor_pointer()
        .child(icons::project_glyph(
            ("project-status", index),
            tab.status,
            p,
        ))
        .child(div().max_w(TAB_NAME_MAX).truncate().child(tab.name.clone()))
        .on_click(move |_, _, cx| {
            host_click.update(cx, |model, cx| {
                model.dispatch(HostEvent::SwitchProject(Selector::Index(index)), cx)
            })
        });
    let status_hint = match tab.status {
        ProjectStatus::Idle => "idle",
        ProjectStatus::Working => "working",
        ProjectStatus::NeedsAttention => "needs you",
    };
    if tab.active {
        let host_close = host.clone();
        base.border_1()
            .border_color(p.border.hsla())
            .bg(p.surface_raised.hsla())
            .text_color(p.ink.hsla())
            .font_weight(FontWeight::MEDIUM)
            .tooltip(icons::tooltip(format!("{} · {status_hint}", tab.name)))
            .child(
                div()
                    .font_family(MONO_FAMILY)
                    .text_size(px(10.5))
                    .text_color(p.muted.hsla())
                    .px(px(5.))
                    .py(px(1.))
                    .rounded(px(4.))
                    .bg(p.chip_bg.hsla())
                    .child(tab.agent_count.to_string()),
            )
            .when(closable, |tab_el| {
                tab_el.child(
                    div()
                        .id(("project-close", index))
                        .debug_selector(|| format!("project-close-{index}"))
                        .flex_none()
                        .rounded(px(4.))
                        .p(px(2.))
                        .hover(|s| s.bg(p.surface_raised_nested.hsla()))
                        .child(icons::icon(icon::CLOSE, px(10.), p.muted))
                        .on_click(move |_, _, cx| {
                            cx.stop_propagation();
                            host_close.update(cx, |model, cx| {
                                model.dispatch(
                                    HostEvent::RunPaletteAction(PaletteAction::CloseProject),
                                    cx,
                                )
                            })
                        }),
                )
            })
    } else {
        base.text_color(p.muted.hsla())
            .hover(|s| s.bg(p.surface_raised.hsla()).text_color(p.ink_2.hsla()))
            .tooltip(icons::tooltip(format!("{} · {status_hint}", tab.name)))
    }
}

/// `+`: pick a folder with the OS's own dialog, then open it through the host.
fn open_project_button(p: &Palette, host: &Entity<HostModel>) -> impl IntoElement {
    let host = host.clone();
    div()
        .id("open-project")
        .debug_selector(|| "open-project".into())
        .flex_none()
        .size(px(30.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(7.))
        .hover(|s| s.bg(p.surface_raised.hsla()))
        .child(icons::icon(icon::PLUS, px(14.), p.muted))
        .tooltip(icons::tooltip("Open project…"))
        .on_click(move |_, _, cx| pick_and_open_project(host.clone(), cx))
}

/// The native folder picker, answered with `HostEvent::OpenProject`.
pub fn pick_and_open_project(host: Entity<HostModel>, cx: &mut App) {
    let picked = cx.prompt_for_paths(PathPromptOptions {
        files: false,
        directories: true,
        multiple: false,
        prompt: Some("Open Project".into()),
    });
    cx.spawn(async move |cx| {
        let Ok(Ok(Some(paths))) = picked.await else {
            return;
        };
        if let Some(path) = paths.into_iter().next() {
            host.update(cx, |model, cx| {
                model.dispatch(HostEvent::OpenProject(path), cx)
            });
        }
    })
    .detach();
}

/// The 200px "Command… ⌃G" field: opens the palette, as Ctrl-g does.
fn command_field(p: &Palette, host: &Entity<HostModel>) -> impl IntoElement {
    let host = host.clone();
    h_flex()
        .id("command-field")
        .debug_selector(|| "command-field".into())
        .flex_none()
        .w(px(200.))
        .h(px(30.))
        .pl(px(12.))
        .pr(px(8.))
        .gap(px(10.))
        .rounded(px(7.))
        .border_1()
        .border_color(p.border.hsla())
        .bg(p.surface_input.hsla())
        .text_size(px(12.5))
        .text_color(p.muted.hsla())
        .cursor_pointer()
        .child(icons::icon(icon::SEARCH, px(13.), p.muted))
        .child(div().flex_1().child("Command…"))
        .child(
            div()
                .px(px(6.))
                .py(px(2.))
                .rounded(px(4.))
                .border_1()
                .border_color(p.border_strong.hsla())
                .bg(p.surface_raised.hsla())
                .child(icons::keycap(
                    keycap("OpenPalette").unwrap_or_default(),
                    p.ink_2,
                )),
        )
        .on_click(move |_, _, cx| perform_id("OpenPalette", &host, cx))
}
