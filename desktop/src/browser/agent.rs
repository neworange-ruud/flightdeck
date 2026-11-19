//! Agents' browser tool calls in a window (`specs/SESSION_BROWSER_PRD.md`
//! §6, B17), as state: the per-session switch "Let the agent use the
//! browser", the calls waiting, which tab each acts on, and the answers to
//! send back. A remote window's model and the local window's both keep one.
//!
//! The window runs what [`AgentDesk::next`] hands it on the web views and
//! reports back through [`AgentDesk::finished`]. This file holds no web view,
//! so its rules are unit tests.
//!
//! - **The switch** is per session and off by default. Off, every call is
//!   refused with [`NOT_ALLOWED`]. Stop (the toolbar's, or the user taking
//!   the browser over by using it) turns it off and answers whatever the
//!   session's agent was doing with [`STOPPED`].
//! - **Which tab:** the one the call names, else the one the agent used
//!   last, else the one on screen, else the session's newest.
//! - **One call per tab at a time**, in the order they came; a tab whose web
//!   view is not built yet (a worker still granting its key) holds its calls.
//! - A call older than the bridge's own timeout is dropped unanswered:
//!   nobody waits for it any more.

// Linux has no session browser yet (B7): only the answering half is used.
#![cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::time::{Duration, Instant};

use flightdeck::browser_tools::{BrowserReply, Tool, NOT_ALLOWED, STOPPED};
use flightdeck::contracts::TabId;

/// How long a call may wait for its tab: the bridge's own timeout.
const STALE: Duration = flightdeck::browser_tools::bridge::CALL_TIMEOUT;

/// What the window knows about one browser tab.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabInfo {
    pub id: u64,
    pub session: TabId,
    /// Its web view exists, so it can act.
    pub ready: bool,
    /// Why it never will: the worker refused its key.
    pub refusal: Option<String>,
    pub title: String,
    pub url: String,
}

/// Something for the window to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// Open a browser tab in `session` (not shown) and tell
    /// [`AgentDesk::opened`] its id.
    Open { call: u64, session: TabId },
    /// Run `tool` on tab `tab`, then report through [`AgentDesk::finished`].
    Run { call: u64, tab: u64, tool: Tool },
}

#[derive(Clone, Debug)]
struct Queued {
    call: u64,
    session: TabId,
    tool: Tool,
    /// The tab it acts on, once bound (a `browser_open`'s new tab).
    bound: Option<u64>,
    since: Instant,
}

/// Answers views report, shared with them.
pub type Finished = Rc<RefCell<Vec<(u64, BrowserReply)>>>;

/// See the module docs.
#[derive(Default)]
pub struct AgentDesk {
    allowed: HashSet<TabId>,
    queue: VecDeque<Queued>,
    /// Calls a view is running: their session and tab.
    running: HashMap<u64, (TabId, u64)>,
    /// The tab each session's agent used last.
    last_tab: HashMap<TabId, u64>,
    finished: Finished,
    answers: Vec<(u64, BrowserReply)>,
}

impl AgentDesk {
    /// Whether `session`'s agent may use its browsers.
    pub fn allowed(&self, session: &TabId) -> bool {
        self.allowed.contains(session)
    }

    /// Turn the switch for `session` on or off. Off answers what its agent
    /// was doing with [`STOPPED`].
    pub fn set_allowed(&mut self, session: &TabId, allowed: bool) {
        if allowed {
            self.allowed.insert(session.clone());
        } else if self.allowed.remove(session) {
            self.stop(session);
        }
    }

    /// Stop `session`'s agent: the switch goes off and its calls are
    /// answered with [`STOPPED`].
    pub fn stop(&mut self, session: &TabId) {
        self.allowed.remove(session);
        let mut kept = VecDeque::new();
        for q in self.queue.drain(..) {
            if &q.session == session {
                self.answers.push((q.call, BrowserReply::error(STOPPED)));
            } else {
                kept.push_back(q);
            }
        }
        self.queue = kept;
        let stopped: Vec<u64> = self
            .running
            .iter()
            .filter(|(_, (s, _))| s == session)
            .map(|(call, _)| *call)
            .collect();
        for call in stopped {
            self.running.remove(&call);
            self.answers.push((call, BrowserReply::error(STOPPED)));
        }
    }

