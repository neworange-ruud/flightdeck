//! The macOS half of B21 (`specs/SESSION_BROWSER_PRD.md`): see a page fail on
//! an untrusted certificate, and accept that certificate once the person
//! chose **Continue anyway** for its `host:port`.
//!
//! WKWebView reports both to its navigation delegate, and `wry`'s delegate
//! class (`WryNavigationDelegate`) implements neither, so WebKit fails such a
//! page silently. This adds the two methods to that class at run time:
//!
//! - `webView:didReceiveAuthenticationChallenge:completionHandler:` — for a
//!   server-trust challenge whose `host:port` this web view was told to
//!   trust ([`allow`]), the certificate is accepted; everything else gets
//!   WebKit's own default handling, which fails an untrusted certificate.
//! - `webView:didFailProvisionalNavigation:withError:` — a load that failed
//!   on a certificate (`NSURLErrorDomain` −1200 … −1206) is recorded for the
//!   view to show our page for ([`take_failure`]).
//!
//! `class_addMethod` never replaces a method a class already has, so a later
//! `wry` that implements either keeps its own. The class is the live
//! delegate's, so its registered name does not matter. The state is per WKWebView
//! (keyed by its pointer) and main-thread only, as every delegate call is.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ffi::{c_char, CStr};

use block2::Block;
use objc2::encode::{Encoding, RefEncode};
use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
use objc2::{class, msg_send, sel};

/// A page that failed on its certificate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CertFailure {
    /// The address that failed.
    pub url: String,
    pub host: String,
    pub port: u16,
    /// WebKit's own words for what was wrong.
    pub message: String,
}

#[derive(Default)]
struct ViewTrust {
    allowed: HashSet<(String, u16)>,
    failure: Option<CertFailure>,
}

thread_local! {
    static VIEWS: RefCell<HashMap<usize, ViewTrust>> = RefCell::new(HashMap::new());
}

/// `SecTrustRef`, as WebKit hands it over: an opaque CoreFoundation pointer.
#[repr(C)]
struct SecTrust {
    _opaque: [u8; 0],
}

// SAFETY: the encoding of `struct __SecTrust *`, as Security declares it.
unsafe impl RefEncode for SecTrust {
    const ENCODING_REF: Encoding = Encoding::Pointer(&Encoding::Struct("__SecTrust", &[]));
}

fn key(webview: &wry::WebView) -> usize {
    use wry::WebViewExtMacOS;
    objc2::rc::Retained::as_ptr(&webview.webview()) as usize
}

/// Watch `webview`, trusting `allowed` (`host`, `port`) from the start.
pub fn register(webview: &wry::WebView, allowed: HashSet<(String, u16)>) {
    install(webview);
    VIEWS.with(|views| {
        views.borrow_mut().insert(
            key(webview),
            ViewTrust {
                allowed,
                failure: None,
            },
        )
    });
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

/// Add the two methods to the class of `webview`'s navigation delegate
/// (`wry`'s), once per class. The class is taken from the live delegate
/// rather than looked up by name, so a renamed class still works.
fn install(webview: &wry::WebView) {
    thread_local! {
        static DONE: RefCell<HashSet<usize>> = RefCell::new(HashSet::new());
    }
    use wry::WebViewExtMacOS;
    let wk = webview.webview();
    // SAFETY: `navigationDelegate` is a plain getter on a live WKWebView.
    let delegate: *mut AnyObject = unsafe { msg_send![&*wk, navigationDelegate] };
    if delegate.is_null() {
        return;
    }
    // SAFETY: a live object's class.
    let class = unsafe { (*delegate).class() } as *const AnyClass;
    let fresh = DONE.with(|done| done.borrow_mut().insert(class as usize));
    if !fresh {
        return;
    }
    let class = class as *mut AnyClass;
    // SAFETY: adding methods to an Objective-C class with implementations
    // whose signatures match the type strings given.
    unsafe {
        objc2::ffi::class_addMethod(
            class,
            sel!(webView:didReceiveAuthenticationChallenge:completionHandler:),
            std::mem::transmute::<ChallengeImp, Imp>(challenge),
            c"v@:@@@?".as_ptr(),
        );
        objc2::ffi::class_addMethod(
            class,
            sel!(webView:didFailProvisionalNavigation:withError:),
            std::mem::transmute::<FailImp, Imp>(failed),
            c"v@:@@@".as_ptr(),
        );
    }
}

type Handler = Block<dyn Fn(isize, *mut AnyObject)>;
type ChallengeImp =
    unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject, *mut AnyObject, *mut Handler);
type FailImp = unsafe extern "C-unwind" fn(
    *mut AnyObject,
    Sel,
    *mut AnyObject,
    *mut AnyObject,
    *mut AnyObject,
);

/// `NSURLSessionAuthChallengeUseCredential`.
const USE_CREDENTIAL: isize = 0;
/// `NSURLSessionAuthChallengePerformDefaultHandling`.
const DEFAULT_HANDLING: isize = 1;

