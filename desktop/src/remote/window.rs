//! A remote window's root view (`specs/DESKTOP_REMOTE_CONTROL_PLAN.md` §2.3,
//! §2.4): the Projects view's layout, drawn from a [`RemoteModel`].
//!
//! ```text
//! +--------------------------------------------------------------+
//! | titlebar: host · address · link state · project tabs · Command|
//! +--------------+-----------------------------------------------+
//! | sidebar      | git strip                                     |
//! | (shared      +-----------------------------------------------+
//! |  view)       | terminal (the mirrored one this window shows) |
//! +--------------+-----------------------------------------------+
//! | status bar: mode · hints · notice · seat · input lock         |
//! +--------------------------------------------------------------+
//!   + the overlay layer (palette, the host's shared dialog, help)
//! ```
//!
//! The sidebar and git strip are the local window's own elements
//! ([`crate::views`]), handed the core's view structs built from the mirror
//! and a [`Surface::Remote`] for their clicks. Keys follow the local window's
//! rule: the root carries the `"Global"` context and performs table chords —
//! through [`crate::commands::perform_remote_entry`] — the sidebar `"App"`,
//! the terminal `"Terminal"`, and focus follows this window's own input mode.
//! Mission control and split view have no remote counterpart in v1.

use std::rc::Rc;

use flightdeck::app::modes::InputMode;
use flightdeck::web::client::views;
use flightdeck::web::client::LinkEnd;
use flightdeck::web::protocol::{Seat, SeatRequest};
use flightdeck_desktop::keys::{app_key_down, KeymapAction};
use flightdeck_desktop::overlays::{Emit, OverlayContext, OverlayLayer};
use gpui::prelude::FluentBuilder;
use gpui::{
    div, px, AppContext, Context, Entity, FocusHandle, FontWeight, InteractiveElement, IntoElement,
    ParentElement, Render, StatefulInteractiveElement, Styled, Window,
};
use gpui_component::{h_flex, v_flex};

use super::RemoteModel;
use crate::commands::{keycap, keymap, perform_remote_entry, perform_remote_id};
use crate::fonts::MONO_FAMILY;
use crate::surface::Surface;
use crate::terminal::view::TerminalView;
use crate::theme::Palette;
use crate::views::git_strip::git_strip;
use crate::views::icons;
use crate::views::sidebar::{sidebar, SidebarData};
use crate::views::status_bar::STATUS_BAR_HEIGHT;
use crate::views::titlebar::TITLEBAR_HEIGHT;

/// Room for the macOS traffic lights, as the local titlebar leaves.
#[cfg(target_os = "macos")]
const LEADING_INSET: gpui::Pixels = px(84.);
#[cfg(not(target_os = "macos"))]
const LEADING_INSET: gpui::Pixels = px(12.);

pub struct RemoteWindow {
    remote: Entity<RemoteModel>,
    terminal: Entity<TerminalView>,
    app_focus: FocusHandle,
    layer: Entity<OverlayLayer>,
    /// macOS: a press that then moves drags the window, as the local
    /// titlebar does.
    #[cfg(target_os = "macos")]
    drag_pending: bool,
}

impl RemoteWindow {
    pub fn new(remote: Entity<RemoteModel>, cx: &mut Context<Self>) -> Self {
        cx.observe(&remote, |_, _, cx| cx.notify()).detach();
        let emit: Emit = {
            let remote = remote.clone();
            Rc::new(move |event, _, cx| remote.update(cx, |model, cx| model.dispatch(event, cx)))
        };
        let terminal = cx.new(|cx| TerminalView::for_remote(remote.clone(), cx));
        let layer = cx.new(|cx| OverlayLayer::new(emit, cx));
        Self {
            remote,
            terminal,
            app_focus: cx.focus_handle(),
            layer,
            #[cfg(target_os = "macos")]
            drag_pending: false,
        }
    }

    fn on_keymap_action(&mut self, action: &KeymapAction, _: &mut Window, cx: &mut Context<Self>) {
        if self.remote.read(cx).overlay().is_some() {
            cx.propagate();
            return;
        }
        if let Some(entry) = action.entry(keymap()) {
            // Quit FlightDeck (Cmd-Q, Ctrl-q) quits this app, as it does from
            // any window — never the host's FlightDeck.
            if entry.action == flightdeck::app::keymap::Action::Quit {
                cx.quit();
                return;
            }
            perform_remote_entry(entry, &self.remote, cx);
        }
    }

