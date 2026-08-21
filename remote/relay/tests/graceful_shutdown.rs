//! Graceful-shutdown coverage (remote-control-0ef.18): a connected `/ws`
//! client must see a clean `bye` + native WebSocket Close frame when the
//! relay is signalled to shut down, instead of the connection just vanishing
//! (a hard TCP reset) when the process later exits.
//!
//! These tests drive `AppState::shutdown_tx` directly (via
//! `support::spawn_app_with_shutdown`) rather than sending the process a real
//! SIGTERM/Ctrl-C — `shutdown_signal` itself is a thin `tokio::select!` over
//! OS signal futures with no branching logic of its own to test; the
//! behavior this issue is actually about lives in `handlers::writer_task`,
//! which is what these tests exercise end-to-end over a real socket.

mod support;

use flightdeck_remote_protocol::{RelayFrame, Role};
use support::{hello_probe, spawn_app_with_shutdown, TestClient};
use tokio_tungstenite::tungstenite::Message as WsMessage;

/// A connected (but not yet authenticated) client receives a `bye` frame
/// followed by a native WS Close frame as soon as shutdown is signalled —
/// the writer task reacts regardless of handshake phase, since a stuck client
/// mid-handshake must not be able to block shutdown either.
#[tokio::test]
async fn shutdown_sends_bye_then_close_before_auth() {
    let (base_url, shutdown_tx) = spawn_app_with_shutdown().await;
    let mut client = TestClient::connect(&base_url, Role::Desktop, "dev_shutdown_preauth").await;

    shutdown_tx.send_modify(|v| *v = true);

    match client.recv().await {
        RelayFrame::Bye { .. } => {}
        other => panic!("expected bye, got {other:?}"),
    }
    match client.recv_raw().await {
        Some(WsMessage::Close(_)) | None => {}
        other => panic!("expected a close frame (or stream end), got {other:?}"),
    }
}

/// Same guarantee once a connection is fully authenticated and routing
/// envelopes — the shutdown path does not depend on connection phase.
#[tokio::test]
async fn shutdown_sends_bye_then_close_after_auth() {
    let (base_url, shutdown_tx) = spawn_app_with_shutdown().await;

    let mut desktop = TestClient::connect(&base_url, Role::Desktop, "dev_shutdown_desktop").await;
    let (pairing_id, claim_token) = desktop.offer_pairing().await;
    desktop.authenticate(vec![pairing_id.clone()]).await;

    let mut phone = TestClient::connect(&base_url, Role::Phone, "dev_shutdown_phone").await;
    phone.claim_pairing(&claim_token).await;
    phone.authenticate(vec![pairing_id]).await;
    // Drain the claim/auth chatter on *both* legs so it isn't mistaken for the
    // shutdown `bye` below: the desktop sees `pairing_claimed` then
    // `peer_presence` (phone connected); separately, the phone's own
    // `auth_response` handling also announces the phone's view of
    // `peer_presence` (desktop connected) right after `auth_ok`, which
    // `authenticate()` does not itself drain.
    desktop
        .recv_until(|f| matches!(f, RelayFrame::PeerPresence { .. }))
        .await;
    phone
        .recv_until(|f| matches!(f, RelayFrame::PeerPresence { .. }))
        .await;

    shutdown_tx.send_modify(|v| *v = true);

    for client in [&mut desktop, &mut phone] {
        match client.recv().await {
            RelayFrame::Bye { .. } => {}
            other => panic!("expected bye, got {other:?}"),
        }
        match client.recv_raw().await {
            Some(WsMessage::Close(_)) | None => {}
            other => panic!("expected a close frame (or stream end), got {other:?}"),
        }
    }
}

/// A client that connects *after* shutdown has already been signalled (the
/// narrow race between SIGTERM and the listener actually stopping) gets a
/// `bye` as its very first frame — before even `hello_ok` — rather than
/// hanging through a handshake the relay is already abandoning. Uses the raw
/// `hello_probe` helper (not `TestClient::connect`, which asserts `hello_ok`
/// is the first frame) since giving up immediately, without completing
/// version negotiation, is exactly the behavior under test.
#[tokio::test]
async fn shutdown_already_signalled_before_connect_still_closes_cleanly() {
    let (base_url, shutdown_tx) = spawn_app_with_shutdown().await;
    shutdown_tx.send_modify(|v| *v = true);

    let frame = hello_probe(&base_url, None).await;
    assert!(
        matches!(frame, RelayFrame::Bye { .. }),
        "expected bye, got {frame:?}"
    );
}
