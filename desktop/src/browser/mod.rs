//! The session browser in a remote window (`specs/SESSION_BROWSER_PRD.md`):
//! the system web view, drawn on this machine, whose every request the
//! worker makes.
//!
//! - [`address`]: what the address field's text means, and which URLs reach
//!   a loopback port the relay forwards first (B22, B24).
//! - [`error_page`]: our own page for what the worker or the relay could not
//!   do (§5).
//! - `view` (macOS, Windows): the [`view::BrowserView`] — toolbar, web view
//!   and where its requests go ([`net`]).
//! - `proxy_macos`: the data-store proxy with credentials (B23).
//! - `host_macos`: the native view the web view lives in, so DevTools docks
//!   inside the browser, and the keyboard's way back to GPUI.
//! - `windows`: WebView2's shared data folder, its proxy (the app's
//!   [`flightdeck::web::client::proxy_hub::ProxyHub`]) and its login
//!   (`specs/SESSION_BROWSER_PRD.md` §4.1, "The Windows design").
//!
//! Which tabs exist is the window's model's ([`tabs`]).

pub mod tabs;

// Only the web view (macOS and Windows) reads addresses and draws error
// pages.
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub mod address;
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub mod cert;
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub mod error_page;
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub mod net;
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub mod smoke;
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub mod trust;
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub mod view;

#[cfg(target_os = "macos")]
mod host_macos;
#[cfg(target_os = "macos")]
mod proxy_macos;
#[cfg(target_os = "macos")]
mod tls_macos;

#[cfg(any(test, target_os = "windows"))]
mod profile_dirs;
#[cfg(target_os = "windows")]
mod tls_windows;
#[cfg(target_os = "windows")]
mod windows;

/// Delete the browser profiles of abandoned worktrees that WebView2 still
/// held last time (B19). Windows only; call at app start, before any
/// window opens.
#[cfg(target_os = "windows")]
pub fn sweep_forgotten_profiles() {
    windows::sweep_forgotten();
}

/// Why this computer cannot run a session browser, or `None` when it can.
///
/// - macOS needs 14 or later (B20): the proxy is set on the web view's data
///   store, which older WebKit cannot do.
/// - Windows needs nothing more: WebView2's proxy is the app's hub, with a
///   login per session (§4.1). A missing WebView2 runtime is the view's own
///   error.
/// - Linux is postponed (B7).
pub fn unavailable() -> Option<&'static str> {
    #[cfg(target_os = "macos")]
    {
        if proxy_macos::supported() {
            None
        } else {
            Some(NEEDS_MACOS_14)
        }
    }
    #[cfg(target_os = "windows")]
    {
        None
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Some(NOT_ON_LINUX)
    }
}

/// Why this computer cannot run a **local** session's browser, or `None`
/// when it can. A local browser needs no proxy on macOS (B9), so any macOS
/// will do; on Windows it goes through the hub's direct route.
pub fn unavailable_here() -> Option<&'static str> {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        None
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Some(NOT_ON_LINUX)
    }
}

/// Why a Mac older than 14 has no session browser (B20).
#[cfg(target_os = "macos")]
pub const NEEDS_MACOS_14: &str = "The session browser needs macOS 14 or later.";

/// Why Linux has no session browser (B7).
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub const NOT_ON_LINUX: &str = "The session browser is not available on Linux yet.";

/// FlightDeck's own folder, `~/.flightdeck` (`%USERPROFILE%\.flightdeck`
/// on Windows), as the rest of FlightDeck finds it.
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub(crate) fn flightdeck_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .or_else(|| std::env::var_os("USERPROFILE").filter(|h| !h.is_empty()))
        .map(|home| std::path::PathBuf::from(home).join(".flightdeck"))
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
/// A 16-byte data store identifier for a session's browser profile (B10):
/// the same for the same worker and session, different for every other, so
/// cookies and storage never cross worktrees.
pub fn profile_id(worker: &str, session: &str) -> [u8; 16] {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(format!("flightdeck-session-browser\0{worker}\0{session}"));
    let mut id = [0u8; 16];
    id.copy_from_slice(&digest[..16]);
    id
}

#[cfg(all(test, any(target_os = "macos", target_os = "windows")))]
mod tests {
    use super::*;

    #[test]
    fn a_profile_belongs_to_one_worker_and_session() {
        let a = profile_id("box:7420", "s1");
        assert_eq!(a, profile_id("box:7420", "s1"));
        assert_ne!(a, profile_id("box:7420", "s2"));
        assert_ne!(a, profile_id("other:7420", "s1"));
        // The separator keeps "a"+"bc" and "ab"+"c" apart.
        assert_ne!(profile_id("a", "bc"), profile_id("ab", "c"));
    }
}
