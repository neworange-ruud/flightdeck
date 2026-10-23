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
use std::sync::mpsc::{Receiver, Sender};
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

/// Send `req` to the worker listening on `socket` and wait up to `timeout`
/// for its answer.
pub fn request(
    _socket: &Path,
    _req: &ControlRequest,
    _timeout: Duration,
) -> Result<ControlResponse, ControlError> {
    // stub: filled by G
    Err(ControlError::NotRunning)
}

/// The running worker's pid and version, or `None` when nothing answers.
pub fn ping(_paths: &WorkerPaths) -> Option<(u32, String)> {
    // stub: filled by G
    None
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

/// The listening control socket; dropping it stops serving.
pub struct ControlServer {
    // stub: filled by G (the listener thread and its stop flag)
    _stub: (),
}

/// Listen on `socket` and hand each request to the returned receiver.
pub fn serve(_socket: &Path) -> std::io::Result<(ControlServer, Receiver<ControlCall>)> {
    // stub: filled by G
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "the worker control socket is not implemented yet",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn request_wire_shapes() {
        let cases = [
            (ControlRequest::Ping, json!({"op": "ping"})),
            (ControlRequest::Status, json!({"op": "status"})),
            (ControlRequest::Pair, json!({"op": "pair"})),
            (ControlRequest::Devices, json!({"op": "devices"})),
            (
                ControlRequest::Revoke {
                    device: "mbp".to_string(),
                },
                json!({"op": "revoke", "device": "mbp"}),
            ),
            (
                ControlRequest::PrepareShutdown { restarting: true },
                json!({"op": "prepare_shutdown", "restarting": true}),
            ),
        ];
        for (req, wire) in cases {
            assert_eq!(serde_json::to_value(&req).unwrap(), wire);
            assert_eq!(serde_json::from_value::<ControlRequest>(wire).unwrap(), req);
        }
    }

    #[test]
    fn response_wire_shapes() {
        let device = DeviceReport {
            id: "tok1".to_string(),
            label: "mbp".to_string(),
            address: None,
            paired_at: 10,
            last_seen: Some(20),
            state: DeviceState::Background,
            revoked: false,
        };
        let device_json = json!({
            "id": "tok1", "label": "mbp", "address": null, "paired_at": 10,
            "last_seen": 20, "state": "background", "revoked": false
        });
        let cases = [
            (
                ControlResponse::Pong {
                    pid: 7,
                    version: "1.0.0".to_string(),
                },
                json!({"result": "pong", "pid": 7, "version": "1.0.0"}),
            ),
            (
                ControlResponse::Paired(PairingCode {
                    code: "1234".to_string(),
                    expires_in_secs: 120,
                    address: Some("box.ts.net:7420".to_string()),
                    listening: true,
                    not_listening_reason: None,
                }),
                json!({
                    "result": "paired", "code": "1234", "expires_in_secs": 120,
                    "address": "box.ts.net:7420", "listening": true,
                    "not_listening_reason": null
                }),
            ),
            (
                ControlResponse::Devices {
                    devices: vec![device.clone()],
                },
                json!({"result": "devices", "devices": [device_json.clone()]}),
            ),
            (
                ControlResponse::Revoked {
                    device,
                    closed_connections: 2,
                },
                json!({"result": "revoked", "device": device_json, "closed_connections": 2}),
            ),
            (ControlResponse::Ok, json!({"result": "ok"})),
            (
                ControlResponse::Error {
                    message: "no such device".to_string(),
                },
                json!({"result": "error", "message": "no such device"}),
            ),
        ];
        for (resp, wire) in cases {
            assert_eq!(serde_json::to_value(&resp).unwrap(), wire);
            assert_eq!(
                serde_json::from_value::<ControlResponse>(wire).unwrap(),
                resp
            );
        }
    }

    #[test]
    fn status_response_round_trips() {
        let resp =
            ControlResponse::Status(Box::new(crate::worker::status::tests::sample_runtime()));
        let wire = serde_json::to_value(&resp).unwrap();
        assert_eq!(wire["result"], "status");
        assert_eq!(
            serde_json::from_value::<ControlResponse>(wire).unwrap(),
            resp
        );
    }

    #[test]
    fn a_reply_to_a_departed_client_is_dropped_quietly() {
        let (tx, rx) = std::sync::mpsc::channel();
        drop(rx);
        let call = ControlCall {
            request: ControlRequest::Ping,
            reply: tx,
        };
        call.reply(ControlResponse::Ok);
    }
}
