//! The resolved data a container launch is built from (SPECS §31).
//!
//! [`ContainerSpec`] is plain owned data — assembled on the impure orchestration
//! layer (host UID, env values, platform mount flags) and consumed by the pure
//! builders in [`crate::runtime::container`]. Keeping it a value type means the
//! builders are trivially unit-testable without a container runtime.

use std::path::PathBuf;

/// A host credential resolved to an absolute path + container destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAuthMount {
    /// Absolute host path (any leading `~` already expanded).
    pub host_path: PathBuf,
    /// Absolute destination inside the container.
    pub container_path: String,
    /// Mount read-write when true, read-only otherwise.
    pub writable: bool,
}

/// Everything needed to launch the primary agent container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerSpec {
    /// Deterministic container name (`flightdeck-<id>`).
    pub name: String,
    /// Labels applied at `run` time (e.g. `flightdeck.tab`, `flightdeck.repo`).
    pub labels: Vec<(String, String)>,
    /// Image to run.
    pub image: String,
    /// Absolute host worktree path, bind-mounted at `/workspace`.
    pub workspace_host: PathBuf,
    /// The agent command run inside the container.
    pub agent_cmd: String,
    /// The agent's arguments.
    pub agent_args: Vec<String>,
    /// `--cpus` value (e.g. `"4"`).
    pub cpu: String,
    /// `--memory` value (e.g. `"8g"`).
    pub memory: String,
    /// `--pids-limit` value.
    pub pids: u32,
    /// Ports published to `127.0.0.1`.
    pub forward_ports: Vec<u16>,
    /// Read-only/-write credential mounts.
    pub auth_mounts: Vec<ResolvedAuthMount>,
    /// Secrets injected as discrete `--env KEY=VALUE` args (never interpolated).
    pub env: Vec<(String, String)>,
    /// Host UID mapped in via `--userns keep-id --user <uid>`.
    pub host_uid: u32,
    /// SELinux relabel suffix for volume mounts: `Some("z")` on Linux, `None`
    /// on macOS (virtiofs share — no relabel).
    pub mount_flags: Option<String>,
}
