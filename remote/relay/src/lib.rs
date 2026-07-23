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

pub mod apns;
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
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::{routing::get, Router};
use tower_http::trace::TraceLayer;

use apns::{NoopPushService, PushService};
use config::Config;
use router::Registry;
use store::{InMemoryStore, RelayStore};

/// Current wall-clock time in unix milliseconds. (Mirrors the private helper in
/// [`session`]; duplicated here to keep the background sweeper self-contained.)
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

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
    /// Wakes an offline phone via APNs when an event is queued for it
    /// (spec §5.5/§11). Behind a seam so it is a no-op when APNs is not
    /// configured, the live sender under the `apns-live` feature, or a
    /// recording double under test.
    pub push: Arc<dyn PushService>,
}

impl AppState {
    /// Build application state from `config`, wiring up the default in-memory
    /// store, an empty connection registry, and a push service derived from the
    /// config (the live APNs sender when both configured and built with the
    /// `apns-live` feature; otherwise a no-op).
    pub fn new(config: Config) -> Self {
        let push = default_push_service(&config);
        Self::with_push(config, push)
    }

    /// Build application state with an explicitly-supplied push service.
    /// Lets tests inject a recording [`PushService`] without touching the
    /// connection state machine.
    pub fn with_push(config: Config, push: Arc<dyn PushService>) -> Self {
        let store = Arc::new(InMemoryStore::new(config.queue_max_per_pairing));
        Self {
            config: Arc::new(config),
            store,
            registry: Arc::new(Registry::new()),
            push,
        }
    }
}

/// Choose the push service for a config: the live APNs sender when APNs is
/// configured *and* the `apns-live` feature is compiled in; a no-op otherwise
/// (so the default build — and CI without Apple secrets — still runs).
fn default_push_service(config: &Config) -> Arc<dyn PushService> {
    #[cfg(feature = "apns-live")]
    if let Some(apns) = config.apns.clone() {
        match apns::live::HttpApnsTransport::new() {
            Ok(transport) => {
                return Arc::new(apns::ApnsPushService::new(apns, transport));
            }
            Err(err) => {
                tracing::warn!(%err, "apns: could not build live transport; push disabled");
            }
        }
    }
    let _ = config; // silence unused warning when `apns-live` is off
    Arc::new(NoopPushService)
}

/// Build the relay's Axum router.
///
/// Exposed as a plain function — rather than only being assembled inside
/// `main` — so integration tests can mount the exact same app on an
/// ephemeral port without going through the process entry point.
pub fn app(config: Config) -> Router {
    let state = AppState::new(config);
    spawn_claim_sweeper(state.store.clone(), state.config.claim_sweep_interval);

    Router::new()
        .route("/healthz", get(handlers::healthz))
        .route("/readyz", get(handlers::readyz))
        .route("/version", get(handlers::version))
        .route("/ws", get(handlers::ws_handler))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Spawn the periodic claim-token sweep (remote-control-0ef.16). A
/// `pairing_offer` whose 4-digit code is never entered leaves a claim entry in
/// the store; redemption/revocation only reap tokens lazily, so without this
/// task the claim table grows unboundedly for the life of the process. The task
/// wakes every [`Config::claim_sweep_interval`] and evicts every entry past its
/// TTL. Detached: it lives for the process (or, in tests, until the runtime is
/// dropped).
fn spawn_claim_sweeper(store: Arc<dyn RelayStore>, interval: Duration) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // The first tick fires immediately; consume it so the first real sweep
        // happens one interval in (nothing can have expired at t=0).
        ticker.tick().await;
        loop {
            ticker.tick().await;
            let removed = store.sweep_expired_claims(now_ms()).await;
            if removed > 0 {
                tracing::debug!(removed, "claim sweep: evicted expired claim tokens");
            }
        }
    });
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

#[cfg(test)]
mod tests {
    use super::*;
    use flightdeck_remote_protocol::{DeviceId, PairingId};

    #[tokio::test]
    async fn claim_sweeper_evicts_expired_tokens() {
        // remote-control-0ef.16: the wired-up background sweep physically evicts
        // an abandoned (issued-but-never-redeemed, now-expired) claim token so
        // the table cannot grow without bound. The sweep *logic* is unit-tested
        // in claims.rs/store.rs; this proves the task is actually spawned and
        // ticking.
        let store = Arc::new(InMemoryStore::new(1000));
        let past = now_ms() - 10_000; // already past its TTL
        store
            .issue_claim(
                "stale".into(),
                PairingId::new("p"),
                DeviceId::new("d"),
                past,
            )
            .await;
        assert_eq!(store.claim_entry_count(), 1, "token is physically present");

        spawn_claim_sweeper(store.clone(), Duration::from_millis(20));
        let mut swept = false;
        for _ in 0..200 {
            tokio::time::sleep(Duration::from_millis(20)).await;
            if store.claim_entry_count() == 0 {
                swept = true;
                break;
            }
        }
        assert!(
            swept,
            "background sweeper should physically evict the expired token"
        );
    }
}
