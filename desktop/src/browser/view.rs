//! One session browser (`specs/SESSION_BROWSER_PRD.md` B1–B24): our toolbar
//! over the system web view (WKWebView on macOS, WebView2 on Windows), and
//! where its requests go ([`Net`]).
//!
//! ```text
//! ┌──────────────────────────────────────────────────────────────────┐
//! │ ‹  ›  ⟳  [ localhost:3000                     ] Go  via studio  ⚙ │  GPUI
//! ├──────────────────────────────────────────────────────────────────┤
//! │                                                                  │
//! │   the web view, a native child placed over this area             │  wry
//! │                                                                  │
//! └──────────────────────────────────────────────────────────────────┘
//! ```
//!
//! - **The network is the worker's.** On macOS the web view's data store
//!   sends everything through the session's relay (SOCKS5 with a password,
//!   B23); loopback, which WKWebView never proxies, gets a forward first
//!   (B22): from the address field when typed (B24), from the navigation
//!   handler for links and redirects. On Windows every browser's proxy is the
//!   app's hub, loopback included, and the session answers its login (§4.1).
//!   A port in use here, `file://` (B12) and a page the worker could not
//!   reach are our own [`error_page`]s.
//! - **One profile per session** (B10): [`super::profile_id`] of the worker
//!   and the session, as the data store identifier (macOS) or the WebView2
//!   profile's name (Windows).
//! - **Placed every frame.** The web view is native and sits above GPUI, so
//!   the canvas under the toolbar places it each frame, and the window hides
//!   it whenever anything covers it or it is not on screen
//!   ([`BrowserView::set_shown`], B13). On macOS it lives in a container of
//!   its own (`Host`), so a docked DevTools stays inside the browser's area;
//!   WebView2's DevTools is a window of its own.
//! - **The keyboard is the page's only while you last clicked it** (the
//!   page, or a docked DevTools). A click anywhere else, hiding the browser
//!   or closing it gives it back to GPUI; on macOS the poll also takes it
//!   back from a page or DevTools that grabbed it on its own.
//! - **Downloads** go to this computer's Downloads folder, under a free
//!   name; `window.open` opens in this tab.
//! - **The agent may drive it** (§6) once the session's switch is on (the
//!   toolbar's Agent button): [`BrowserView::agent_run`] runs one tool
//!   call with the scripts in [`super::agent_js`]. While it does, the
//!   toolbar shows **Agent driving** with **Stop**; clicking or typing in
//!   the page, or using the toolbar, takes the browser back (the window
//!   reads [`AgentSignal`]s and stops the agent).

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use flightdeck::browser_tools::{BrowserReply, Tool};

use flightdeck::web::client::http::HttpAccess;
use flightdeck::web::client::relay::RelayEvent;
use gpui::{
    canvas, div, px, AppContext, AsyncApp, Bounds, Context, Entity, Focusable, InteractiveElement,
    IntoElement, ParentElement, Pixels, Render, StatefulInteractiveElement, Styled, Subscription,
    Task, WeakEntity, Window,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{h_flex, v_flex};
use wry::dpi::{LogicalPosition, LogicalSize};
use wry::{NewWindowResponse, PageLoadEvent, Rect, WebView, WebViewBuilder};

use super::address::{self, Address, FILE_REFUSED};
use super::agent::Finished;
use super::agent_js;
use super::error_page;
#[cfg(target_os = "macos")]
use super::host_macos::Host;
use super::net::{direct_net, worker_net, Net};
#[cfg(target_os = "macos")]
use super::tls_macos as tls;
#[cfg(target_os = "windows")]
use super::tls_windows as tls;
use crate::theme::Palette;

/// Windows has no container view (WebView2 places and hides itself, and its
/// DevTools is a window of its own): the field stays `None`.
#[cfg(not(target_os = "macos"))]
#[allow(dead_code)]
struct Host;

#[cfg(not(target_os = "macos"))]
#[allow(dead_code)]
impl Host {
    fn place(&self, _x: f64, _y: f64, _width: f64, _height: f64) {}
    fn set_hidden(&self, _hidden: bool) {}
    fn enforce(&self, _shown: bool) {}
    fn release_keyboard(&self) {}
}

/// How often the view looks at what the web view and the relay reported.
const POLL: Duration = Duration::from_millis(100);

/// How long **Agent driving** stays up after the agent's last call, so a
/// run of calls reads as one drive and the user's click right after one
/// still takes the browser back.
const DRIVING_LINGER: Duration = Duration::from_secs(4);

/// How long a navigation the agent asked for is waited for.
const LOAD_WAIT: Duration = Duration::from_secs(20);

/// What the user did that the window must act on (§6, B17).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentSignal {
    /// The toolbar's Agent button: let the session's agent use the browser,
    /// or not.
    SetAllowed(bool),
    /// Stop, or the user used the browser while the agent drove it.
    Stop,
}

/// What the web view's handlers report, read by the view's poll. The
/// handlers run on the main thread outside GPUI, so they only write here.
#[derive(Default)]
struct PageState {
    /// The page's address, as last started.
    url: String,
    /// The address last asked for (typed, a port, `window.open`) until a
    /// page starts. A page the worker refused never starts, so its refusal
    /// is matched against this to show our error page instead of nothing.
    requested: String,
    /// The page's title.
    title: String,
    /// Our own page to show instead: a title and its detail.
    blocked: Option<(String, String)>,
    /// `window.open` asked for these; they open in this tab.
    opened: Vec<String>,
    /// The certificate page's Continue anyway was chosen (B21).
    continue_requested: bool,
    /// The main frame's latest navigation (Windows, where wry's navigation
    /// handler sees only the main frame): a certificate failure there is
    /// the page's own.
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    navigating: String,
    /// Pages started and finished loading, counted, so the agent's tools can
    /// wait for a navigation.
    starts: u64,
    loads: u64,
    /// The page on screen is one of ours (an error page), not the web's.
    own_page: bool,
    /// The last error page shown: its title and detail, for the agent.
    last_error: Option<(String, String)>,
    /// The user clicked or typed in the page (a trusted event).
    user_input: bool,
    /// Something above changed.
    dirty: bool,
}

/// The web view, shared by the view and the canvas that places it.
type Shared = Rc<RefCell<Option<WebView>>>;

