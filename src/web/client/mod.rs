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
//! | [`store`] | `~/.flightdeck/remotes.json`: paired instances and their tokens |
//! | [`terminals`] | [`StreamPty`]: a remote terminal behind `PtySession` |
//! | [`views`] | the core's view structs, built from the mirror |
//! | [`worker`] | [`ProjectPicker`]: a worker's browse / scan / clone picker (v7) |
//! | [`config`] | [`RemoteConfigEditor`]: the host's configuration manager, staged here |
//!
//! [`RemoteClient`] ties them together for a synchronous front-end: start it,
//! call [`RemoteClient::pump`] once per frame, read [`RemoteClient::workspace`].
//!
//! Against a **worker** (v7, `specs/WORKER_PRD.md`) the client also sizes the
//! worker's PTYs, drives its project picker and restarts its shell. None of
//! that is ever sent to a TUI or Desktop host: every worker call checks
//! [`RemoteClient::is_worker`] first and sends nothing otherwise.

pub mod config;
pub mod exchange;
#[cfg(test)]
pub(crate) mod fake_host;
pub mod input;
pub mod link;
pub mod mirror;
pub mod store;
pub mod terminals;
pub mod views;
pub mod worker;

use std::sync::mpsc::{channel, Receiver};

use crate::contracts::{PtySize, TabId};
use crate::web::protocol::{
    command as names, worker_command, Ack, AckOutcome, ConfigView, DirListing, GitStatusView,
    HostMessage, NotificationView, ProjectId, ProjectScan, ResizeTerminal, SeatRequest, TerminalId,
    WireError,
};

pub use config::{EditorEffect, RemoteConfigEditor};
pub use exchange::{exchange_code, probe_session, AccessToken, ExchangeError, Probe};
pub use input::AckFor;
pub use link::{LinkConfig, LinkEnd, LinkEvent, LinkHandle, LinkOut, LinkState, Outbound};
pub use mirror::{RemoteWorkspace, WorkerMirror};
pub use terminals::{RemoteTerminals, StreamPty};
pub use worker::{CloneEvent, ProjectPicker};

/// The web interface's default port (`[web] port`).
pub const DEFAULT_PORT: u16 = 7420;

/// The `User-Agent` this client sends, so the controlled instance lists it as
/// `FlightDeck Desktop/<version>` and can revoke it by name.
pub fn user_agent(app: &str, version: &str) -> String {
    format!("{app}/{version}")
}

/// What this machine is called, for a worker's device list and its "taken
/// over by …" line (design A11): the host name, else `COMPUTERNAME` on
/// Windows, else "this computer".
pub fn device_name() -> String {
    hostname()
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "this computer".to_string())
}

#[cfg(unix)]
fn hostname() -> Option<String> {
    let mut buf = [0u8; 256];
    // SAFETY: the buffer is valid for its whole length, and `gethostname`
    // writes at most that many bytes into it.
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
    if rc != 0 {
        return None;
    }
    let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    Some(String::from_utf8_lossy(&buf[..end]).into_owned())
}

#[cfg(not(unix))]
fn hostname() -> Option<String> {
    std::env::var("COMPUTERNAME").ok()
}

/// The label this client attaches and pairs with: `{app}/{version}
/// ({device})`, e.g. `FlightDeck Desktop/1.4.0 (studio)`. A worker shows the
/// device; a TUI host's chip still reads the `{app}` prefix (design A11).
pub fn client_label(app: &str, version: &str) -> String {
    format!("{} ({})", user_agent(app, version), device_name())
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
    /// A project, acting on its own selected session.
    Project(ProjectId),
    /// A whole session.
    Session(TabId),
    /// One terminal (and through it, its session).
    Terminal(TerminalId),
}

impl Target {
    /// The command `args` that name this target.
    pub fn args(&self) -> serde_json::Value {
        match self {
            Target::Project(id) => serde_json::json!({ "project_id": id.as_str() }),
            Target::Session(id) => serde_json::json!({ "session_id": id.0 }),
            Target::Terminal(id) => serde_json::json!({ "terminal_id": id.as_str() }),
        }
    }
}

