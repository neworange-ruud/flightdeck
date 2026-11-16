//! The Windows half of the session browser (`specs/SESSION_BROWSER_PRD.md`
//! §4.1, "The Windows design"; `remote-control-cefv.12.2`).
//!
//! WebView2 fixes a browser process's arguments, its proxy among them, when
//! the first web view of a data folder starts. So every session browser in
//! the app shares one data folder ([`data_dir`]), one argument line
//! ([`browser_args`]) and so one proxy: the app's [`hub`], with
//! `--proxy-bypass-list=<-loopback>` so loopback goes through it too. Each
//! session browses in a profile of its own ([`profile_name`], B10), and
//! answers the hub's login with its session's username and password
//! ([`answer_hub_login`]); WebView2 caches that login per profile, so it
//! never reaches another session's route (the `cefv.11` spike).

use std::path::PathBuf;
use std::sync::OnceLock;

use flightdeck::web::client::proxy_hub::ProxyHub;
use webview2_com::Microsoft::Web::WebView2::Win32::{ICoreWebView2, ICoreWebView2_10};
use webview2_com::{take_pwstr, BasicAuthenticationRequestedEventHandler};
use windows::core::{Interface, HSTRING, PWSTR};

/// The app's one proxy hub, started on first use, which is also before
/// any web view starts: profiles left to delete go first (B19).
pub fn hub() -> Result<&'static ProxyHub, String> {
    static HUB: OnceLock<Result<ProxyHub, String>> = OnceLock::new();
    HUB.get_or_init(|| {
        if let (Some(data), Some(pending)) = (data_dir(), forget_list()) {
            super::profile_dirs::sweep(&data, &pending);
        }
        ProxyHub::start().map_err(|e| e.to_string())
    })
    .as_ref()
    .map_err(|e| format!("The browser could not start its proxy: {e}"))
}

/// The GPUI window a web view is built in, by its handle alone, so it can
/// be built outside GPUI's borrow of the app (see `BrowserView::build`).
pub struct ParentWindow(wry::raw_window_handle::RawWindowHandle);

impl ParentWindow {
    /// `window`'s handle.
    pub fn of(window: &gpui::Window) -> Result<ParentWindow, String> {
        // `Window` has a `window_handle` of its own (GPUI's handle).
        wry::raw_window_handle::HasWindowHandle::window_handle(window)
            .map(|handle| ParentWindow(handle.as_raw()))
            .map_err(|e| format!("The browser could not find its window: {e}"))
    }
}

impl wry::raw_window_handle::HasWindowHandle for ParentWindow {
    fn window_handle(
        &self,
    ) -> Result<wry::raw_window_handle::WindowHandle<'_>, wry::raw_window_handle::HandleError> {
        // SAFETY: the handle is of a GPUI window, which outlives the task
        // that builds in it; a window closed meanwhile makes the build fail,
        // and the view that would own the web view is gone with it.
        Ok(unsafe { wry::raw_window_handle::WindowHandle::borrow_raw(self.0) })
    }
}

/// The data folder every session browser shares.
pub fn data_dir() -> Option<PathBuf> {
    super::flightdeck_dir().map(|dir| dir.join("browser").join("webview2"))
}

/// Where profiles that could not be deleted yet are listed.
fn forget_list() -> Option<PathBuf> {
    super::flightdeck_dir().map(|dir| dir.join("browser").join("webview2-forget.txt"))
}

/// Delete `profile`'s WebView2 data now if WebView2 has let go of it, else
/// when the app next starts (B19, `super::profile_dirs`).
pub fn forget_profile(profile: &[u8; 16]) {
    if let (Some(data), Some(pending)) = (data_dir(), forget_list()) {
        super::profile_dirs::forget(&data, &pending, &profile_name(profile));
    }
}

/// WebView2's argument line: the hub as proxy, loopback through it too.
/// The same for every web view, as WebView2 requires within a data folder.
pub fn browser_args(hub_port: u16) -> String {
    format!(
        "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection \
         --proxy-server=http://127.0.0.1:{hub_port} --proxy-bypass-list=<-loopback>"
    )
}

/// A session's WebView2 profile name: letters and digits, as WebView2 wants.
pub fn profile_name(profile: &[u8; 16]) -> String {
    format!("fd{}", super::trust::profile_key(profile))
}

/// Whether an authentication request for `uri` is the hub's own 407 (on
/// `127.0.0.1:hub_port`) and not a page asking for a password, which must
/// never be given a session's login.
pub fn is_hub_challenge(uri: &str, hub_port: u16) -> bool {
    let uri = uri.trim();
    let rest = match uri.get(..7) {
        Some(scheme) if scheme.eq_ignore_ascii_case("http://") => &uri[7..],
        _ => uri,
    };
    rest.trim_end_matches('/') == format!("127.0.0.1:{hub_port}")
}

/// Answer the hub's login for `webview` with `username` / `password`. A
/// page's own login request is left to WebView2's dialog.
pub fn answer_hub_login(
    webview: &wry::WebView,
    hub_port: u16,
    username: String,
    password: String,
) -> Result<(), String> {
    use wry::WebViewExtWindows;
    let core: ICoreWebView2 = webview.webview();
    let core: ICoreWebView2_10 = core
        .cast()
        .map_err(|e| format!("This WebView2 runtime is too old for the session browser: {e}"))?;
    let handler = BasicAuthenticationRequestedEventHandler::create(Box::new(move |_, args| {
        let Some(args) = args else { return Ok(()) };
        // SAFETY: WebView2 COM calls on its own callback's arguments, on the
        // UI thread it calls us on.
        unsafe {
            let mut uri = PWSTR::null();
            args.Uri(&mut uri)?;
            if !is_hub_challenge(&take_pwstr(uri), hub_port) {
                return Ok(());
            }
            let response = args.Response()?;
            response.SetUserName(&HSTRING::from(username.as_str()))?;
            response.SetPassword(&HSTRING::from(password.as_str()))?;
        }
        Ok(())
    }));
    let mut token = 0i64;
    // SAFETY: registering a handler, which WebView2 keeps alive itself.
    unsafe { core.add_BasicAuthenticationRequested(&handler, &mut token) }
        .map_err(|e| format!("The browser could not log in to its proxy: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_hubs_own_challenge_gets_the_login() {
        assert!(is_hub_challenge("http://127.0.0.1:56992/", 56992));
        assert!(is_hub_challenge("http://127.0.0.1:56992", 56992));
        assert!(is_hub_challenge("HTTP://127.0.0.1:56992/", 56992));
        assert!(is_hub_challenge("127.0.0.1:56992", 56992));
        assert!(!is_hub_challenge("http://127.0.0.1:56993/", 56992));
        assert!(!is_hub_challenge("http://localhost:56992/", 56992));
        assert!(!is_hub_challenge("https://example.com/login", 56992));
        assert!(!is_hub_challenge("http://127.0.0.1:56992/private", 56992));
        assert!(!is_hub_challenge("", 56992));
    }

    #[test]
    fn every_browser_shares_one_argument_line_and_has_its_own_profile() {
        assert_eq!(browser_args(4000), browser_args(4000));
        assert!(browser_args(4000).contains("--proxy-server=http://127.0.0.1:4000"));
        assert!(browser_args(4000).contains("--proxy-bypass-list=<-loopback>"));
        let a = profile_name(&super::super::profile_id("box", "s1"));
        let b = profile_name(&super::super::profile_id("box", "s2"));
        assert_ne!(a, b);
        assert!(a.len() <= 64 && a.chars().all(|c| c.is_ascii_alphanumeric()));
    }
}