/// See the module docs.
pub struct BrowserView {
    webview: Shared,
    /// The native view the web view lives in; `None` until built, or if the
    /// web view could not be moved into it (then it is placed directly), and
    /// always on Windows.
    host: Option<Rc<Host>>,
    /// Where the web view's requests go.
    net: Arc<Net>,
    /// The worker's name, for the badge and the error pages; `None` for a
    /// local session's browser, which connects directly (B9).
    via: Option<String>,
    address: Entity<InputState>,
    /// The page's address, to put in the field on the next render (which has
    /// the window the field needs), unless someone is typing there.
    pending_address: Option<String>,
    /// Why the address field's last text was not loaded.
    note: Option<String>,
    page: Rc<RefCell<PageState>>,
    /// The last bounds the web view was placed at.
    placed: Rc<Cell<Option<Bounds<Pixels>>>>,
    shown: bool,
    /// Why the browser could not start, when it could not.
    failed: Option<String>,
    /// An address asked for before the web view was built (Windows builds
    /// it a moment later), loaded once it is.
    pending_load: Option<String>,
    /// The profile it browses in (B10), for what it trusts (B21).
    profile: [u8; 16],
    /// The certificate failure our page is showing, until Continue anyway.
    pending_cert: Option<super::cert::CertFailure>,
    /// The ports the session listens on (B11), each with whether this
    /// computer has it in use (a worker session's forward was refused).
    ports: Vec<(u16, bool)>,
    /// The session's switch, "Let the agent use the browser" (B17), as the
    /// window last said.
    agent_allowed: bool,
    /// An agent's call is running here.
    agent_busy: bool,
    /// **Agent driving** stays up until then after the last call.
    driving_until: Option<Instant>,
    /// What the user did that the window must act on, until taken.
    signals: Vec<AgentSignal>,
    _poll: Task<()>,
    _address_events: Subscription,
}