unsafe extern "C-unwind" fn challenge(
    _this: *mut AnyObject,
    _cmd: Sel,
    webview: *mut AnyObject,
    challenge: *mut AnyObject,
    handler: *mut Handler,
) {
    // SAFETY: WebKit's challenge and protection space are valid objects for
    // the call, and `handler` is the completion block it must be given.
    unsafe {
        let space: *mut AnyObject = msg_send![challenge, protectionSpace];
        let method = text(msg_send![space, authenticationMethod]);
        let host = text(msg_send![space, host]).to_ascii_lowercase();
        let port: isize = msg_send![space, port];
        let trusted = method == "NSURLAuthenticationMethodServerTrust"
            && VIEWS.with(|views| {
                views
                    .borrow()
                    .get(&(webview as usize))
                    .is_some_and(|view| view.allowed.contains(&(host, port as u16)))
            });
        if trusted {
            let trust: *mut SecTrust = msg_send![space, serverTrust];
            let credential: *mut AnyObject =
                msg_send![class!(NSURLCredential), credentialForTrust: trust];
            (*handler).call((USE_CREDENTIAL, credential));
        } else {
            (*handler).call((DEFAULT_HANDLING, std::ptr::null_mut()));
        }
    }
}

unsafe extern "C-unwind" fn failed(
    _this: *mut AnyObject,
    _cmd: Sel,
    webview: *mut AnyObject,
    _navigation: *mut AnyObject,
    error: *mut AnyObject,
) {
    // SAFETY: WebKit's error is a valid NSError for the call.
    unsafe {
        let domain = text(msg_send![error, domain]);
        let code: isize = msg_send![error, code];
        if domain != "NSURLErrorDomain" || !(-1206..=-1200).contains(&code) {
            return;
        }
        let info: *mut AnyObject = msg_send![error, userInfo];
        let url_key: *mut AnyObject = msg_send![
            class!(NSString),
            stringWithUTF8String: c"NSErrorFailingURLStringKey".as_ptr()
        ];
        let mut url = if info.is_null() {
            String::new()
        } else {
            text(msg_send![info, objectForKey: url_key])
        };
        // Some errors carry only the NSURL form.
        if url.is_empty() && !info.is_null() {
            let nsurl_key: *mut AnyObject = msg_send![
                class!(NSString),
                stringWithUTF8String: c"NSErrorFailingURLKey".as_ptr()
            ];
            let nsurl: *mut AnyObject = msg_send![info, objectForKey: nsurl_key];
            if !nsurl.is_null() {
                url = text(msg_send![nsurl, absoluteString]);
            }
        }
        let message = text(msg_send![error, localizedDescription]);
        let Some((host, port)) = host_port(&url) else {
            return;
        };
        VIEWS.with(|views| {
            if let Some(view) = views.borrow_mut().get_mut(&(webview as usize)) {
                view.failure = Some(CertFailure {
                    url,
                    host,
                    port,
                    message,
                });
            }
        });
    }
}

/// An `NSString`'s text; empty for nil or anything else.
unsafe fn text(object: *mut AnyObject) -> String {
    if object.is_null() {
        return String::new();
    }
    // SAFETY: `UTF8String` on an NSString returns a NUL-terminated buffer
    // that lives as long as the string; it is copied out at once.
    unsafe {
        let is_string: bool = msg_send![object, isKindOfClass: class!(NSString)];
        if !is_string {
            return String::new();
        }
        let utf8: *const c_char = msg_send![object, UTF8String];
        if utf8.is_null() {
            String::new()
        } else {
            CStr::from_ptr(utf8).to_string_lossy().into_owned()
        }
    }
}

/// An `https://` address's host (lowercase, IPv6 unbracketed) and port.
pub fn host_port(url: &str) -> Option<(String, u16)> {
    let (scheme, rest) = url.split_once("://")?;
    if !scheme.eq_ignore_ascii_case("https") && !scheme.eq_ignore_ascii_case("wss") {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority.rsplit_once('@').map_or(authority, |(_, a)| a);
    let (host, port) = if let Some(after) = authority.strip_prefix('[') {
        let (host, tail) = after.split_once(']')?;
        (host, tail.strip_prefix(':'))
    } else {
        match authority.rsplit_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        }
    };
    let port = match port {
        Some(port) => port.parse().ok()?,
        None => 443,
    };
    Some((host.to_ascii_lowercase(), port))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_https_address_names_its_host_and_port() {
        assert_eq!(
            host_port("https://localhost:8443/a"),
            Some(("localhost".into(), 8443))
        );
        assert_eq!(
            host_port("https://Dev.Test/"),
            Some(("dev.test".into(), 443))
        );
        assert_eq!(host_port("https://[::1]:9443/"), Some(("::1".into(), 9443)));
        assert_eq!(
            host_port("wss://localhost:24678/hmr"),
            Some(("localhost".into(), 24678))
        );
        assert_eq!(host_port("http://localhost:3000"), None, "not TLS");
        assert_eq!(host_port(""), None);
    }
}
