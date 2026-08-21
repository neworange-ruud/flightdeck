//! FlightDeck Remote relay — process entry point.
//!
//! Wires together config, logging, and the axum app defined in `lib.rs`,
//! binds a TCP listener, and serves with graceful shutdown. Business logic
//! lives in the library crate so integration tests can exercise the same
//! `app()` in-process, without spawning this binary.

use std::net::SocketAddr;
use std::time::Duration;

use flightdeck_relay::{build, config::Config, shutdown_signal, telemetry};

/// Upper bound on the *entire* graceful-shutdown window: once the shutdown
/// signal fires, connections are notified (see `shutdown_signal`,
/// `handlers::writer_task`) and given this long to finish before the process
/// exits regardless. Bounds it so a stuck/unresponsive client can never hold
/// the process open indefinitely — Azure Container Apps' own SIGKILL grace
/// period is the backstop of last resort, but this keeps a normal redeploy
/// fast and predictable rather than riding that backstop every time
/// (remote-control-0ef.18).
const SHUTDOWN_GRACE_PERIOD: Duration = Duration::from_secs(5);

#[tokio::main]
async fn main() {
    let config = Config::from_env();
    telemetry::init(config.log_format);

    let addr = SocketAddr::from(([0, 0, 0, 0], config.port));
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(listener) => listener,
        Err(err) => {
            tracing::error!(%addr, error = %err, "failed to bind listener");
            std::process::exit(1);
        }
    };

    tracing::info!(
        %addr,
        version = env!("CARGO_PKG_VERSION"),
        git_sha = %config.git_sha,
        "flightdeck-relay listening"
    );

    let (router, shutdown_tx) = build(config);

    // Observe the shutdown signal so the grace timer below starts only *after*
    // shutdown is actually signalled — subscribe before moving `shutdown_tx`
    // into the signal future. A plain `timeout(GRACE, serve)` would instead
    // bound the *entire* server lifetime, force-exiting every long-lived
    // session after a few seconds even with no shutdown in sight.
    let mut shutdown_rx = shutdown_tx.subscribe();
    let serve = axum::serve(listener, router).with_graceful_shutdown(shutdown_signal(shutdown_tx));

    tokio::select! {
        // The server finished on its own — either it never shut down (runs
        // until the process is killed) or graceful drain completed in time.
        res = serve => {
            if let Err(err) = res {
                tracing::error!(error = %err, "server error");
                std::process::exit(1);
            }
        }
        // Bound only the *post-signal* drain: park on `changed()` until the
        // shutdown signal flips the watch to `true`, then give connections
        // SHUTDOWN_GRACE_PERIOD to finish before forcing exit. Connections were
        // already notified (bye + Close frame) the instant the signal fired.
        _ = async move {
            loop {
                if *shutdown_rx.borrow_and_update() {
                    break;
                }
                if shutdown_rx.changed().await.is_err() {
                    // Sender dropped without ever signalling: park forever so
                    // this branch never wins the race — the server future is
                    // then the only path to completion.
                    std::future::pending::<()>().await;
                }
            }
            tokio::time::sleep(SHUTDOWN_GRACE_PERIOD).await;
        } => {
            tracing::warn!(
                grace_secs = SHUTDOWN_GRACE_PERIOD.as_secs(),
                "graceful shutdown grace period elapsed with connections still open; forcing exit"
            );
        }
    }
}
