//! Runtime configuration, sourced entirely from the process environment.
//!
//! Azure Container Apps (and most container platforms) configure processes
//! purely through env vars and signals, never config files or CLI flags —
//! this module is deliberately that small. Later tasks (routing, auth) will
//! likely extend `Config`, not replace this pattern.

use std::env;

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
        }
    }

    /// Read configuration from the environment. Panics only on truly
    /// unrecoverable input (none today); malformed optional values fall back
    /// to their defaults rather than failing startup.
    pub fn from_env() -> Self {
        Self::from_vars(
            env::var("PORT").ok(),
            env::var("LOG_FORMAT").ok(),
            env::var("GIT_SHA").ok(),
        )
    }

    /// Pure parsing logic, factored out of [`Config::from_env`] so it can be
    /// unit-tested without mutating process-global env vars (which would
    /// race across parallel test threads).
    fn from_vars(
        port: Option<String>,
        log_format: Option<String>,
        git_sha: Option<String>,
    ) -> Self {
        let port = port.and_then(|v| v.parse::<u16>().ok()).unwrap_or(8080);

        let log_format = match log_format {
            Some(v) if v.eq_ignore_ascii_case("json") => LogFormat::Json,
            _ => LogFormat::Pretty,
        };

        let git_sha = git_sha.unwrap_or_else(|| "unknown".to_string());

        Self {
            port,
            log_format,
            git_sha,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_when_unset() {
        let config = Config::from_vars(None, None, None);
        assert_eq!(config.port, 8080);
        assert_eq!(config.log_format, LogFormat::Pretty);
        assert_eq!(config.git_sha, "unknown");
    }

    #[test]
    fn parses_valid_overrides() {
        let config = Config::from_vars(
            Some("9090".to_string()),
            Some("JSON".to_string()),
            Some("abc1234".to_string()),
        );
        assert_eq!(config.port, 9090);
        assert_eq!(config.log_format, LogFormat::Json);
        assert_eq!(config.git_sha, "abc1234");
    }

    #[test]
    fn falls_back_on_invalid_port() {
        let config = Config::from_vars(Some("not-a-port".to_string()), None, None);
        assert_eq!(config.port, 8080);
    }
}