    fn titlebar(&mut self, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let model = self.remote.read(cx);
        let ws = model.workspace();
        let tabs = views::project_tabs(ws);
        let live = model.client().state().is_live();
        let link = model.link_label();
        let name = model.label().to_string();
        let address = model.address().to_string();
        let remote = self.remote.clone();

        let tabs_row = h_flex()
            .id("remote-project-tabs")
            .flex_1()
            .min_w_0()
            .overflow_x_scroll()
            .text_size(px(12.5))
            .children(tabs.iter().enumerate().map(|(i, tab)| {
                let remote = remote.clone();
                h_flex()
                    .id(("remote-project-tab", i))
                    .debug_selector(move || format!("remote-project-tab-{i}"))
                    .flex_none()
                    .h(px(30.))
                    .px(px(10.))
                    .gap(px(8.))
                    .rounded(px(7.))
                    .cursor_pointer()
                    .child(icons::project_glyph(
                        ("remote-project-status", i),
                        tab.status,
                        p,
                    ))
                    .child(div().max_w(px(160.)).truncate().child(tab.name.clone()))
                    .when_else(
                        tab.active,
                        |t| {
                            t.border_1()
                                .border_color(p.border.hsla())
                                .bg(p.surface_raised.hsla())
                                .text_color(p.ink.hsla())
                                .font_weight(FontWeight::MEDIUM)
                        },
                        |t| {
                            t.text_color(p.muted.hsla())
                                .hover(|s| s.bg(p.surface_raised.hsla()))
                        },
                    )
                    .on_click(move |_, _, cx| {
                        remote.update(cx, |model, cx| {
                            model.select_project(i);
                            cx.notify();
                        })
                    })
            }));

        let identity = h_flex()
            .id("remote-identity")
            .debug_selector(|| "remote-identity".into())
            .flex_none()
            .gap(px(8.))
            .child(
                div().size(px(8.)).rounded_full().bg(if live {
                    p.status_working
                } else {
                    p.status_attention
                }
                .hsla()),
            )
            .child(
                div()
                    .text_size(px(12.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(p.ink.hsla())
                    .child(name),
            )
            .child(
                div()
                    .font_family(MONO_FAMILY)
                    .text_size(px(11.))
                    .text_color(p.muted.hsla())
                    .child(address),
            )
            .child(
                div()
                    .debug_selector(|| "remote-link-state".into())
                    .text_size(px(11.))
                    .text_color(if live { p.muted } else { p.status_attention }.hsla())
                    .child(link),
            );

        let emit_remote = self.remote.clone();
        let command = h_flex()
            .id("remote-command-field")
            .debug_selector(|| "remote-command-field".into())
            .flex_none()
            .w(px(180.))
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
            .child(icons::icon(crate::assets::icon::SEARCH, px(13.), p.muted))
            .child(div().flex_1().child("Command…"))
            .child(icons::keycap(
                keycap("OpenPalette").unwrap_or_default(),
                p.ink_2,
            ))
            .on_click(move |_, _, cx| perform_remote_id("OpenPalette", &emit_remote, cx));

        let bar = h_flex()
            .id("remote-titlebar")
            .flex_shrink_0()
            .h(TITLEBAR_HEIGHT)
            .pl(LEADING_INSET)
            .pr_3()
            .gap(px(14.))
            .bg(p.surface_window.hsla())
            .border_b_1()
            .border_color(p.hairline.hsla())
            .child(identity)
            .child(div().flex_none().w(px(1.)).h(px(20.)).bg(p.border.hsla()))
            .child(tabs_row)
            .child(command);

        #[cfg(target_os = "macos")]
        let bar = bar
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, event: &gpui::MouseDownEvent, window, _| {
                    if event.click_count >= 2 {
                        window.titlebar_double_click();
                    } else {
                        this.drag_pending = true;
                    }
                }),
            )
            .on_mouse_up(
                gpui::MouseButton::Left,
                cx.listener(|this, _, _, _| this.drag_pending = false),
            )
            .on_mouse_move(cx.listener(|this, _, window, _| {
                if std::mem::take(&mut this.drag_pending) {
                    window.start_window_move();
                }
            }));
        bar
    }

