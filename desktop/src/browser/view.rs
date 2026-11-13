//! One session browser (`specs/SESSION_BROWSER_PRD.md` B1–B24): our toolbar
//! over the system web view, and the relay its requests go through.
//!
//! ```text
//! ┌──────────────────────────────────────────────────────────────────┐
//! │ ‹  ›  ⟳  [ localhost:3000                     ] Go  via studio  ⚙ │  GPUI
//! ├──────────────────────────────────────────────────────────────────┤
//! │                                                                  │
//! │      WKWebView, a native child view placed over this area        │  wry
//! │                                                                  │
//! └──────────────────────────────────────────────────────────────────┘
//! ```
//!
//! - **The network is the worker's.** The web view's data store sends
//!   everything through the session's [`Relay`] (SOCKS5 with a password,
//!   B23); loopback, which WKWebView never proxies, gets a forward first
//!   (B22): from the address field when typed (B24), from the navigation
//!   handler for links and redirects. A port in use here, `file://` (B12) and
//!   a page the worker could not reach are our own [`error_page`]s.
//! - **One profile per session** (B10): the data store identifier is
//!   [`super::profile_id`] of the worker and the session.
//! - **Placed every frame.** The web view is native and sits above GPUI, so
//!   the canvas under the toolbar places it each frame, and the window hides
//!   it whenever anything covers it or it is not on screen
//!   ([`BrowserView::set_shown`], B13).
//! - **Downloads** go to this computer's Downloads folder, under a free
//!   name; `window.open` opens in this tab.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use flightdeck::web::client::http::HttpAccess;
use flightdeck::web::client::relay::{Relay, RelayEvent};
use gpui::{
    canvas, div, px, AppContext, Bounds, Context, Entity, Focusable, InteractiveElement,
    IntoElement, ParentElement, Pixels, Render, StatefulInteractiveElement, Styled, Subscription,
    Task, Window,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{h_flex, v_flex};
use wry::dpi::{LogicalPosition, LogicalSize};
use wry::{NewWindowResponse, PageLoadEvent, Rect, WebView, WebViewBuilder};

use super::address::{self, Address, FILE_REFUSED};
use super::error_page;
use crate::theme::Palette;

/// How often the view looks at what the web view and the relay reported.
const POLL: Duration = Duration::from_millis(100);

/// What the web view's handlers report, read by the view's poll. The
/// handlers run on the main thread outside GPUI, so they only write here.
#[derive(Default)]
struct PageState {
    /// The page's address, as last started.
    url: String,
    /// The page's title.
    title: String,
    /// Our own page to show instead: a title and its detail.
    blocked: Option<(String, String)>,
    /// `window.open` asked for these; they open in this tab.
    opened: Vec<String>,
    /// Something above changed.
    dirty: bool,
}

/// The web view, shared by the view and the canvas that places it.
type Shared = Rc<RefCell<Option<WebView>>>;

/// See the module docs.
pub struct BrowserView {
    webview: Shared,
    relay: Option<Arc<Relay>>,
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
        match Relay::start(access, key) {
            Ok(relay) => {
                view.relay = Some(Arc::new(relay));
                view.build(profile, window);
            }
            Err(e) => view.failed = Some(format!("The browser could not start its relay: {e}")),
        }
        view
    }

    /// A local session's browser: this computer's own network, no relay and
    /// no proxy (B9), in the profile `profile`.
    pub fn new_direct(
        profile: [u8; 16],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> BrowserView {
        let mut view = Self::empty(None, window, cx);
        view.build(profile, window);
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
            relay: None,
            via,
            address,
            pending_address: None,
            note: None,
            page: Rc::default(),
            placed: Rc::default(),
            shown: false,
            failed: None,
            _poll,
            _address_events,
        }
    }

    fn build(&mut self, profile: [u8; 16], window: &mut Window) {
        use wry::WebViewBuilderExtDarwin;
        let relay = self.relay.clone();
        let nav_page = self.page.clone();
        let nav_relay = relay.clone();
        let load_page = self.page.clone();
        let title_page = self.page.clone();
        let open_page = self.page.clone();
        let built = WebViewBuilder::new()
            .with_bounds(Rect {
                position: LogicalPosition::new(0, 0).into(),
                size: LogicalSize::new(1, 1).into(),
            })
            .with_visible(false)
            .with_devtools(true)
            .with_data_store_identifier(profile)
            .with_navigation_handler(move |url| {
                allow_navigation(&url, nav_relay.as_deref(), &nav_page)
            })
            .with_on_page_load_handler(move |event, url| {
                let mut page = load_page.borrow_mut();
                if matches!(event, PageLoadEvent::Started) && is_web(&url) {
                    page.url = url;
                }
                page.dirty = true;
            })
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
            .build_as_child(window);
        match built {
            Ok(webview) => {
                if let Some(relay) = &relay {
                    let (user, pass) = relay.credentials();
                    super::proxy_macos::install(&webview, relay.port(), user, pass);
                }
                let _ = webview.load_html(&start_page(self.via.as_deref()));
                *self.webview.borrow_mut() = Some(webview);
            }
            Err(e) => self.failed = Some(format!("The browser could not start: {e}")),
        }
    }

    /// Load what was typed into the address field (its Enter, or Go).
    pub fn go(&mut self, text: &str, cx: &mut Context<Self>) {
        self.note = None;
        match address::parse(text) {
            Address::Refused(why) => self.note = Some(why),
            Address::Load(url) => {
                if let (Some(port), Some(relay)) = (address::loopback_port(&url), &self.relay) {
                    if let Err(e) = relay.forward(port) {
                        self.show_error(
                            &format!("Can't show {}", shown_address(&url)),
                            &e.to_string(),
                        );
                        cx.notify();
                        return;
                    }
                }
                if let Some(webview) = self.webview.borrow().as_ref() {
                    let _ = webview.load_url(&url);
                }
            }
        }
        cx.notify();
    }

    /// Send new tunnels with `key`: the window reconnected and the worker
    /// granted a new one.
    pub fn set_key(&self, key: String) {
        if let Some(relay) = &self.relay {
            relay.set_key(key);
        }
    }

    /// Whether the window has the browser on screen and uncovered. A hidden
    /// web view is not drawn at all, so GPUI's overlays show over the area.
    pub fn set_shown(&mut self, shown: bool) {
        if self.shown == shown {
            return;
        }
        self.shown = shown;
        if let Some(webview) = self.webview.borrow().as_ref() {
            let _ = webview.set_visible(shown);
        }
        if shown {
            // Placed again on the next frame.
            self.placed.set(None);
        }
    }

    /// The page's title, for the sidebar row.
    pub fn title(&self) -> String {
        self.page.borrow().title.clone()
    }

    fn show_error(&self, title: &str, detail: &str) {
        if let Some(webview) = self.webview.borrow().as_ref() {
            let _ = webview.load_html(&error_page::page(title, detail, self.via.as_deref()));
        }
    }

    /// What the handlers and the relay reported since the last look.
    fn poll(&mut self, cx: &mut Context<Self>) {
        let (dirty, blocked, opened, url) = {
            let mut page = self.page.borrow_mut();
            (
                std::mem::take(&mut page.dirty),
                page.blocked.take(),
                std::mem::take(&mut page.opened),
                page.url.clone(),
            )
        };
        let mut changed = dirty;
        if let Some((title, detail)) = blocked {
            self.show_error(&title, &detail);
        }
        for url in opened {
            if allow_navigation(&url, self.relay.as_deref(), &self.page) {
                if let Some(webview) = self.webview.borrow().as_ref() {
                    let _ = webview.load_url(&url);
                }
            }
        }
        if let Some(relay) = self.relay.clone() {
            // A tunnel the worker refused for the page itself is our error
            // page; one for a script or an image is the page's own business.
            for event in relay.take_events() {
                let RelayEvent::Refused {
                    target, message, ..
                } = event;
                if targets_page(&url, &target) {
                    self.show_error(&format!("Can't reach {}", shown_address(&url)), &message);
                    changed = true;
                }
            }
        }
        if dirty && !url.is_empty() {
            self.pending_address = Some(shown_address(&url));
        }
        if changed {
            cx.notify();
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
                move |_, _, cx| entity.read(cx).back()
            }))
            .child(button("forward", "›", "Forward").on_click({
                let entity = entity.clone();
                move |_, _, cx| entity.read(cx).forward()
            }))
            .child(button("reload", "⟳", "Reload").on_click({
                let entity = entity.clone();
                move |_, _, cx| entity.read(cx).reload()
            }))
            .child(div().flex_1().min_w_0().child(Input::new(&self.address)))
            .child(button("go", "Go", "Load this address").on_click({
                let entity = entity.clone();
                let field = self.address.clone();
                move |_, _, cx| {
                    let text = field.read(cx).value().to_string();
                    entity.update(cx, |view, cx| view.go(&text, cx));
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
            .child(
                button("devtools", "DevTools", "Open the page's developer tools").on_click({
                    let entity = entity.clone();
                    move |_, _, cx| entity.read(cx).devtools()
                }),
            );
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
                let placed = self.placed.clone();
                canvas(
                    move |bounds, _, _| place(&webview, &placed, bounds),
                    |_, _, _, _| {},
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
            .children(note)
            .child(body)
    }
}

/// Delete browser profile `profile` (B19: its worktree was abandoned). Done
/// a moment later on the main thread: WebKit refuses to remove a data store
/// a web view still uses, and the views of the session that closed are
/// dropped at the end of this frame.
pub fn forget_profile(profile: [u8; 16], cx: &mut gpui::App) {
    cx.spawn(async move |cx| {
        cx.background_executor().timer(Duration::from_secs(1)).await;
        cx.update(|_| {
            use wry::WebViewExtDarwin;
            WebView::remove_data_store(&profile, |result| {
                if let Err(e) = result {
                    eprintln!("flightdeck-desktop: could not delete a browser profile: {e}");
                }
            });
        });
    })
    .detach();
}

/// Place the web view at `bounds` (window-relative logical pixels, as GPUI
/// lays out and wry places a child view).
fn place(webview: &Shared, placed: &Cell<Option<Bounds<Pixels>>>, bounds: Bounds<Pixels>) {
    if placed.get() == Some(bounds) {
        return;
    }
    placed.set(Some(bounds));
    if let Some(webview) = webview.borrow().as_ref() {
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
fn allow_navigation(url: &str, relay: Option<&Relay>, page: &RefCell<PageState>) -> bool {
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("about:") || lower.starts_with("data:") || lower.starts_with("blob:") {
        return true;
    }
    if lower.starts_with("file:") {
        block(page, "This page is not shown here", FILE_REFUSED);
        return false;
    }
    // A local browser's loopback is this computer's own (B9).
    if let (Some(port), Some(relay)) = (address::loopback_port(url), relay) {
        if let Err(e) = relay.forward(port) {
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
