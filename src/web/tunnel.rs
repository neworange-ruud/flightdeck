//! Browser tunnels: a worker making TCP connections on behalf of the session
//! browser in a FlightDeck Desktop window (`specs/SESSION_BROWSER_PRD.md`
//! B4, B14; `remote-control-cefv.2`).
//!
//! The window asks first, over its socket
//! ([`crate::web::protocol::worker_command::OPEN_BROWSER_TUNNEL`]), and is
//! granted a **key**. Each connection the browser then makes is one
//! `GET /tunnel` on the worker's port, carrying the window's cookie, the key
//! and the `host:port` to reach. The worker resolves and connects **from the
//! worker**, answers `101 Switching Protocols`, and pipes bytes both ways
//! until either side closes. TLS to the target stays end to end: the worker
//! only ever sees the bytes.
//!
//! A key, unlike an upload or download ticket, is **reusable**: a page opens
//! many connections. It is bound to the credential that asked and to the
//! window's connection, and it ends — taking every open tunnel with it — when
//! the window closes it, when that connection ends (a detach, or the worker
//! taken over by another window, W17), or when the worker stops.
//!
//! This module is the bookkeeping, kept free of sockets so it is unit tested:
//! [`TunnelGrants`] and [`parse_target`]. The route is
//! `web::server::tunnel_route`.

use std::collections::HashMap;

use tokio::sync::watch;

use crate::web::credentials::TokenId;
use crate::web::protocol::ViewerId;

/// The route a worker serves tunnels on.
pub const TUNNEL_PATH: &str = "/tunnel";

/// The `Upgrade` token a tunnel asks for.
pub const TUNNEL_UPGRADE: &str = "flightdeck-tunnel";

/// The header carrying the key.
pub const TUNNEL_KEY_HEADER: &str = "x-flightdeck-tunnel";

/// The header carrying the `host:port` to reach.
pub const TUNNEL_TARGET_HEADER: &str = "x-flightdeck-target";

/// The most keys one window holds at once (one per open browser tab, B6).
pub const MAX_KEYS_PER_VIEWER: usize = 32;

/// The most tunnels one key carries at once: far more than a page keeps open,
/// and a bound on what a runaway page can make the worker hold.
pub const MAX_TUNNELS_PER_KEY: usize = 256;

/// One granted key.
struct Grant {
    token: TokenId,
    viewer: ViewerId,
    /// Flips to `true` when the key ends; every open tunnel watches it.
    ended: watch::Sender<bool>,
    /// How many tunnels are open on it.
    open: usize,
}

/// Why a tunnel was not allowed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// No such key, or it is another credential's, or it ended.
    InvalidKey,
    /// [`MAX_TUNNELS_PER_KEY`] are open on it.
    TooMany,
}

/// Every key a worker has granted.
#[derive(Default)]
pub struct TunnelGrants {
    grants: HashMap<String, Grant>,
}

impl TunnelGrants {
    /// Grant `viewer`, which authenticated with `token`, a new key; `None`
    /// once it holds [`MAX_KEYS_PER_VIEWER`].
    pub fn grant(&mut self, token: &TokenId, viewer: &ViewerId) -> Option<String> {
        let held = self.grants.values().filter(|g| &g.viewer == viewer).count();
        if held >= MAX_KEYS_PER_VIEWER {
            return None;
        }
        let key = new_key();
        let (ended, _) = watch::channel(false);
        self.grants.insert(
            key.clone(),
            Grant {
                token: token.clone(),
                viewer: viewer.clone(),
                ended,
                open: 0,
            },
        );
        Some(key)
    }

    /// Open one tunnel on `key` for `token`: the viewer it belongs to, and a
    /// receiver that turns `true` when the key ends. Pair it with
    /// [`Self::closed`] when the tunnel ends.
    pub fn open(
        &mut self,
        key: &str,
        token: &TokenId,
    ) -> Result<(ViewerId, watch::Receiver<bool>), Refusal> {
        let grant = self.grants.get_mut(key).ok_or(Refusal::InvalidKey)?;
        if &grant.token != token {
            return Err(Refusal::InvalidKey);
        }
        if grant.open >= MAX_TUNNELS_PER_KEY {
            return Err(Refusal::TooMany);
        }
        grant.open += 1;
        Ok((grant.viewer.clone(), grant.ended.subscribe()))
    }

