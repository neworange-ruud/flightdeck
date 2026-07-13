//! HTTP and WebSocket handlers.
//!
//! No business logic lives here yet — just the surface: liveness/readiness
//! probes, a version endpoint, and a WebSocket endpoint that accepts a
//! connection and closes cleanly. Pairing-ID routing plugs into
//! [`ws_handler`] later; see the [`crate::router`] module doc comment.

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::IntoResponse,
    Json,
};
use serde::Serialize;

use crate::AppState;

/// Liveness probe: "is the process up and able to handle a request at all."
/// Container Apps restarts the replica if this fails repeatedly. Deliberately
/// has no dependencies on anything else being healthy.
pub async fn healthz() -> &'static str {
    "ok"
}

/// Readiness probe: "should traffic be routed to this replica right now."
/// Identical to `healthz` today because the scaffold has no external
/// dependencies (DB, queue, etc) to be un-ready for. Once the relay gains
/// dependencies, this is the handler to wire a real check into — Container
/// Apps stops sending new traffic (but doesn't restart) on failure here.
pub async fn readyz() -> &'static str {
    "ok"
}

#[derive(Debug, Serialize)]
pub struct VersionInfo {
    /// The crate version (`CARGO_PKG_VERSION`), i.e. `Cargo.toml`'s
    /// `[package].version`.
    pub version: &'static str,
    /// Git commit SHA of the running build; see [`crate::config::Config::git_sha`]
    /// for how it's sourced.
    pub git_sha: String,
}

/// Reports the running build's crate version and git SHA. Useful for
/// confirming which revision a Container Apps replica is actually running.
pub async fn version(State(state): State<AppState>) -> Json<VersionInfo> {
    Json(VersionInfo {
        version: env!("CARGO_PKG_VERSION"),
        git_sha: state.config.git_sha.clone(),
    })
}

/// Upgrades an HTTP connection to a WebSocket and hands it to
/// [`handle_socket`].
pub async fn ws_handler(ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(handle_socket)
}

/// Services a single WebSocket connection.
///
/// Scaffold behavior only: read frames until the peer closes or the
/// connection errors, then return (which sends a close frame and drops the
/// socket). Ping frames need no handling here — axum's WebSocket
/// implementation (via `tungstenite`) answers them with a Pong
/// automatically as part of driving the read loop; see the note on
/// `axum::extract::ws::Message::Ping`.
///
/// Text/Binary frames are intentionally ignored: this scaffold does not
/// route anything. That's the [`crate::router`] seam's job.
async fn handle_socket(mut socket: WebSocket) {
    loop {
        match socket.recv().await {
            // The WebSocket close handshake (RFC 6455 §7.1.5) requires the
            // side that receives a close frame to echo one back before the
            // TCP connection goes away — otherwise the peer sees an abrupt
            // reset rather than a clean close. The underlying `tungstenite`
            // stack queues that reply automatically when it reads a Close
            // frame, but only flushes it as a side effect of the *next*
            // read — so one more `recv()` (whatever it returns; the
            // connection is going away either way) is what actually puts
            // the reply on the wire before we drop the socket.
            Some(Ok(Message::Close(_))) => {
                let _ = socket.recv().await;
                break;
            }
            // Peer sent a real message; nothing to do with it yet (no
            // routing in this scaffold) but keep servicing the connection so
            // ping/pong keeps flowing and the loop notices a later close.
            Some(Ok(_)) => continue,
            // Peer error or the underlying connection dropped — nothing more
            // to service.
            Some(Err(_)) => break,
            // Peer closed the stream without a close frame.
            None => break,
        }
    }
}
