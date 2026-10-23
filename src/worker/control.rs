//! The control socket: how the `flightdeck worker …` CLI reaches a running
//! worker (SPECS §33).
//!
//! A Unix socket at `~/.flightdeck/worker/control.sock` (mode 0600, in a 0700
//! directory), so only the account itself can mint a pairing code or revoke a
//! device: getting a code already requires a shell on the box. One request per
//! connection, each a line of JSON: a [`ControlRequest`] in, a
//! [`ControlResponse`] out. The server side never touches the `AppHost` (it is
//! not `Send`): it hands each request to the run loop as a [`ControlCall`] and
//! waits for the reply.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::worker::paths::WorkerPaths;
use crate::worker::status::WorkerRuntimeReport;

/// A request from the CLI.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ControlRequest {
    /// Is a worker answering? (`worker start` polls this.)
    Ping,
    /// The runtime half of `worker status`.
    Status,
    /// Mint a single-use pairing code (`worker pair`).
    Pair,
    /// Paired Desktops (`worker devices`).
    Devices,
    /// Revoke a Desktop's token and close its connections (`worker revoke`).
    /// `device` is an id, a label or a unique id prefix.
    Revoke { device: String },
    /// Sent by `worker restart` before stopping, so connections close with
    /// "restarting" rather than "stopped".
    PrepareShutdown { restarting: bool },
}

/// The worker's answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum ControlResponse {
    Pong {
        pid: u32,
        version: String,
    },
    Status(Box<WorkerRuntimeReport>),
    Paired(PairingCode),
    Devices {
        devices: Vec<DeviceReport>,
    },
    Revoked {
        device: DeviceReport,
        /// How many live connections (window and background) were closed.
        closed_connections: u32,
    },
    /// Done, nothing to report (`PrepareShutdown`).
    Ok,
    /// The request was refused or failed; `message` is for a person.
    Error {
        message: String,
    },
}

/// A freshly minted pairing code, with where to type it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingCode {
    pub code: String,
    /// How long the code stays valid.
    pub expires_in_secs: u64,
    /// The address to type in the Desktop: the advertised MagicDNS name, else
    /// the first bound address, with the port. `None` while not listening.
    pub address: Option<String>,
    pub listening: bool,
    pub not_listening_reason: Option<String>,
}

/// One paired Desktop.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceReport {
    /// The credential's token id: what `worker revoke` takes.
    pub id: String,
    /// The Desktop's host name, or the label it paired with.
    pub label: String,
    /// The address of its live connection, when it has one.
    pub address: Option<String>,
    /// When it paired (unix seconds).
    pub paired_at: u64,
    /// When it last connected (unix seconds).
    pub last_seen: Option<u64>,
    pub state: DeviceState,
    pub revoked: bool,
}

/// Whether a paired Desktop is connected, and how.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceState {
    /// It has the worker's window open.
    Attached,
    /// It holds a status-only background link.
    Background,
    Offline,
}

/// Why a control request got no answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ControlError {
    /// No worker listens on the socket (missing, or connection refused).
    NotRunning,
    /// The worker did not answer in time.
    Timeout,
    /// The socket failed in another way, with the OS's reason.
    Io(String),
    /// The worker answered something that is not a [`ControlResponse`].
    Protocol(String),
}

impl std::fmt::Display for ControlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ControlError::NotRunning => f.write_str("the worker is not running"),
            ControlError::Timeout => f.write_str("the worker did not answer in time"),
            ControlError::Io(e) => write!(f, "control socket: {e}"),
            ControlError::Protocol(e) => write!(f, "unexpected answer from the worker: {e}"),
        }
    }
}

impl std::error::Error for ControlError {}

