//! FlightDeck Remote relay: a New Orange–operated, zero-knowledge broker
//! between the FlightDeck desktop app and the FlightDeck Remote iOS app, run
//! on Azure Container Apps. See `specs/MOBILE_REMOTE_PRD.md` §9 for the full
//! architecture (desktop keeps an outbound connection; phones connect in;
//! the relay routes ciphertext by pairing ID and can never read content).
//!
//! The relay implements the phone ⇄ desktop wire protocol
//! (`flightdeck-remote-protocol`): version negotiation, per-device ECDSA P-256
//! challenge-response auth ([`auth`]), pairing bootstrap + claim tokens
//! ([`claims`]), zero-knowledge envelope routing by pairing id ([`router`]),
//! and a server-side pending-event queue with gapless sequencing, resume, and
//! cumulative-ack pruning ([`queue`]). Durable state sits behind the
//! [`store::RelayStore`] seam (in-memory for v1). The per-connection state
//! machine lives in [`session`].

pub mod auth;
pub mod claims;
pub mod config;
pub mod handlers;
pub mod ids;
pub mod queue;
pub mod router;
pub mod session;
pub mod store;
pub mod telemetry;

use std::sync::Arc;

use axum::{routing::get, Router};
use tower_http::trace::TraceLayer;

use config::Config;
use router::Registry;
use store::{InMemoryStore, RelayStore};

/// Shared application state, handed to every handler via `axum::State`.
#[derive(Clone)]
pub struct AppState {
    /// Immutable runtime configuration.
    pub config: Arc<Config>,
    /// Durable relay state (device keys, pairings, claim tokens, queues) behind
    /// the persistence seam. v1 is [`InMemoryStore`]; a Redis/Azure impl slots
    /// in here without touching the connection state machine.
    pub store: Arc<dyn RelayStore>,
    /// The live, ephemeral connection routing table.
    pub registry: Arc<Registry>,
}

impl AppState {
    /// Build application state from `config`, wiring up the default in-memory
    /// store and an empty connection registry.
    pub fn new(config: Config) -> Self {
        let store = Arc::new(InMemoryStore::new(config.queue_max_per_pairing));
        Self {
            config: Arc::new(config),
            store,
            registry: Arc::new(Registry::new()),
        }
    }
}

/// Build the relay's Axum router.
///
/// Exposed as a plain function — rather than only being assembled inside
/// `main` — so integration tests can mount the exact same app on an
/// ephemeral port without going through the process entry point.
pub fn app(config: Config) -> Router {
    let state = AppState::new(config);

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
