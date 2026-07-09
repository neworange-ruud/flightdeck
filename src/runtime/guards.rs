//! Hard, non-disableable security guardrails over a built `podman run` argv
//! (SPECS §31), mirroring DAC's `guards.ts`.
//!
//! Our own builder never emits a violation; guardrails defend against
//! config-driven mounts (auth paths, future custom Containerfiles) and
//! regressions. Enforced immediately before any `run` spawn.

use crate::contracts::{FlightDeckError, Result};
use std::path::{Path, PathBuf};

/// Enforce the guardrails, reading `$HOME` from the environment for the
/// home-mount check.
pub fn enforce_guardrails(args: &[String]) -> Result<()> {
    let home = std::env::var("HOME").ok().map(PathBuf::from);
    enforce_guardrails_with_home(args, home.as_deref())
}

/// Pure form: `home` is injected so the home-mount rule is testable without
/// touching the environment.
pub fn enforce_guardrails_with_home(args: &[String], home: Option<&Path>) -> Result<()> {
    for arg in args {
        if arg == "--privileged" || arg.starts_with("--privileged=") {
            return refuse("--privileged is not permitted");
        }
        if arg == "--env-host" || arg.starts_with("--env-host=") {
            return refuse("--env-host is not permitted (no full host env inheritance)");
        }
        if mentions_runtime_socket(arg) {
            return refuse("mounting a container-runtime socket is not permitted");
        }
    }

    for vol in flag_values(args, &["--volume", "-v"]) {
        let host = vol.split(':').next().unwrap_or("");
        if is_home_mount(host, home) {
            return refuse("mounting the home directory is not permitted (use a subdirectory)");
        }
    }

    for publish in flag_values(args, &["--publish", "-p"]) {
        if !publish.starts_with("127.0.0.1:") {
            return refuse(&format!(
                "port publishing must bind 127.0.0.1 (got '{publish}')"
            ));
        }
    }

    Ok(())
}

fn refuse(msg: &str) -> Result<()> {
    Err(FlightDeckError::Refused(format!(
        "container security violation: {msg}"
    )))
}

/// Whether an arg references a Docker/Podman control socket or `DOCKER_HOST`.
fn mentions_runtime_socket(arg: &str) -> bool {
    arg.contains("docker.sock")
        || arg.contains("podman.sock")
        || arg.contains("/run/podman")
        || arg.contains("/var/run/podman")
        || arg == "DOCKER_HOST"
        || arg.contains("DOCKER_HOST=")
}

/// Collect the values of repeated flags, handling both `--flag value` and
/// `--flag=value` forms.
fn flag_values(args: &[String], names: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    for (i, a) in args.iter().enumerate() {
        for n in names {
            if a == n {
                if let Some(v) = args.get(i + 1) {
                    out.push(v.clone());
                }
            } else if let Some(rest) = a.strip_prefix(&format!("{n}=")) {
                out.push(rest.to_string());
            }
        }
    }
    out
}

/// Whether `host` resolves to the same path as `home` (canonicalizing both when
/// they exist; falling back to a literal comparison otherwise).
fn is_home_mount(host: &str, home: Option<&Path>) -> bool {
    let Some(home) = home else {
        return false;
    };
    if host.is_empty() {
        return false;
    }
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    canon(Path::new(host)) == canon(home)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn allows_a_clean_run() {
        let a = args(&[
            "run",
            "--rm",
            "--cap-drop",
            "all",
            "--volume",
            "/repo/wt:/workspace",
            "--publish",
            "127.0.0.1:3000:3000",
            "img",
            "claude",
        ]);
        assert!(enforce_guardrails_with_home(&a, Some(Path::new("/home/me"))).is_ok());
    }

    #[test]
    fn rejects_privileged() {
        let a = args(&["run", "--privileged", "img"]);
        assert!(enforce_guardrails_with_home(&a, None).is_err());
    }

    #[test]
    fn rejects_env_host() {
        let a = args(&["run", "--env-host", "img"]);
        assert!(enforce_guardrails_with_home(&a, None).is_err());
    }

    #[test]
    fn rejects_privileged_equals_form() {
        let a = args(&["run", "--privileged=true", "img"]);
        assert!(enforce_guardrails_with_home(&a, None).is_err());
    }

    #[test]
    fn rejects_env_host_equals_form() {
        let a = args(&["run", "--env-host=true", "img"]);
        assert!(enforce_guardrails_with_home(&a, None).is_err());
    }

    #[test]
    fn rejects_docker_socket_mount() {
        let a = args(&[
            "run",
            "--volume",
            "/var/run/docker.sock:/var/run/docker.sock",
            "img",
        ]);
        assert!(enforce_guardrails_with_home(&a, None).is_err());
    }

    #[test]
    fn rejects_podman_socket_mount() {
        let a = args(&["run", "-v", "/run/podman/podman.sock:/x", "img"]);
        assert!(enforce_guardrails_with_home(&a, None).is_err());
    }

    #[test]
    fn rejects_docker_host_env() {
        let a = args(&["run", "--env", "DOCKER_HOST=tcp://x", "img"]);
        assert!(enforce_guardrails_with_home(&a, None).is_err());
    }

    #[test]
    fn rejects_home_mount() {
        let a = args(&["run", "--volume", "/home/me:/workspace", "img"]);
        let err = enforce_guardrails_with_home(&a, Some(Path::new("/home/me"))).unwrap_err();
        assert!(err.to_string().contains("home directory"));
    }

    #[test]
    fn allows_home_subdirectory_mount() {
        let a = args(&[
            "run",
            "--volume",
            "/home/me/.claude:/home/agent/.claude:ro",
            "img",
        ]);
        assert!(enforce_guardrails_with_home(&a, Some(Path::new("/home/me"))).is_ok());
    }

    #[test]
    fn rejects_non_loopback_publish() {
        let a = args(&["run", "--publish", "0.0.0.0:3000:3000", "img"]);
        assert!(enforce_guardrails_with_home(&a, None).is_err());
        let b = args(&["run", "-p", "3000:3000", "img"]);
        assert!(enforce_guardrails_with_home(&b, None).is_err());
    }

    #[test]
    fn handles_equals_form_flags() {
        let a = args(&["run", "--publish=0.0.0.0:80:80", "img"]);
        assert!(enforce_guardrails_with_home(&a, None).is_err());
        let b = args(&["run", "--publish=127.0.0.1:80:80", "img"]);
        assert!(enforce_guardrails_with_home(&b, None).is_ok());
    }
}