    /// One tunnel on `key` ended. A key already ended is a no-op.
    pub fn closed(&mut self, key: &str) {
        if let Some(grant) = self.grants.get_mut(key) {
            grant.open = grant.open.saturating_sub(1);
        }
    }

    /// End `key` for `token`, closing its tunnels; whether it was theirs.
    pub fn end(&mut self, key: &str, token: &TokenId) -> bool {
        match self.grants.get(key) {
            Some(grant) if &grant.token == token => {
                self.remove(key);
                true
            }
            _ => false,
        }
    }

    /// End every key `viewer` holds: its connection ended.
    pub fn end_viewer(&mut self, viewer: &ViewerId) {
        let keys: Vec<String> = self
            .grants
            .iter()
            .filter(|(_, g)| &g.viewer == viewer)
            .map(|(k, _)| k.clone())
            .collect();
        for key in keys {
            self.remove(&key);
        }
    }

    /// End every key (the worker stops).
    pub fn end_all(&mut self) {
        let keys: Vec<String> = self.grants.keys().cloned().collect();
        for key in keys {
            self.remove(&key);
        }
    }

    /// How many keys are granted.
    pub fn len(&self) -> usize {
        self.grants.len()
    }

    /// Whether no key is granted.
    pub fn is_empty(&self) -> bool {
        self.grants.is_empty()
    }

    fn remove(&mut self, key: &str) {
        if let Some(grant) = self.grants.remove(key) {
            let _ = grant.ended.send(true);
        }
    }
}

/// A fresh, unguessable key: 128 random bits, made as an upload ticket is.
fn new_key() -> String {
    crate::web::upload::new_ticket()
}

/// A `host:port` a tunnel may reach.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    /// A name, an IPv4 address, or an IPv6 address without its brackets.
    pub host: String,
    pub port: u16,
}

