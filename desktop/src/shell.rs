//! The main window's root view: the Projects view (design A1).
//!
//! ```text
//! +--------------------------------------------------------------+
//! | titlebar (44px): view switcher · project tabs · command      |
//! +--------------+-----------------------------------------------+
//! | sidebar      | git strip (42px)                              |
//! | (272px)      +-----------------------------------------------+
//! | "App" focus  | terminal ("Terminal" focus)                   |
//! +--------------+-----------------------------------------------+
//! | status bar (30px)                                            |
//! +--------------------------------------------------------------+
//!   + the overlay layer (flightdeck_desktop::overlays), the root's last child
//! ```
//!
//! ## Keys and focus
//!
//! The root carries the `"Global"` key context and the one
//! `on_action::<KeymapAction>` handler; the sidebar carries `"App"` and the
//! terminal `"Terminal"` — siblings, never nested (see `flightdeck_desktop::
//! keys`). GPUI focus follows the host's input mode each frame: TERMINAL mode
//! focuses the terminal, APP mode the sidebar. An overlay is the overlay
//! layer's: each render hands it `AppHost::overlay()`, it takes the keyboard
//! while one is up (its "Overlay" context disables every Global chord, as the
//! TUI's modal swallows every key) and gives focus back when it closes.
//!
//! ## The view switch
//!
//! While the persisted main view is Mission control (`views::mission`), its
//! view replaces the sidebar and main area, and its status bar the Projects
//! one; it carries the same `"App"` focus on its own rail, and in TERMINAL
//! mode shows this window's terminal view full size, so the focus rule above
//! holds unchanged. Its moving keys are taken before a chord is performed
//! (`MissionControl::intercept`).

use flightdeck::app::modes::InputMode;
use flightdeck::contracts::PtySize;
use flightdeck::persistence::workspace::MainView;
use flightdeck_desktop::keys::{app_key_down, KeymapAction};
use gpui::prelude::FluentBuilder;
use gpui::{
    div, px, AppContext, Context, Entity, FocusHandle, InteractiveElement, IntoElement,
    ParentElement, Render, Styled, Window,
};
use gpui_component::{h_flex, v_flex};

use std::rc::Rc;

use flightdeck_desktop::overlays::update::{update_banner, OnDismiss};
use flightdeck_desktop::overlays::{Emit, OverlayContext, OverlayLayer};

use crate::commands::{keymap, perform_entry};
use crate::host::HostModel;
use crate::terminal::view::TerminalView;
use crate::theme::Palette;
use crate::views::git_strip::git_strip;
use crate::views::mission::{mission_status_bar, MissionControl};
use crate::views::sidebar::{sidebar, SidebarData};
use crate::views::status_bar::status_bar;
use crate::views::titlebar::TitleBar;

/// The PTY size agents are first spawned at, before the terminal element has
/// measured itself: the default 1280×800 window minus the chrome, over a
/// nominal 13px Geist Mono cell (≈7.8×17px). The element's first frame
/// replaces it with the measured size.
pub const NOMINAL_PTY_SIZE: PtySize = PtySize {
    rows: 38,
    cols: 123,
};

pub struct FlightDeckWindow {
    host: Entity<HostModel>,
    titlebar: Entity<TitleBar>,
    terminal: Entity<TerminalView>,
    app_focus: FocusHandle,
    /// Mission control (A2/A3), drawn instead of the Projects view's sidebar
    /// and main area while it is the persisted main view.
    mission: Entity<MissionControl>,
    /// Draws the host's overlay and answers it through `emit`.
    layer: Entity<OverlayLayer>,
    /// The update banner was dismissed for this session.
    update_dismissed: bool,
}

impl FlightDeckWindow {
    pub fn new(host: Entity<HostModel>, cx: &mut Context<Self>) -> Self {
        cx.observe(&host, |_, _, cx| cx.notify()).detach();
        // Overlay answers go to the host; the layer is refreshed from the
        // re-render that follows (never from inside `emit`).
        let emit: Emit = {
            let host = host.clone();
            Rc::new(move |event, _, cx| host.update(cx, |model, cx| model.dispatch(event, cx)))
        };
        let titlebar = cx.new(|cx| TitleBar::new(host.clone(), emit.clone(), cx));
        let terminal = cx.new(|cx| TerminalView::for_host(host.clone(), cx));
        let layer = cx.new(|cx| OverlayLayer::new(emit, cx));
        let app_focus = cx.focus_handle();
        let mission =
            cx.new(|cx| MissionControl::new(host.clone(), terminal.clone(), app_focus.clone(), cx));
        Self {
            host,
            titlebar,
            terminal,
            app_focus,
            mission,
            layer,
            update_dismissed: false,
        }
    }

