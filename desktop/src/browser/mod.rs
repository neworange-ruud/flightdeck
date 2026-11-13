//! The session browser in a remote window (`specs/SESSION_BROWSER_PRD.md`):
//! the system web view, drawn on this machine, whose every request the
//! worker makes.
//!
//! - [`address`]: what the address field's text means, and which URLs reach
//!   a loopback port the relay forwards first (B22, B24).
//! - [`error_page`]: our own page for what the worker or the relay could not
//!   do (§5).
//! - `view` (macOS): the [`view::BrowserView`] — toolbar, web view and the
//!   relay it sends through.
//! - `proxy_macos`: the data-store proxy with credentials (B23).
//!
//! Which tabs exist is the window's model's ([`tabs`]).

pub mod tabs;

// Only the web view (macOS for now) reads addresses and draws error pages.
#[cfg(target_os = "macos")]
pub mod address;
#[cfg(target_os = "macos")]
pub mod error_page;

#[cfg(target_os = "macos")]
mod proxy_macos;
#[cfg(target_os = "macos")]
pub mod smoke;
#[cfg(target_os = "macos")]
mod tls_macos;
#[cfg(target_os = "macos")]
pub mod trust;
#[cfg(target_os = "macos")]
pub mod view;

/// Why this computer cannot run a session browser, or `None` when it can.
///
/// - macOS needs 14 or later (B20): the proxy is set on the web view's data
///   store, which older WebKit cannot do.
/// - Windows waits for its spike (`remote-control-cefv.8`): WebView2 cannot
///   send the relay a password, so the relay could not keep other programs
///   out yet.
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
        Some(NOT_ON_WINDOWS_YET)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Some(NOT_ON_LINUX)
    }
}

/// Why this computer cannot run a **local** session's browser, or `None`
/// when it can. A local browser needs no proxy (B9), so any macOS will do;
/// the view itself is macOS-only for now.
pub fn unavailable_here() -> Option<&'static str> {
    #[cfg(target_os = "macos")]
    {
        None
    }
    #[cfg(target_os = "windows")]
    {
        Some(NOT_ON_WINDOWS_YET)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Some(NOT_ON_LINUX)
    }
}

/// Why a Mac older than 14 has no session browser (B20).
#[cfg(target_os = "macos")]
pub const NEEDS_MACOS_14: &str = "The session browser needs macOS 14 or later.";

/// Why Windows has no session browser yet.
#[cfg(target_os = "windows")]
pub const NOT_ON_WINDOWS_YET: &str = "The session browser is not available on Windows yet.";

/// Why Linux has no session browser (B7).
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub const NOT_ON_LINUX: &str = "The session browser is not available on Linux yet.";

#[cfg(target_os = "macos")]
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

#[cfg(all(test, target_os = "macos"))]
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
