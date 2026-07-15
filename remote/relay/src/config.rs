//! Runtime configuration, sourced entirely from the process environment.
//!
//! Azure Container Apps (and most container platforms) configure processes
//! purely through env vars and signals, never config files or CLI flags —
//! this module is deliberately that small. Later tasks (routing, auth) will
//! likely extend `Config`, not replace this pattern.

use std::env;

use crate::apns::ApnsConfig;

/// Log output format. Controlled by `LOG_FORMAT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    /// Human-readable output, for local development.
    Pretty,
    /// Structured JSON lines, for container log collection (Container Apps
    /// Log Analytics, etc).
    Json,
}

/// Relay service configuration, assembled once at startup.
#[derive(Debug, Clone)]
pub struct Config {
    /// TCP port to bind. `PORT`, default `8080` — the Azure Container Apps
    /// convention (the platform's ingress expects the app on this port
    /// unless configured otherwise).
    pub port: u16,

    /// Log format; see [`LogFormat`]. Log *level* is controlled separately
    /// by the standard `RUST_LOG` env var, read directly by
    /// `tracing_subscriber::EnvFilter` in [`crate::telemetry::init`].
    pub log_format: LogFormat,

    /// Git commit SHA of the running build, surfaced on `/version`. Expected
    /// to be injected by the deployment environment (e.g. a Container Apps
    /// revision env var set from the CI job's `github.sha`, or `--build-arg`
    /// threaded through the Dockerfile into an `ENV`). Falls back to
    /// `"unknown"` for local `cargo run`.
    pub git_sha: String,

    /// How long an unauthenticated connection may stay open before the relay
    /// drops it (spec §5.1 "unauthenticated connections dropped after ~10s").
    /// `AUTH_TIMEOUT_SECS`, default `10`.
    pub auth_timeout_secs: u64,

    /// Maximum number of un-acked envelopes held per `(pairing, sender)` stream
    /// before drop-oldest overflow kicks in (spec §6 amendment).
    /// `QUEUE_MAX_PER_PAIRING`, default `1000`.
    pub queue_max_per_pairing: usize,

    /// Time-to-live of a pairing claim token, in seconds (spec §5.2 "short-TTL
    /// and single-use"). `CLAIM_TOKEN_TTL_SECS`, default `120`.
    pub claim_token_ttl_secs: u64,

    /// APNs push credentials, if configured (spec §5.5). `None` disables push:
    /// the relay still queues events for `resume`, it just can't wake a
    /// backgrounded phone. Populated from `APNS_*` env vars by
    /// [`ApnsConfig::from_env`]; never set by [`Config::new`] (test builds) so
    /// tests never need Apple secrets.
    pub apns: Option<ApnsConfig>,
}

impl Config {
    /// Build a config directly, bypassing the environment. Useful for tests
    /// (this crate's own, and integration tests in `tests/`) that want a
    /// known-good `Config` — e.g. bound to an ephemeral port — without
    /// mutating process-global env vars.
    pub fn new(port: u16, log_format: LogFormat, git_sha: impl Into<String>) -> Self {
        Self {
            port,
            log_format,
            git_sha: git_sha.into(),
            auth_timeout_secs: 10,
            queue_max_per_pairing: 1000,
            claim_token_ttl_secs: 120,
            apns: None,
        }
    }

    /// Read configuration from the environment. Panics only on truly
    /// unrecoverable input (none today); malformed optional values fall back
    /// to their defaults rather than failing startup.
    pub fn from_env() -> Self {
        let mut config = Self::from_vars(
            env::var("PORT").ok(),
            env::var("LOG_FORMAT").ok(),
            env::var("GIT_SHA").ok(),
            env::var("AUTH_TIMEOUT_SECS").ok(),
            env::var("QUEUE_MAX_PER_PAIRING").ok(),
            env::var("CLAIM_TOKEN_TTL_SECS").ok(),
        );
        // APNs is read separately (not through `from_vars`) so the pure-parser
        // tests stay small and no test ever needs Apple credentials.
        config.apns = ApnsConfig::from_env();
        config
    }

    /// Pure parsing logic, factored out of [`Config::from_env`] so it can be
    /// unit-tested without mutating process-global env vars (which would
    /// race across parallel test threads).
    #[allow(clippy::too_many_arguments)]
    fn from_vars(
        port: Option<String>,
        log_format: Option<String>,
        git_sha: Option<String>,
        auth_timeout_secs: Option<String>,
        queue_max_per_pairing: Option<String>,
        claim_token_ttl_secs: Option<String>,
    ) -> Self {
        let port = port.and_then(|v| v.parse::<u16>().ok()).unwrap_or(8080);

        let log_format = match log_format {
            Some(v) if v.eq_ignore_ascii_case("json") => LogFormat::Json,
            _ => LogFormat::Pretty,
        };

        let git_sha = git_sha.unwrap_or_else(|| "unknown".to_string());

        // Malformed optional values fall back to their defaults rather than
        // failing startup (matches the existing `PORT` behavior).
        let auth_timeout_secs = auth_timeout_secs
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|&v| v > 0)
            .unwrap_or(10);
        let queue_max_per_pairing = queue_max_per_pairing
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|&v| v > 0)
            .unwrap_or(1000);
        let claim_token_ttl_secs = claim_token_ttl_secs
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|&v| v > 0)
            .unwrap_or(120);

        Self {
            port,
            log_format,
            git_sha,
            auth_timeout_secs,
            queue_max_per_pairing,
            claim_token_ttl_secs,
            apns: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_when_unset() {
        let config = Config::from_vars(None, None, None, None, None, None);
        assert_eq!(config.port, 8080);
        assert_eq!(config.log_format, LogFormat::Pretty);
        assert_eq!(config.git_sha, "unknown");
        assert_eq!(config.auth_timeout_secs, 10);
        assert_eq!(config.queue_max_per_pairing, 1000);
        assert_eq!(config.claim_token_ttl_secs, 120);
    }

    #[test]
    fn parses_valid_overrides() {
        let config = Config::from_vars(
            Some("9090".to_string()),
            Some("JSON".to_string()),
            Some("abc1234".to_string()),
            Some("30".to_string()),
            Some("50".to_string()),
            Some("300".to_string()),
        );
        assert_eq!(config.port, 9090);
        assert_eq!(config.log_format, LogFormat::Json);
        assert_eq!(config.git_sha, "abc1234");
        assert_eq!(config.auth_timeout_secs, 30);
        assert_eq!(config.queue_max_per_pairing, 50);
        assert_eq!(config.claim_token_ttl_secs, 300);
    }

    #[test]
    fn falls_back_on_invalid_port() {
        let config =
            Config::from_vars(Some("not-a-port".to_string()), None, None, None, None, None);
        assert_eq!(config.port, 8080);
    }

    #[test]
    fn falls_back_on_invalid_or_zero_tuning_values() {
        let config = Config::from_vars(
            None,
            None,
            None,
            Some("0".to_string()),   // zero timeout is nonsensical → default
            Some("nan".to_string()), // unparseable → default
            Some("0".to_string()),   // zero TTL → default
        );
        assert_eq!(config.auth_timeout_secs, 10);
        assert_eq!(config.queue_max_per_pairing, 1000);
        assert_eq!(config.claim_token_ttl_secs, 120);
    }
}
