//! The Windows half of B21 (`specs/SESSION_BROWSER_PRD.md`;
//! `remote-control-cefv.12.3`): see a page fail on an untrusted
//! certificate, and accept that certificate once the person chose
//! **Continue anyway** for its `host:port`. The same calls as `tls_macos`.
//!
//! WebView2 raises `ServerCertificateErrorDetected` for a certificate it
//! cannot verify, for the page and for every resource on it, and lets the
//! handler decide. A `host:port` this web view was told to trust
//! ([`allow`]) is allowed (WebView2 remembers that for the session);
//! anything else is cancelled and recorded ([`take_failure`]), and the view
//! shows our page with Continue anyway when it is the page's own
//! (`cert::is_page_failure`).
//!
//! The state is per web view (keyed by its `ICoreWebView2`) and main-thread
//! only, as WebView2 calls its handlers there.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use webview2_com::Microsoft::Web::WebView2::Win32::{
    ICoreWebView2, ICoreWebView2_14, COREWEBVIEW2_SERVER_CERTIFICATE_ERROR_ACTION_ALWAYS_ALLOW,
    COREWEBVIEW2_SERVER_CERTIFICATE_ERROR_ACTION_CANCEL, COREWEBVIEW2_WEB_ERROR_STATUS,
    COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_COMMON_NAME_IS_INCORRECT,
    COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_EXPIRED,
    COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_IS_INVALID,
    COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_REVOKED,
};
use webview2_com::{take_pwstr, ServerCertificateErrorDetectedEventHandler};
use windows::core::{Interface, PWSTR};

use super::cert::host_port;
pub use super::cert::CertFailure;

#[derive(Default)]
struct ViewTrust {
    allowed: HashSet<(String, u16)>,
    failure: Option<CertFailure>,
}

thread_local! {
    static VIEWS: RefCell<HashMap<usize, ViewTrust>> = RefCell::new(HashMap::new());
}

fn key_of(core: &ICoreWebView2) -> usize {
    core.as_raw() as usize
}

fn key(webview: &wry::WebView) -> usize {
    use wry::WebViewExtWindows;
    key_of(&webview.webview())
}

/// Watch `webview`, trusting `allowed` (`host`, `port`) from the start. A
/// runtime too old for the event leaves WebView2's own handling: an
/// untrusted certificate fails, with no Continue anyway.
pub fn register(webview: &wry::WebView, allowed: HashSet<(String, u16)>) {
    use wry::WebViewExtWindows;
    let core = webview.webview();
    let key = key_of(&core);
    VIEWS.with(|views| {
        views.borrow_mut().insert(
            key,
            ViewTrust {
                allowed,
                failure: None,
            },
        )
    });
    let Ok(core) = core.cast::<ICoreWebView2_14>() else {
        return;
    };
    let handler = ServerCertificateErrorDetectedEventHandler::create(Box::new(move |_, args| {
        let Some(args) = args else { return Ok(()) };
        // SAFETY: WebView2 COM calls on its own callback's arguments, on the
        // UI thread it calls us on.
        unsafe {
            let mut uri = PWSTR::null();
            args.RequestUri(&mut uri)?;
            let url = take_pwstr(uri);
            let mut status = COREWEBVIEW2_WEB_ERROR_STATUS::default();
            args.ErrorStatus(&mut status)?;
            let target = host_port(&url);
            let trusted = target.as_ref().is_some_and(|target| {
                VIEWS.with(|views| {
                    views
                        .borrow()
                        .get(&key)
                        .is_some_and(|view| view.allowed.contains(target))
                })
            });
            if trusted {
                args.SetAction(COREWEBVIEW2_SERVER_CERTIFICATE_ERROR_ACTION_ALWAYS_ALLOW)?;
                return Ok(());
            }
            args.SetAction(COREWEBVIEW2_SERVER_CERTIFICATE_ERROR_ACTION_CANCEL)?;
            if let Some((host, port)) = target {
                VIEWS.with(|views| {
                    if let Some(view) = views.borrow_mut().get_mut(&key) {
                        view.failure = Some(CertFailure {
                            url,
                            host,
                            port,
                            message: describe(status),
                        });
                    }
                });
            }
        }
        Ok(())
    }));
    let mut token = 0i64;
    // SAFETY: registering a handler, which WebView2 keeps alive itself.
    let _ = unsafe { core.add_ServerCertificateErrorDetected(&handler, &mut token) };
}

/// Stop watching `webview` (it is being dropped).
pub fn unregister(webview: &wry::WebView) {
    VIEWS.with(|views| views.borrow_mut().remove(&key(webview)));
}

/// Trust `host:port`'s certificate in `webview` from now on.
pub fn allow(webview: &wry::WebView, host: &str, port: u16) {
    VIEWS.with(|views| {
        if let Some(view) = views.borrow_mut().get_mut(&key(webview)) {
            view.allowed.insert((host.to_ascii_lowercase(), port));
        }
    });
}

/// The certificate failure `webview` reported since the last call.
pub fn take_failure(webview: &wry::WebView) -> Option<CertFailure> {
    VIEWS.with(|views| {
        views
            .borrow_mut()
            .get_mut(&key(webview))
            .and_then(|view| view.failure.take())
    })
}

/// WebView2's error status, in the words our page uses.
fn describe(status: COREWEBVIEW2_WEB_ERROR_STATUS) -> String {
    match status {
        s if s == COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_COMMON_NAME_IS_INCORRECT => {
            "The certificate is for another name.".to_string()
        }
        s if s == COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_EXPIRED => {
            "The certificate has expired.".to_string()
        }
        s if s == COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_REVOKED => {
            "The certificate was revoked.".to_string()
        }
        s if s == COREWEBVIEW2_WEB_ERROR_STATUS_CERTIFICATE_IS_INVALID => {
            "The certificate is not valid.".to_string()
        }
        _ => "The certificate could not be verified.".to_string(),
    }
}
