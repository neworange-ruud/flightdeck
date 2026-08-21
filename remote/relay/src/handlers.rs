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
use flightdeck_remote_protocol::RelayFrame;
use futures_util::{
    stream::{SplitSink, StreamExt},
    SinkExt,
};
use serde::Serialize;
use tokio::sync::{mpsc, watch};
use tokio::time::{Duration, MissedTickBehavior};

use crate::session::Connection;
use crate::AppState;

/// Bound on a connection's outbound frame channel. Small: the queue's job is to
/// smooth momentary write bursts, not to buffer without limit — a peer that
/// cannot keep up applies back-pressure to whoever is forwarding to it (spec
/// §12 "treat delivery as at-least-once", correctness via receiver dedup).
const OUTBOUND_CHANNEL_BOUND: usize = 256;

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
/// [`handle_socket`], carrying the shared [`AppState`] in.
pub async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

/// Services a single WebSocket connection.
///
/// Splits the socket into a read half (driven by the [`Connection`] state
/// machine, spec §5) and a write half (drained by a dedicated writer task from
/// a bounded outbound channel). Keeping writes on their own task lets the relay
/// push to a connection from anywhere — its own state machine *and* the peer
/// leg forwarding envelopes — through one serialized sink, while the bounded
/// channel supplies back-pressure.
async fn handle_socket(socket: WebSocket, state: AppState) {
    let (sink, stream) = socket.split();
    let (out_tx, out_rx) = mpsc::channel::<RelayFrame>(OUTBOUND_CHANNEL_BOUND);

    // Subscribe to the process-wide shutdown notice (remote-control-0ef.18).
    // `subscribe()` works even though the original receiver returned by
    // `watch::channel` was dropped in `AppState::with_push` — the sender stays
    // usable for new subscribers for as long as it lives (in `AppState`, for
    // the process lifetime).
    let shutdown_rx = state.shutdown_tx.subscribe();
    let ping_interval = state.config.ping_interval;

    // Writer task owns the sink; it exits when the channel closes (all senders
    // dropped once the state machine returns and the registry handle is
    // detached) or when shutdown is signalled. While alive it emits a
    // server-initiated WS ping every `ping_interval` so a healthy-but-quiet
    // client keeps sending Pongs (which reset the read loop's liveness clock)
    // and a half-open socket surfaces a write error (remote-control-0ef.1).
    // Either exit completes the close handshake so the peer sees a clean close,
    // not a TCP reset.
    let writer = tokio::spawn(writer_task(sink, out_rx, ping_interval, shutdown_rx));

    // Run the state machine to completion (this also cleans up the registry).
    Connection::new(state, out_tx).run(stream).await;

    // Dropping the last sender lets the writer drain and close.
    let _ = writer.await;
}

/// Drains outbound [`RelayFrame`]s to the WebSocket sink as JSON text and, on a
/// fixed `ping_interval`, emits a server-initiated WS ping; finishes with a
/// Close frame to complete the RFC 6455 handshake cleanly.
///
/// The ping is a protocol-level [`Message::Ping`] (not a `RelayFrame`), so it
/// needs no wire change: a conformant client's WS stack answers with a Pong,
/// which the read loop counts as inbound liveness (remote-control-0ef.1). A
/// dead peer eventually fails the ping write, which also ends the task.
///
/// Also watches `shutdown_rx` (remote-control-0ef.18): long-lived `/ws`
/// connections otherwise never self-close, so a bare `axum` graceful shutdown
/// just waits on them until Azure Container Apps' post-SIGTERM grace period
/// expires and SIGKILLs the process — a hard TCP reset with no `bye`, no
/// `Close` frame, and no peer `Disconnected` presence hint. As soon as the
/// shutdown notice flips to `true`, this task proactively sends a
/// [`RelayFrame::Bye`] (reconnect-shortly hint) followed by a native WS Close.
async fn writer_task(
    mut sink: SplitSink<WebSocket, Message>,
    mut rx: mpsc::Receiver<RelayFrame>,
    ping_interval: Duration,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    // Rare race: the connection was accepted in the brief window after
    // shutdown was already signalled (`subscribe()` sees the current value
    // but `changed()` only fires on a *future* transition), so check the
    // current value once up front rather than relying solely on `changed()`.
    if *shutdown_rx.borrow() {
        send_bye_and_close(&mut sink, "relay shutting down; reconnect shortly").await;
        return;
    }

    let mut ping = tokio::time::interval(ping_interval);
    // The first tick fires immediately; consume it so the first *real* ping is
    // one full interval out, and never let a stall bunch up missed ticks.
    ping.set_missed_tick_behavior(MissedTickBehavior::Delay);
    ping.tick().await;

    loop {
        tokio::select! {
            frame = rx.recv() => {
                let Some(frame) = frame else { break }; // all senders dropped
                // Serialization of these small, well-typed frames does not fail
                // in practice; if the socket write fails the peer is gone, so
                // stop.
                match serde_json::to_string(&frame) {
                    Ok(text) => {
                        if sink.send(Message::Text(text.into())).await.is_err() {
                            return;
                        }
                    }
                    Err(err) => {
                        tracing::error!(error = %err, "failed to serialize outbound frame");
                    }
                }
            }
            _ = ping.tick() => {
                if sink.send(Message::Ping(Vec::<u8>::new().into())).await.is_err() {
                    return; // socket gone
                }
            }
            Ok(()) = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    send_bye_and_close(&mut sink, "relay shutting down; reconnect shortly").await;
                    return;
                }
            }
        }
    }
    // Drive the RFC 6455 close handshake: `close()` emits a Close frame (if one
    // has not already gone out) and flushes, so the peer sees a clean shutdown
    // rather than a TCP reset.
    let _ = sink.close().await;
}

/// Sends a [`RelayFrame::Bye`] notice (best-effort) followed by a native WS
/// Close frame, then flushes. Used on the shutdown path so a peer gets both
/// an application-level reconnect hint and a protocol-level clean close,
/// rather than the connection just vanishing when the process exits.
async fn send_bye_and_close(sink: &mut SplitSink<WebSocket, Message>, reason: &str) {
    let bye = RelayFrame::Bye {
        reason: Some(reason.to_string()),
    };
    match serde_json::to_string(&bye) {
        Ok(text) => {
            let _ = sink.send(Message::Text(text.into())).await;
        }
        Err(err) => {
            tracing::error!(error = %err, "failed to serialize shutdown bye frame");
        }
    }
    let _ = sink.close().await;
}
