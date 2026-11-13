//! The macOS half of B23 (`specs/SESSION_BROWSER_PRD.md`): point a web view's
//! data store at the session's relay, with the relay's username and password.
//!
//! `wry`'s own `with_proxy_config` cannot carry credentials, so this builds
//! the Network.framework configuration itself and sets it as the store's
//! `proxyConfigurations` (macOS 14+, B20). Network.framework never falls back
//! to a direct connection unless told to, so a dead relay is an error, never
//! a page quietly loaded from this Mac's network. Loopback never goes through
//! it (B22): the relay's forwards handle that.

use std::ffi::{c_char, c_void, CString};

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};

type NwObject = *mut c_void;

#[link(name = "Network", kind = "framework")]
extern "C" {
    fn nw_endpoint_create_host(hostname: *const c_char, port: *const c_char) -> NwObject;
    fn nw_proxy_config_create_socksv5(endpoint: NwObject) -> NwObject;
    fn nw_proxy_config_set_username_and_password(
        config: NwObject,
        username: *const c_char,
        password: *const c_char,
    );
}

/// Send everything `webview` loads (loopback aside) through the SOCKS5 relay
/// on `127.0.0.1:port`, as `username` / `password`. Call before the first
/// load.
pub fn install(webview: &wry::WebView, port: u16, username: &str, password: &str) {
    use wry::WebViewExtMacOS;
    let (Ok(host), Ok(port), Ok(user), Ok(pass)) = (
        CString::new("127.0.0.1"),
        CString::new(port.to_string()),
        CString::new(username),
        CString::new(password),
    ) else {
        return;
    };
    // SAFETY: Network.framework C calls with valid C strings; the returned
    // objects are OS objects, which an NSArray retains. The KVC key is the
    // documented `WKWebsiteDataStore.proxyConfigurations` (macOS 14+).
    unsafe {
        let endpoint = nw_endpoint_create_host(host.as_ptr(), port.as_ptr());
        let config = nw_proxy_config_create_socksv5(endpoint);
        nw_proxy_config_set_username_and_password(config, user.as_ptr(), pass.as_ptr());
        let wk = webview.webview();
        let wk: &AnyObject = &wk;
        let configuration: Retained<AnyObject> = msg_send![wk, configuration];
        let store: Retained<AnyObject> = msg_send![&*configuration, websiteDataStore];
        let array: Retained<AnyObject> =
            msg_send![class!(NSArray), arrayWithObject: config as *mut AnyObject];
        let key: Retained<AnyObject> = msg_send![
            class!(NSString),
            stringWithUTF8String: c"proxyConfigurations".as_ptr()
        ];
        let _: () = msg_send![&*store, setValue: &*array, forKey: &*key];
    }
}

/// Whether this Mac can put a proxy on a web view's data store (macOS 14+).
pub fn supported() -> bool {
    // SAFETY: `NSProcessInfo.processInfo.operatingSystemVersion` is a plain
    // struct return of three integers.
    #[repr(C)]
    struct OsVersion {
        major: isize,
        minor: isize,
        patch: isize,
    }
    unsafe impl objc2::Encode for OsVersion {
        const ENCODING: objc2::Encoding =
            objc2::Encoding::Struct("?", &[isize::ENCODING, isize::ENCODING, isize::ENCODING]);
    }
    unsafe {
        let info: Retained<AnyObject> = msg_send![class!(NSProcessInfo), processInfo];
        let version: OsVersion = msg_send![&*info, operatingSystemVersion];
        version.major >= 14
    }
}