/// The hand-driven side of [`RemoteClient::scripted`].
pub struct Script {
    events: std::sync::mpsc::Sender<LinkEvent>,
    sent: tokio::sync::mpsc::UnboundedReceiver<link::LinkOut>,
    outbound: Outbound,
}

impl Script {
    /// Deliver a frame as the link would.
    pub fn send(&self, event: LinkEvent) {
        let _ = self.events.send(event);
    }

    /// Deliver a snapshot and go live, as the link does after an attach.
    pub fn snapshot(&self, snapshot: crate::web::protocol::Snapshot) {
        self.outbound.go_live_for_test();
        self.send(LinkEvent::State(LinkState::Live {
            latency_ms: Some(12),
        }));
        self.send(LinkEvent::Snapshot(Box::new(snapshot)));
    }

    /// Everything the client has sent since the last call.
    pub fn sent(&mut self) -> Vec<link::LinkOut> {
        let mut out = Vec::new();
        while let Ok(item) = self.sent.try_recv() {
            out.push(item);
        }
        out
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
    /// A worker's latest directory listing, until taken (v7).
    dir_listing: Option<DirListing>,
    /// A worker's latest project scan, until taken (v7).
    project_scan: Option<ProjectScan>,
    /// A worker's clone progress and results, until taken (v7).
    clone_events: Vec<CloneEvent>,
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
            dir_listing: None,
            project_scan: None,
            clone_events: Vec::new(),
            resync: None,
        }
    }

    /// A client with no socket behind it, driven by hand: what a link would
    /// have said goes in through the returned [`Script`], and what the client
    /// would have sent comes out of it. For front-end tests that draw a remote
    /// surface from recorded frames.
    pub fn scripted() -> (RemoteClient, Script) {
        let (tx, events) = channel();
        let (outbound, sent) = Outbound::detached();
        let client = RemoteClient {
            link: None,
            events,
            outbound: outbound.clone(),
            workspace: RemoteWorkspace::new(),
            terminals: RemoteTerminals::new(),
            state: LinkState::Connecting,
            results: Vec::new(),
            refused_input: Vec::new(),
            errors: Vec::new(),
            git_status: None,
            configuration: None,
            dir_listing: None,
            project_scan: None,
            clone_events: Vec::new(),
            resync: None,
        };
        (
            client,
            Script {
                events: tx,
                sent,
                outbound,
            },
        )
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
                self.terminals.sync(&self.workspace.streamed_terminals());
                self.resync = None;
                true
            }
            LinkEvent::Delta(delta) => {
                let applied = self.workspace.apply_delta(delta);
                if applied.resync && self.resync.is_none() {
                    self.resync = self.outbound.command(names::REQUEST_SNAPSHOT, None);
                }
                if applied.changed {
                    self.terminals.sync(&self.workspace.streamed_terminals());
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
            LinkEvent::DirListing(listing) => {
                self.dir_listing = Some(listing);
                true
            }
            LinkEvent::ProjectScan(scan) => {
                self.project_scan = Some(scan);
                true
            }
            LinkEvent::CloneProgress(progress) => {
                self.clone_events.push(CloneEvent::Progress(progress));
                true
            }
            LinkEvent::CloneResult(result) => {
                self.clone_events.push(CloneEvent::Result(result));
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

    /// Ask for the host's configuration manager (`open_configuration`, a plain
    /// read), resolved for `project` — the project this window is looking at,
    /// since it browses independently of the host's selection (v7). A v6 host
    /// ignores the project and answers for its own active one.
    pub fn open_configuration(&self, project: Option<&ProjectId>) -> Option<u64> {
        let args = project.map(|id| serde_json::json!({ "project_id": id.as_str() }));
        self.outbound.command(names::OPEN_CONFIGURATION, args)
    }

    /// Send staged configuration edits: save, then read back
    /// ([`RemoteConfigEditor`]'s [`EditorEffect::Send`]).
    pub fn save_configuration(
        &self,
        request: &crate::web::protocol::ConfigSaveRequest,
    ) -> Option<u64> {
        let args = serde_json::to_value(request).ok()?;
        self.outbound.command(names::OPEN_CONFIGURATION, Some(args))
    }

    // -- a worker host (v7) -------------------------------------------------

    /// Whether the host is a FlightDeck worker. Every call below sends
    /// nothing unless it is.
    pub fn is_worker(&self) -> bool {
        self.workspace.is_worker()
    }

    fn worker_command(&self, name: &str, args: Option<serde_json::Value>) -> Option<u64> {
        if !self.is_worker() {
            return None;
        }
        self.outbound.command(name, args)
    }

    /// List a directory on the worker (`None` lists `$HOME`). The answer
    /// arrives as [`RemoteClient::take_dir_listing`]. `None` when the host is
    /// not a worker or the link is down.
    pub fn browse_dir(&self, path: Option<&str>, show_hidden: bool) -> Option<u64> {
        let mut args = serde_json::json!({ "show_hidden": show_hidden });
        if let Some(path) = path {
            args["path"] = serde_json::json!(path);
        }
        self.worker_command(worker_command::BROWSE_DIR, Some(args))
    }

    /// Scan the worker's projects root for repositories. The answer arrives as
    /// [`RemoteClient::take_project_scan`].
    pub fn scan_projects(&self) -> Option<u64> {
        self.worker_command(worker_command::SCAN_PROJECTS, None)
    }

    /// Open the repository at `path` on the worker as a project.
    pub fn open_project_path(&self, path: &str) -> Option<u64> {
        self.worker_command(
            worker_command::OPEN_PROJECT_PATH,
            Some(serde_json::json!({ "path": path })),
        )
    }

    /// Clone `url` into the worker's projects root (as `name`, else the name
    /// the URL implies) and open it. The command's seq is the clone's id in
    /// [`RemoteClient::take_clone_events`].
    pub fn clone_project(&self, url: &str, name: Option<&str>) -> Option<u64> {
        let mut args = serde_json::json!({ "url": url });
        if let Some(name) = name.map(str::trim).filter(|n| !n.is_empty()) {
            args["name"] = serde_json::json!(name);
        }
        self.worker_command(worker_command::CLONE_PROJECT, Some(args))
    }

    /// Restart the worker's shell after it exited.
    pub fn restart_worker_shell(&self) -> Option<u64> {
        self.worker_command(worker_command::RESTART_WORKER_SHELL, None)
    }

    /// Size `terminal_id`'s PTY to this window's pane (PRD W5). Worker hosts
    /// only — a TUI or Desktop host owns its own geometry (D4) — and only
    /// while attached; not held for a reconnect. Returns whether it went out.
    pub fn resize_terminal(&self, terminal_id: &TerminalId, size: PtySize) -> bool {
        if !self.is_worker() {
            return false;
        }
        self.outbound.resize_terminal(ResizeTerminal {
            terminal_id: terminal_id.clone(),
            cols: size.cols,
            rows: size.rows,
        })
    }

    /// The directory listing the last `browse_dir` produced, once.
    pub fn take_dir_listing(&mut self) -> Option<DirListing> {
        self.dir_listing.take()
    }

    /// The project scan the last `scan_projects` produced, once.
    pub fn take_project_scan(&mut self) -> Option<ProjectScan> {
        self.project_scan.take()
    }

    /// Clone progress and results since the last call, in order.
    pub fn take_clone_events(&mut self) -> Vec<CloneEvent> {
        std::mem::take(&mut self.clone_events)
    }

    /// Notifications the worker raised since the last call.
    pub fn take_notifications(&mut self) -> Vec<NotificationView> {
        self.workspace.take_notifications()
    }

    /// Host messages that arrived since the last call.
    pub fn take_new_messages(&mut self) -> Vec<HostMessage> {
        self.workspace.take_new_messages()
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
    use crate::web::protocol::{
        ClientMsg, HostKind, ServerMsg, VersionMismatch, PROTOCOL_VERSION, WORKER_SHELL_TERMINAL_ID,
    };

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
    fn the_client_label_names_the_app_its_version_and_this_device() {
        let device = device_name();
        assert!(!device.is_empty());
        assert_eq!(device, device.trim());
        assert_eq!(
            client_label("FlightDeck Desktop", "1.4.0"),
            format!("FlightDeck Desktop/1.4.0 ({device})")
        );
        // The TUI's chip still reads the app off the front.
        assert!(client_label("FlightDeck Desktop", "1.4.0")
            .starts_with(&user_agent("FlightDeck Desktop", "1.4.0")));
    }

    fn sent_frames(script: &mut Script) -> Vec<ClientMsg> {
        script
            .sent()
            .into_iter()
            .filter_map(LinkOut::into_frame)
            .collect()
    }

    fn command_names(frames: &[ClientMsg]) -> Vec<String> {
        frames
            .iter()
            .filter_map(|f| match f {
                ClientMsg::Command(c) => Some(c.name.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn worker_calls_send_nothing_to_a_host_that_is_not_a_worker() {
        for kind in [None, Some(HostKind::Tui), Some(HostKind::Desktop)] {
            let (mut client, mut script) = RemoteClient::scripted();
            script.snapshot(fake_host::snapshot(PROTOCOL_VERSION, kind));
            client.pump();
            assert!(!client.is_worker());
            assert_eq!(client.browse_dir(None, false), None);
            assert_eq!(client.scan_projects(), None);
            assert_eq!(client.open_project_path("/x"), None);
            assert_eq!(client.clone_project("u", None), None);
            assert_eq!(client.restart_worker_shell(), None);
            assert!(!client.resize_terminal(
                &"t1:primary".into(),
                PtySize {
                    rows: 40,
                    cols: 100
                }
            ));
            assert!(sent_frames(&mut script).is_empty(), "{kind:?}");
        }
    }

    #[test]
    fn worker_calls_go_out_as_commands_and_resize_as_its_own_frame() {
        let (mut client, mut script) = RemoteClient::scripted();
        script.snapshot(fake_host::snapshot(
            PROTOCOL_VERSION,
            Some(HostKind::Worker),
        ));
        client.pump();
        assert!(client.is_worker());

        let browse = client.browse_dir(Some("~/src"), true).unwrap();
        client.scan_projects().unwrap();
        client.open_project_path("/home/u/src/app").unwrap();
        let clone = client
            .clone_project("git@host:me/app.git", Some(" "))
            .unwrap();
        client.restart_worker_shell().unwrap();
        assert!(client.resize_terminal(
            &WORKER_SHELL_TERMINAL_ID.into(),
            PtySize {
                rows: 40,
                cols: 100
            }
        ));
        let frames = sent_frames(&mut script);
        assert_eq!(
            command_names(&frames),
            vec![
                worker_command::BROWSE_DIR,
                worker_command::SCAN_PROJECTS,
                worker_command::OPEN_PROJECT_PATH,
                worker_command::CLONE_PROJECT,
                worker_command::RESTART_WORKER_SHELL,
            ]
        );
        let args: Vec<Option<serde_json::Value>> = frames
            .iter()
            .filter_map(|f| match f {
                ClientMsg::Command(c) => Some(c.args.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            args[0],
            Some(serde_json::json!({ "path": "~/src", "show_hidden": true }))
        );
        assert_eq!(
            args[3],
            Some(serde_json::json!({ "url": "git@host:me/app.git" })),
            "a blank name is left to the worker"
        );
        assert!(browse < clone);
        assert_eq!(
            frames.last(),
            Some(&ClientMsg::ResizeTerminal(ResizeTerminal {
                terminal_id: WORKER_SHELL_TERMINAL_ID.into(),
                cols: 100,
                rows: 40,
            }))
        );

        // The answers are kept until taken.
        script.send(LinkEvent::CloneProgress(
            crate::web::protocol::CloneProgress {
                clone_id: clone,
                url: "git@host:me/app.git".to_string(),
                phase: "Receiving objects".to_string(),
                percent: Some(5),
            },
        ));
        script.send(LinkEvent::DirListing(DirListing {
            seq: browse,
            path: "/home/u/src".to_string(),
            parent: Some("/home/u".to_string()),
            home: "/home/u".to_string(),
            entries: Vec::new(),
            truncated: false,
        }));
        assert!(client.pump());
        assert_eq!(client.take_dir_listing().map(|l| l.seq), Some(browse));
        assert!(client.take_dir_listing().is_none());
        assert_eq!(client.take_clone_events().len(), 1);
        assert!(client.take_project_scan().is_none());
    }

    #[test]
    fn the_worker_shell_is_streamed_like_any_terminal() {
        let (mut client, script) = RemoteClient::scripted();
        let mut snapshot = fake_host::snapshot(PROTOCOL_VERSION, Some(HostKind::Worker));
        snapshot.worker.as_mut().unwrap().shell = Some(crate::web::protocol::WorkerShellView {
            terminal_id: WORKER_SHELL_TERMINAL_ID.into(),
            title: "zsh".to_string(),
            geometry: crate::web::protocol::Geometry { cols: 80, rows: 24 },
            byte_len: 0,
            replay_from: 0,
            alive: true,
            exit_code: None,
        });
        script.snapshot(snapshot);
        client.pump();
        let mut pty = client.pty(&WORKER_SHELL_TERMINAL_ID.into());
        script.send(LinkEvent::Bytes(crate::web::protocol::TermBytes::live(
            WORKER_SHELL_TERMINAL_ID.into(),
            0,
            b"$ ".to_vec(),
        )));
        client.pump();
        use crate::contracts::PtySession;
        assert_eq!(pty.try_read_output().unwrap(), b"$ ");
    }

    #[test]
    fn a_v6_host_is_redialled_at_v6_and_sent_nothing_of_v7() {
        let (listener, address) = fake_host::listen();
        let host = std::thread::spawn(move || {
            let mut ws = fake_host::accept(&listener).expect("the client dials");
            let Some(ClientMsg::Attach(first)) = fake_host::recv(&mut ws) else {
                panic!("an attach first");
            };
            // What a v6 host says to a v7 attach.
            fake_host::send(
                &mut ws,
                &ServerMsg::Error(WireError::version_mismatch(VersionMismatch {
                    local: 6,
                    peer: first.protocol_version,
                    min_supported: 6,
                    max_supported: 6,
                })),
            );
            drop(ws);
            let mut ws = fake_host::accept(&listener).expect("the client redials");
            let Some(ClientMsg::Attach(second)) = fake_host::recv(&mut ws) else {
                panic!("an attach again");
            };
            fake_host::send(
                &mut ws,
                &ServerMsg::Snapshot(Box::new(fake_host::snapshot(6, None))),
            );
            let rest = fake_host::recv_all(&mut ws);
            (first, second, rest)
        });

        let mut client = RemoteClient::connect(LinkConfig {
            address,
            token: AccessToken::new("t".to_string()),
            seat: SeatRequest::Write,
            user_agent: client_label("FlightDeck Desktop", "test"),
            connection: crate::web::protocol::ConnectionKind::Window,
            retry_cap_ms: None,
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !(client.state().is_live() && client.workspace().ready) {
            assert!(
                std::time::Instant::now() < deadline,
                "never went live: {:?}",
                client.state()
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
            client.pump();
        }
        assert_eq!(client.workspace().protocol_version, 6);
        assert!(!client.is_worker());
        assert!(!client.resize_terminal(
            &"t1:primary".into(),
            PtySize {
                rows: 40,
                cols: 100
            }
        ));
        assert_eq!(client.browse_dir(None, false), None);
        client.stop();

        let (first, second, rest) = host.join().unwrap();
        assert_eq!(first.protocol_version, PROTOCOL_VERSION);
        assert_eq!(second.protocol_version, 6);
        assert!(
            !rest.iter().any(|f| matches!(f, ClientMsg::ResizeTerminal(_))
                || matches!(f, ClientMsg::Command(c) if worker_command::ALL.contains(&c.name.as_str()))),
            "{rest:?}"
        );
    }

    #[test]
    fn a_background_link_sends_no_input() {
        assert!(!Outbound::detached().0.is_background());
        // What `connect` builds for a background link drops input (and, in a
        // debug build, says so loudly).
        let (tx, _events) = channel();
        let (_link, outbound) = LinkHandle::start(
            LinkConfig {
                address: "127.0.0.1:9".to_string(),
                token: AccessToken::new("t".to_string()),
                seat: SeatRequest::Observe,
                user_agent: "test".to_string(),
                connection: crate::web::protocol::ConnectionKind::Background,
                retry_cap_ms: Some(60_000),
            },
            tx,
        );
        assert!(outbound.is_background());
        outbound.go_live_for_test();
        let typed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            outbound.input("t".into(), b"x".to_vec())
        }));
        assert_eq!(typed.is_err(), cfg!(debug_assertions));
        assert_eq!(outbound.held_len(), 0, "nothing held for a replay");
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
