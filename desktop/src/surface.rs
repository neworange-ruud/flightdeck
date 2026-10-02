//! Which surface a view's controls act on: the local workspace, or a
//! FlightDeck on another machine (`specs/DESKTOP_REMOTE_CONTROL_PLAN.md` §2.3).
//!
//! The leaf views (the sidebar, the git strip) draw the core's view structs,
//! so what they *show* already does not care where it came from. What they
//! *do* — a row click, a button, a context-menu item — goes through this
//! handle, so the same element works in a local window and a remote one.
//! Local delegates to exactly the functions the views called before; remote
//! to [`RemoteModel`], which translates into wire commands.

use flightdeck::host::HostEvent;
use flightdeck::view::TerminalRef;
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

    /// Hand an event to the model.
    pub fn dispatch(&self, event: HostEvent, cx: &mut App) {
        match self {
            Surface::Local(host) => host.update(cx, |model, cx| model.dispatch(event, cx)),
            Surface::Remote(remote) => remote.update(cx, |model, cx| model.dispatch(event, cx)),
        }
    }
}
