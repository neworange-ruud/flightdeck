//! A hand-scripted host for the link's own tests: the blocking `tungstenite`
//! server on a `std::thread`, as `src/remote/client/tests.rs` uses, so a test
//! can play an *older* host (or one that misbehaves) frame by frame — which
//! the real embedded server, being this build, cannot.

use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

use tungstenite::{Message, WebSocket};

use crate::contracts::AgentTabPosition;
use crate::web::protocol::{
    ClientMsg, Geometry, HostKind, Seat, Selection, ServerMsg, Snapshot, WorkerSnapshot,
};

/// The host's side of one socket.
pub(crate) type Ws = WebSocket<TcpStream>;

/// How long the fake waits for a connection or a frame before giving up, so
/// a broken test fails instead of hanging.
const PATIENCE: Duration = Duration::from_secs(10);

/// A loopback listener and the `host:port` a [`super::LinkConfig`] dials.
pub(crate) fn listen() -> (TcpListener, String) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let address = listener.local_addr().expect("a bound address").to_string();
    (listener, address)
}

/// Accept the next client's WebSocket, or `None` if none arrives in time.
pub(crate) fn accept(listener: &TcpListener) -> Option<Ws> {
    listener.set_nonblocking(true).ok()?;
    let deadline = Instant::now() + PATIENCE;
    let stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return None;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => return None,
        }
    };
    stream.set_nonblocking(false).ok()?;
    stream.set_read_timeout(Some(PATIENCE)).ok()?;
    tungstenite::accept(stream).ok()
}

/// The next frame the client sent; `None` once it closed (or went silent).
pub(crate) fn recv(ws: &mut Ws) -> Option<ClientMsg> {
    loop {
        match ws.read() {
            Ok(Message::Text(text)) => return serde_json::from_str(text.as_str()).ok(),
            Ok(Message::Close(_)) | Err(_) => return None,
            Ok(_) => continue,
        }
    }
}

/// Every frame the client sends until it closes the socket.
pub(crate) fn recv_all(ws: &mut Ws) -> Vec<ClientMsg> {
    std::iter::from_fn(|| recv(ws)).collect()
}

/// Send one frame.
pub(crate) fn send(ws: &mut Ws, frame: &ServerMsg) {
    let json = serde_json::to_string(frame).expect("a frame serialises");
    let _ = ws.send(Message::Text(json.into()));
}

/// An empty workspace at `version`, from a host of `host_kind`.
pub(crate) fn snapshot(version: u16, host_kind: Option<HostKind>) -> Snapshot {
    Snapshot {
        protocol_version: version,
        host_version: "1.0.0".to_string(),
        server_time_ms: 0,
        viewer_id: "v1".into(),
        seat: Seat::Writing,
        seats: Vec::new(),
        last_input_seq: 0,
        projects: Vec::new(),
        selection: Selection::default(),
        geometry: Geometry { cols: 80, rows: 24 },
        replay_capacity_bytes: 0,
        activity: Vec::new(),
        dialog: None,
        commands: crate::web::commands::inventory(),
        help: None,
        about: None,
        update: None,
        sidebar_position: AgentTabPosition::default(),
        host_machine: None,
        host_kind,
        worker: (host_kind == Some(HostKind::Worker)).then(|| WorkerSnapshot {
            home: "/home/u".to_string(),
            shell: None,
            health: None,
            status: Default::default(),
            connections: Default::default(),
            host_messages: Vec::new(),
        }),
    }
}
