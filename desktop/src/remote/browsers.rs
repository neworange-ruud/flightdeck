//! A remote window's session browsers, as state (`specs/SESSION_BROWSER_PRD.md`
//! B1, B6): which browser tabs exist, which session each belongs to, its
//! tunnel key once the worker granted one, and which one is on screen.
//!
//! The web views themselves are the window's ([`crate::browser`]), built from
//! this list and dropped with it, so they never outlive the window (B6, B18:
//! nothing is restored after a restart). This file holds no web view and no
//! socket, so its rules are unit tests.

use flightdeck::contracts::TabId;
use flightdeck::web::protocol::TerminalId;

/// One browser tab.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BrowserTab {
    /// Stable for the window's life; never reused.
    pub id: u64,
    /// The session it belongs to.
    pub session: TabId,
    /// The `open_browser_tunnel` this tab is waiting on.
    pub seq: Option<u64>,
    /// The worker's key, once granted.
    pub key: Option<String>,
    /// Why the worker granted none.
    pub refusal: Option<String>,
    /// The page's title, as the web view last reported it.
    pub title: String,
}

/// Which browser is on screen, and what the window was looking at when it
/// was chosen: once the window looks at anything else (another terminal,
/// session, project or tab), the browser leaves the screen.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Shown {
    id: u64,
    terminal: Option<TerminalId>,
}

/// See the module docs.
#[derive(Default)]
pub struct Browsers {
    tabs: Vec<BrowserTab>,
    next: u64,
    shown: Option<Shown>,
}

impl Browsers {
    /// A new tab for `session`, waiting on `seq` (`None`: the request could
    /// not be sent), shown at once over `terminal`, the terminal on screen.
    pub fn open(&mut self, session: TabId, seq: Option<u64>, terminal: Option<TerminalId>) -> u64 {
        self.next += 1;
        let id = self.next;
        self.tabs.push(BrowserTab {
            id,
            session,
            seq,
            key: None,
            refusal: seq.is_none().then(|| NOT_SENT.to_string()),
            title: String::new(),
        });
        self.shown = Some(Shown { id, terminal });
        id
    }

    /// The tabs waiting on an answer, as `(id, seq)`.
    pub fn waiting(&self) -> Vec<(u64, u64)> {
        self.tabs
            .iter()
            .filter_map(|t| t.seq.map(|seq| (t.id, seq)))
            .collect()
    }

    /// The worker answered tab `id`: a key, or why not.
    pub fn answered(&mut self, id: u64, key: Option<String>, refusal: Option<String>) {
        if let Some(tab) = self.tab_mut(id) {
            tab.seq = None;
            tab.refusal = match (&key, refusal) {
                (Some(_), _) => None,
                (None, Some(why)) => Some(why),
                (None, None) => Some(NOT_GRANTED.to_string()),
            };
            tab.key = key;
        }
    }

    /// Tab `id` asks for a key again, as `seq` (a reconnect ended its old
    /// one). The tab keeps its old key until the new one arrives, so the
    /// window has one to hand its relay.
    pub fn rekey(&mut self, id: u64, seq: Option<u64>) {
        if let Some(tab) = self.tab_mut(id) {
            tab.seq = seq;
            if seq.is_none() {
                tab.refusal = Some(NOT_SENT.to_string());
            }
        }
    }

    /// Forget tab `id`, returning its key so the worker can be told.
    pub fn close(&mut self, id: u64) -> Option<String> {
        let at = self.tabs.iter().position(|t| t.id == id)?;
        let tab = self.tabs.remove(at);
        if self.shown.as_ref().is_some_and(|s| s.id == id) {
            self.shown = None;
        }
        tab.key
    }

    /// Put tab `id` on screen over `terminal`.
    pub fn show(&mut self, id: u64, terminal: Option<TerminalId>) {
        if self.tabs.iter().any(|t| t.id == id) {
            self.shown = Some(Shown { id, terminal });
        }
    }

    /// Take the browser off screen (the window looks at something else).
    pub fn hide(&mut self) {
        self.shown = None;
    }

    /// The tab on screen, while the window still looks at `terminal` in
    /// `session`; otherwise none (and it stays off).
    pub fn on_screen(
        &mut self,
        session: Option<&TabId>,
        terminal: Option<&TerminalId>,
    ) -> Option<u64> {
        let shown = self.shown.as_ref()?;
        let tab = self.tabs.iter().find(|t| t.id == shown.id)?;
        if Some(&tab.session) == session && shown.terminal.as_ref() == terminal {
            return Some(tab.id);
        }
        self.shown = None;
        None
    }

    /// The id of the tab on screen, without checking what the window looks
    /// at.
    #[cfg(test)]
    pub fn shown_id(&self) -> Option<u64> {
        self.shown.as_ref().map(|s| s.id)
    }

    /// Every tab of `session`, in the order opened.
    pub fn of_session<'a>(&'a self, session: &'a TabId) -> impl Iterator<Item = &'a BrowserTab> {
        self.tabs.iter().filter(move |t| &t.session == session)
    }

    /// Every tab.
    pub fn all(&self) -> &[BrowserTab] {
        &self.tabs
    }

    /// Tab `id`.
    pub fn tab(&self, id: u64) -> Option<&BrowserTab> {
        self.tabs.iter().find(|t| t.id == id)
    }