impl BrowserView {
    /// A browser sending through the worker at `access` with tunnel `key`,
    /// in the profile `profile`, built as a child of `window`, hidden until
    /// the window shows it. Starts on a page saying where its requests go.
    pub fn new(
        access: HttpAccess,
        key: String,
        via: String,
        profile: [u8; 16],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> BrowserView {
        let mut view = Self::empty(Some(via), window, cx);
        match worker_net(access, key) {
            Ok(net) => {
                view.net = Arc::new(net);
                view.build(profile, window, cx);
            }
            Err(e) => view.failed = Some(e),
        }
        view
    }

    /// A local session's browser: this computer's own network (B9), in the
    /// profile `profile`. No relay on macOS; on Windows the hub's direct
    /// route, since every WebView2 browser shares the hub as its proxy.
    pub fn new_direct(
        profile: [u8; 16],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> BrowserView {
        let mut view = Self::empty(None, window, cx);
        match direct_net() {
            Ok(net) => {
                view.net = Arc::new(net);
                view.build(profile, window, cx);
            }
            Err(e) => view.failed = Some(e),
        }
        view
    }

    fn empty(via: Option<String>, window: &mut Window, cx: &mut Context<Self>) -> BrowserView {
        let address = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Type an address, like localhost:3000")
        });
        let _address_events = cx.subscribe_in(
            &address,
            window,
            |this: &mut BrowserView, input, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    let text = input.read(cx).value().to_string();
                    this.go(&text, cx);
                }
            },
        );
        let _poll = cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(POLL).await;
            if this.update(cx, |view, cx| view.poll(cx)).is_err() {
                break;
            }
        });
        BrowserView {
            webview: Rc::default(),
            host: None,
            net: Arc::new(Net::Direct),
            via,
            address,
            pending_address: None,
            note: None,
            page: Rc::default(),
            placed: Rc::default(),
            shown: false,
            failed: None,
            pending_load: None,
            profile: [0; 16],
            pending_cert: None,
            ports: Vec::new(),
            agent_allowed: false,
            agent_busy: false,
            driving_until: None,
            signals: Vec::new(),
            _poll,
            _address_events,
        }
    }

    /// Build the web view for `profile` as a child of `window`.
    ///
    /// On Windows not now but from a task: WebView2's creation runs the
    /// thread's message loop while it waits, and so GPUI's own tasks, which
    /// must not find the app borrowed, as it is inside any update. The task
    /// keeps only the parent window's handle and installs the web view when
    /// it is there; until then an address asked for waits in
    /// `pending_load`.
    fn build(&mut self, profile: [u8; 16], window: &mut Window, cx: &mut Context<Self>) {
        self.profile = profile;
        let plan = Plan {
            page: self.page.clone(),
            net: self.net.clone(),
            profile,
            via: self.via.clone(),
        };
        #[cfg(not(target_os = "windows"))]
        {
            let _ = &cx;
            let built = create_webview(plan, window);
            self.install(built);
        }
        #[cfg(target_os = "windows")]
        {
            let parent = match super::windows::ParentWindow::of(window) {
                Ok(parent) => parent,
                Err(e) => {
                    self.failed = Some(e);
                    return;
                }
            };
            cx.spawn(async move |this, cx| {
                let built = create_webview(plan, &parent);
                let _ = this.update(cx, |view, cx| {
                    view.install(built);
                    cx.notify();
                });
            })
            .detach();
        }
    }

    /// Take the built web view: shown and placed if the browser is already
    /// on screen, and loading what was asked for while it was being built.
    fn install(&mut self, built: Result<WebView, String>) {
        let webview = match built {
            Ok(webview) => webview,
            Err(e) => {
                self.failed = Some(e);
                return;
            }
        };
        #[cfg(target_os = "macos")]
        {
            self.host = Host::adopt(&webview).map(Rc::new);
            if let Some(host) = &self.host {
                host.set_hidden(!self.shown);
            }
        }
        if self.shown {
            let _ = webview.set_visible(true);
        }
        if let Some(url) = self.pending_load.take() {
            let _ = webview.load_url(&url);
        }
        *self.webview.borrow_mut() = Some(webview);
        // Placed on the next frame, whatever was placed before.
        self.placed.set(None);
    }

    /// Load `url` now, or once the web view is built.
    fn load(&mut self, url: &str) {
        match self.webview.borrow().as_ref() {
            Some(webview) => {
                let _ = webview.load_url(url);
            }
            None => self.pending_load = Some(url.to_string()),
        }
    }

    /// Load what was typed into the address field (its Enter, or Go).
    pub fn go(&mut self, text: &str, cx: &mut Context<Self>) {
        self.note = None;
        match address::parse(text) {
            Address::Refused(why) => self.note = Some(why),
            Address::Load(url) => {
                if let Some(port) = address::loopback_port(&url) {
                    if let Err(e) = self.net.forward(port) {
                        self.show_error(
                            &format!("Can't show {}", shown_address(&url)),
                            &e.to_string(),
                        );
                        cx.notify();
                        return;
                    }
                }
                self.page.borrow_mut().requested = url.clone();
                self.load(&url);
            }
        }
        cx.notify();
    }

    /// Send new tunnels with `key`: the window reconnected and the worker
    /// granted a new one.
    pub fn set_key(&self, key: String) {
        self.net.set_key(key);
    }

    /// Whether the window has the browser on screen and uncovered. A hidden
    /// web view is not drawn at all, so GPUI's overlays show over the area.
    pub fn set_shown(&mut self, shown: bool) {
        if self.shown == shown {
            return;
        }
        self.shown = shown;
        if let Some(host) = &self.host {
            // Hiding gives the keyboard back to whatever covers it, before
            // the web view hides.
            host.set_hidden(!shown);
        }
        if let Some(webview) = self.webview.borrow().as_ref() {
            #[cfg(target_os = "windows")]
            if !shown {
                let _ = webview.focus_parent();
            }
            let _ = webview.set_visible(shown);
        }
        if shown {
            // Placed again on the next frame.
            self.placed.set(None);
        }
    }

    /// The session's listening ports (B11): offered under the toolbar, and —
    /// in a worker session — forwarded now, so a page's own requests to them
    /// reach the worker too (B24).
    pub fn set_ports(&mut self, ports: &[u16], cx: &mut Context<Self>) {
        let same = self.ports.len() == ports.len()
            && self
                .ports
                .iter()
                .zip(ports)
                .all(|((have, _), want)| have == want);
        if same {
            return;
        }
        self.ports = ports
            .iter()
            .map(|port| (*port, self.net.forward(*port).is_err()))
            .collect();
        cx.notify();
    }

    /// The `host:port` whose certificate the page on screen failed on, while
    /// our page offers Continue anyway (B21).
    pub fn certificate_failure(&self) -> Option<(String, u16)> {
        self.pending_cert.as_ref().map(|f| (f.host.clone(), f.port))
    }

    /// Choose Continue anyway, as the page's button does.
    pub fn continue_anyway(&mut self) {
        let mut page = self.page.borrow_mut();
        page.continue_requested = true;
        page.dirty = true;
    }

    /// The address of the page last started, for `--spike-browser`.
    pub fn url(&self) -> String {
        self.page.borrow().url.clone()
    }

    /// Whether the page or its DevTools holds the keyboard, for
    /// `--spike-browser`.
    #[cfg(target_os = "macos")]
    pub fn page_has_keyboard(&self) -> bool {
        self.host
            .as_ref()
            .is_some_and(|host| host.page_has_keyboard())
    }

    /// Open DevTools, as the toolbar button does, for `--spike-browser`.
    #[cfg(target_os = "macos")]
    pub fn open_devtools(&self) {
        self.devtools();
    }

    /// The page's title, for the sidebar row.
    pub fn title(&self) -> String {
        self.page.borrow().title.clone()
    }

    // -- the agent (§6) -----------------------------------------------------

    /// The session's switch, as the window has it.
    pub fn set_agent_allowed(&mut self, allowed: bool, cx: &mut Context<Self>) {
        if self.agent_allowed != allowed {
            self.agent_allowed = allowed;
            if !allowed {
                self.driving_until = None;
            }
            cx.notify();
        }
    }

    /// What the user did that the window must act on, once.
    pub fn take_signals(&mut self) -> Vec<AgentSignal> {
        std::mem::take(&mut self.signals)
    }

    /// Whether an agent drives the browser now, or did a moment ago.
    fn driving(&self) -> bool {
        self.agent_busy
            || self
                .driving_until
                .is_some_and(|until| Instant::now() < until)
    }

    /// The user took the browser over: the agent stops.
    fn take_over(&mut self) {
        self.driving_until = None;
        self.signals.push(AgentSignal::Stop);
    }

    /// The user used the toolbar: while the agent drives, that takes the
    /// browser over.
    fn user_acted(&mut self, cx: &mut Context<Self>) {
        if self.driving() {
            self.take_over();
            cx.notify();
        }
    }

    /// Run an agent's `tool` (call `call`) on this browser and put the
    /// answer in `finished`. The window runs one call per tab at a time.
    pub fn agent_run(&mut self, call: u64, tool: Tool, finished: Finished, cx: &mut Context<Self>) {
        self.agent_busy = true;
        self.driving_until = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let reply = run_tool(&this, cx, call, tool).await;
            finished.borrow_mut().push((call, reply));
            let _ = this.update(cx, |view, cx| {
                view.agent_busy = false;
                view.driving_until = Some(Instant::now() + DRIVING_LINGER);
                cx.notify();
            });
        })
        .detach();
    }

    /// Start `js` in the page; its answer (JSON, as wry hands it back)
    /// arrives on the receiver. `None` without a web view.
    fn start_js(&self, js: &str) -> Option<std::sync::mpsc::Receiver<String>> {
        let webview = self.webview.borrow();
        let webview = webview.as_ref()?;
        let (tx, rx) = std::sync::mpsc::channel();
        webview
            .evaluate_script_with_callback(js, move |raw| {
                let _ = tx.send(raw);
            })
            .ok()?;
        Some(rx)
    }

    /// Take a screenshot; its PNG arrives on the receiver.
    fn start_capture(&self) -> Option<std::sync::mpsc::Receiver<Result<Vec<u8>, String>>> {
        let webview = self.webview.borrow();
        let webview = webview.as_ref()?;
        let (tx, rx) = std::sync::mpsc::channel();
        super::capture(webview, move |png| {
            let _ = tx.send(png);
        });
        Some(rx)
    }

    /// Pages started and finished so far.
    fn load_counts(&self) -> (u64, u64) {
        let page = self.page.borrow();
        (page.starts, page.loads)
    }

    /// The page as the agent is told it: its address and title, or our
    /// error page's words.
    fn page_summary(&self) -> Result<String, String> {
        let page = self.page.borrow();
        if page.own_page {
            if let Some((title, detail)) = &page.last_error {
                return Err(format!("The browser shows an error: {title}. {detail}"));
            }
        }
        let title = if page.title.trim().is_empty() {
            "(no title)".to_string()
        } else {
            page.title.trim().to_string()
        };
        Ok(format!("{} — {title}", page.url))
    }

    /// Load `text` for the agent, as the address field would. Refused
    /// addresses come back as the field's note.
    fn agent_go(&mut self, text: &str, cx: &mut Context<Self>) -> Result<(), String> {
        self.go(text, cx);
        match self.note.take() {
            Some(why) => Err(why),
            None => Ok(()),
        }
    }

    fn show_error(&self, title: &str, detail: &str) {
        self.page.borrow_mut().last_error = Some((title.to_string(), detail.to_string()));
        if let Some(webview) = self.webview.borrow().as_ref() {
            let _ = webview.load_html(&error_page::page(title, detail, self.via.as_deref()));
        }
    }

    /// What the handlers and the relay reported since the last look.
    fn poll(&mut self, cx: &mut Context<Self>) {
        let (dirty, blocked, opened, url, continue_requested) = {
            let mut page = self.page.borrow_mut();
            (
                std::mem::take(&mut page.dirty),
                page.blocked.take(),
                std::mem::take(&mut page.opened),
                page.url.clone(),
                std::mem::take(&mut page.continue_requested),
            )
        };
        let mut changed = dirty;
        if let Some(host) = &self.host {
            host.enforce(self.shown);
        }
        // The user used the page while the agent drove it: theirs again.
        let used = std::mem::take(&mut self.page.borrow_mut().user_input);
        if used && self.driving() {
            self.take_over();
            changed = true;
        }
        if self
            .driving_until
            .is_some_and(|until| Instant::now() >= until)
        {
            self.driving_until = None;
            changed = true;
        }
        self.poll_certificates(continue_requested);
        if let Some((title, detail)) = blocked {
            self.show_error(&title, &detail);
        }
        for url in opened {
            if allow_navigation(&url, &self.net, &self.page) {
                self.page.borrow_mut().requested = url.clone();
                self.load(&url);
            }
        }
        // A tunnel the worker refused for the page itself is our error
        // page; one for a script or an image is the page's own business.
        // The page is the one asked for while it has not started yet.
        let requested = self.page.borrow().requested.clone();
        let page = if requested.is_empty() {
            url.clone()
        } else {
            requested
        };
        for event in self.net.take_events() {
            let RelayEvent::Refused {
                target, message, ..
            } = event;
            if targets_page(&page, &target) {
                self.page.borrow_mut().requested.clear();
                self.show_error(&format!("Can't reach {}", shown_address(&page)), &message);
                changed = true;
            }
        }
        if dirty && !url.is_empty() {
            self.pending_address = Some(shown_address(&url));
        }
        if changed {
            cx.notify();
        }
    }

    /// B21: a page that failed on its certificate shows our page, with
    /// **Continue anyway**; choosing it trusts that `host:port` in this
    /// profile from now on, and loads the page again. On Windows only a
    /// failure of the page itself counts; one of a script or an image on it
    /// was simply cancelled.
    fn poll_certificates(&mut self, continue_requested: bool) {
        let webview = self.webview.borrow();
        let Some(webview) = webview.as_ref() else {
            return;
        };
        let failure = tls::take_failure(webview);
        #[cfg(target_os = "windows")]
        let failure = failure.filter(|failure| {
            let page = self.page.borrow();
            super::cert::is_page_failure(failure, &page.navigating)
                || super::cert::is_page_failure(failure, &page.requested)
        });
        if let Some(failure) = failure {
            let detail = format!(
                "{} The certificate {}:{} presented is not trusted. A development server \
                 with a self-signed certificate looks like this; anywhere else it can mean \
                 someone is in the way. Continue only if you expect this certificate.",
                failure.message, failure.host, failure.port
            );
            let _ = webview.load_html(&error_page::page_with_action(
                &format!("Can't verify {}:{}", failure.host, failure.port),
                &detail,
                self.via.as_deref(),
                "Continue anyway",
            ));
            self.pending_cert = Some(failure);
        }
        if continue_requested {
            if let Some(failure) = self.pending_cert.take() {
                tls::allow(webview, &failure.host, failure.port);
                super::trust::TrustStore::at(super::trust::default_path()).allow(
                    &self.profile,
                    &failure.host,
                    failure.port,
                );
                let _ = webview.load_url(&failure.url);
            }
        }
    }

    fn back(&self) {
        self.script("history.back()");
    }

    fn forward(&self) {
        self.script("history.forward()");
    }

    fn reload(&self) {
        if let Some(webview) = self.webview.borrow().as_ref() {
            let _ = webview.reload();
        }
    }

    fn devtools(&self) {
        if let Some(webview) = self.webview.borrow().as_ref() {
            webview.open_devtools();
        }
    }

    fn script(&self, js: &str) {
        if let Some(webview) = self.webview.borrow().as_ref() {
            let _ = webview.evaluate_script(js);
        }
    }
}