impl Target {
    /// `host:port`, bracketing an IPv6 address, as `TcpStream::connect` and
    /// a log line take it.
    pub fn address(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

/// Read a target header: `name:port`, `1.2.3.4:port` or `[::1]:port`. The
/// host must be non-empty, at most 253 bytes, and free of whitespace, `/`
/// and `@`; the port 1 to 65535.
pub fn parse_target(value: &str) -> Option<Target> {
    let value = value.trim();
    let (host, port) = if let Some(rest) = value.strip_prefix('[') {
        let (host, rest) = rest.split_once(']')?;
        (host, rest.strip_prefix(':')?)
    } else {
        let (host, port) = value.rsplit_once(':')?;
        if host.contains(':') {
            // An unbracketed IPv6 address is ambiguous with its port.
            return None;
        }
        (host, port)
    };
    let port: u16 = port.parse().ok().filter(|p| *p != 0)?;
    let bad = |c: char| c.is_whitespace() || c.is_control() || matches!(c, '/' | '@' | '[' | ']');
    if host.is_empty() || host.len() > 253 || host.chars().any(bad) {
        return None;
    }
    Some(Target {
        host: host.to_string(),
        port,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The same credential for the same `n` within one test.
    fn token(n: u8) -> TokenId {
        thread_local! {
            static TOKENS: std::cell::RefCell<HashMap<u8, TokenId>> = Default::default();
        }
        TOKENS.with(|t| {
            t.borrow_mut()
                .entry(n)
                .or_insert_with(TokenId::generate)
                .clone()
        })
    }

    fn viewer(name: &str) -> ViewerId {
        ViewerId::new(name)
    }

    #[test]
    fn a_key_opens_tunnels_for_its_own_credential_only() {
        let mut grants = TunnelGrants::default();
        let key = grants.grant(&token(1), &viewer("w")).unwrap();
        assert_eq!(key.len(), 22);
        let (who, ended) = grants.open(&key, &token(1)).unwrap();
        assert_eq!(who, viewer("w"));
        assert!(!*ended.borrow());
        assert_eq!(
            grants.open(&key, &token(2)).unwrap_err(),
            Refusal::InvalidKey
        );
        assert_eq!(
            grants.open("nope", &token(1)).unwrap_err(),
            Refusal::InvalidKey
        );
        // Reusable: a page opens many connections.
        assert!(grants.open(&key, &token(1)).is_ok());
    }

    #[test]
    fn keys_are_unguessable_and_distinct() {
        let mut grants = TunnelGrants::default();
        let a = grants.grant(&token(1), &viewer("w")).unwrap();
        let b = grants.grant(&token(1), &viewer("w")).unwrap();
        assert_ne!(a, b);
        assert!(a
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    }

    #[test]
    fn ending_a_key_ends_its_tunnels_and_refuses_new_ones() {
        let mut grants = TunnelGrants::default();
        let key = grants.grant(&token(1), &viewer("w")).unwrap();
        let (_, ended) = grants.open(&key, &token(1)).unwrap();
        assert!(!grants.end(&key, &token(2)), "not theirs to end");
        assert!(grants.end(&key, &token(1)));
        assert!(*ended.borrow(), "the open tunnel is told");
        assert_eq!(
            grants.open(&key, &token(1)).unwrap_err(),
            Refusal::InvalidKey
        );
    }

    #[test]
    fn a_window_leaving_ends_every_key_it_holds_and_no_other() {
        let mut grants = TunnelGrants::default();
        let mine = grants.grant(&token(1), &viewer("w1")).unwrap();
        let other = grants.grant(&token(1), &viewer("w2")).unwrap();
        let (_, ended) = grants.open(&mine, &token(1)).unwrap();
        grants.end_viewer(&viewer("w1"));
        assert!(*ended.borrow());
        assert!(grants.open(&mine, &token(1)).is_err());
        assert!(grants.open(&other, &token(1)).is_ok());
        grants.end_all();
        assert!(grants.is_empty());
    }

    #[test]
    fn keys_and_tunnels_are_capped() {
        let mut grants = TunnelGrants::default();
        for _ in 0..MAX_KEYS_PER_VIEWER {
            assert!(grants.grant(&token(1), &viewer("w")).is_some());
        }
        assert!(grants.grant(&token(1), &viewer("w")).is_none());
        assert!(
            grants.grant(&token(1), &viewer("w2")).is_some(),
            "per window"
        );

        let mut grants = TunnelGrants::default();
        let key = grants.grant(&token(1), &viewer("w")).unwrap();
        for _ in 0..MAX_TUNNELS_PER_KEY {
            grants.open(&key, &token(1)).unwrap();
        }
        assert_eq!(grants.open(&key, &token(1)).unwrap_err(), Refusal::TooMany);
        grants.closed(&key);
        assert!(grants.open(&key, &token(1)).is_ok(), "one closed, one fits");
    }

    #[test]
    fn targets_are_names_addresses_and_bracketed_ipv6() {
        let t = |host: &str, port| {
            Some(Target {
                host: host.to_string(),
                port,
            })
        };
        assert_eq!(parse_target("localhost:3000"), t("localhost", 3000));
        assert_eq!(parse_target(" example.com:443 "), t("example.com", 443));
        assert_eq!(parse_target("127.0.0.1:80"), t("127.0.0.1", 80));
        assert_eq!(parse_target("[::1]:5173"), t("::1", 5173));
        assert_eq!(parse_target("[::1]:5173").unwrap().address(), "[::1]:5173");
        assert_eq!(
            parse_target("localhost:3000").unwrap().address(),
            "localhost:3000"
        );
    }

    #[test]
    fn malformed_targets_are_refused() {
        for bad in [
            "",
            "localhost",
            "localhost:",
            ":80",
            "localhost:0",
            "localhost:65536",
            "localhost:http",
            "::1:80",
            "[::1]80",
            "a b:80",
            "user@host:80",
            "host/path:80",
            "host\r\nx:80",
        ] {
            assert_eq!(parse_target(bad), None, "{bad:?}");
        }
        let long = format!("{}:80", "a".repeat(254));
        assert_eq!(parse_target(&long), None);
    }
}