    /// Record tab `id`'s page title.
    #[cfg(any(test, target_os = "macos"))]
    pub fn set_title(&mut self, id: u64, title: String) -> bool {
        match self.tab_mut(id) {
            Some(tab) if tab.title != title => {
                tab.title = title;
                true
            }
            _ => false,
        }
    }

    /// Forget the tabs of sessions that no longer exist, returning their
    /// keys.
    pub fn retain_sessions(&mut self, live: &[TabId]) -> Vec<String> {
        let gone: Vec<u64> = self
            .tabs
            .iter()
            .filter(|t| !live.contains(&t.session))
            .map(|t| t.id)
            .collect();
        gone.into_iter().filter_map(|id| self.close(id)).collect()
    }

    fn tab_mut(&mut self, id: u64) -> Option<&mut BrowserTab> {
        self.tabs.iter_mut().find(|t| t.id == id)
    }
}

/// Said when the request for a key could not be sent.
pub const NOT_SENT: &str = "Not connected to the worker, so the browser could not be opened.";

/// Said when the worker answered with neither a key nor a reason.
pub const NOT_GRANTED: &str = "The worker did not allow a browser.";

/// A tab's sidebar label: its page title, else "browser".
pub fn label(tab: &BrowserTab) -> String {
    let title = tab.title.trim();
    if title.is_empty() {
        "browser".to_string()
    } else {
        title.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: &str) -> TabId {
        TabId(id.to_string())
    }

    fn term(id: &str) -> Option<TerminalId> {
        Some(TerminalId::new(id))
    }

    #[test]
    fn a_tab_waits_for_its_key_and_is_shown_at_once() {
        let mut b = Browsers::default();
        let id = b.open(session("s1"), Some(7), term("s1:primary"));
        assert_eq!(b.waiting(), vec![(id, 7)]);
        assert_eq!(
            b.on_screen(Some(&session("s1")), term("s1:primary").as_ref()),
            Some(id)
        );
        b.answered(id, Some("k".into()), None);
        assert!(b.waiting().is_empty());
        assert_eq!(b.tab(id).unwrap().key.as_deref(), Some("k"));
        assert_eq!(b.tab(id).unwrap().refusal, None);
    }

    #[test]
    fn a_refusal_or_an_unsent_request_is_kept_in_words() {
        let mut b = Browsers::default();
        let a = b.open(session("s1"), Some(1), None);
        b.answered(a, None, Some("Too many browsers.".into()));
        assert_eq!(
            b.tab(a).unwrap().refusal.as_deref(),
            Some("Too many browsers.")
        );
        let c = b.open(session("s1"), Some(2), None);
        b.answered(c, None, None);
        assert_eq!(b.tab(c).unwrap().refusal.as_deref(), Some(NOT_GRANTED));
        let d = b.open(session("s1"), None, None);
        assert_eq!(b.tab(d).unwrap().refusal.as_deref(), Some(NOT_SENT));
        assert!(b.waiting().iter().all(|(id, _)| *id != d));
    }

    #[test]
    fn looking_at_anything_else_takes_the_browser_off_screen_for_good() {
        let mut b = Browsers::default();
        let id = b.open(session("s1"), Some(1), term("s1:primary"));
        // Another terminal of the same session.
        assert_eq!(
            b.on_screen(Some(&session("s1")), term("s1:child:1").as_ref()),
            None
        );
        // Coming back does not bring it back: the person chose the terminal.
        assert_eq!(
            b.on_screen(Some(&session("s1")), term("s1:primary").as_ref()),
            None
        );
        b.show(id, term("s1:primary"));
        assert_eq!(
            b.on_screen(Some(&session("s2")), term("s1:primary").as_ref()),
            None,
            "another session"
        );
        b.show(id, term("s1:primary"));
        b.hide();
        assert_eq!(b.shown_id(), None);
    }

    #[test]
    fn closing_returns_the_key_and_clears_the_screen() {
        let mut b = Browsers::default();
        let id = b.open(session("s1"), Some(1), None);
        b.answered(id, Some("k".into()), None);
        assert_eq!(b.close(id), Some("k".to_string()));
        assert_eq!(b.shown_id(), None);
        assert!(b.all().is_empty());
        assert_eq!(b.close(id), None);
    }

    #[test]
    fn tabs_of_a_closed_session_go_and_give_back_their_keys() {
        let mut b = Browsers::default();
        let a = b.open(session("s1"), Some(1), None);
        let c = b.open(session("s2"), Some(2), None);
        b.answered(a, Some("ka".into()), None);
        b.answered(c, Some("kc".into()), None);
        assert_eq!(b.retain_sessions(&[session("s2")]), vec!["ka".to_string()]);
        assert_eq!(b.all().len(), 1);
        assert_eq!(b.of_session(&session("s2")).count(), 1);
    }

    #[test]
    fn a_tab_is_labelled_by_its_title_or_as_a_browser() {
        let mut b = Browsers::default();
        let id = b.open(session("s1"), Some(1), None);
        assert_eq!(label(b.tab(id).unwrap()), "browser");
        assert!(b.set_title(id, "Vite App".into()));
        assert!(!b.set_title(id, "Vite App".into()), "unchanged");
        assert_eq!(label(b.tab(id).unwrap()), "Vite App");
    }
}
