//! Pure `podman` argv builders (SPECS §31).
//!
//! These are the **only** place `run`/`attach`/`exec` argv is constructed. They
//! are pure functions over a [`ContainerSpec`] / name, so they unit-test against
//! the expected vector with no container runtime present (mirroring DAC's
//! `buildRunArgs`). Every value crosses as a discrete argv element, so secrets
//! are never shell-interpolated.

use crate::runtime::name::{LABEL_REPO, LABEL_TAB};
use crate::runtime::spec::{ContainerSpec, ResolvedAuthMount};
use std::path::Path;

/// The container workdir the host worktree is mounted at.
pub const WORKSPACE: &str = "/workspace";

/// Build the `podman run …` argv (everything after the `podman` binary name).
pub fn build_run_args(spec: &ContainerSpec) -> Vec<String> {
    // Accumulate into `a`; the `flag!` macro pushes one or more `&str` literals.
    let mut a: Vec<String> = Vec::new();
    macro_rules! flag {
        ($($s:expr),+ $(,)?) => {{ $( a.push($s.to_string()); )+ }};
    }

    flag!("run", "-it", "--rm");

    flag!("--name");
    a.push(spec.name.clone());

    for (k, v) in &spec.labels {
        flag!("--label");
        a.push(format!("{k}={v}"));
    }

    // Hard security posture (also guaranteed by guardrails).
    flag!("--cap-drop", "all", "--security-opt", "no-new-privileges");

    flag!("--cpus");
    a.push(spec.cpu.clone());
    flag!("--memory");
    a.push(spec.memory.clone());
    flag!("--pids-limit");
    a.push(spec.pids.to_string());

    // 1:1 host UID mapping so the agent owns the mounted workspace.
    flag!("--userns", "keep-id", "--user");
    a.push(spec.host_uid.to_string());

    flag!("--workdir", WORKSPACE);

    // Workspace bind mount (read-write).
    flag!("--volume");
    a.push(volume_arg(
        &spec.workspace_host,
        WORKSPACE,
        false,
        &spec.mount_flags,
    ));

    // Credential mounts.
    for m in &spec.auth_mounts {
        flag!("--volume");
        a.push(auth_volume_arg(m, &spec.mount_flags));
    }

    // Loopback-only published ports.
    for p in &spec.forward_ports {
        flag!("--publish");
        a.push(format!("127.0.0.1:{p}:{p}"));
    }

    // Injected secrets (discrete args).
    for (k, v) in &spec.env {
        flag!("--env");
        a.push(format!("{k}={v}"));
    }

    // Marker env so the agent (and its hooks) can tell it is containerized.
    flag!("--env", "FLIGHTDECK=1", "--env");
    a.push(format!(
        "FLIGHTDECK_TAB={}",
        label_value(&spec.labels, LABEL_TAB)
    ));

    // Image, then the agent command line.
    a.push(spec.image.clone());
    a.push(spec.agent_cmd.clone());
    a.extend(spec.agent_args.iter().cloned());

    a
}

/// Build the `podman attach <name>` argv used to reconnect to a still-running
/// container after a FlightDeck restart.
pub fn build_attach_args(name: &str) -> Vec<String> {
    vec!["attach".to_string(), name.to_string()]
}

/// Build the `podman exec -it <name> <shell> [args…]` argv for a child shell
/// inside the agent's container.
pub fn build_exec_args(name: &str, shell_cmd: &str, shell_args: &[String]) -> Vec<String> {
    let mut a = vec![
        "exec".to_string(),
        "-it".to_string(),
        name.to_string(),
        shell_cmd.to_string(),
    ];
    a.extend(shell_args.iter().cloned());
    a
}

/// `host:dst[:opts]` for the workspace mount.
fn volume_arg(host: &Path, dst: &str, read_only: bool, flags: &Option<String>) -> String {
    let mut opts: Vec<&str> = Vec::new();
    if read_only {
        opts.push("ro");
    }
    if let Some(f) = flags {
        opts.push(f);
    }
    if opts.is_empty() {
        format!("{}:{}", host.display(), dst)
    } else {
        format!("{}:{}:{}", host.display(), dst, opts.join(","))
    }
}

/// `host:dst[:opts]` for an auth mount (read-only unless `writable`).
fn auth_volume_arg(m: &ResolvedAuthMount, flags: &Option<String>) -> String {
    volume_arg(&m.host_path, &m.container_path, !m.writable, flags)
}

/// Look up a label's value, defaulting to the empty string.
fn label_value(labels: &[(String, String)], key: &str) -> String {
    labels
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.clone())
        .unwrap_or_default()
}