    fn status_bar(&self, p: &Palette, cx: &Context<Self>) -> impl IntoElement {
        let model = self.remote.read(cx);
        let ws = model.workspace();
        let mode = model.mode();
        let (pill_bg, pill_ink, word, pill_entry) = match mode {
            InputMode::Terminal => (
                p.pill_terminal_bg,
                p.pill_terminal_ink,
                "TERMINAL",
                "FocusApp",
            ),
            InputMode::App => (p.pill_app_bg, p.pill_app_ink, "APP", "FocusTerminal"),
        };
        let remote = self.remote.clone();
        let pill = div()
            .id("remote-mode-pill")
            .debug_selector(|| "remote-mode-pill".into())
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
            .child(word)
            .on_click(move |_, _, cx| perform_remote_id(pill_entry, &remote, cx));

        let observing = ws.seat == Some(Seat::Observing);
        let holder = ws
            .input_holder()
            .filter(|s| !s.is_you)
            .map(|s| s.label.clone());
        let seat_text = match (observing, &holder) {
            (true, _) => "observing".to_string(),
            (false, Some(label)) => format!("{label} is typing"),
            (false, None) => "in control".to_string(),
        };
        let seat_button = {
            let remote = self.remote.clone();
            let (id, label, seat) = if observing {
                ("remote-seat-control", "Take control", SeatRequest::Write)
            } else if holder.is_some() {
                ("remote-seat-take-over", "Take over", SeatRequest::TakeOver)
            } else {
                ("remote-seat-observe", "Observe", SeatRequest::Observe)
            };
            h_flex()
                .id(id)
                .debug_selector(move || id.to_string())
                .flex_none()
                .px_2()
                .py(px(1.))
                .rounded(px(4.))
                .border_1()
                .border_color(p.border.hsla())
                .cursor_pointer()
                .hover(|s| s.bg(p.surface_raised.hsla()))
                .child(label)
                .on_click(move |_, _, cx| {
                    remote.update(cx, |model, cx| {
                        model.request_seat(seat);
                        cx.notify();
                    })
                })
        };
        let notice = model.notice().map(str::to_string);

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
            .child(hint(p, "OpenPalette", "commands"))
            .child(div().flex_1().min_w_0().truncate().children(notice))
            .child(
                div()
                    .debug_selector(|| "remote-seat".into())
                    .flex_none()
                    .child(seat_text),
            )
            .child(seat_button)
    }

    /// The main area: the terminal, or why there is none.
    fn main_area(&self, p: &Palette, cx: &Context<Self>) -> gpui::AnyElement {
        let model = self.remote.read(cx);
        let ws = model.workspace();
        if let Some(end) = model.ended() {
            return ended(p, end, &self.remote).into_any_element();
        }
        if !ws.ready {
            return notice_card(
                p,
                &format!("Connecting to {}…", model.address()),
                &model.link_label(),
            )
            .into_any_element();
        }
        if model.active_terminal().is_some() {
            return div()
                .flex_1()
                .min_h_0()
                .child(self.terminal.clone())
                .into_any_element();
        }
        let text = if ws.selected_session().is_none() {
            "No agent selected"
        } else {
            "This agent's terminal has not started on the host yet."
        };
        notice_card(p, text, "").into_any_element()
    }
}

impl Render for RemoteWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = *Palette::global(cx);
        let model = self.remote.read(cx);
        if model.close_requested() {
            window.remove_window();
        }
        let ws = model.workspace();
        let overlay = model.overlay();
        let mode = model.mode();
        let has_terminal = model.active_terminal().is_some();
        let data = SidebarData {
            rows: views::agent_rows(ws),
            focused_child: views::focused_child(ws),
            isolated: false,
            host_marker: views::host_marker(ws),
        };
        let strip = views::git_strip(ws);
        let context = OverlayContext {
            project: ws.selected_project().map(|p| p.name.clone()),
            agent: ws.selected_session().map(|s| s.name.clone()),
        };
        let overlay_open = overlay.is_some();
        self.layer.update(cx, |layer, cx| {
            layer.set_context(context, cx);
            layer.set_view(overlay, window, cx);
        });
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

        let surface = Surface::Remote(self.remote.clone());
        let app_remote = self.remote.clone();
        let side = sidebar(&data, &surface, &p)
            .key_context(flightdeck::app::keymap::Context::App.name())
            .track_focus(&self.app_focus)
            .on_key_down(move |event, _, cx| {
                if let Some(entry) = app_key_down(keymap(), event) {
                    perform_remote_entry(entry, &app_remote, cx);
                    cx.stop_propagation();
                }
            });
        let main = v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .bg(p.surface_terminal.hsla())
            .child(git_strip(&strip, &surface, &p))
            .child(self.main_area(&p, cx));
        let titlebar = self.titlebar(&p, cx).into_any_element();
        let status = self.status_bar(&p, cx).into_any_element();

        v_flex()
            .key_context(flightdeck::app::keymap::Context::Global.name())
            .on_action(cx.listener(Self::on_keymap_action))
            .relative()
            .size_full()
            .bg(p.surface_window.hsla())
            .text_color(p.ink.hsla())
            .font_family(crate::fonts::UI_FAMILY)
            .child(titlebar)
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .items_start()
                    .child(side)
                    .child(main),
            )
            .child(status)
            .child(self.layer.clone())
    }
}