/// How long the server waits for the run loop to answer one request before
/// answering [`ControlResponse::Error`] itself. The run loop serves calls
/// between ticks, so only a stuck turn gets near it.
pub const REPLY_TIMEOUT: Duration = Duration::from_secs(5);
/// How long [`ping`] waits. Short: the supervisor pings every 500 ms while a
/// worker starts, and `worker start` polls it.
pub const PING_TIMEOUT: Duration = Duration::from_millis(500);
/// How long the server gives a connected client to send its request, and to
/// take its answer.
#[cfg(unix)]
const CLIENT_IO_TIMEOUT: Duration = Duration::from_secs(2);
/// How often the accept loop looks at its stop flag.
#[cfg(unix)]
const ACCEPT_POLL: Duration = Duration::from_millis(25);

/// The size of `sockaddr_un.sun_path` on this platform, NUL included.
#[cfg(any(target_os = "linux", target_os = "android"))]
const SUN_PATH_LEN: usize = 108;
#[cfg(not(any(target_os = "linux", target_os = "android")))]
const SUN_PATH_LEN: usize = 104;
/// The longest control-socket path the platform can bind (`sun_path` less
/// its NUL): 103 bytes on macOS, 107 on Linux.
pub const MAX_SOCKET_PATH_BYTES: usize = SUN_PATH_LEN - 1;

/// Refuse a socket path the platform would truncate or reject, with a
/// message that says what to do (a home directory with a very long path).
pub fn check_socket_path(socket: &Path) -> std::io::Result<()> {
    let len = socket.as_os_str().len();
    if len > MAX_SOCKET_PATH_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "the control socket path {} is {len} bytes long, over this platform's \
                 {MAX_SOCKET_PATH_BYTES}-byte limit for a Unix socket; run the worker \
                 under an account whose home directory has a shorter path",
                socket.display()
            ),
        ));
    }
    Ok(())
}

/// Send `req` to the worker listening on `socket` and wait up to `timeout`
/// for its answer.
pub fn request(
    socket: &Path,
    req: &ControlRequest,
    timeout: Duration,
) -> Result<ControlResponse, ControlError> {
    #[cfg(unix)]
    {
        unix::request(socket, req, timeout)
    }
    #[cfg(not(unix))]
    {
        let _ = (socket, req, timeout);
        Err(ControlError::NotRunning)
    }
}

/// The running worker's pid and version, or `None` when nothing answers
/// within [`PING_TIMEOUT`].
pub fn ping(paths: &WorkerPaths) -> Option<(u32, String)> {
    match request(&paths.control_socket, &ControlRequest::Ping, PING_TIMEOUT) {
        Ok(ControlResponse::Pong { pid, version }) => Some((pid, version)),
        _ => None,
    }
}

/// One request handed from the socket thread to the run loop.
pub struct ControlCall {
    pub request: ControlRequest,
    reply: Sender<ControlResponse>,
}

impl ControlCall {
    /// Answer the request. A client that has given up is not an error.
    pub fn reply(self, r: ControlResponse) {
        let _ = self.reply.send(r);
    }
}

/// The listening control socket; dropping it stops serving and removes the
/// socket file.
pub struct ControlServer {
    socket: std::path::PathBuf,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for ControlServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = std::fs::remove_file(&self.socket);
    }
}

/// Listen on `socket` and hand each request to the returned receiver.
///
/// A socket file already there is removed first: the caller holds
/// `run.lock`, so it was left by a worker that is gone. The socket is made
/// 0600. Each connection is served on its own short-lived thread, which
/// waits up to [`REPLY_TIMEOUT`] for the run loop's answer; the receiver
/// side must never block on anything but its own turn.
pub fn serve(socket: &Path) -> std::io::Result<(ControlServer, Receiver<ControlCall>)> {
    serve_with(socket, REPLY_TIMEOUT)
}