impl Drop for BrowserView {
    fn drop(&mut self) {
        // Before the web view leaves the window: a first responder taken out
        // of it leaves no view with the keyboard.
        if let Some(host) = &self.host {
            host.release_keyboard();
        }
        if let Some(webview) = self.webview.borrow().as_ref() {
            tls::unregister(webview);
            #[cfg(target_os = "windows")]
            let _ = webview.focus_parent();
        }
    }
}

impl Render for BrowserView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The page's address into the field, unless someone is typing there.
        if let Some(text) = self.pending_address.take() {
            let typing = self.address.read(cx).focus_handle(cx).is_focused(window);
            if !typing {
                self.address
                    .update(cx, |state, cx| state.set_value(text, window, cx));
            }
        }
        let p = *Palette::global(cx);
        let entity = cx.entity();
        let button = |id: &'static str, label: &'static str, tip: &'static str| {
            div()
                .id(id)
                .debug_selector(move || format!("browser-{id}"))
                .flex_none()
                .px(px(8.))
                .h(px(26.))
                .flex()
                .items_center()
                .rounded(px(6.))
                .cursor_pointer()
                .text_color(p.ink_2.hsla())
                .hover(|s| s.bg(p.surface_raised.hsla()))
                .tooltip(crate::views::icons::tooltip(tip))
                .child(label)
        };
        let toolbar = h_flex()
            .flex_none()
            .w_full()
            .px(px(10.))
            .py(px(6.))
            .gap(px(4.))
            .border_b_1()
            .border_color(p.hairline.hsla())
            .bg(p.surface_window.hsla())
            .text_size(px(13.))
            .child(button("back", "‹", "Back").on_click({
                let entity = entity.clone();
                move |_, _, cx| {
                    entity.update(cx, |view, cx| {
                        view.user_acted(cx);
                        view.back();
                    })
                }
            }))
            .child(button("forward", "›", "Forward").on_click({
                let entity = entity.clone();
                move |_, _, cx| {
                    entity.update(cx, |view, cx| {
                        view.user_acted(cx);
                        view.forward();
                    })
                }
            }))
            .child(button("reload", "⟳", "Reload").on_click({
                let entity = entity.clone();
                move |_, _, cx| {
                    entity.update(cx, |view, cx| {
                        view.user_acted(cx);
                        view.reload();
                    })
                }
            }))
            .child(div().flex_1().min_w_0().child(Input::new(&self.address)))
            .child(button("go", "Go", "Load this address").on_click({
                let entity = entity.clone();
                let field = self.address.clone();
                move |_, _, cx| {
                    let text = field.read(cx).value().to_string();
                    entity.update(cx, |view, cx| {
                        view.user_acted(cx);
                        view.go(&text, cx);
                    });
                }
            }))
            .children(self.via.clone().map(|via| {
                div()
                    .flex_none()
                    .px(px(8.))
                    .py(px(2.))
                    .rounded(px(10.))
                    .border_1()
                    .border_color(p.border.hsla())
                    .text_size(px(11.5))
                    .text_color(p.muted.hsla())
                    .child(format!("via {via}"))
            }))
            // §6: the session's switch, and while the agent drives, Stop.
            .children(self.driving().then(|| {
                h_flex()
                    .flex_none()
                    .gap(px(6.))
                    .pl(px(8.))
                    .pr(px(3.))
                    .py(px(2.))
                    .rounded(px(10.))
                    .bg(p.accent.hsla().opacity(0.15))
                    .text_size(px(11.5))
                    .text_color(p.accent.hsla())
                    .child("Agent driving")
                    .child(
                        div()
                            .id("agent-stop")
                            .debug_selector(|| "browser-agent-stop".to_string())
                            .px(px(7.))
                            .rounded(px(8.))
                            .cursor_pointer()
                            .bg(p.surface_window.hsla())
                            .text_color(p.ink_2.hsla())
                            .hover(|s| s.bg(p.surface_raised.hsla()))
                            .tooltip(crate::views::icons::tooltip(
                                "Stop the agent and turn off its use of the browser",
                            ))
                            .child("Stop")
                            .on_click({
                                let entity = entity.clone();
                                move |_, _, cx| {
                                    entity.update(cx, |view, cx| {
                                        view.take_over();
                                        cx.notify();
                                    })
                                }
                            }),
                    )
            }))
            .child({
                let allowed = self.agent_allowed;
                let label = if allowed { "Agent: on" } else { "Agent: off" };
                let tip = if allowed {
                    "The session's agent may use this browser. Click to stop it."
                } else {
                    "Let the agent use the browser: the session's agent can then open pages, read them, click and type here while you watch."
                };
                let chip = button("agent", label, tip).on_click({
                    let entity = entity.clone();
                    move |_, _, cx| {
                        entity.update(cx, |view, cx| {
                            view.signals.push(AgentSignal::SetAllowed(!allowed));
                            cx.notify();
                        })
                    }
                });
                if allowed {
                    chip.text_color(p.accent.hsla())
                } else {
                    chip
                }
            })
            .child(
                button("devtools", "DevTools", "Open the page's developer tools").on_click({
                    let entity = entity.clone();
                    move |_, _, cx| entity.read(cx).devtools()
                }),
            );
        let ports = (!self.ports.is_empty()).then(|| {
            h_flex()
                .flex_none()
                .px(px(14.))
                .py(px(4.))
                .gap(px(6.))
                .text_size(px(11.5))
                .text_color(p.muted.hsla())
                .child("Listening:")
                .children(self.ports.iter().enumerate().map(|(i, (port, in_use))| {
                    let entity = entity.clone();
                    let port = *port;
                    let chip = div()
                        .id(("browser-port", i))
                        .debug_selector(move || format!("browser-port-{i}"))
                        .px(px(7.))
                        .py(px(1.))
                        .rounded(px(9.))
                        .border_1()
                        .border_color(p.border.hsla())
                        .font_family(crate::fonts::MONO_FAMILY)
                        .child(format!("localhost:{port}"));
                    if *in_use {
                        chip.opacity(0.5).tooltip(crate::views::icons::tooltip(format!(
                            "Port {port} is in use on this computer, so the worker's cannot be shown here"
                        )))
                    } else {
                        chip.cursor_pointer()
                            .text_color(p.ink_2.hsla())
                            .hover(|s| s.bg(p.surface_raised.hsla()))
                            .on_click(move |_, _, cx| {
                                entity.update(cx, |view, cx| view.go(&format!("localhost:{port}"), cx))
                            })
                    }
                }))
        });
        let note = self.note.clone().map(|why| {
            div()
                .flex_none()
                .px(px(14.))
                .py(px(4.))
                .text_size(px(12.))
                .text_color(p.muted.hsla())
                .child(why)
        });
        let body = match &self.failed {
            Some(why) => div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_color(p.muted.hsla())
                .child(why.clone())
                .into_any_element(),
            None => {
                let webview = self.webview.clone();
                let host = self.host.clone();
                let placed = self.placed.clone();
                #[cfg(target_os = "windows")]
                let focus = self.webview.clone();
                canvas(
                    move |bounds, _, _| place(&webview, host.as_deref(), &placed, bounds),
                    move |_, _, _window, _| {
                        // WebView2 keeps the keyboard once clicked: a click
                        // in GPUI (the page takes its own) hands it back.
                        #[cfg(target_os = "windows")]
                        {
                            let focus = focus.clone();
                            _window.on_mouse_event(move |_: &gpui::MouseDownEvent, phase, _, _| {
                                if phase == gpui::DispatchPhase::Capture {
                                    if let Some(webview) = focus.borrow().as_ref() {
                                        let _ = webview.focus_parent();
                                    }
                                }
                            });
                        }
                    },
                )
                .flex_1()
                .size_full()
                .into_any_element()
            }
        };
        v_flex()
            .size_full()
            .min_h_0()
            .child(toolbar)
            .children(ports)
            .children(note)
            .child(body)
    }
}

