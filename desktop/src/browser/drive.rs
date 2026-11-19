//! A window's share of the agent browser tools (`specs/SESSION_BROWSER_PRD.md`
//! §6): each frame, hand the user's Agent switch and Stop from the views to
//! the model's [`AgentDesk`], tell each view whether its session's agent may
//! drive it, and run what the desk says on the views. The local window and a
//! remote window do the same through [`AgentModel`].

use flightdeck::contracts::TabId;
use gpui::{App, Entity};

use super::agent::{AgentDesk, Step, TabInfo};
use super::tabs::Browsers;
use super::view::{AgentSignal, BrowserView};

/// What a window's model offers the agent tools.
pub trait AgentModel: 'static {
    /// Every browser tab.
    fn agent_browsers(&self) -> &Browsers;
    /// The model's desk.
    fn desk(&mut self) -> &mut AgentDesk;
    fn desk_ref(&self) -> &AgentDesk;
    /// Open a browser tab in `session` for its agent, not shown.
    fn open_agent_browser(&mut self, session: &TabId) -> Result<u64, String>;
    /// The browser on screen.
    fn shown_browser(&mut self) -> Option<u64>;
}

/// See the module docs. `views` are the window's built web views, by tab.
/// True when a tab was opened, whose view the next frame builds.
pub fn drive<M: AgentModel>(
    model: &Entity<M>,
    views: &[(u64, Entity<BrowserView>)],
    cx: &mut App,
) -> bool {
    // The user's switch and Stop, by the tab's session.
    let mut signals = Vec::new();
    for (id, view) in views {
        for signal in view.update(cx, |view, _| view.take_signals()) {
            signals.push((*id, signal));
        }
    }
    let urls: Vec<(u64, String)> = views
        .iter()
        .map(|(id, view)| (*id, view.read(cx).url()))
        .collect();
    let (steps, opened, allowed, finished) = model.update(cx, |model, _| {
        for (id, signal) in signals {
            let Some(session) = model.agent_browsers().tab(id).map(|t| t.session.clone()) else {
                continue;
            };
            match signal {
                AgentSignal::SetAllowed(on) => model.desk().set_allowed(&session, on),
                AgentSignal::Stop => model.desk().stop(&session),
            }
        }
        let shown = model.shown_browser();
        let tabs: Vec<TabInfo> = model
            .agent_browsers()
            .all()
            .iter()
            .map(|t| TabInfo {
                id: t.id,
                session: t.session.clone(),
                ready: urls.iter().any(|(id, _)| *id == t.id),
                refusal: t.refusal.clone(),
                title: t.title.clone(),
                url: urls
                    .iter()
                    .find(|(id, _)| *id == t.id)
                    .map(|(_, url)| url.clone())
                    .unwrap_or_default(),
            })
            .collect();
        let mut runs = Vec::new();
        let mut opened = false;
        for step in model.desk().next(&tabs, shown) {
            match step {
                Step::Open { call, session } => {
                    let tab = model.open_agent_browser(&session);
                    opened |= tab.is_ok();
                    model.desk().opened(call, tab);
                }
                Step::Run { call, tab, tool } => runs.push((call, tab, tool)),
            }
        }
        let allowed: Vec<(u64, bool)> = model
            .agent_browsers()
            .all()
            .iter()
            .map(|t| (t.id, model.desk_ref().allowed(&t.session)))
            .collect();
        (runs, opened, allowed, model.desk_ref().finished())
    });
    for (id, view) in views {
        let on = allowed.iter().any(|(tab, on)| tab == id && *on);
        view.update(cx, |view, cx| view.set_agent_allowed(on, cx));
    }
    for (call, tab, tool) in steps {
        if let Some((_, view)) = views.iter().find(|(id, _)| *id == tab) {
            let finished = finished.clone();
            view.update(cx, |view, cx| view.agent_run(call, tool, finished, cx));
        }
    }
    opened
}