/// [`serve`] with an explicit reply timeout (tests shorten it).
fn serve_with(
    socket: &Path,
    reply_timeout: Duration,
) -> std::io::Result<(ControlServer, Receiver<ControlCall>)> {
    #[cfg(unix)]
    {
        unix::serve(socket, reply_timeout)
    }
    #[cfg(not(unix))]
    {
        let _ = (socket, reply_timeout);
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "the worker control socket needs Linux or macOS",
        ))
    }
}

#[cfg(unix)]
mod unix {
    use std::io::{ErrorKind, Read, Write};
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use super::{
        check_socket_path, ControlCall, ControlError, ControlRequest, ControlResponse,
        ControlServer, ACCEPT_POLL, CLIENT_IO_TIMEOUT,
    };

    /// The longest request line the server reads.
    pub(super) const MAX_REQUEST_BYTES: usize = 64 * 1024;
    /// The longest answer the client reads (a status with many projects and
    /// agents is the big one).
    const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

    /// Why reading a line failed.
    #[derive(Debug)]
    enum LineError {
        Timeout,
        TooLong,
        Io(std::io::Error),
    }

    /// The server's answer to one request line: parse it, hand it to the run
    /// loop, wait for the reply. Everything that goes wrong becomes an
    /// [`ControlResponse::Error`] the client can print.
    pub(super) fn respond(
        line: &[u8],
        calls: &Sender<ControlCall>,
        reply_timeout: Duration,
    ) -> ControlResponse {
        let request = match serde_json::from_slice::<ControlRequest>(line) {
            Ok(request) => request,
            Err(e) => {
                return ControlResponse::Error {
                    message: format!("not a control request: {e}"),
                }
            }
        };
        let (reply, answer) = mpsc::channel();
        let shutting_down = || ControlResponse::Error {
            message: "the worker is shutting down".to_string(),
        };
        if calls.send(ControlCall { request, reply }).is_err() {
            return shutting_down();
        }
        match answer.recv_timeout(reply_timeout) {
            Ok(response) => response,
            Err(RecvTimeoutError::Timeout) => ControlResponse::Error {
                message: "the worker did not answer in time".to_string(),
            },
            Err(RecvTimeoutError::Disconnected) => shutting_down(),
        }
    }

    pub(super) fn request(
        socket: &Path,
        req: &ControlRequest,
        timeout: Duration,
    ) -> Result<ControlResponse, ControlError> {
        check_socket_path(socket).map_err(|e| ControlError::Io(e.to_string()))?;
        let deadline = Instant::now() + timeout;
        let mut stream = UnixStream::connect(socket).map_err(|e| match e.kind() {
            ErrorKind::NotFound | ErrorKind::ConnectionRefused => ControlError::NotRunning,
            _ => ControlError::Io(e.to_string()),
        })?;
        let mut line =
            serde_json::to_string(req).map_err(|e| ControlError::Protocol(e.to_string()))?;
        line.push('\n');
        if !timeout.is_zero() {
            let _ = stream.set_write_timeout(Some(timeout));
        }
        stream
            .write_all(line.as_bytes())
            .map_err(|e| match e.kind() {
                ErrorKind::WouldBlock | ErrorKind::TimedOut => ControlError::Timeout,
                _ => ControlError::Io(e.to_string()),
            })?;
        let answer = match read_line(&mut stream, deadline, MAX_RESPONSE_BYTES) {
            Ok(answer) => answer,
            Err(LineError::Timeout) => return Err(ControlError::Timeout),
            Err(LineError::TooLong) => {
                return Err(ControlError::Protocol("the answer is too long".to_string()))
            }
            Err(LineError::Io(e)) => return Err(ControlError::Io(e.to_string())),
        };
        if answer.is_empty() {
            return Err(ControlError::Protocol(
                "the worker closed the connection without answering".to_string(),
            ));
        }
        serde_json::from_slice(&answer).map_err(|e| ControlError::Protocol(e.to_string()))
    }

