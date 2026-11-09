//! What one frame of the TUI shows, as data the renderer draws
//! (`remote-control-ki05.1`).
//!
//! [`crate::tui::render`] used to read every fact straight off [`AppState`].
//! That tied the TUI's picture to a *local* host: a TUI driving a FlightDeck
//! on another machine (`flightdeck connect`, `specs/WORKER_PRD.md` §9) holds a
//! [`crate::web::client::RemoteWorkspace`], not an `AppState`. So the renderer
//! now draws a [`Screen`], and a [`Screen`] can be built from either:
//!
//! * [`Screen::from_state`] — the local TUI, from `AppState` and its git cache;
//! * the remote TUI, from the core's view structs, which
//!   [`crate::web::client::views`] already builds from the mirror for
//!   FlightDeck Desktop's remote window.
//!
//! Everything in it is either a core view struct ([`AgentRowView`],
//! [`GitStripView`]) or a terminal the frame draws, so the two TUIs show one
//! picture of an agent row rather than two that drift.

use crate::app::modes::InputMode;
use crate::app::state::{AppState, TabPhase};
use crate::contracts::UiConfig;
use crate::terminal::session::Terminal;
use crate::tui::render::{ChildTarget, GitStatusCache};
use crate::view::{AgentRowView, GitStripView, TerminalRef};

/// One frame's worth of facts for [`crate::tui::render::draw_screen`].
pub struct Screen<'a> {
    /// The UI settings: sidebar side, borders, dimming, animations, keys.
    pub ui: &'a UiConfig,
    /// Which pane receives keys.
    pub mode: InputMode,
    /// An `--isolated` run (SPECS §32): the status bar says so.
    pub isolated: bool,
    /// A newer release, for the status bar's update notice (SPECS §30).
    pub update_available: Option<&'a str>,
    /// Terminals side by side instead of the tab bar and one viewport.
    pub split_view: bool,
    /// The sidebar: one row per agent, in tab order.
    pub agents: Vec<AgentRowView>,
    /// The git info bar (and the compact status bar's base context).
    pub git: GitStripView,
    /// The selected agent's terminals, or `None` when no agent is selected.
    pub session: Option<SessionPane<'a>>,
}

/// The selected agent as the main pane shows it.
pub struct SessionPane<'a> {
    /// `Some(branch)` while its worktree is still being created: there is no
    /// terminal yet, so the viewport shows a progress line for that branch.
    pub creating: Option<String>,
    /// Its terminals, in tab-bar order: the primary agent first.
    pub terminals: Vec<TerminalPane<'a>>,
    /// The terminal on screen.
    pub active: ChildTarget,
}

/// One terminal tab of the selected agent.
pub struct TerminalPane<'a> {
    /// Which terminal of the agent this is.
    pub target: ChildTarget,
    /// Its tab-bar label ("agent", "agent 2", "shell 1").
    pub label: String,
    /// The terminal itself, or `None` while it is still starting.
    pub terminal: Option<&'a Terminal>,
}

impl<'a> Screen<'a> {
    /// The local TUI's frame: everything from `state`, git facts from `cache`.
    pub fn from_state(state: &'a AppState, cache: &GitStatusCache, now_ms: u64) -> Screen<'a> {
        // The TUI shows no elapsed-status text, so the unix seconds argument is
        // only an approximation.
        let agents = crate::view::agent_row_views(state, cache, now_ms, now_ms / 1000);
        let session = state.selected().map(|tab| SessionPane {
            creating: (tab.phase == TabPhase::Creating).then(|| tab.meta.branch.clone()),
            terminals: crate::view::terminal_views(tab)
                .into_iter()
                .map(|t| {
                    let (target, terminal) = match t.target {
                        TerminalRef::Primary => (ChildTarget::Primary, tab.session.primary()),
                        TerminalRef::Child(i) => (ChildTarget::Child(i), tab.session.child(i)),
                    };
                    TerminalPane {
                        target,
                        label: t.label,
                        terminal,
                    }
                })
                .collect(),
            active: match tab.session.selected_child() {
                Some(c) => ChildTarget::Child(c),
                None => ChildTarget::Primary,
            },
        });
        Screen {
            ui: &state.config.ui,
            mode: state.mode(),
            isolated: state.isolated,
            update_available: state.update_available.as_deref(),
            split_view: state.split_view,
            agents,
            git: crate::view::git_strip_view(state, cache),
            session,
        }
    }

    /// The terminal tabs as `(target, label)`, for the tab bar and its hit
    /// test. With no agent selected there is still one "agent" slot, so the
    /// bar's geometry does not depend on there being a selection.
    pub fn child_entries(&self) -> Vec<(ChildTarget, String)> {
        match &self.session {
            Some(session) => session
                .terminals
                .iter()
                .map(|t| (t.target, t.label.clone()))
                .collect(),
            None => vec![(ChildTarget::Primary, "agent".to_string())],
        }
    }

    /// The terminal on screen, if one is running.
    pub fn active_terminal(&self) -> Option<&'a Terminal> {
        let session = self.session.as_ref()?;
        session
            .terminals
            .iter()
            .find(|t| t.target == session.active)
            .and_then(|t| t.terminal)
    }
}
