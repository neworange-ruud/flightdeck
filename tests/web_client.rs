//! The native web client (`flightdeck::web::client`) against the **real**
//! embedded server (`specs/DESKTOP_REMOTE_CONTROL_PLAN.md` §2.2, M1).
//!
//! The host side is assembled the way `tests/web_server.rs` assembles it — a
//! real listener, a real credential store in a temp directory, and a `Fleet`
//! of fake PTYs streamed through the same `TerminalStreams` registry the TUI
//! uses — and the client is the real `RemoteClient` on the shared runtime. The
//! test thread plays the TUI's tick: it drains what the server forwarded,
//! pumps PTY output, and pumps the client, until the condition under test
//! holds or the deadline passes.

use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use flightdeck::agents::status::DisplayStatus;
use flightdeck::contracts::domain::WebConfig;
use flightdeck::contracts::real::{RealClock, RealFs};
use flightdeck::contracts::traits::Clock;
use flightdeck::contracts::{InterpretedStatus, PtySession, PtySize, TabId};
use flightdeck::remote::runtime;
use flightdeck::terminal::session::Session;
use flightdeck::testing::{FakePty, FakePtyHandle};
use flightdeck::web::client::{
    exchange_code, probe_session, AccessToken, ExchangeError, LinkConfig, LinkEnd, LinkState,
    Probe, RemoteClient, Target,
};
use flightdeck::web::credentials::CredentialStore;
use flightdeck::web::protocol::{
    command as names, Geometry, GitBar, ProjectView, SeatRequest, Selection, ServerMsg,
    SessionPhase, SessionStatus, SessionView, ShutdownReason, StatusBucket, TerminalId,
    TerminalRole, TerminalView,
};
use flightdeck::web::server::{
    self, HostState, ShutdownNotice, WebInbound, WebOutbound, WebServerHandle,
};
use flightdeck::web::stream::{
    child_terminal_id, deltas, primary_terminal_id, write_into_session, TerminalHost,
    TerminalStreams, Written,
};

const WAIT: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------------------
// The host
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Tabs {
    tabs: Vec<(String, Session)>,
}

impl TerminalHost for Tabs {
    fn write_terminal_input(&mut self, terminal_id: &TerminalId, bytes: &[u8]) -> Written {
        for (tab_id, session) in self.tabs.iter_mut() {
            let tab_id = tab_id.clone();
            if let Some(written) = write_into_session(session, &tab_id, terminal_id, bytes) {
                return written;
            }
        }
        Written::NoSuchTerminal
    }
}

/// The controlled instance: a server plus the tabs it streams.
struct Host {
    handle: Option<WebServerHandle>,
    credentials: Arc<Mutex<CredentialStore>>,
    inbound: Receiver<WebInbound>,
    inbound_tx: std::sync::mpsc::Sender<WebInbound>,
    port: u16,
    streams: TerminalStreams,
    tabs: Tabs,
    backend: FakePty,
    /// Session id → (name, status), the facts the published state carries.
    sessions: Vec<(String, String, InterpretedStatus)>,
    selected: usize,
    published: HostState,
    _dir: tempfile::TempDir,
}

impl Host {
    fn start() -> Host {
        let dir = tempfile::tempdir().expect("a temp dir");
        let clock = Arc::new(RealClock);
        let store =
            CredentialStore::open(Arc::new(RealFs), clock.clone(), dir.path().join("web.json"));
        let credentials = Arc::new(Mutex::new(store));
        let (inbound_tx, inbound) = std::sync::mpsc::channel::<WebInbound>();
        let mut host = Host {
            handle: None,
            credentials,
            inbound,
            inbound_tx,
            port: 0,
            streams: TerminalStreams::new(65_536),
            tabs: Tabs::default(),
            backend: FakePty::new(),
            sessions: Vec::new(),
            selected: 0,
            published: HostState::default(),
            _dir: dir,
        };
        host.listen();
        host
    }

    fn listen(&mut self) {
        let config = WebConfig {
            enabled: true,
            port: self.port,
            bind: WebConfig::default().bind,
            replay_bytes: 65_536,
        };
        let state = self.state();
        let handle = server::start(
            &config,
            Arc::clone(&self.credentials),
            Arc::new(RealClock) as Arc<dyn Clock + Send + Sync>,
            state.clone(),
            self.inbound_tx.clone(),
        )
        .expect("the test host can bind a loopback port");
        self.port = handle.bound_addr().port();
        self.published = state;
        self.handle = Some(handle);
    }

