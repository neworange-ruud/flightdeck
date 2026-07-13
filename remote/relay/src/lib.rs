//! FlightDeck Remote relay: a New Orange–operated, zero-knowledge broker
//! between the FlightDeck desktop app and the FlightDeck Remote iOS app, run
//! on Azure Container Apps. See `specs/MOBILE_REMOTE_PRD.md` §9 for the full
//! architecture (desktop keeps an outbound connection; phones connect in;
//! the relay routes ciphertext by pairing ID and can never read content).
//!
//! **This crate is a scaffold.** It stands up the production shape — HTTP/WS
//! surface, env-based config, structured logging, graceful shutdown — with
//! no business logic. Routing (matching a phone to its desktop by pairing
//! ID and forwarding ciphertext) and auth (verifying per-device identity
//! keypairs) are separate, later tasks; see the [`router`] module doc
//! comment for the seam they plug into.

pub mod config;
pub mod handlers;
pub mod router;
pub mod telemetry;

use std::sync::Arc;

use axum::{routing::get, Router};
use tower_http::trace::TraceLayer;

use config::Config;

/// Shared application state, handed to every handler via `axum::State`.
///
/// Kept to just the config for now. The routing task will likely add a
/// connection registry here (see [`router`]); the auth task will likely add
/// whatever key material / verifier it needs.
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
}

/// Build the relay's Axum router.
///
/// Exposed as a plain function — rather than only being assembled inside
/// `main` — so integration tests can mount the exact same app on an
/// ephemeral port without going through the process entry point.
pub fn app(config: Config) -> Router {
    let state = AppState {
        config: Arc::new(config),
    };

    Router::new()
        .route("/healthz", get(handlers::healthz))
        .route("/readyz", get(handlers::readyz))
        .route("/version", get(handlers::version))
        .route("/ws", get(handlers::ws_handler))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Resolves once the process receives a shutdown signal: SIGTERM (what
/// Azure Container Apps sends on scale-down, redeploy, or revision
/// deactivation) or Ctrl-C (local `cargo run`), whichever comes first.
///
/// Intended to be passed to `axum::serve(..).with_graceful_shutdown(..)` so
/// in-flight requests (including open WebSocket connections) get a chance
/// to drain instead of being hard-killed.
pub async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => tracing::info!("received Ctrl+C, shutting down"),
        _ = terminate => tracing::info!("received SIGTERM, shutting down"),
    }
}
