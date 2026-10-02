//! A native client of FlightDeck Web: one FlightDeck controlling another
//! (`specs/DESKTOP_REMOTE_CONTROL_PLAN.md` §2.2).
//!
//! The browser SPA is one client of the embedded web server; this is the
//! second, written in Rust so FlightDeck Desktop can attach to a FlightDeck
//! running on another machine (TUI or Desktop) over a LAN or VPN and drive it
//! with its own native views. It speaks the same protocol as the browser, with
//! one difference the host grew for it in protocol v6: it browses
//! **independently** (R2) and names the session or terminal every command acts
//! on, so looking around never moves the host's selection.
//!
//! | Module | What it owns |
//! | --- | --- |
//! | [`exchange`] | `POST /auth/exchange` and `GET /auth/session` |
//! | [`link`] | the WebSocket task: attach, pump, reconnect, cursors |
//! | [`mirror`] | [`RemoteWorkspace`]: snapshot + deltas, local selection |
//! | [`input`] | the held-input queue and its one seq counter |
//! | [`terminals`] | [`StreamPty`]: a remote terminal behind `PtySession` |
//!
//! [`RemoteClient`] ties them together for a synchronous front-end: start it,
//! call [`RemoteClient::pump`] once per frame, read [`RemoteClient::workspace`].

pub mod exchange;
pub mod input;
pub mod link;
pub mod mirror;
pub mod terminals;

use std::sync::mpsc::{channel, Receiver};

use crate::contracts::TabId;
use crate::web::protocol::{
    command as names, Ack, AckOutcome, ConfigView, GitStatusView, SeatRequest, TerminalId,
    WireError,
};

pub use exchange::{exchange_code, probe_session, AccessToken, ExchangeError, Probe};
pub use input::AckFor;
pub use link::{LinkConfig, LinkEnd, LinkEvent, LinkHandle, LinkState, Outbound};
pub use mirror::RemoteWorkspace;
pub use terminals::{RemoteTerminals, StreamPty};

/// The web interface's default port (`[web] port`).
pub const DEFAULT_PORT: u16 = 7420;

/// The `User-Agent` this client sends, so the controlled instance lists it as
/// `FlightDeck Desktop/<version>` and can revoke it by name.
pub fn user_agent(app: &str, version: &str) -> String {
    format!("{app}/{version}")
}

/// Normalise what a person typed into `host:port`: a missing port becomes
/// [`DEFAULT_PORT`], and a pasted `http://…/` or `ws://…/ws` is trimmed to its
/// address. `None` when nothing usable is left.
pub fn normalize_address(raw: &str) -> Option<String> {
    let mut text = raw.trim();
    for scheme in ["http://", "https://", "ws://", "wss://"] {
        if let Some(rest) = text.strip_prefix(scheme) {
            text = rest;
        }
    }
    let text = text.split('/').next().unwrap_or("").trim();
    if text.is_empty() || text.contains(char::is_whitespace) {
        return None;
    }
    // `[v6]:port`, `[v6]`, a bare v6 address, `host:port`, or `host`.
    if let Some(rest) = text.strip_prefix('[') {
        let (host, after) = rest.split_once(']')?;
        let port = match after.strip_prefix(':') {
            Some(port) => port.parse::<u16>().ok()?,
            None if after.is_empty() => DEFAULT_PORT,
            None => return None,
        };
        return Some(format!("[{host}]:{port}"));
    }
    if text.matches(':').count() > 1 {
        return Some(format!("[{text}]:{DEFAULT_PORT}"));
    }
    match text.split_once(':') {
        Some((host, port)) if !host.is_empty() => {
            let port = port.parse::<u16>().ok().filter(|p| *p != 0)?;
            Some(format!("{host}:{port}"))
        }
        Some(_) => None,
        None => Some(format!("{text}:{DEFAULT_PORT}")),
    }
}