    /// An agent's call arrived for `session`.
    pub fn arrived(&mut self, call: u64, session: TabId, tool: &str, args: &serde_json::Value) {
        match Tool::parse(tool, args) {
            Ok(tool) => self.queue.push_back(Queued {
                call,
                session,
                tool,
                bound: None,
                since: Instant::now(),
            }),
            Err(why) => self.answers.push((call, BrowserReply::error(why))),
        }
    }

    /// A call for a session this window does not have.
    pub fn unknown_session(&mut self, call: u64) {
        self.answers.push((
            call,
            BrowserReply::error(
                "This session is not open in the FlightDeck window, so it has no browser.",
            ),
        ));
    }

    /// What the window should do now, given its tabs and the one on screen.
    /// Calls that can be answered from the tab list are answered here.
    pub fn next(&mut self, tabs: &[TabInfo], shown: Option<u64>) -> Vec<Step> {
        let mut steps = Vec::new();
        let mut busy: HashSet<u64> = self.running.values().map(|(_, tab)| *tab).collect();
        let mut kept = VecDeque::new();
        let queue: Vec<Queued> = self.queue.drain(..).collect();
        for q in queue {
            if q.since.elapsed() > STALE {
                continue;
            }
            if !self.allowed.contains(&q.session) {
                self.answers
                    .push((q.call, BrowserReply::error(NOT_ALLOWED)));
                continue;
            }
            match &q.tool {
                Tool::Tabs => {
                    self.answers
                        .push((q.call, BrowserReply::text(list(tabs, &q.session, shown))));
                    continue;
                }
                Tool::Open { .. } if q.bound.is_none() => {
                    steps.push(Step::Open {
                        call: q.call,
                        session: q.session.clone(),
                    });
                    // Waits for its tab, bound by `opened`.
                    kept.push_back(Queued {
                        bound: Some(0),
                        ..q
                    });
                    continue;
                }
                _ => {}
            }
            let tab = match q.bound {
                Some(0) => {
                    // An open whose tab is not there yet.
                    kept.push_back(q);
                    continue;
                }
                Some(tab) => Ok(tab),
                None => self.pick(&q, tabs, shown),
            };
            let tab = match tab {
                Ok(tab) => tab,
                Err(why) => {
                    self.answers.push((q.call, BrowserReply::error(why)));
                    continue;
                }
            };
            let Some(info) = tabs.iter().find(|t| t.id == tab) else {
                self.answers
                    .push((q.call, BrowserReply::error("That browser tab was closed.")));
                continue;
            };
            if let Some(why) = &info.refusal {
                self.answers
                    .push((q.call, BrowserReply::error(why.clone())));
                continue;
            }
            if !info.ready || busy.contains(&tab) {
                kept.push_back(q);
                continue;
            }
            busy.insert(tab);
            self.last_tab.insert(q.session.clone(), tab);
            self.running.insert(q.call, (q.session.clone(), tab));
            // An open runs as a navigation on its new tab.
            let tool = match q.tool {
                Tool::Open { url } => Tool::Navigate {
                    url,
                    tab: Some(tab),
                },
                tool => tool,
            };
            steps.push(Step::Run {
                call: q.call,
                tab,
                tool,
            });
        }
        self.queue = kept;
        steps
    }

