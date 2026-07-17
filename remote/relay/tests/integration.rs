//! End-to-end tests against a real, in-process instance of the relay.
//!
//! Each test binds the app from `flightdeck_relay::app` to an ephemeral
//! (`:0`) TCP port so tests can run concurrently without port clashes, then
//! talks to it exactly as a real client would: plain HTTP for the probes,
//! `tokio-tungstenite` as a WebSocket client for `/ws`.

use flightdeck_relay::{
    app,
    config::{Config, LogFormat},
};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message as WsMessage;

/// Binds the app to an ephemeral port and spawns it on a background task.
/// Returns the base HTTP URL (e.g. `http://127.0.0.1:54321`).
async fn spawn_app() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("failed to bind ephemeral port");
    let addr = listener.local_addr().expect("failed to read local addr");

    let config = Config::new(addr.port(), LogFormat::Pretty, "test-sha");
    tokio::spawn(async move {
        axum::serve(listener, app(config))
            .await
            .expect("server exited unexpectedly");
    });

    format!("http://{addr}")
}

#[tokio::test]
async fn healthz_reports_ok() {
    let base_url = spawn_app().await;

    let response = reqwest::get(format!("{base_url}/healthz"))
        .await
        .expect("request failed");

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(response.text().await.unwrap(), "ok");
}

#[tokio::test]
async fn readyz_reports_ok() {
    let base_url = spawn_app().await;

    let response = reqwest::get(format!("{base_url}/readyz"))
        .await
        .expect("request failed");

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(response.text().await.unwrap(), "ok");
}

#[tokio::test]
async fn version_reports_crate_version_and_git_sha() {
    let base_url = spawn_app().await;

    let response = reqwest::get(format!("{base_url}/version"))
        .await
        .expect("request failed");
    assert_eq!(response.status(), reqwest::StatusCode::OK);

    let body: serde_json::Value = response.json().await.expect("invalid JSON body");
    assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(body["git_sha"], "test-sha");
}

#[tokio::test]
async fn websocket_ping_pong_and_clean_close() {
    let base_url = spawn_app().await;
    let ws_url = format!("{}/ws", base_url.replacen("http://", "ws://", 1));

    let (mut socket, response) = tokio_tungstenite::connect_async(ws_url)
        .await
        .expect("websocket handshake failed");
    assert_eq!(response.status(), 101, "expected a protocol switch");

    // The underlying tungstenite stack answers pings automatically as part
    // of driving the connection; sending one and reading the reply exercises
    // that the server is actually pumping the socket, not just accepting the
    // upgrade and going silent.
    socket
        .send(WsMessage::Ping(vec![1, 2, 3].into()))
        .await
        .expect("failed to send ping");

    let pong = socket
        .next()
        .await
        .expect("connection closed before a reply arrived")
        .expect("websocket error");
    assert_eq!(pong, WsMessage::Pong(vec![1, 2, 3].into()));

    // Now close cleanly and confirm the server responds with its own close
    // frame rather than dropping the TCP connection abruptly. Sending a bare
    // `Message::Close` (rather than `SinkExt::close`, which tears down the
    // sink half immediately) mirrors what a real client does: send a close
    // frame, then keep reading until the peer's close frame — or end of
    // stream — completes the handshake.
    socket
        .send(WsMessage::Close(None))
        .await
        .expect("failed to send close frame");

    let after_close = socket.next().await;
    match after_close {
        // A close frame echoed back, or the stream simply ending, both count
        // as a clean shutdown.
        Some(Ok(WsMessage::Close(_))) | None => {}
        other => panic!("expected a clean close, got: {other:?}"),
    }
}