/// Whether the link to `address` is plain WebSocket over a network the user
/// may not trust. Loopback and Tailscale (its CGNAT range, its ULA prefix and
/// `*.ts.net` names) are exempt; everything else — a LAN address, a public
/// one, a WireGuard tunnel FlightDeck cannot recognise by range — gets the
/// one-time warning (plan §4).
pub fn link_is_unprotected(address: &str) -> bool {
    let host = match address.strip_prefix('[') {
        Some(rest) => rest.split(']').next().unwrap_or(""),
        None => address.rsplit_once(':').map_or(address, |(h, _)| h),
    };
    let host = host.to_ascii_lowercase();
    if host == "localhost" || host.ends_with(".ts.net") || host.ends_with(".ts.net.") {
        return false;
    }
    match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(v4)) => {
            let [a, b, ..] = v4.octets();
            let tailscale = a == 100 && (64..=127).contains(&b);
            !(v4.is_loopback() || tailscale)
        }
        Ok(std::net::IpAddr::V6(v6)) => {
            let s = v6.segments();
            let tailscale = s[0] == 0xfd7a && s[1] == 0x115c && s[2] == 0xa1e0;
            !(v6.is_loopback() || tailscale)
        }
        Err(_) => true,
    }
}

/// What a session-scoped command acts on (web protocol v6).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// A whole session.
    Session(TabId),
    /// One terminal (and through it, its session).
    Terminal(TerminalId),
}

impl Target {
    /// The command `args` that name this target.
    pub fn args(&self) -> serde_json::Value {
        match self {
            Target::Session(id) => serde_json::json!({ "session_id": id.0 }),
            Target::Terminal(id) => serde_json::json!({ "terminal_id": id.as_str() }),
        }
    }
}

/// One controlled instance, for a synchronous front-end.
pub struct RemoteClient {
    link: Option<LinkHandle>,
    events: Receiver<LinkEvent>,
    outbound: Outbound,
    workspace: RemoteWorkspace,
    terminals: RemoteTerminals,
    state: LinkState,
    results: Vec<Ack>,
    refused_input: Vec<String>,
    errors: Vec<WireError>,
    git_status: Option<GitStatusView>,
    configuration: Option<ConfigView>,
    /// The seq of an outstanding `request_snapshot`, so a burst of deltas the
    /// mirror cannot describe asks once.
    resync: Option<u64>,
}

impl RemoteClient {
    /// Start dialling. Nothing blocks: the link runs on the shared runtime.
    pub fn connect(config: LinkConfig) -> RemoteClient {
        let (tx, events) = channel();
        let (link, outbound) = LinkHandle::start(config, tx);
        RemoteClient {
            link: Some(link),
            events,
            outbound,
            workspace: RemoteWorkspace::new(),
            terminals: RemoteTerminals::new(),
            state: LinkState::Connecting,
            results: Vec::new(),
            refused_input: Vec::new(),
            errors: Vec::new(),
            git_status: None,
            configuration: None,
            resync: None,
        }
    }

    /// Apply everything the link has said since the last call. Returns whether
    /// anything a view draws changed — terminal bytes included.
    pub fn pump(&mut self) -> bool {
        let mut changed = false;
        while let Ok(event) = self.events.try_recv() {
            changed |= self.apply(event);
        }
        changed
    }

    fn apply(&mut self, event: LinkEvent) -> bool {
        match event {
            LinkEvent::State(state) => {
                let changed = state != self.state;
                self.state = state;
                changed
            }
            LinkEvent::Snapshot(snapshot) => {
                self.workspace.apply_snapshot(*snapshot);
                self.terminals.sync(self.workspace.terminals());
                self.resync = None;
                true
            }
            LinkEvent::Delta(delta) => {
                let applied = self.workspace.apply_delta(delta);
                if applied.resync && self.resync.is_none() {
                    self.resync = self.outbound.command(names::REQUEST_SNAPSHOT, None);
                }
                if applied.changed {
                    self.terminals.sync(self.workspace.terminals());
                }
                applied.changed
            }
            LinkEvent::Bytes(bytes) => {
                self.terminals.deliver(bytes);
                true
            }
            LinkEvent::StreamReset(id) => {
                self.terminals.reset(&id);
                true
            }
            LinkEvent::Ack(ack, AckFor::Command) => {
                if Some(ack.seq) != self.resync {
                    self.results.push(ack);
                }
                false
            }
            LinkEvent::Ack(ack, AckFor::Input) => {
                if ack.outcome == AckOutcome::Rejected {
                    if let Some(detail) = ack.detail {
                        self.refused_input.push(detail);
                    }
                }
                false
            }
            LinkEvent::GitStatus(view) => {
                self.git_status = Some(view);
                true
            }
            LinkEvent::Configuration(view) => {
                self.configuration = Some(view);
                true
            }
            LinkEvent::Error(error) => {
                self.errors.push(error);
                true
            }
        }
    }