/// What [`create_webview`] needs, taken from the view so the build holds
/// no borrow of it.
struct Plan {
    page: Rc<RefCell<PageState>>,
    net: Arc<Net>,
    profile: [u8; 16],
    via: Option<String>,
}

/// The web view for `plan`, a child of `parent`, hidden, on its start page,
/// its requests going where `plan.net` says.
fn create_webview(
    plan: Plan,
    parent: &impl wry::raw_window_handle::HasWindowHandle,
) -> Result<WebView, String> {
    let Plan {
        page,
        net,
        profile,
        via,
    } = plan;
    let ipc_page = page.clone();
    let nav_page = page.clone();
    let nav_net = net.clone();
    let load_page = page.clone();
    let title_page = page.clone();
    let open_page = page;
    // Every WebView2 browser shares one data folder (§4.1).
    #[cfg(target_os = "windows")]
    let mut context = super::windows::data_dir().map(|dir| wry::WebContext::new(Some(dir)));
    #[cfg(target_os = "windows")]
    let builder = match context.as_mut() {
        Some(context) => WebViewBuilder::new_with_web_context(context),
        None => WebViewBuilder::new(),
    };
    #[cfg(not(target_os = "windows"))]
    let builder = WebViewBuilder::new();
    let builder = builder
        .with_bounds(Rect {
            position: LogicalPosition::new(0, 0).into(),
            size: LogicalSize::new(1, 1).into(),
        })
        .with_visible(false)
        .with_devtools(true)
        .with_navigation_handler(move |url| {
            #[cfg(target_os = "windows")]
            {
                nav_page.borrow_mut().navigating = url.clone();
            }
            allow_navigation(&url, &nav_net, &nav_page)
        })
        .with_on_page_load_handler(move |event, url| {
            let mut page = load_page.borrow_mut();
            match event {
                PageLoadEvent::Started => {
                    page.requested.clear();
                    page.starts += 1;
                    page.own_page = !is_web(&url);
                    if is_web(&url) {
                        page.url = url;
                        page.last_error = None;
                    }
                }
                PageLoadEvent::Finished => page.loads += 1,
            }
            page.dirty = true;
        })
        // §6: the page's console for the agent, and the user's own clicks
        // and keys, which take the browser back from a driving agent.
        .with_initialization_script(agent_js::INIT)
        .with_document_title_changed_handler(move |title| {
            let mut page = title_page.borrow_mut();
            page.title = title;
            page.dirty = true;
        })
        .with_new_window_req_handler(move |url, _| {
            let mut page = open_page.borrow_mut();
            page.opened.push(url);
            page.dirty = true;
            NewWindowResponse::Deny
        })
        .with_download_started_handler(|url, path| {
            *path = download_path(&url, path);
            true
        })
        .with_ipc_handler(move |request| {
            if request.body() == error_page::CONTINUE_MESSAGE {
                let mut page = ipc_page.borrow_mut();
                page.continue_requested = true;
                page.dirty = true;
            } else if request.body() == agent_js::USER_INPUT_MESSAGE {
                let mut page = ipc_page.borrow_mut();
                page.user_input = true;
                page.dirty = true;
            }
        });
    // B10: the profile; on Windows also the hub as proxy, the same
    // arguments for every browser.
    #[cfg(target_os = "macos")]
    let builder = {
        use wry::WebViewBuilderExtDarwin;
        builder.with_data_store_identifier(profile)
    };
    #[cfg(target_os = "windows")]
    let (builder, hub_port) = {
        use wry::WebViewBuilderExtWindows;
        let hub = super::windows::hub()?;
        let builder = builder
            .with_additional_browser_args(super::windows::browser_args(hub.port()))
            .with_profile_name(super::windows::profile_name(&profile));
        (builder, hub.port())
    };
    let webview = builder
        .build_as_child(parent)
        .map_err(|e| format!("The browser could not start: {e}"))?;
    // B21: the certificates this profile trusts.
    let trusted = super::trust::TrustStore::at(super::trust::default_path()).allowed(&profile);
    tls::register(&webview, trusted);
    #[cfg(target_os = "macos")]
    {
        if let Net::Relay(relay) = &*net {
            let (user, pass) = relay.credentials();
            super::proxy_macos::install(&webview, relay.port(), user, pass);
        }
    }
    #[cfg(target_os = "windows")]
    if let Net::Hub(session) = &*net {
        let (user, pass) = session.credentials();
        super::windows::answer_hub_login(&webview, hub_port, user.to_string(), pass.to_string())?;
    }
    let _ = webview.load_html(&start_page(via.as_deref()));
    Ok(webview)
}

