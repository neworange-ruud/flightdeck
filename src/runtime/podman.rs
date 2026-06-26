//! The production [`ContainerRuntime`] — shells out to the `podman` binary
//! (SPECS §31). Mirrors how `GitExecutor`/`PtyBackend` keep their real impls
//! beside their owning module rather than in `contracts::real`.
//!
//! Only the non-interactive control plane lives here; `run`/`attach`/`exec` go
//! through the [`crate::contracts::PtyBackend`] with argv from
//! [`crate::runtime::container`].

use crate::contracts::{ContainerRuntime, ContainerState, FlightDeckError, Result};
use std::path::Path;
use std::process::Command;

/// `podman`-backed [`ContainerRuntime`].
#[derive(Debug, Default, Clone, Copy)]
pub struct PodmanCli;

impl PodmanCli {
    /// Run `podman <args>` and capture output.
    fn output(args: &[&str]) -> Result<std::process::Output> {
        Command::new("podman")
            .args(args)
            .output()
            .map_err(|e| FlightDeckError::Refused(format!("podman not runnable: {e}")))
    }
}

impl ContainerRuntime for PodmanCli {
    fn available(&self) -> Result<()> {
        let out = Self::output(&["info", "--format", "{{.Host.Arch}}"])?;
        if out.status.success() {
            Ok(())
        } else {
            Err(FlightDeckError::Refused(format!(
                "podman is not ready (is the machine running?): {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )))
        }
    }

    fn image_exists(&self, tag: &str) -> Result<bool> {
        // `podman image exists` exits 0 when present, non-zero otherwise.
        Ok(Self::output(&["image", "exists", tag])?.status.success())
    }

    fn image_label(&self, tag: &str, key: &str) -> Result<Option<String>> {
        let fmt = format!("{{{{ index .Config.Labels \"{key}\" }}}}");
        let out = Self::output(&["image", "inspect", tag, "--format", &fmt])?;
        if !out.status.success() {
            return Ok(None);
        }
        let val = String::from_utf8_lossy(&out.stdout).trim().to_string();
        // Go templates print "<no value>" for a missing map key.
        if val.is_empty() || val == "<no value>" {
            Ok(None)
        } else {
            Ok(Some(val))
        }
    }

    fn build_image(
        &self,
        tag: &str,
        containerfile: &Path,
        context: &Path,
        labels: &[(String, String)],
    ) -> Result<()> {
        let cf = containerfile.to_string_lossy().to_string();
        let ctx = context.to_string_lossy().to_string();
        let mut args: Vec<String> = vec!["build".into(), "-t".into(), tag.into(), "-f".into(), cf];
        for (k, v) in labels {
            args.push("--label".into());
            args.push(format!("{k}={v}"));
        }
        args.push(ctx);
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = Self::output(&arg_refs)?;
        if out.status.success() {
            Ok(())
        } else {
            Err(FlightDeckError::Other(format!(
                "podman build failed for {tag}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )))
        }
    }

    fn container_state(&self, name: &str) -> Result<ContainerState> {
        let out = Self::output(&[
            "container",
            "inspect",
            name,
            "--format",
            "{{.State.Status}}",
        ])?;
        if !out.status.success() {
            // No such container.
            return Ok(ContainerState::Absent);
        }
        let status = String::from_utf8_lossy(&out.stdout).trim().to_string();
        Ok(if status == "running" {
            ContainerState::Running
        } else {
            ContainerState::Exited
        })
    }

    fn remove_container(&self, name: &str, force: bool) -> Result<()> {
        let mut args = vec!["rm"];
        if force {
            args.push("-f");
        }
        args.push(name);
        // Ignore "no such container" — removal is idempotent from our view.
        let _ = Self::output(&args)?;
        Ok(())
    }

    fn list_by_label(&self, label: &str) -> Result<Vec<String>> {
        let filter = format!("label={label}");
        let out = Self::output(&["ps", "-a", "--filter", &filter, "--format", "{{.Names}}"])?;
        if !out.status.success() {
            return Ok(Vec::new());
        }
        Ok(String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect())
    }

    fn host_uid(&self) -> u32 {
        // Dependency-free: ask the system. Falls back to 0 (root) if `id` is
        // unavailable, which is harmless for `--user`.
        Command::new("id")
            .arg("-u")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .and_then(|s| s.trim().parse::<u32>().ok())
            .unwrap_or(0)
    }
}
