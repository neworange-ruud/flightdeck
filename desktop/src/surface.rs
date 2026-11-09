//! Which surface a view's controls act on: the local workspace, or a
//! FlightDeck on another machine (`specs/DESKTOP_REMOTE_CONTROL_PLAN.md` §2.3).
//!
//! The leaf views (the sidebar, the git strip, Mission control) draw the
//! core's view structs, so what they *show* already does not care where it
//! came from. What they *do* — a row click, a button, a context-menu item —
//! goes through this handle, so the same element works in a local window and
//! a remote one. Local delegates to exactly the functions the views called
//! before; remote to [`RemoteModel`], which translates into wire commands and
//! keeps browsing (the selection, the main view, Mission control's scope) its
//! own.

use flightdeck::app::keymap::KeymapEntry;
use flightdeck::app::modes::InputMode;
use flightdeck::host::HostEvent;
use flightdeck::persistence::workspace::{MainView, MissionScope};
use flightdeck::terminal::session::Terminal;
use flightdeck::view::{SessionKey, TerminalRef};
use gpui::{App, Entity};

use crate::host::HostModel;
use crate::remote::RemoteModel;

/// A view's way back to the model it was drawn from.
#[derive(Clone)]
pub enum Surface {
    Local(Entity<HostModel>),
    Remote(Entity<RemoteModel>),
}

impl Surface {
    /// Select agent row `index` and enter APP mode.
    pub fn select_agent(&self, index: usize, cx: &mut App) {
        match self {
            Surface::Local(host) => crate::views::sidebar::select_agent(host, index, cx),
            Surface::Remote(remote) => remote.update(cx, |model, cx| {
                model.select_agent(index);
                cx.notify();
            }),
        }
    }

    /// Focus one of the selected agent's terminals.
    pub fn focus_terminal(&self, target: TerminalRef, cx: &mut App) {
        match self {
            Surface::Local(host) => crate::views::sidebar::focus_terminal(host, target, cx),
            Surface::Remote(remote) => remote.update(cx, |model, cx| {
                model.focus_terminal(target);
                cx.notify();
            }),
        }
    }

    /// Perform the keymap entry `id`, as its chord does.
    pub fn perform_id(&self, id: &str, cx: &mut App) {
        match self {
            Surface::Local(host) => crate::commands::perform_id(id, host, cx),
            Surface::Remote(remote) => crate::commands::perform_remote_id(id, remote, cx),
        }
    }

    /// Perform the keymap entry `entry`, as its chord does.
    pub fn perform_entry(&self, entry: &KeymapEntry, cx: &mut App) {
        match self {
            Surface::Local(host) => crate::commands::perform_entry(entry, host, cx),
            Surface::Remote(remote) => crate::commands::perform_remote_entry(entry, remote, cx),
        }
    }

    /// Hand an event to the model.
    pub fn dispatch(&self, event: HostEvent, cx: &mut App) {
        match self {
            Surface::Local(host) => host.update(cx, |model, cx| model.dispatch(event, cx)),
            Surface::Remote(remote) => remote.update(cx, |model, cx| model.dispatch(event, cx)),
        }
    }

    // -- Mission control ----------------------------------------------------

    /// Projects or Mission control.
    pub fn main_view(&self, cx: &App) -> MainView {
        match self {
            Surface::Local(host) => host.read(cx).host().workspace_ui().view,
            Surface::Remote(remote) => remote.read(cx).main_view(),
        }
    }

    /// Whether the terminal on screen has the keyboard.
    pub fn terminal_focused(&self, cx: &App) -> bool {
        match self {
            Surface::Local(host) => host.read(cx).host().terminal_focused(),
            Surface::Remote(remote) => remote.read(cx).terminal_focused(),
        }
    }

    /// Select Mission control's `key` and be in `mode` (APP: the grid,
    /// TERMINAL: the focus view). Locally that is the host's selection;
    /// remotely this window's own, which the host never sees (R2).
    pub fn select_session(
        &self,
        key: &SessionKey,
        tab_index: usize,
        mode: InputMode,
        cx: &mut App,
    ) {
        match self {
            Surface::Local(host) => {
                crate::views::mission::select_session(host, key, tab_index, mode, cx)
            }
            Surface::Remote(remote) => remote.update(cx, |model, cx| {
                model.select_mission_session(key, mode);
                cx.notify();
            }),
        }
    }

    /// Show `to`.
    pub fn switch_view(&self, to: MainView, cx: &mut App) {
        match self {
            Surface::Local(host) => crate::views::mission::switch_view(host, to, cx),
            Surface::Remote(remote) => remote.update(cx, |model, cx| {
                model.set_main_view(to);
                cx.notify();
            }),
        }
    }

    /// The other view (Alt-m).
    pub fn toggle_view(&self, cx: &mut App) {
        let to = self.main_view(cx).toggled();
        self.switch_view(to, cx);
    }

    /// Replace Mission control's scope.
    pub fn set_scope(&self, scope: MissionScope, cx: &mut App) {
        match self {
            Surface::Local(host) => crate::views::mission::set_scope(host, scope, cx),
            Surface::Remote(remote) => remote.update(cx, |model, cx| {
                model.set_mission_scope(scope);
                cx.notify();
            }),
        }
    }

    /// The terminal a Mission control tile for `key` draws, read-only.
    pub fn tile_terminal<'a>(&self, key: &SessionKey, cx: &'a App) -> Option<&'a Terminal> {
        match self {
            Surface::Local(host) => host.read(cx).host().tab_terminal(key.project, &key.tab_id),
            Surface::Remote(remote) => remote.read(cx).session_terminal(key),
        }
    }
}