    /// The tab a call acts on, or why there is none.
    fn pick(&self, q: &Queued, tabs: &[TabInfo], shown: Option<u64>) -> Result<u64, String> {
        let mine = |id: u64| tabs.iter().any(|t| t.id == id && t.session == q.session);
        if let Some(tab) = q.tool.tab() {
            return if mine(tab) {
                Ok(tab)
            } else {
                Err(format!(
                    "Tab {tab} is not one of this session's browser tabs; see browser_tabs."
                ))
            };
        }
        if let Some(tab) = self.last_tab.get(&q.session).copied().filter(|t| mine(*t)) {
            return Ok(tab);
        }
        if let Some(tab) = shown.filter(|t| mine(*t)) {
            return Ok(tab);
        }
        tabs.iter()
            .rev()
            .find(|t| t.session == q.session)
            .map(|t| t.id)
            .ok_or_else(|| {
                "There is no browser open in this session. Open one with browser_open.".to_string()
            })
    }

    /// The window opened tab `tab` for the `browser_open` `call`, or could
    /// not (`None`, with why).
    pub fn opened(&mut self, call: u64, tab: Result<u64, String>) {
        let Some(at) = self.queue.iter().position(|q| q.call == call) else {
            return;
        };
        match tab {
            Ok(tab) => self.queue[at].bound = Some(tab),
            Err(why) => {
                self.queue.remove(at);
                self.answers.push((call, BrowserReply::error(why)));
            }
        }
    }

    /// Where views report their answers.
    pub fn finished(&self) -> Finished {
        Rc::clone(&self.finished)
    }

    /// Whether tab `tab` is running an agent's call.
    #[cfg(test)]
    pub fn driving(&self, tab: u64) -> bool {
        self.running.values().any(|(_, t)| *t == tab)
    }

    /// The answers to send, once: what views finished (unless it was
    /// stopped meanwhile), what was refused, and the calls whose tab is
    /// gone (`live` lists the tabs there are).
    pub fn take_answers(&mut self, live: &[u64]) -> Vec<(u64, BrowserReply)> {
        let done: Vec<(u64, BrowserReply)> = self.finished.borrow_mut().drain(..).collect();
        for (call, reply) in done {
            if self.running.remove(&call).is_some() {
                self.answers.push((call, reply));
            }
        }
        let orphans: Vec<u64> = self
            .running
            .iter()
            .filter(|(_, (_, tab))| !live.contains(tab))
            .map(|(call, _)| *call)
            .collect();
        for call in orphans {
            self.running.remove(&call);
            self.answers.push((
                call,
                BrowserReply::error("The user closed the browser tab before it answered."),
            ));
        }
        std::mem::take(&mut self.answers)
    }
}