    /// The link's state.
    pub fn state(&self) -> &LinkState {
        &self.state
    }

    /// The mirrored workspace.
    pub fn workspace(&self) -> &RemoteWorkspace {
        &self.workspace
    }

    /// The mirrored workspace, for moving this client's own selection.
    pub fn workspace_mut(&mut self) -> &mut RemoteWorkspace {
        &mut self.workspace
    }

    /// Every remote terminal's feed.
    pub fn terminals(&self) -> &RemoteTerminals {
        &self.terminals
    }

    /// A [`StreamPty`] for `terminal_id`.
    pub fn pty(&mut self, terminal_id: &TerminalId) -> StreamPty {
        self.terminals.pty(terminal_id, &self.outbound)
    }

    /// The way in, for whoever needs to send without the client at hand.
    pub fn outbound(&self) -> &Outbound {
        &self.outbound
    }

    /// Send a command. `None` when the link is down.
    pub fn command(&self, name: &str, args: Option<serde_json::Value>) -> Option<u64> {
        self.outbound.command(name, args)
    }

    /// Send a session-scoped command against `target` (protocol v6).
    pub fn command_for(&self, name: &str, target: &Target) -> Option<u64> {
        self.outbound.command(name, Some(target.args()))
    }

    /// Ask for a different seat: observe, write, or take the input lock.
    pub fn request_seat(&self, seat: SeatRequest) {
        self.outbound.attach(seat);
    }

    /// Command acks since the last call, in order.
    pub fn take_results(&mut self) -> Vec<Ack> {
        std::mem::take(&mut self.results)
    }

    /// The host's reasons for keystrokes it refused since the last call.
    pub fn take_refused_input(&mut self) -> Vec<String> {
        std::mem::take(&mut self.refused_input)
    }

    /// Error frames since the last call.
    pub fn take_errors(&mut self) -> Vec<WireError> {
        std::mem::take(&mut self.errors)
    }

    /// The git status panel the last `show_git_status` produced, once.
    pub fn take_git_status(&mut self) -> Option<GitStatusView> {
        self.git_status.take()
    }

    /// The configuration view the last `open_configuration` produced, once.
    pub fn take_configuration(&mut self) -> Option<ConfigView> {
        self.configuration.take()
    }

    /// Close the link and wait briefly for the socket to close.
    pub fn stop(mut self) {
        if let Some(link) = self.link.take() {
            link.stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_are_normalised_with_the_default_port() {
        let cases = [
            ("192.168.2.20", Some("192.168.2.20:7420")),
            ("192.168.2.20:8000", Some("192.168.2.20:8000")),
            (" http://studio.local:7420/ ", Some("studio.local:7420")),
            ("ws://studio.local:7420/ws", Some("studio.local:7420")),
            ("[fd7a:115c:a1e0::1]:7420", Some("[fd7a:115c:a1e0::1]:7420")),
            ("fd7a:115c:a1e0::1", Some("[fd7a:115c:a1e0::1]:7420")),
            ("host:notaport", None),
            ("host:0", None),
            (":7420", None),
            ("", None),
            ("two words", None),
        ];
        for (raw, want) in cases {
            assert_eq!(normalize_address(raw).as_deref(), want, "for {raw:?}");
        }
    }

    #[test]
    fn only_loopback_and_tailscale_skip_the_plain_websocket_warning() {
        for safe in [
            "127.0.0.1:7420",
            "localhost:7420",
            "100.101.102.103:7420",
            "studio.tail1234.ts.net:7420",
            "[::1]:7420",
            "[fd7a:115c:a1e0::5]:7420",
        ] {
            assert!(!link_is_unprotected(safe), "{safe}");
        }
        for warned in [
            "192.168.2.20:7420",
            "10.0.0.5:7420",
            "100.128.0.1:7420",
            "studio.local:7420",
            "[fe80::1]:7420",
        ] {
            assert!(link_is_unprotected(warned), "{warned}");
        }
    }

    #[test]
    fn a_target_names_its_session_or_terminal() {
        assert_eq!(
            Target::Session(TabId("t1".to_string())).args(),
            serde_json::json!({ "session_id": "t1" })
        );
        assert_eq!(
            Target::Terminal("t1:child:2".into()).args(),
            serde_json::json!({ "terminal_id": "t1:child:2" })
        );
    }
}