/// Delete browser profile `profile` (B19: its worktree was abandoned). Done
/// a moment later on the main thread: WebKit refuses to remove a data store
/// a web view still uses (WebView2 a folder, until its browser process lets
/// go), and the views of the session that closed are dropped at the end of
/// this frame.
pub fn forget_profile(profile: [u8; 16], cx: &mut gpui::App) {
    super::trust::TrustStore::at(super::trust::default_path()).forget(&profile);
    // WebView2 keeps the profile in a folder; see `profile_dirs`.
    #[cfg(target_os = "windows")]
    cx.spawn(async move |cx| {
        cx.background_executor().timer(Duration::from_secs(1)).await;
        cx.update(|_| super::windows::forget_profile(&profile));
    })
    .detach();
    // Before macOS 14 there is no session browser (B20), so no profile to
    // delete, and no `removeDataStoreForIdentifier:` to delete it with.
    #[cfg(target_os = "macos")]
    if super::unavailable().is_some() {
        return;
    }
    #[cfg(target_os = "macos")]
    cx.spawn(async move |cx| {
        cx.background_executor().timer(Duration::from_secs(1)).await;
        cx.update(|_| {
            use wry::WebViewExtDarwin;
            warm_webkit();
            WebView::remove_data_store(&profile, |result| {
                if let Err(e) = result {
                    eprintln!("flightdeck-desktop: could not delete a browser profile: {e}");
                }
            });
        });
    })
    .detach();
}

/// Start WebKit up in this process, if no web view has yet. Without this,
/// `WKWebsiteDataStore.removeDataStoreForIdentifier:` crashes the app: its
/// work on WebKit's IO queue hands the result back to WebKit's main run loop,
/// which only exists once WebKit has started, and dispatches to a null one
/// (SIGSEGV in `WTF::RunLoop::dispatch`). A worker window forgets the profiles
/// of the worker's abandoned worktrees whether or not a browser was ever open
/// in it, so this is the usual case there. `defaultDataStore` is enough to
/// start WebKit, and cheaper than a web view: it starts no web content process.
#[cfg(target_os = "macos")]
fn warm_webkit() {
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    // SAFETY: a class method with no arguments that returns the shared,
    // autoreleased default store; called on the main thread (`cx.update`).
    let _: Option<Retained<AnyObject>> =
        unsafe { msg_send![class!(WKWebsiteDataStore), defaultDataStore] };
}

/// Place the web view at `bounds` (window-relative logical pixels, as GPUI
/// lays out and wry places a child view): its container when it has one.
fn place(
    webview: &Shared,
    host: Option<&Host>,
    placed: &Cell<Option<Bounds<Pixels>>>,
    bounds: Bounds<Pixels>,
) {
    if placed.get() == Some(bounds) {
        return;
    }
    placed.set(Some(bounds));
    if let Some(host) = host {
        host.place(
            f64::from(f32::from(bounds.origin.x)),
            f64::from(f32::from(bounds.origin.y)),
            f64::from(f32::from(bounds.size.width)),
            f64::from(f32::from(bounds.size.height)),
        );
    } else if let Some(webview) = webview.borrow().as_ref() {
        let _ = webview.set_bounds(Rect {
            position: LogicalPosition::new(f32::from(bounds.origin.x), f32::from(bounds.origin.y))
                .into(),
            size: LogicalSize::new(f32::from(bounds.size.width), f32::from(bounds.size.height))
                .into(),
        });
    }
}

/// The navigation rule: our own pages and the web load; `file:` is refused
/// (B12); a loopback address gets its forward first (B22, B24), and a port in
/// use here is refused with our page instead.
fn allow_navigation(url: &str, net: &Net, page: &RefCell<PageState>) -> bool {
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("about:") || lower.starts_with("data:") || lower.starts_with("blob:") {
        return true;
    }
    if lower.starts_with("file:") {
        block(page, "This page is not shown here", FILE_REFUSED);
        return false;
    }
    // A local browser's loopback is this computer's own (B9); on Windows
    // loopback goes through the hub, and `forward` has nothing to do.
    if let Some(port) = address::loopback_port(url) {
        if let Err(e) = net.forward(port) {
            block(
                page,
                &format!("Can't show {}", shown_address(url)),
                &e.to_string(),
            );
            return false;
        }
    }
    true
}

fn block(page: &RefCell<PageState>, title: &str, detail: &str) {
    let mut page = page.borrow_mut();
    page.blocked = Some((title.to_string(), detail.to_string()));
    page.dirty = true;
}