/// Convenience: the standard label set for a tab/repo.
pub fn standard_labels(tab_id: &str, repo_hash: &str) -> Vec<(String, String)> {
    vec![
        (LABEL_TAB.to_string(), tab_id.to_string()),
        (LABEL_REPO.to_string(), repo_hash.to_string()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn base_spec() -> ContainerSpec {
        ContainerSpec {
            name: "flightdeck-x".to_string(),
            labels: standard_labels("x", "deadbeef"),
            image: "localhost/img:local".to_string(),
            workspace_host: PathBuf::from("/repo/.flightdeck/worktrees/x"),
            agent_cmd: "claude".to_string(),
            agent_args: vec!["--foo".to_string()],
            cpu: "4".to_string(),
            memory: "8g".to_string(),
            pids: 512,
            forward_ports: vec![],
            auth_mounts: vec![],
            env: vec![],
            host_uid: 501,
            mount_flags: None,
        }
    }

    /// Find the value following the i-th occurrence of `flag`.
    fn value_after<'a>(args: &'a [String], flag: &str) -> Option<&'a String> {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
    }
    fn all_values_after(args: &[String], flag: &str) -> Vec<String> {
        let mut out = Vec::new();
        for (i, a) in args.iter().enumerate() {
            if a == flag {
                if let Some(v) = args.get(i + 1) {
                    out.push(v.clone());
                }
            }
        }
        out
    }

    #[test]
    fn run_args_have_security_posture() {
        let a = build_run_args(&base_spec());
        assert_eq!(a[0], "run");
        assert!(a.contains(&"--rm".to_string()));
        // cap-drop all + no-new-privileges present.
        assert_eq!(value_after(&a, "--cap-drop"), Some(&"all".to_string()));
        assert_eq!(
            value_after(&a, "--security-opt"),
            Some(&"no-new-privileges".to_string())
        );
    }

    #[test]
    fn run_args_map_uid_and_limits() {
        let a = build_run_args(&base_spec());
        assert_eq!(value_after(&a, "--userns"), Some(&"keep-id".to_string()));
        assert_eq!(value_after(&a, "--user"), Some(&"501".to_string()));
        assert_eq!(value_after(&a, "--cpus"), Some(&"4".to_string()));
        assert_eq!(value_after(&a, "--memory"), Some(&"8g".to_string()));
        assert_eq!(value_after(&a, "--pids-limit"), Some(&"512".to_string()));
    }

    #[test]
    fn run_args_mount_workspace_and_workdir() {
        let a = build_run_args(&base_spec());
        assert_eq!(
            value_after(&a, "--workdir"),
            Some(&"/workspace".to_string())
        );
        let vols = all_values_after(&a, "--volume");
        assert_eq!(
            vols,
            vec!["/repo/.flightdeck/worktrees/x:/workspace".to_string()]
        );
    }

    #[test]
    fn run_args_workspace_uses_mount_flags_when_linux() {
        let mut spec = base_spec();
        spec.mount_flags = Some("z".to_string());
        let a = build_run_args(&spec);
        let vols = all_values_after(&a, "--volume");
        assert_eq!(vols[0], "/repo/.flightdeck/worktrees/x:/workspace:z");
    }

    #[test]
    fn run_args_image_then_agent_command_last() {
        let a = build_run_args(&base_spec());
        let img_idx = a.iter().position(|x| x == "localhost/img:local").unwrap();
        assert_eq!(a[img_idx + 1], "claude");
        assert_eq!(a[img_idx + 2], "--foo");
        assert_eq!(img_idx + 2, a.len() - 1, "agent command is the tail");
    }

    #[test]
    fn run_args_publish_ports_loopback_only() {
        let mut spec = base_spec();
        spec.forward_ports = vec![3000, 8080];
        let a = build_run_args(&spec);
        let pubs = all_values_after(&a, "--publish");
        assert_eq!(pubs, vec!["127.0.0.1:3000:3000", "127.0.0.1:8080:8080"]);
    }

    #[test]
    fn run_args_inject_env_and_auth_mounts() {
        let mut spec = base_spec();
        spec.env = vec![("ANTHROPIC_API_KEY".to_string(), "sk-xyz".to_string())];
        spec.auth_mounts = vec![ResolvedAuthMount {
            host_path: PathBuf::from("/home/me/.claude"),
            container_path: "/home/agent/.claude".to_string(),
            writable: false,
        }];
        let a = build_run_args(&spec);
        let envs = all_values_after(&a, "--env");
        assert!(envs.contains(&"ANTHROPIC_API_KEY=sk-xyz".to_string()));
        assert!(envs.contains(&"FLIGHTDECK=1".to_string()));
        assert!(envs.contains(&"FLIGHTDECK_TAB=x".to_string()));
        let vols = all_values_after(&a, "--volume");
        assert!(vols.contains(&"/home/me/.claude:/home/agent/.claude:ro".to_string()));
    }

    #[test]
    fn writable_auth_mount_has_no_ro() {
        let mut spec = base_spec();
        spec.auth_mounts = vec![ResolvedAuthMount {
            host_path: PathBuf::from("/home/me/.claude"),
            container_path: "/home/agent/.claude".to_string(),
            writable: true,
        }];
        let a = build_run_args(&spec);
        let vols = all_values_after(&a, "--volume");
        assert!(vols.contains(&"/home/me/.claude:/home/agent/.claude".to_string()));
    }

    #[test]
    fn attach_and_exec_args() {
        assert_eq!(
            build_attach_args("flightdeck-x"),
            vec!["attach", "flightdeck-x"]
        );
        assert_eq!(
            build_exec_args("flightdeck-x", "/bin/zsh", &[]),
            vec!["exec", "-it", "flightdeck-x", "/bin/zsh"]
        );
        assert_eq!(
            build_exec_args("flightdeck-x", "bash", &["-l".to_string()]),
            vec!["exec", "-it", "flightdeck-x", "bash", "-l"]
        );
    }
}