/// `browser_tabs`' answer.
fn list(tabs: &[TabInfo], session: &TabId, shown: Option<u64>) -> String {
    let mine: Vec<&TabInfo> = tabs.iter().filter(|t| &t.session == session).collect();
    if mine.is_empty() {
        return "No browser tabs are open in this session. Open one with browser_open.".to_string();
    }
    let mut out = String::from("This session's browser tabs:\n");
    for t in mine {
        let title = if t.title.trim().is_empty() {
            "(no title)"
        } else {
            t.title.trim()
        };
        let url = if t.url.is_empty() { "-" } else { &t.url };
        out.push_str(&format!(
            "- tab {}: {title} — {url}{}\n",
            t.id,
            if shown == Some(t.id) {
                " (on screen)"
            } else {
                ""
            }
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn s(id: &str) -> TabId {
        TabId(id.to_string())
    }

    fn tab(id: u64, session: &str) -> TabInfo {
        TabInfo {
            id,
            session: s(session),
            ready: true,
            refusal: None,
            title: format!("Page {id}"),
            url: format!("http://localhost:300{id}/"),
        }
    }

    fn texts(answers: &[(u64, BrowserReply)]) -> Vec<(u64, bool, String)> {
        answers
            .iter()
            .map(|(c, r)| (*c, r.is_error, r.joined_text()))
            .collect()
    }

    #[test]
    fn with_the_switch_off_every_call_is_refused() {
        let mut desk = AgentDesk::default();
        desk.arrived(1, s("a"), "browser_snapshot", &json!({}));
        assert!(desk.next(&[tab(1, "a")], None).is_empty());
        let answers = desk.take_answers(&[1]);
        assert_eq!(answers.len(), 1);
        assert!(answers[0].1.is_error);
        assert_eq!(answers[0].1.joined_text(), NOT_ALLOWED);
    }

    #[test]
    fn a_call_acts_on_the_named_tab_else_the_last_used_else_the_shown_else_the_newest() {
        let mut desk = AgentDesk::default();
        desk.set_allowed(&s("a"), true);
        let tabs = [tab(1, "a"), tab(2, "a"), tab(3, "b")];
        // Nothing used yet, nothing shown: the newest of the session.
        desk.arrived(1, s("a"), "browser_snapshot", &json!({}));
        assert_eq!(
            desk.next(&tabs, None),
            vec![Step::Run {
                call: 1,
                tab: 2,
                tool: Tool::Snapshot { tab: None }
            }]
        );
        desk.finished()
            .borrow_mut()
            .push((1, BrowserReply::text("ok")));
        desk.take_answers(&[1, 2, 3]);
        // The last used wins over the one on screen.
        desk.arrived(2, s("a"), "browser_reload", &json!({}));
        assert!(matches!(
            desk.next(&tabs, Some(1))[0],
            Step::Run { tab: 2, .. }
        ));
        desk.finished()
            .borrow_mut()
            .push((2, BrowserReply::text("ok")));
        desk.take_answers(&[1, 2, 3]);
        // A named tab wins over both; another session's is refused.
        desk.arrived(3, s("a"), "browser_back", &json!({"tab": 1}));
        assert!(matches!(
            desk.next(&tabs, None)[0],
            Step::Run { tab: 1, .. }
        ));
        desk.arrived(4, s("a"), "browser_back", &json!({"tab": 3}));
        assert!(desk.next(&tabs, None).is_empty());
        let answers = texts(&desk.take_answers(&[1, 2, 3]));
        assert!(answers
            .iter()
            .any(|(c, e, t)| *c == 4 && *e && t.contains("not one of")));
    }

    #[test]
    fn the_shown_tab_is_used_before_the_newest_when_none_was_used() {
        let mut desk = AgentDesk::default();
        desk.set_allowed(&s("a"), true);
        desk.arrived(1, s("a"), "browser_snapshot", &json!({}));
        assert!(matches!(
            desk.next(&[tab(1, "a"), tab(2, "a")], Some(1))[0],
            Step::Run { tab: 1, .. }
        ));
    }

    #[test]
    fn calls_on_one_tab_run_one_at_a_time_in_order() {
        let mut desk = AgentDesk::default();
        desk.set_allowed(&s("a"), true);
        let tabs = [tab(1, "a")];
        desk.arrived(1, s("a"), "browser_click", &json!({"ref": "e1"}));
        desk.arrived(2, s("a"), "browser_snapshot", &json!({}));
        let steps = desk.next(&tabs, None);
        assert_eq!(steps.len(), 1);
        assert!(matches!(steps[0], Step::Run { call: 1, .. }));
        assert!(desk.driving(1));
        assert!(desk.next(&tabs, None).is_empty(), "the tab is busy");
        desk.finished()
            .borrow_mut()
            .push((1, BrowserReply::text("clicked")));
        assert_eq!(
            texts(&desk.take_answers(&[1])),
            vec![(1, false, "clicked".to_string())]
        );
        assert!(matches!(
            desk.next(&tabs, None)[0],
            Step::Run { call: 2, .. }
        ));
    }

    #[test]
    fn a_tab_without_its_web_view_holds_its_calls_and_a_refused_one_answers_why() {
        let mut desk = AgentDesk::default();
        desk.set_allowed(&s("a"), true);
        let mut t = tab(1, "a");
        t.ready = false;
        desk.arrived(1, s("a"), "browser_snapshot", &json!({}));
        assert!(desk.next(&[t.clone()], None).is_empty());
        assert!(desk.take_answers(&[1]).is_empty());
        t.refusal = Some("The worker did not allow a browser.".to_string());
        assert!(desk.next(&[t], None).is_empty());
        assert_eq!(
            texts(&desk.take_answers(&[1])),
            vec![(1, true, "The worker did not allow a browser.".to_string())]
        );
    }

    #[test]
    fn open_asks_for_a_tab_then_navigates_in_it() {
        let mut desk = AgentDesk::default();
        desk.set_allowed(&s("a"), true);
        desk.arrived(7, s("a"), "browser_open", &json!({"url": "localhost:3000"}));
        assert_eq!(
            desk.next(&[], None),
            vec![Step::Open {
                call: 7,
                session: s("a")
            }]
        );
        // Not opened yet: nothing more.
        assert!(desk.next(&[], None).is_empty());
        desk.opened(7, Ok(4));
        let mut t = tab(4, "a");
        t.ready = false;
        assert!(
            desk.next(&[t.clone()], None).is_empty(),
            "waits for the view"
        );
        t.ready = true;
        assert_eq!(
            desk.next(&[t], None),
            vec![Step::Run {
                call: 7,
                tab: 4,
                tool: Tool::Navigate {
                    url: "localhost:3000".to_string(),
                    tab: Some(4)
                }
            }]
        );
        // A failed open answers why.
        desk.arrived(8, s("a"), "browser_open", &json!({"url": "x"}));
        desk.next(&[], None);
        desk.opened(8, Err("Not connected".to_string()));
        assert!(texts(&desk.take_answers(&[4]))
            .iter()
            .any(|(c, e, t)| *c == 8 && *e && t == "Not connected"));
    }

    #[test]
    fn tabs_lists_the_sessions_own_tabs() {
        let mut desk = AgentDesk::default();
        desk.set_allowed(&s("a"), true);
        desk.arrived(1, s("a"), "browser_tabs", &json!({}));
        assert!(desk.next(&[tab(1, "a"), tab(2, "b")], Some(1)).is_empty());
        let text = desk.take_answers(&[]).remove(0).1.joined_text();
        assert!(text.contains("tab 1: Page 1 — http://localhost:3001/ (on screen)"));
        assert!(!text.contains("tab 2"));
    }

    #[test]
    fn stop_turns_the_switch_off_and_answers_what_was_running_and_waiting() {
        let mut desk = AgentDesk::default();
        desk.set_allowed(&s("a"), true);
        desk.set_allowed(&s("b"), true);
        let tabs = [tab(1, "a"), tab(2, "b")];
        desk.arrived(1, s("a"), "browser_snapshot", &json!({}));
        desk.arrived(2, s("a"), "browser_snapshot", &json!({}));
        desk.arrived(3, s("b"), "browser_snapshot", &json!({}));
        assert_eq!(desk.next(&tabs, None).len(), 2);
        desk.stop(&s("a"));
        assert!(!desk.allowed(&s("a")));
        assert!(desk.allowed(&s("b")));
        // The view finishing afterwards changes nothing.
        desk.finished()
            .borrow_mut()
            .push((1, BrowserReply::text("late")));
        let answers = texts(&desk.take_answers(&[1, 2]));
        assert_eq!(answers.len(), 2);
        assert!(answers
            .iter()
            .all(|(c, e, t)| [1, 2].contains(c) && *e && t == STOPPED));
        assert!(desk.driving(2), "the other session's call runs on");
    }

    #[test]
    fn a_closed_tab_answers_its_running_call() {
        let mut desk = AgentDesk::default();
        desk.set_allowed(&s("a"), true);
        desk.arrived(1, s("a"), "browser_snapshot", &json!({}));
        desk.next(&[tab(1, "a")], None);
        let answers = texts(&desk.take_answers(&[]));
        assert_eq!(answers.len(), 1);
        assert!(answers[0].1 && answers[0].2.contains("closed"));
    }

    #[test]
    fn a_malformed_call_is_answered_at_once() {
        let mut desk = AgentDesk::default();
        desk.arrived(1, s("a"), "browser_click", &json!({}));
        assert!(desk.take_answers(&[])[0].1.is_error);
        desk.unknown_session(2);
        assert!(desk.take_answers(&[])[0].1.is_error);
    }
}