/// Whether `url` is a web page (not one of our own HTML pages).
fn is_web(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

/// `url` as the address field shows it: without the scheme for `http://`
/// (the field adds it back), whole otherwise.
fn shown_address(url: &str) -> String {
    url.strip_prefix("http://")
        .map(|rest| rest.trim_end_matches('/').to_string())
        .unwrap_or_else(|| url.to_string())
}

/// Whether a refused tunnel to `target` (`host:port` as the relay saw it)
/// was for the page at `url` itself. A loopback forward reaches the worker
/// as `127.0.0.1` or `[::1]` whatever loopback name the page used.
fn targets_page(url: &str, target: &str) -> bool {
    if url.is_empty() {
        return false;
    }
    if let Some(port) = address::loopback_port(url) {
        return ["127.0.0.1", "[::1]", "localhost"]
            .iter()
            .any(|host| target == format!("{host}:{port}"));
    }
    let Some((scheme, rest)) = url.split_once("://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let authority = authority.rsplit_once('@').map_or(authority, |(_, a)| a);
    let with_port = if authority
        .rsplit_once(':')
        .is_some_and(|(_, p)| p.parse::<u16>().is_ok())
        && !authority.ends_with(']')
    {
        authority.to_string()
    } else {
        let port = if scheme.eq_ignore_ascii_case("https") {
            443
        } else {
            80
        };
        format!("{authority}:{port}")
    };
    target.eq_ignore_ascii_case(&with_port)
}

/// Where a download goes: this computer's Downloads folder, under the name
/// the page suggested (else the URL's last segment), never over a file
/// already there.
fn download_path(url: &str, suggested: &Path) -> PathBuf {
    let dir = flightdeck::web::client::download::default_destination();
    let name = suggested
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::to_string)
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| {
            url.split(['?', '#'])
                .next()
                .and_then(|u| u.rsplit('/').next())
                .filter(|n| !n.is_empty())
                .unwrap_or("download")
                .to_string()
        });
    let mut path = dir.join(&name);
    let mut n = 1;
    while path.exists() {
        let (stem, ext) = match name.rsplit_once('.') {
            Some((stem, ext)) if !stem.is_empty() => (stem.to_string(), format!(".{ext}")),
            _ => (name.clone(), String::new()),
        };
        path = dir.join(format!("{stem} ({n}){ext}"));
        n += 1;
    }
    path
}

// -- the agent's tools (§6) -------------------------------------------------

/// What the agent hears when the browser went away mid-call.
const GONE: &str = "The browser tab was closed before it answered.";