/// One clickable hint: its keycap and word.
fn hint(p: &Palette, entry: &'static str, label: &'static str) -> impl IntoElement {
    h_flex()
        .flex_none()
        .gap_1()
        .child(
            div()
                .font_family(MONO_FAMILY)
                .text_color(p.ink_2.hsla())
                .child(keycap(entry).unwrap_or_default()),
        )
        .child(label)
}

fn notice_card(p: &Palette, title: &str, detail: &str) -> impl IntoElement {
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
                .child(title.to_string()),
        )
        .when(!detail.is_empty(), |d| {
            d.child(
                div()
                    .text_size(px(12.))
                    .text_color(p.muted.hsla())
                    .child(detail.to_string()),
            )
        })
}

/// The link ended for good: say why, and offer to close the window.
fn ended(p: &Palette, end: &LinkEnd, remote: &Entity<RemoteModel>) -> impl IntoElement {
    let (title, detail) = match end {
        LinkEnd::Revoked => (
            "The host withdrew this app's access.",
            "Pair again with a new code from the host's web access overlay.",
        ),
        LinkEnd::Unauthorized => (
            "The host does not know this app any more.",
            "Pair again with a new code from the host's web access overlay.",
        ),
        LinkEnd::HostQuit => (
            "The host quit FlightDeck.",
            "Its agents stopped with it. Reconnect once it is running again.",
        ),
        LinkEnd::ServerStopped => (
            "The host stopped its web interface.",
            "FlightDeck is still running there; start the web interface again to reconnect.",
        ),
        LinkEnd::VersionMismatch(_) => (
            "The host runs a different FlightDeck version.",
            "Update one side so both speak the same web protocol.",
        ),
        LinkEnd::Stopped => ("Disconnected.", ""),
    };
    let pair_again = matches!(end, LinkEnd::Revoked | LinkEnd::Unauthorized).then(|| {
        let remote = remote.clone();
        div()
            .id("remote-pair-again")
            .debug_selector(|| "remote-pair-again".into())
            .px_3()
            .py_1()
            .rounded(px(6.))
            .bg(p.button_primary_bg.hsla())
            .text_color(p.button_primary_ink.hsla())
            .cursor_pointer()
            .text_size(px(12.))
            .child("Pair again…")
            .on_click(move |_, _, cx| {
                let address = remote.read(cx).address().to_string();
                remote.update(cx, |model, cx| {
                    model.request_close();
                    cx.notify();
                });
                super::connect::open_connect_window(Some(address), cx);
            })
    });
    let remote = remote.clone();
    v_flex()
        .debug_selector(|| "remote-ended".into())
        .flex_1()
        .size_full()
        .items_center()
        .justify_center()
        .gap_2()
        .child(div().text_sm().text_color(p.ink.hsla()).child(title))
        .when(!detail.is_empty(), |d| {
            d.child(
                div()
                    .text_size(px(12.))
                    .text_color(p.muted.hsla())
                    .child(detail),
            )
        })
        .children(pair_again)
        .child(
            div()
                .id("remote-close-window")
                .debug_selector(|| "remote-close-window".into())
                .mt_2()
                .px_3()
                .py_1()
                .rounded(px(6.))
                .border_1()
                .border_color(p.border.hsla())
                .cursor_pointer()
                .hover(|s| s.bg(p.surface_raised.hsla()))
                .text_size(px(12.))
                .child("Close window")
                .on_click(move |_, _, cx| {
                    remote.update(cx, |model, cx| {
                        model.request_close();
                        cx.notify();
                    })
                }),
        )
}