    pub(super) fn serve(
        socket: &Path,
        reply_timeout: Duration,
    ) -> std::io::Result<(ControlServer, Receiver<ControlCall>)> {
        check_socket_path(socket)?;
        match std::fs::symlink_metadata(socket) {
            Ok(_) => std::fs::remove_file(socket)?,
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let listener = UnixListener::bind(socket)?;
        let ready = std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))
            .and_then(|()| listener.set_nonblocking(true));
        if let Err(e) = ready {
            let _ = std::fs::remove_file(socket);
            return Err(e);
        }
        let (calls, received) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let thread = std::thread::Builder::new()
            .name("worker-control".to_string())
            .spawn({
                let stop = Arc::clone(&stop);
                move || accept_loop(listener, calls, stop, reply_timeout)
            });
        let thread = match thread {
            Ok(thread) => thread,
            Err(e) => {
                let _ = std::fs::remove_file(socket);
                return Err(e);
            }
        };
        let server = ControlServer {
            socket: socket.to_path_buf(),
            stop,
            thread: Some(thread),
        };
        Ok((server, received))
    }

    fn accept_loop(
        listener: UnixListener,
        calls: Sender<ControlCall>,
        stop: Arc<AtomicBool>,
        reply_timeout: Duration,
    ) {
        while !stop.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((stream, _)) => {
                    let calls = calls.clone();
                    // A thread per connection: a client that never sends
                    // must not hold up the next one.
                    let _ = std::thread::Builder::new()
                        .name("worker-control-conn".to_string())
                        .spawn(move || serve_connection(stream, &calls, reply_timeout));
                }
                // Nothing pending, or a transient failure (out of file
                // descriptors): look again shortly.
                Err(_) => std::thread::sleep(ACCEPT_POLL),
            }
        }
    }

    fn serve_connection(
        mut stream: UnixStream,
        calls: &Sender<ControlCall>,
        reply_timeout: Duration,
    ) {
        // An accepted socket inherits the listener's non-blocking mode on
        // macOS (not on Linux).
        if stream.set_nonblocking(false).is_err() {
            return;
        }
        let deadline = Instant::now() + CLIENT_IO_TIMEOUT;
        let response = match read_line(&mut stream, deadline, MAX_REQUEST_BYTES) {
            Ok(line) if line.is_empty() => return,
            Ok(line) => respond(&line, calls, reply_timeout),
            Err(LineError::TooLong) => ControlResponse::Error {
                message: format!("the request is longer than {MAX_REQUEST_BYTES} bytes"),
            },
            Err(LineError::Timeout | LineError::Io(_)) => return,
        };
        let Ok(mut line) = serde_json::to_string(&response) else {
            return;
        };
        line.push('\n');
        let _ = stream.set_write_timeout(Some(CLIENT_IO_TIMEOUT));
        let _ = stream.write_all(line.as_bytes());
    }

    /// Read up to the first newline (excluded) or the end of the stream,
    /// whichever comes first, by `deadline`. More than `max` bytes without
    /// a newline is [`LineError::TooLong`].
    fn read_line(
        stream: &mut UnixStream,
        deadline: Instant,
        max: usize,
    ) -> Result<Vec<u8>, LineError> {
        let mut line = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(LineError::Timeout);
            }
            stream
                .set_read_timeout(Some(remaining))
                .map_err(LineError::Io)?;
            let n = match stream.read(&mut buf) {
                Ok(n) => n,
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                    return Err(LineError::Timeout)
                }
                Err(e) => return Err(LineError::Io(e)),
            };
            if n == 0 {
                return Ok(line);
            }
            if let Some(end) = buf[..n].iter().position(|&b| b == b'\n') {
                line.extend_from_slice(&buf[..end]);
                return if line.len() > max {
                    Err(LineError::TooLong)
                } else {
                    Ok(line)
                };
            }
            line.extend_from_slice(&buf[..n]);
            if line.len() > max {
                return Err(LineError::TooLong);
            }
        }
    }
}

#[cfg(test)]
mod tests;