/// Run one tool on the view `this` and answer it.
async fn run_tool(
    this: &WeakEntity<BrowserView>,
    cx: &mut AsyncApp,
    call: u64,
    tool: Tool,
) -> BrowserReply {
    match tool {
        // The window answers these itself.
        Tool::Tabs | Tool::Open { .. } => BrowserReply::error("Not a tab's tool."),
        Tool::Navigate { url, .. } => {
            let Some((_, before)) = counts(this, cx) else {
                return BrowserReply::error(GONE);
            };
            match this.update(cx, |view, cx| view.agent_go(&url, cx)) {
                Err(_) => return BrowserReply::error(GONE),
                Ok(Err(why)) => return BrowserReply::error(why),
                Ok(Ok(())) => {}
            }
            loaded(this, cx, before, LOAD_WAIT).await
        }
        Tool::Back { .. } => {
            let Some((_, before)) = counts(this, cx) else {
                return BrowserReply::error(GONE);
            };
            let _ = this.update(cx, |view, _| view.script("history.back()"));
            if !wait_load(this, cx, before, Duration::from_secs(5)).await {
                let here = summary(this, cx).unwrap_or_else(|e| e);
                return BrowserReply::text(format!(
                    "There was no page to go back to (or it is still loading). Now on {here}"
                ));
            }
            loaded(this, cx, before, Duration::ZERO).await
        }
        Tool::Reload { .. } => {
            let Some((_, before)) = counts(this, cx) else {
                return BrowserReply::error(GONE);
            };
            let _ = this.update(cx, |view, _| view.reload());
            loaded(this, cx, before, LOAD_WAIT).await
        }
        Tool::Snapshot { .. } => {
            let Some(v) = js(this, cx, agent_js::snapshot(), Duration::from_secs(10)).await else {
                return BrowserReply::error(
                    "The page did not answer. It may still be loading; try again.",
                );
            };
            if let Some(why) = v["error"].as_str() {
                return BrowserReply::error(format!("Could not read the page: {why}"));
            }
            let mut text = format!(
                "Page: {}\nAddress: {}\n",
                v["title"].as_str().unwrap_or_default(),
                v["url"].as_str().unwrap_or_default()
            );
            if v["dialog"].as_bool() == Some(true) {
                text.push_str("A dialog is open.\n");
            }
            text.push('\n');
            let outline = v["text"].as_str().unwrap_or_default();
            text.push_str(if outline.is_empty() {
                "(The page shows nothing that can be read.)"
            } else {
                outline
            });
            if v["truncated"].as_bool() == Some(true) {
                text.push_str("\n(The outline was cut short: the page is long.)");
            }
            BrowserReply::text(text)
        }
        Tool::Click { r#ref, .. } => act(this, cx, agent_js::click(&r#ref), "Clicked").await,
        Tool::Type {
            r#ref,
            text,
            submit,
            ..
        } => {
            act(
                this,
                cx,
                agent_js::type_text(&r#ref, &text, submit),
                "Typed into",
            )
            .await
        }
        Tool::Press { key, .. } => {
            act(
                this,
                cx,
                agent_js::press(&key),
                &format!("Pressed {key} on"),
            )
            .await
        }
        Tool::Screenshot { .. } => {
            let Ok(Some(rx)) = this.update(cx, |view, _| view.start_capture()) else {
                return BrowserReply::error(GONE);
            };
            match recv(&rx, cx, Duration::from_secs(15)).await {
                Some(Ok(png)) => {
                    let here = summary(this, cx).unwrap_or_else(|e| e);
                    BrowserReply::png_bytes(&png, format!("Screenshot of {here}"))
                }
                Some(Err(why)) => BrowserReply::error(why),
                None => BrowserReply::error("The browser did not take the screenshot in time."),
            }
        }
        Tool::Console { clear, .. } => {
            let Some(v) = js(this, cx, agent_js::console(clear), Duration::from_secs(10)).await
            else {
                return BrowserReply::error("The page did not answer; try again.");
            };
            let messages = v["messages"].as_array().cloned().unwrap_or_default();
            if messages.is_empty() {
                return BrowserReply::text("The console is empty.");
            }
            let lines: Vec<String> = messages
                .iter()
                .map(|m| {
                    format!(
                        "[{}] {}",
                        m["level"].as_str().unwrap_or("log"),
                        m["text"].as_str().unwrap_or_default()
                    )
                })
                .collect();
            BrowserReply::text(lines.join("\n"))
        }
        Tool::Eval { expression, .. } => eval(this, cx, call, &expression).await,
    }
}

/// The view's page counts, or `None` once it is gone.
fn counts(this: &WeakEntity<BrowserView>, cx: &mut AsyncApp) -> Option<(u64, u64)> {
    this.update(cx, |view, _| view.load_counts()).ok()
}

fn summary(this: &WeakEntity<BrowserView>, cx: &mut AsyncApp) -> Result<String, String> {
    this.update(cx, |view, _| view.page_summary())
        .unwrap_or_else(|_| Err(GONE.to_string()))
}

/// `rx`'s value, waiting at most `wait`.
async fn recv<T>(
    rx: &std::sync::mpsc::Receiver<T>,
    cx: &mut AsyncApp,
    wait: Duration,
) -> Option<T> {
    let deadline = Instant::now() + wait;
    loop {
        match rx.try_recv() {
            Ok(value) => return Some(value),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => return None,
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }
        if Instant::now() >= deadline {
            return None;
        }
        cx.background_executor()
            .timer(Duration::from_millis(20))
            .await;
    }
}

/// Run `script` in the page and decode its answer.
async fn js(
    this: &WeakEntity<BrowserView>,
    cx: &mut AsyncApp,
    script: String,
    wait: Duration,
) -> Option<serde_json::Value> {
    let rx = this.update(cx, |view, _| view.start_js(&script)).ok()??;
    let raw = recv(&rx, cx, wait).await?;
    agent_js::decode(&raw)
}

/// Wait until more than `before` pages finished loading.
async fn wait_load(
    this: &WeakEntity<BrowserView>,
    cx: &mut AsyncApp,
    before: u64,
    wait: Duration,
) -> bool {
    let deadline = Instant::now() + wait;
    loop {
        match counts(this, cx) {
            Some((_, loads)) if loads > before => return true,
            None => return false,
            Some(_) => {}
        }
        if Instant::now() >= deadline {
            return false;
        }
        cx.background_executor()
            .timer(Duration::from_millis(50))
            .await;
    }
}

/// Answer a navigation once the page loaded (or `wait` ran out).
async fn loaded(
    this: &WeakEntity<BrowserView>,
    cx: &mut AsyncApp,
    before: u64,
    wait: Duration,
) -> BrowserReply {
    let done = wait == Duration::ZERO || wait_load(this, cx, before, wait).await;
    match summary(this, cx) {
        Err(why) => BrowserReply::error(why),
        Ok(here) if done => BrowserReply::text(format!("Loaded {here}")),
        Ok(here) => BrowserReply::text(format!(
            "Still loading after {} s: {here}. Take a browser_snapshot to see how far it got.",
            wait.as_secs()
        )),
    }
}

/// Run an action `script` (click, type, press) and say what it did,
/// waiting for a page it started to load.
async fn act(
    this: &WeakEntity<BrowserView>,
    cx: &mut AsyncApp,
    script: String,
    verb: &str,
) -> BrowserReply {
    let Some((starts, loads)) = counts(this, cx) else {
        return BrowserReply::error(GONE);
    };
    let Some(v) = js(this, cx, script, Duration::from_secs(10)).await else {
        return BrowserReply::error("The page did not answer. It may still be loading; try again.");
    };
    if let Some(why) = v["error"].as_str() {
        return BrowserReply::error(why.to_string());
    }
    let desc = v["desc"].as_str().unwrap_or("the element").to_string();
    // A page it started (a link, a submit) is waited for.
    cx.background_executor()
        .timer(Duration::from_millis(350))
        .await;
    match counts(this, cx) {
        Some((now, _)) if now > starts => {
            let done = wait_load(this, cx, loads, LOAD_WAIT).await;
            let here = summary(this, cx).unwrap_or_else(|e| e);
            BrowserReply::text(if done {
                format!("{verb} {desc}. A new page loaded: {here}")
            } else {
                format!("{verb} {desc}. A new page is still loading: {here}")
            })
        }
        _ => BrowserReply::text(format!(
            "{verb} {desc}. Take a browser_snapshot to see the result."
        )),
    }
}

/// `browser_eval`: start the expression, then wait for its value, giving
/// up when the page navigates away.
async fn eval(
    this: &WeakEntity<BrowserView>,
    cx: &mut AsyncApp,
    call: u64,
    expression: &str,
) -> BrowserReply {
    const LONGEST: usize = 50_000;
    let Some((starts, _)) = counts(this, cx) else {
        return BrowserReply::error(GONE);
    };
    let started = js(
        this,
        cx,
        agent_js::eval_start(call, expression),
        Duration::from_secs(10),
    )
    .await;
    if started.is_none() {
        return BrowserReply::error(
            "The expression did not run: it is not valid JavaScript, or the page is still \
             loading. Give one expression, or a function like () => { ... }.",
        );
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(v) = js(this, cx, agent_js::eval_poll(call), Duration::from_secs(5)).await {
            if !v.is_null() {
                let mut value = v["value"].as_str().unwrap_or_default().to_string();
                if value.len() > LONGEST {
                    let mut cut = LONGEST;
                    while !value.is_char_boundary(cut) {
                        cut -= 1;
                    }
                    value.truncate(cut);
                    value.push_str("\n(cut short)");
                }
                return if v["ok"].as_bool() == Some(true) {
                    BrowserReply::text(value)
                } else {
                    BrowserReply::error(format!("The expression threw: {value}"))
                };
            }
        }
        match counts(this, cx) {
            None => return BrowserReply::error(GONE),
            Some((now, _)) if now > starts => {
                return BrowserReply::error(
                    "The page navigated away before the expression finished.",
                )
            }
            Some(_) => {}
        }
        if Instant::now() >= deadline {
            return BrowserReply::error("The expression did not finish within 30 s.");
        }
        cx.background_executor()
            .timer(Duration::from_millis(100))
            .await;
    }
}

/// The page a new browser starts on.
fn start_page(via: Option<&str>) -> String {
    match via {
        Some(via) => error_page::page(
            "New browser",
            "Type an address above, like localhost:3000. Every request this browser makes \
             is made by the worker, so localhost is the worker's.",
            Some(via),
        ),
        None => error_page::page(
            "New browser",
            "Type an address above, like localhost:3000.",
            None,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_address_field_shows_http_pages_without_their_scheme() {
        assert_eq!(shown_address("http://localhost:3000/"), "localhost:3000");
        assert_eq!(
            shown_address("http://localhost:3000/a/"),
            "localhost:3000/a"
        );
        assert_eq!(
            shown_address("https://example.com/"),
            "https://example.com/"
        );
    }

    #[test]
    fn a_refused_tunnel_is_the_pages_own_only_for_its_host_and_port() {
        assert!(targets_page("http://localhost:3000/x", "127.0.0.1:3000"));
        assert!(targets_page("http://localhost:3000/x", "[::1]:3000"));
        assert!(!targets_page("http://localhost:3000/x", "127.0.0.1:5173"));
        assert!(targets_page("https://example.com/a", "example.com:443"));
        assert!(targets_page(
            "http://example.com:8080/a",
            "example.com:8080"
        ));
        assert!(!targets_page(
            "https://example.com/a",
            "cdn.example.com:443"
        ));
        assert!(!targets_page("", "example.com:443"));
    }

    #[test]
    fn a_download_never_overwrites_and_has_a_name() {
        let named = download_path("https://x/y/report.pdf?z=1", Path::new(""));
        assert!(named
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("report"));
        let suggested = download_path("https://x/", Path::new("/tmp/data.csv"));
        assert!(suggested
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("data"));
        let bare = download_path("https://x/", Path::new(""));
        assert!(
            bare.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("download")
                || bare.file_name().unwrap().to_str().unwrap().starts_with('x')
        );
    }
}
