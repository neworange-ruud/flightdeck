//! Where a session browser's requests go, by platform
//! (`specs/SESSION_BROWSER_PRD.md` §4.1).
//!
//! - **macOS, worker session**: the session's own [`Relay`] — SOCKS5 with a
//!   password on the web view's data store, and loopback forwards, since
//!   WKWebView never proxies loopback (B22, B23).
//! - **macOS, local session**: nothing; the web view uses this computer's
//!   network (B9).
//! - **Windows, either**: the session's place in the app's one
//!   [`ProxyHub`](flightdeck::web::client::proxy_hub::ProxyHub), a worker
//!   route or a direct one. WebView2 shares one proxy across every browser,
//!   loopback included (`<-loopback>`), so there are no forwards.

#[cfg(target_os = "macos")]
use std::sync::Arc;

use flightdeck::web::client::http::HttpAccess;
#[cfg(target_os = "windows")]
use flightdeck::web::client::proxy_hub::HubSession;
#[cfg(target_os = "macos")]
use flightdeck::web::client::relay::Relay;
use flightdeck::web::client::relay::{ForwardError, RelayEvent};

/// See the module docs.
pub enum Net {
    #[cfg(target_os = "macos")]
    Relay(Arc<Relay>),
    #[cfg(target_os = "windows")]
    Hub(HubSession),
    /// A macOS local session: the web view's own network.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Direct,
}

impl Net {
    /// Make loopback `port` reach the worker before a page asks for it:
    /// a forward on macOS (refused while this computer uses the port);
    /// nothing to do on Windows, where loopback goes through the hub.
    pub fn forward(&self, port: u16) -> Result<(), ForwardError> {
        match self {
            #[cfg(target_os = "macos")]
            Net::Relay(relay) => relay.forward(port),
            _ => {
                let _ = port;
                Ok(())
            }
        }
    }

    /// The worker granted a new tunnel key (the window reconnected).
    pub fn set_key(&self, key: String) {
        match self {
            #[cfg(target_os = "macos")]
            Net::Relay(relay) => relay.set_key(key),
            #[cfg(target_os = "windows")]
            Net::Hub(session) => session.set_key(key),
            Net::Direct => {
                let _ = key;
            }
        }
    }

    /// What the relay or the hub saw since the last call.
    pub fn take_events(&self) -> Vec<RelayEvent> {
        match self {
            #[cfg(target_os = "macos")]
            Net::Relay(relay) => relay.take_events(),
            #[cfg(target_os = "windows")]
            Net::Hub(session) => session.take_events(),
            Net::Direct => Vec::new(),
        }
    }
}

/// A worker session's way out: its own relay (macOS), or a worker route in
/// the app's hub (Windows). The error is said to the person.
pub fn worker_net(access: HttpAccess, key: String) -> Result<Net, String> {
    #[cfg(target_os = "macos")]
    return Relay::start(access, key)
        .map(|relay| Net::Relay(Arc::new(relay)))
        .map_err(|e| format!("The browser could not start its relay: {e}"));
    #[cfg(target_os = "windows")]
    return super::windows::hub().map(|hub| Net::Hub(hub.worker_session(access, key)));
}

/// A local session's way out: the web view's own network (macOS), or the
/// hub's direct route (Windows), every WebView2 browser having the hub as
/// its proxy.
pub fn direct_net() -> Result<Net, String> {
    #[cfg(target_os = "macos")]
    return Ok(Net::Direct);
    #[cfg(target_os = "windows")]
    return super::windows::hub().map(|hub| Net::Hub(hub.direct_session()));
}