    fn handle(&self) -> &WebServerHandle {
        self.handle.as_ref().expect("the server is running")
    }

    fn addr(&self) -> String {
        format!("127.0.0.1:{}", self.port)
    }

    fn mint_code(&self) -> String {
        self.credentials
            .lock()
            .unwrap()
            .mint_bootstrap_code()
            .reveal()
            .to_string()
    }

    /// Open a session with a primary agent terminal.
    fn tab(&mut self, id: &str, name: &str) -> FakePtyHandle {
        let handle = self.backend.queue_session();
        let mut session = Session::new();
        session
            .spawn_primary(
                &self.backend,
                "agent",
                &[],
                std::path::Path::new("."),
                PtySize {
                    rows: 34,
                    cols: 120,
                },
            )
            .expect("the fake backend spawns");
        self.streams.open(primary_terminal_id(id));
        self.tabs.tabs.push((id.to_string(), session));
        self.sessions
            .push((id.to_string(), name.to_string(), InterpretedStatus::Working));
        handle
    }

    /// Add a child shell to `tab`, returning its handle and wire id.
    fn child(&mut self, tab: &str) -> (FakePtyHandle, TerminalId) {
        let handle = self.backend.queue_session();
        let (_, session) = self
            .tabs
            .tabs
            .iter_mut()
            .find(|(id, _)| id == tab)
            .expect("that tab is open");
        let index = session
            .spawn_child(
                &self.backend,
                "bash",
                &[],
                std::path::Path::new("."),
                PtySize {
                    rows: 34,
                    cols: 120,
                },
            )
            .expect("the fake backend spawns");
        let id = child_terminal_id(tab, session.child(index).unwrap().stream_id());
        self.streams.open(id.clone());
        (handle, id)
    }

    fn terminal_views(&self, tab: &str) -> Vec<TerminalView> {
        let (_, session) = self.tabs.tabs.iter().find(|(id, _)| id == tab).unwrap();
        let mut ids = vec![(primary_terminal_id(tab), TerminalRole::Primary, "agent")];
        for c in 0..session.child_count() {
            let stream = session.child(c).unwrap().stream_id();
            ids.push((child_terminal_id(tab, stream), TerminalRole::Shell, "shell"));
        }
        ids.into_iter()
            .map(|(terminal_id, role, title)| TerminalView {
                byte_len: self.streams.byte_len(&terminal_id),
                replay_from: self.streams.replay_from(&terminal_id),
                alive: self.streams.alive(&terminal_id),
                exit_code: self.streams.exit_code(&terminal_id),
                terminal_id,
                session_id: TabId(tab.to_string()),
                role,
                title: title.to_string(),
                geometry: Geometry {
                    cols: 120,
                    rows: 34,
                },
            })
            .collect()
    }

    fn state(&self) -> HostState {
        let sessions = self
            .sessions
            .iter()
            .map(|(id, name, status)| SessionView {
                session_id: TabId(id.clone()),
                project_id: "/repo".into(),
                name: name.clone(),
                agent: "claude".to_string(),
                agent_display_name: "Claude".to_string(),
                phase: SessionPhase::Ready,
                status: SessionStatus::from_display(
                    DisplayStatus {
                        process: flightdeck::contracts::ProcessState::Running,
                        interpreted: *status,
                        manual: None,
                    },
                    0,
                ),
                git: GitBar::default(),
                terminals: self.terminal_views(id),
                lifecycle_reporting: true,
                recovered: false,
                attached_existing_branch: false,
            })
            .collect::<Vec<_>>();
        let selected = self
            .sessions
            .get(self.selected)
            .map(|(id, _, _)| id.clone());
        HostState {
            host_version: "test-host".to_string(),
            projects: vec![ProjectView {
                project_id: "/repo".into(),
                name: "repo".to_string(),
                root: "/repo".to_string(),
                base_branch: "main".to_string(),
                dot: StatusBucket::rollup(sessions.iter().map(|s| s.status.bucket)),
                sessions,
            }],
            selection: Selection {
                project_id: Some("/repo".into()),
                terminal_id: selected.as_deref().map(primary_terminal_id),
                session_id: selected.map(TabId),
                split_view: false,
            },
            geometry: Geometry {
                cols: 120,
                rows: 34,
            },
            replay_capacity_bytes: 65_536,
            ..HostState::default()
        }
    }

