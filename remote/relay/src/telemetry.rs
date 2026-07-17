//! Structured logging setup (`tracing` + `tracing-subscriber`).

use crate::config::LogFormat;
use tracing_subscriber::EnvFilter;

/// Initialize the global tracing subscriber. Call exactly once, at process
/// start, before any other code logs.
///
/// - **Level**: controlled by the standard `RUST_LOG` env var (e.g.
///   `RUST_LOG=info,flightdeck_relay=debug`); defaults to `info` when unset
///   or invalid.
/// - **Format**: `LogFormat::Json` emits one JSON object per line (for
///   Container Apps / Log Analytics ingestion); `LogFormat::Pretty` emits
///   human-readable output (for local `cargo run`).
pub fn init(format: LogFormat) {
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let builder = tracing_subscriber::fmt().with_env_filter(env_filter);

    match format {
        LogFormat::Json => builder.json().with_target(true).init(),
        LogFormat::Pretty => builder.init(),
    }
}
