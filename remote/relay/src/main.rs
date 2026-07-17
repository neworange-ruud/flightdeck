//! FlightDeck Remote relay — process entry point.
//!
//! Wires together config, logging, and the axum app defined in `lib.rs`,
//! binds a TCP listener, and serves with graceful shutdown. Business logic
//! lives in the library crate so integration tests can exercise the same
//! `app()` in-process, without spawning this binary.

use std::net::SocketAddr;

use flightdeck_relay::{app, config::Config, shutdown_signal, telemetry};

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

    if let Err(err) = axum::serve(listener, app(config))
        .with_graceful_shutdown(shutdown_signal())
        .await
    {
        tracing::error!(error = %err, "server error");
        std::process::exit(1);
    }
}