    /// Publish the current facts and push the deltas that describe the change,
    /// as the TUI's tick does.
    fn publish(&mut self) {
        let next = self.state();
        for delta in deltas(&self.published, &next) {
            self.handle()
                .send(WebOutbound::All(ServerMsg::Delta(delta)));
        }
        self.handle().publish_state(next.clone());
        self.published = next;
    }

    /// One tick: apply what the server forwarded, stream PTY output, and keep
    /// the published state (byte lengths included) current.
    fn tick(&mut self) {
        let events: Vec<WebInbound> = self.inbound.try_iter().collect();
        if let Some(handle) = self.handle.as_ref() {
            for event in &events {
                for out in self.streams.apply_inbound(event, &mut self.tabs) {
                    handle.send(out);
                }
            }
            for (tab_id, session) in self.tabs.tabs.iter_mut() {
                let mut frames = Vec::new();
                if let Some(primary) = session.primary_mut() {
                    let bytes = primary.session_mut().try_read_output().unwrap_or_default();
                    if !bytes.is_empty() {
                        frames.extend(
                            self.streams
                                .pty_output(&primary_terminal_id(tab_id), &bytes),
                        );
                    }
                }
                for c in 0..session.child_count() {
                    let Some(child) = session.child_mut(c) else {
                        continue;
                    };
                    let id = child_terminal_id(tab_id, child.stream_id());
                    let bytes = child.session_mut().try_read_output().unwrap_or_default();
                    if !bytes.is_empty() {
                        frames.extend(self.streams.pty_output(&id, &bytes));
                    }
                }
                for frame in frames {
                    handle.send(WebOutbound::All(ServerMsg::TermBytes(frame)));
                }
            }
        }
        if self.handle.is_some() {
            let state = self.state();
            self.handle().publish_state(state);
        }
    }

    fn stop(&mut self, reason: ShutdownReason) {
        if let Some(handle) = self.handle.take() {
            handle.stop(ShutdownNotice {
                reason,
                detail: None,
                initiator: None,
            });
        }
    }
}

// ---------------------------------------------------------------------------
// The client
// ---------------------------------------------------------------------------

fn on_runtime<F: std::future::Future>(body: F) -> F::Output {
    runtime::shared().handle().block_on(body)
}

fn paired(host: &Host) -> AccessToken {
    let code = host.mint_code();
    on_runtime(exchange_code(
        &host.addr(),
        &code,
        "FlightDeck Desktop/test",
    ))
    .expect("the code is exchanged")
}

fn connect(host: &Host, seat: SeatRequest) -> RemoteClient {
    RemoteClient::connect(LinkConfig {
        address: host.addr(),
        token: paired(host),
        seat,
        user_agent: "FlightDeck Desktop/test".to_string(),
    })
}