    /// A table chord fired (or a menu item dispatched one): perform it. (With
    /// an overlay up the layer's context has already disabled the Global
    /// chords; this guard only keeps a stray one from acting behind it.)
    fn on_keymap_action(&mut self, action: &KeymapAction, _: &mut Window, cx: &mut Context<Self>) {
        if self.host.read(cx).host().overlay().is_some() {
            cx.propagate();
            return;
        }
        if let Some(entry) = action.entry(keymap()) {
            // In Mission control the moving keys follow tile order.
            let mission = self.host.read(cx).host().workspace_ui().view == MainView::Mission;
            if mission && MissionControl::intercept(&self.host, entry, cx) {
                return;
            }
            perform_entry(entry, &self.host, cx);
        }
    }
}

impl Render for FlightDeckWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = *Palette::global(cx);
        let model = self.host.read(cx);
        let host = model.host();
        let overlay = host.overlay();
        let has_terminal = host.active_terminal().is_some();
        let mode = host.active_state().mode();
        let sidebar_data = SidebarData::read(model);
        let strip = host.git_strip();
        let leave = crate::commands::keycap("FocusApp").unwrap_or_default();
        let help = crate::commands::keycap("OpenHelp").unwrap_or_default();
        let bar = host.mode_bar(&leave, &help);
        let remote = host.remote_status();
        let notices = host.notices();
        let view = host.workspace_ui().view;
        let banner = if self.update_dismissed {
            None
        } else {
            let this = cx.entity().downgrade();
            let dismiss: OnDismiss = Rc::new(move |_, cx| {
                let _ = this.update(cx, |view, cx| {
                    view.update_dismissed = true;
                    cx.notify();
                });
            });
            update_banner(&notices, Some(dismiss), cx)
        };

        let context = OverlayContext {
            project: host
                .project_name(host.active_project_index())
                .map(str::to_string),
            agent: host.active_state().selected().map(|t| t.meta.name.clone()),
        };
        let overlay_open = overlay.is_some();
        // The layer takes (and later returns) the keyboard itself.
        self.layer.update(cx, |layer, cx| {
            layer.set_context(context, cx);
            layer.set_view(overlay, window, cx);
        });

        // Focus follows the host (see the module docs).
        if !overlay_open {
            let want = if mode == InputMode::Terminal && has_terminal {
                self.terminal.read(cx).focus_handle().clone()
            } else {
                self.app_focus.clone()
            };
            if !want.is_focused(window) {
                window.focus(&want, cx);
            }
        }

        let app_host = self.host.clone();
        let sidebar = sidebar(&sidebar_data, &self.host, &p)
            .key_context(flightdeck::app::keymap::Context::App.name())
            .track_focus(&self.app_focus)
            .on_key_down(move |event, _, cx| {
                // The TUI's leniency for App-mode chords GPUI did not bind
                // exactly (Ctrl-Alt-n, …).
                if let Some(entry) = app_key_down(keymap(), event) {
                    perform_entry(entry, &app_host, cx);
                    cx.stop_propagation();
                }
            });

        let main = v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .bg(p.surface_terminal.hsla())
            .child(git_strip(&strip, &self.host, &p))
            .child(if has_terminal {
                div()
                    .flex_1()
                    .min_h_0()
                    .child(self.terminal.clone())
                    .into_any_element()
            } else {
                empty_terminal(&p, sidebar_data.rows.is_empty()).into_any_element()
            });

        v_flex()
            .key_context(flightdeck::app::keymap::Context::Global.name())
            .on_action(cx.listener(Self::on_keymap_action))
            .relative()
            .size_full()
            .bg(p.surface_window.hsla())
            .text_color(p.ink.hsla())
            .font_family(crate::fonts::UI_FAMILY)
            .child(self.titlebar.clone())
            .children(banner)
            // The view switch: Mission control replaces the sidebar, the main
            // area and the status bar's hints; the titlebar and the overlay
            // layer are the same in both.
            .map(|root| match view {
                MainView::Projects => root
                    .child(
                        h_flex()
                            .flex_1()
                            .min_h_0()
                            .items_start()
                            .child(sidebar)
                            .child(main),
                    )
                    .child(status_bar(&bar, &remote, &notices, &self.host, &p, cx)),
                MainView::Mission => root
                    .child(self.mission.clone())
                    .child(mission_status_bar(&self.host, &p, cx)),
            })
            .child(self.layer.clone())
    }
}

/// The terminal well with no agent selected.
fn empty_terminal(p: &Palette, no_agents: bool) -> impl IntoElement {
    let hint = if no_agents {
        format!(
            "{} starts an agent in its own worktree.",
            crate::commands::keycap("NewAgentTab").unwrap_or_default()
        )
    } else {
        "Select an agent to see its terminal.".to_string()
    };
    v_flex()
        .flex_1()
        .size_full()
        .items_center()
        .justify_center()
        .gap_1()
        .child(
            div()
                .text_sm()
                .text_color(p.ink_2.hsla())
                .child("No agent selected"),
        )
        .child(
            div()
                .text_size(px(12.))
                .text_color(p.muted.hsla())
                .child(hint),
        )
}