/// Tick the host and pump the client until `done` holds.
fn until(
    host: &mut Host,
    client: &mut RemoteClient,
    what: &str,
    mut done: impl FnMut(&mut RemoteClient) -> bool,
) {
    let deadline = Instant::now() + WAIT;
    loop {
        host.tick();
        client.pump();
        if done(client) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for: {what} (state {:?})",
            client.state()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Read everything a remote terminal has been sent so far.
fn read_all(pty: &mut flightdeck::web::client::StreamPty, into: &mut Vec<u8>) {
    into.extend(pty.try_read_output().unwrap());
}

fn live(client: &mut RemoteClient) -> bool {
    client.state().is_live() && client.workspace().ready
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn a_code_is_exchanged_for_a_token_the_session_probe_accepts() {
    let host = Host::start();
    let token = paired(&host);
    assert_eq!(
        on_runtime(probe_session(&host.addr(), &token)).unwrap(),
        Probe::Valid
    );

    // A wrong code is refused with the host's own reason.
    host.mint_code();
    let refused = on_runtime(exchange_code(
        &host.addr(),
        "0000",
        "FlightDeck Desktop/test",
    ));
    assert!(
        matches!(&refused, Err(ExchangeError::Refused { reason, .. }) if reason == "wrong_code" || reason == "code_already_used"),
        "{refused:?}"
    );

    // And a made-up token is not one the host knows.
    assert!(matches!(
        on_runtime(probe_session(&host.addr(), &AccessToken::new("forged"))).unwrap(),
        Probe::Refused(_)
    ));
}

#[test]
fn nothing_listening_is_unreachable_not_a_hang() {
    let host = Host::start();
    let addr = host.addr();
    let mut host = host;
    host.stop(ShutdownReason::ServerStopped);
    let result = on_runtime(exchange_code(&addr, "1234", "x"));
    assert!(
        matches!(result, Err(ExchangeError::Unreachable(_))),
        "{result:?}"
    );
}

#[test]
fn attaching_mirrors_the_snapshot_and_then_every_delta() {
    let mut host = Host::start();
    host.tab("s1", "login");
    host.tab("s2", "search");
    host.selected = 1;
    host.publish();
    let mut client = connect(&host, SeatRequest::Write);

    until(&mut host, &mut client, "the snapshot", live);
    let ws = client.workspace();
    let names: Vec<&str> = ws.projects[0]
        .sessions
        .iter()
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(names, vec!["login", "search"]);
    assert_eq!(
        ws.selected_session().map(|s| s.name.as_str()),
        Some("search"),
        "the first snapshot starts on the host's selection"
    );
    assert!(ws.host_is_viewing(&TabId("s2".to_string())));
    assert!(
        !ws.commands.is_empty(),
        "the host's command inventory arrived"
    );

    // A status change and a new session, as deltas.
    host.sessions[0].2 = InterpretedStatus::WaitingForInput;
    host.tab("s3", "footer");
    host.publish();
    until(&mut host, &mut client, "the deltas", |c| {
        let ws = c.workspace();
        ws.session(&TabId("s3".to_string())).is_some()
            && ws
                .session(&TabId("s1".to_string()))
                .map(|s| s.status.bucket)
                == Some(StatusBucket::Waiting)
    });

    // The host moving its own selection moves only the marker.
    host.selected = 0;
    host.publish();
    until(&mut host, &mut client, "the host's selection marker", |c| {
        c.workspace().host_is_viewing(&TabId("s1".to_string()))
    });
    assert_eq!(
        client
            .workspace()
            .selected_session()
            .map(|s| s.name.as_str()),
        Some("search"),
        "R2: this client's selection did not follow"
    );
    client.stop();
}

#[test]
fn bytes_and_keystrokes_flow_for_a_terminal_the_host_is_not_showing() {
    let mut host = Host::start();
    let _shown = host.tab("s1", "login");
    let hidden = host.tab("s2", "search");
    let (shell, shell_id) = host.child("s2");
    host.selected = 0;
    host.publish();
    let mut client = connect(&host, SeatRequest::Write);
    until(&mut host, &mut client, "the snapshot", live);

    // Browse to the hidden session's shell, here only.
    assert!(client.workspace_mut().select_terminal(&shell_id));
    let mut pty = client.pty(&shell_id);
    let mut agent_pty = client.pty(&primary_terminal_id("s2"));

    shell.push_output("hidden shell output\r\n");
    hidden.push_output("hidden agent output\r\n");
    let mut seen = Vec::new();
    let mut agent_seen = Vec::new();
    until(&mut host, &mut client, "the hidden terminals' bytes", |c| {
        let _ = c;
        read_all(&mut pty, &mut seen);
        read_all(&mut agent_pty, &mut agent_seen);
        seen.ends_with(b"hidden shell output\r\n")
            && agent_seen.ends_with(b"hidden agent output\r\n")
    });

    pty.write_input(b"ls -la\r").unwrap();
    until(
        &mut host,
        &mut client,
        "the keystrokes at the host's PTY",
        |_| shell.input() == b"ls -la\r",
    );
    assert!(
        hidden.input().is_empty(),
        "only the named terminal got them"
    );
    assert_eq!(host.selected, 0, "the host never moved");
    client.stop();
}

#[test]
fn a_restart_resumes_from_the_cursor_and_replays_held_input_exactly_once() {
    let mut host = Host::start();
    let agent = host.tab("s1", "login");
    host.publish();
    let mut client = connect(&host, SeatRequest::Write);
    until(&mut host, &mut client, "the snapshot", live);
    let mut pty = client.pty(&primary_terminal_id("s1"));
    let mut seen = Vec::new();

    agent.push_output("one\r\n");
    pty.write_input(b"a").unwrap();
    until(&mut host, &mut client, "first bytes and first key", |_| {
        read_all(&mut pty, &mut seen);
        seen == b"one\r\n" && agent.input() == b"a"
    });
    until(&mut host, &mut client, "the first key acked", |c| {
        c.outbound().held_len() == 0
    });

    // The web server restarts (the host itself keeps running, so its streams
    // and their offsets carry on). Output keeps coming and the user keeps
    // typing while the link is down.
    host.stop(ShutdownReason::Restarting);
    until(&mut host, &mut client, "the client noticing", |c| {
        matches!(c.state(), LinkState::Reconnecting { .. })
    });
    agent.push_output("two\r\n");
    host.tick();
    pty.write_input(b"b").unwrap();
    pty.write_input(b"c").unwrap();
    assert_eq!(
        client.outbound().held_len(),
        2,
        "typed offline: held, not dropped"
    );

    host.listen();
    until(&mut host, &mut client, "the resume", |c| {
        read_all(&mut pty, &mut seen);
        live(c) && seen == b"one\r\ntwo\r\n" && agent.input() == b"abc"
    });
    // Give a duplicate the chance to arrive, then check it did not.
    for _ in 0..30 {
        host.tick();
        client.pump();
        read_all(&mut pty, &mut seen);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(seen, b"one\r\ntwo\r\n", "no byte twice, none missing");
    assert_eq!(agent.input(), b"abc", "every key exactly once, in order");
    client.stop();
}

#[test]
fn a_targeted_command_reaches_the_host_with_its_target() {
    let mut host = Host::start();
    host.tab("s1", "login");
    host.tab("s2", "search");
    host.publish();
    let mut client = connect(&host, SeatRequest::Write);
    until(&mut host, &mut client, "the snapshot", live);

    let seq = client
        .command_for(
            names::RESTART_AGENT,
            &Target::Session(TabId("s1".to_string())),
        )
        .expect("live, so the command goes out");
    let deadline = Instant::now() + WAIT;
    let command = loop {
        client.pump();
        if let Some(WebInbound::Command { command, .. }) = host
            .inbound
            .try_iter()
            .find(|e| matches!(e, WebInbound::Command { .. }))
        {
            break command;
        }
        assert!(
            Instant::now() < deadline,
            "the command never reached the host"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(command.seq, seq);
    assert_eq!(command.name, names::RESTART_AGENT);
    assert_eq!(
        command.args,
        Some(serde_json::json!({ "session_id": "s1" }))
    );
    client.stop();
}

#[test]
fn an_observer_seat_cannot_type() {
    let mut host = Host::start();
    let agent = host.tab("s1", "login");
    host.publish();
    let mut client = connect(&host, SeatRequest::Observe);
    until(&mut host, &mut client, "the snapshot", live);
    assert_eq!(
        client.workspace().seat,
        Some(flightdeck::web::protocol::Seat::Observing)
    );

    let mut pty = client.pty(&primary_terminal_id("s1"));
    pty.write_input(b"x").unwrap();
    for _ in 0..30 {
        host.tick();
        client.pump();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        agent.input().is_empty(),
        "an observer's keys never reach the PTY"
    );

    // Asking for the write seat is one request away.
    client.request_seat(SeatRequest::Write);
    until(&mut host, &mut client, "the write seat", |c| {
        c.workspace().seat == Some(flightdeck::web::protocol::Seat::Writing)
    });
    client.stop();
}

#[test]
fn a_revoked_token_ends_the_link_and_it_stays_ended() {
    let mut host = Host::start();
    host.tab("s1", "login");
    host.publish();
    let mut client = connect(&host, SeatRequest::Write);
    until(&mut host, &mut client, "the snapshot", live);

    let id = host.credentials.lock().unwrap().records()[0].id.clone();
    assert!(host.credentials.lock().unwrap().revoke(&id).unwrap());
    host.handle().recheck_credentials();
    until(&mut host, &mut client, "the revocation", |c| {
        c.state() == &LinkState::Ended(LinkEnd::Revoked)
    });
    // And it does not come back by itself.
    for _ in 0..50 {
        host.tick();
        client.pump();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(client.state(), &LinkState::Ended(LinkEnd::Revoked));
    client.stop();
}

#[test]
fn a_host_quit_ends_the_link() {
    let mut host = Host::start();
    host.tab("s1", "login");
    host.publish();
    let mut client = connect(&host, SeatRequest::Write);
    until(&mut host, &mut client, "the snapshot", live);
    host.stop(ShutdownReason::HostQuit);
    until(&mut host, &mut client, "the quit", |c| {
        c.state() == &LinkState::Ended(LinkEnd::HostQuit)
    });
    client.stop();
}
