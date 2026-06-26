//! Image tags, staleness hashing, Containerfile templating, and the
//! build-if-needed flow (SPECS §31 image strategy).
//!
//! The tag/hash/template helpers are pure; [`ensure_image`] drives the control
//! plane through [`ContainerRuntime`] and is unit-tested with the fake.

use crate::contracts::{ContainerRuntime, ContainersConfig, FileSystem, FlightDeckError, Result};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

/// Image label recording the customization hash, so a changed
/// packages/setup-script/Containerfile triggers a rebuild.
pub const BUILD_LABEL: &str = "flightdeck.build";

/// The default base image FlightDeck builds on when the project does not
/// override it. A **trusted, fully-qualified Docker Official Image** (Docker
/// Hub `library/node`) so `podman build` pulls from a real registry — the
/// fully-qualified name also avoids relying on a `registries.conf` default.
pub const DEFAULT_BASE_IMAGE: &str = "docker.io/library/node:22-bookworm-slim";

/// The command that installs an agent's CLI on top of [`DEFAULT_BASE_IMAGE`].
/// Returns `None` for an agent FlightDeck has no built-in recipe for — such an
/// agent must set `containers.image`, `containers.base_image`, or
/// `containers.containerfile` itself.
pub fn agent_install_command(agent: &str) -> Option<&'static str> {
    match agent {
        "claude" => Some("npm install -g @anthropic-ai/claude-code"),
        "codex" => Some("npm install -g @openai/codex"),
        "opencode" => Some("npm install -g opencode-ai"),
        _ => None,
    }
}

/// A FlightDeck-owned base image tag (for users who prefer to pre-build a base
/// and point `containers.base_image` at it; not used by the default flow).
pub fn base_image_tag(agent: &str) -> String {
    format!("localhost/flightdeck-{agent}-base:latest")
}

/// The per-project, per-agent image built from base + customization.
pub fn project_image_tag(repo_hash: &str, agent: &str) -> String {
    format!("localhost/flightdeck-{repo_hash}-{agent}:local")
}

/// The image tag a launch will use: an explicit `containers.image`, else the
/// computed per-project tag.
pub fn resolve_image_tag(repo_hash: &str, agent: &str, exec: &ContainersConfig) -> String {
    exec.image
        .clone()
        .unwrap_or_else(|| project_image_tag(repo_hash, agent))
}

/// Stable hash over everything that affects the built image.
pub fn customization_hash(
    base: &str,
    packages: &[String],
    setup_script_body: Option<&str>,
    containerfile_body: Option<&str>,
) -> String {
    let mut h = DefaultHasher::new();
    base.hash(&mut h);
    packages.hash(&mut h);
    setup_script_body.hash(&mut h);
    containerfile_body.hash(&mut h);
    format!("{:016x}", h.finish())
}

/// Generate the Containerfile to build for an agent (Debian-family base, so
/// packages install via `apt-get`).
///
/// Two modes:
/// - **`base_override = None`** (the default): a **self-contained** file built
///   from the trusted [`DEFAULT_BASE_IMAGE`] that installs git + the agent CLI
///   and creates the non-root, UID-mappable `agent` user — so a plain
///   `flightdeck image build` works with no pre-built local base. Requires a
///   built-in recipe for `agent` ([`agent_install_command`]).
/// - **`base_override = Some(base)`**: layer declarative customization onto a
///   base that is assumed to already carry the agent CLI + `agent` user.
///
/// `setup_script` is the build-context-relative path the script is `COPY`'d from.
pub fn generate_containerfile(
    agent: &str,
    base_override: Option<&str>,
    packages: &[String],
    setup_script: Option<&str>,
) -> Result<String> {
    let mut s = String::new();

    match base_override {
        Some(base) => {
            // Layer-on-base: the base already provides the agent + `agent` user.
            s.push_str(&format!("FROM {base}\n"));
            s.push_str("USER root\n");
            push_packages(&mut s, packages);
            push_setup(&mut s, setup_script);
            s.push_str("USER agent\n");
        }
        None => {
            // Self-contained from the trusted default base.
            let install = agent_install_command(agent).ok_or_else(|| {
                FlightDeckError::Config(format!(
                    "no built-in container recipe for agent '{agent}'; set \
                     containers.image, containers.base_image, or containers.containerfile \
                     in .flightdeck/config.toml"
                ))
            })?;
            s.push_str(&format!("FROM {DEFAULT_BASE_IMAGE}\n"));
            s.push_str(
                "RUN apt-get update \\\n    \
                 && apt-get install -y --no-install-recommends git ca-certificates curl ripgrep less zsh \\\n    \
                 && rm -rf /var/lib/apt/lists/*\n",
            );
            s.push_str(&format!("RUN {install}\n"));
            // Non-root user; UID-mapped at run time via `--userns keep-id --user
            // <host-uid>`. The process runs as the host UID (not `agent`'s UID),
            // so make the home tree world-writable and pre-create the XDG dirs —
            // otherwise the agent cannot create e.g. ~/.local/state (EACCES).
            s.push_str(
                "RUN useradd --create-home --shell /usr/bin/zsh agent \\\n    \
                 && mkdir -p /home/agent/.local/state /home/agent/.local/share \
                 /home/agent/.config /home/agent/.cache \\\n    \
                 && chmod -R 0777 /home/agent\n",
            );
            push_packages(&mut s, packages);
            push_setup(&mut s, setup_script);
            s.push_str("USER agent\n");
            s.push_str("WORKDIR /workspace\n");
        }
    }
    Ok(s)
}

fn push_packages(s: &mut String, packages: &[String]) {
    if !packages.is_empty() {
        s.push_str(&format!(
            "RUN apt-get update && apt-get install -y --no-install-recommends {} \\\n    && rm -rf /var/lib/apt/lists/*\n",
            packages.join(" ")
        ));
    }
}

fn push_setup(s: &mut String, setup_script: Option<&str>) {
    if let Some(script) = setup_script {
        s.push_str(&format!("COPY {script} /tmp/flightdeck-setup\n"));
        s.push_str("RUN chmod +x /tmp/flightdeck-setup && /tmp/flightdeck-setup\n");
    }
}

/// Where a generated Containerfile is written (under the repo's `.flightdeck`).
pub fn generated_containerfile_path(repo_root: &Path, agent: &str) -> PathBuf {
    repo_root
        .join(".flightdeck")
        .join("containers")
        .join(format!("{agent}.generated.Containerfile"))
}

/// Ensure the image for `agent` exists and is current, building it if needed,
/// and return the tag to run.
///
/// - An explicit `containers.image` is returned as-is (BYO; existence is checked
///   at launch, not here).
/// - Otherwise the per-project tag is (re)built from base + customization when
///   the baked `flightdeck.build` label does not match the current hash.
pub fn ensure_image(
    runtime: &dyn ContainerRuntime,
    fs: &dyn FileSystem,
    repo_root: &Path,
    repo_hash: &str,
    agent: &str,
    exec: &ContainersConfig,
) -> Result<String> {
    if let Some(tag) = &exec.image {
        return Ok(tag.clone());
    }

    let tag = project_image_tag(repo_hash, agent);
    // The base used for the staleness hash: the override, else our trusted
    // default (the generated content also encodes it).
    let base = exec
        .base_image
        .clone()
        .unwrap_or_else(|| DEFAULT_BASE_IMAGE.to_string());

    // Resolve the Containerfile to build + the staleness inputs.
    let (containerfile_path, context, hash) = if let Some(rel) = &exec.containerfile {
        let path = repo_root.join(rel);
        let body = fs.read_to_string(&path)?;
        let hash = customization_hash(&base, &[], None, Some(&body));
        (path, repo_root.to_path_buf(), hash)
    } else {
        let setup_body = match &exec.setup_script {
            Some(rel) => Some(fs.read_to_string(&repo_root.join(rel))?),
            None => None,
        };
        let content = generate_containerfile(
            agent,
            exec.base_image.as_deref(),
            &exec.packages,
            exec.setup_script.as_deref(),
        )?;
        let path = generated_containerfile_path(repo_root, agent);
        if let Some(parent) = path.parent() {
            fs.create_dir_all(parent)?;
        }
        fs.write(&path, &content)?;
        let hash = customization_hash(&base, &exec.packages, setup_body.as_deref(), Some(&content));
        (path, repo_root.to_path_buf(), hash)
    };

    // Up to date? (image present AND label matches.)
    if runtime.image_exists(&tag)? && runtime.image_label(&tag, BUILD_LABEL)? == Some(hash.clone())
    {
        return Ok(tag);
    }

    runtime.build_image(
        &tag,
        &containerfile_path,
        &context,
        &[(BUILD_LABEL.to_string(), hash)],
    )?;
    Ok(tag)
}

/// A clear error explaining a containerized launch cannot proceed because the
/// image is missing (used on the fast launch path, which does not build).
pub fn missing_image_error(tag: &str) -> FlightDeckError {
    FlightDeckError::Refused(format!(
        "container image '{tag}' not found — build it first with `flightdeck image build`"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::ContainerState;
    use crate::testing::FakeFs;
    use std::sync::Mutex;

    #[test]
    fn tags_have_expected_shape() {
        assert_eq!(
            base_image_tag("claude"),
            "localhost/flightdeck-claude-base:latest"
        );
        assert_eq!(
            project_image_tag("deadbeef", "claude"),
            "localhost/flightdeck-deadbeef-claude:local"
        );
    }

    #[test]
    fn resolve_prefers_explicit_image() {
        let mut exec = ContainersConfig::default();
        assert_eq!(
            resolve_image_tag("h", "claude", &exec),
            "localhost/flightdeck-h-claude:local"
        );
        exec.image = Some("localhost/custom:1".to_string());
        assert_eq!(
            resolve_image_tag("h", "claude", &exec),
            "localhost/custom:1"
        );
    }

    #[test]
    fn hash_changes_with_packages() {
        let a = customization_hash("base", &["jq".to_string()], None, None);
        let b = customization_hash("base", &["jq".to_string(), "curl".to_string()], None, None);
        assert_ne!(a, b);
        let c = customization_hash("base", &["jq".to_string()], None, None);
        assert_eq!(a, c, "same inputs → same hash");
    }

    #[test]
    fn default_containerfile_is_self_contained_from_trusted_base() {
        // No base override → build from the trusted public base, install the
        // agent CLI, and create the non-root user — no pre-built local base.
        let cf = generate_containerfile(
            "claude",
            None,
            &["postgresql-client".to_string(), "jq".to_string()],
            Some(".flightdeck/setup.sh"),
        )
        .unwrap();
        assert!(cf.starts_with("FROM docker.io/library/node:22-bookworm-slim\n"));
        assert!(cf.contains("npm install -g @anthropic-ai/claude-code"));
        assert!(cf.contains("useradd --create-home --shell /usr/bin/zsh agent"));
        // Home is made writable by the UID-mapped (host) user.
        assert!(cf.contains("chmod -R 0777 /home/agent"));
        assert!(cf.contains("apt-get install -y --no-install-recommends postgresql-client jq"));
        assert!(cf.contains("COPY .flightdeck/setup.sh /tmp/flightdeck-setup"));
        assert!(cf.contains("USER agent"));
    }

    #[test]
    fn base_override_layers_without_installing_agent() {
        let cf = generate_containerfile(
            "claude",
            Some("localhost/my-base:1"),
            &["jq".to_string()],
            None,
        )
        .unwrap();
        assert!(cf.starts_with("FROM localhost/my-base:1\n"));
        // The override is assumed to already carry the agent CLI.
        assert!(!cf.contains("npm install"));
        assert!(cf.contains("apt-get install -y --no-install-recommends jq"));
        assert!(cf.trim_end().ends_with("USER agent"));
    }

    #[test]
    fn unknown_agent_without_base_is_an_error() {
        let err = generate_containerfile("mystery", None, &[], None).unwrap_err();
        assert!(err.to_string().contains("no built-in container recipe"));
    }

    // A FakeContainerRuntime focused on image calls (the full fake lives in
    // src/testing; this local one keeps the image test self-contained).
    #[derive(Default)]
    struct ImgFake {
        exists: bool,
        label: Option<String>,
        builds: Mutex<Vec<(String, String)>>, // (tag, build-hash)
    }
    impl ContainerRuntime for ImgFake {
        fn available(&self) -> Result<()> {
            Ok(())
        }
        fn image_exists(&self, _t: &str) -> Result<bool> {
            Ok(self.exists)
        }
        fn image_label(&self, _t: &str, _k: &str) -> Result<Option<String>> {
            Ok(self.label.clone())
        }
        fn build_image(
            &self,
            tag: &str,
            _cf: &Path,
            _ctx: &Path,
            labels: &[(String, String)],
        ) -> Result<()> {
            let h = labels
                .iter()
                .find(|(k, _)| k == BUILD_LABEL)
                .map(|(_, v)| v.clone())
                .unwrap_or_default();
            self.builds.lock().unwrap().push((tag.to_string(), h));
            Ok(())
        }
        fn start_detached(&self, _run_args: &[String]) -> Result<()> {
            Ok(())
        }
        fn container_state(&self, _n: &str) -> Result<ContainerState> {
            Ok(ContainerState::Absent)
        }
        fn remove_container(&self, _n: &str, _f: bool) -> Result<()> {
            Ok(())
        }
        fn list_by_label(&self, _l: &str) -> Result<Vec<String>> {
            Ok(vec![])
        }
        fn host_uid(&self) -> u32 {
            501
        }
    }

    #[test]
    fn ensure_image_builds_when_absent() {
        let rt = ImgFake::default(); // exists=false
        let fs = FakeFs::new();
        let exec = ContainersConfig {
            enabled: true,
            packages: vec!["jq".to_string()],
            ..Default::default()
        };
        let tag = ensure_image(&rt, &fs, Path::new("/repo"), "h", "claude", &exec).unwrap();
        assert_eq!(tag, "localhost/flightdeck-h-claude:local");
        assert_eq!(rt.builds.lock().unwrap().len(), 1, "must build when absent");
        // The generated Containerfile was written.
        assert!(fs
            .file_contents(&generated_containerfile_path(Path::new("/repo"), "claude"))
            .is_some());
    }

    #[test]
    fn ensure_image_skips_build_when_label_matches() {
        let fs = FakeFs::new();
        let exec = ContainersConfig {
            enabled: true,
            packages: vec!["jq".to_string()],
            ..Default::default()
        };
        // Pre-compute the hash the same way ensure_image will.
        let content = generate_containerfile("claude", None, &exec.packages, None).unwrap();
        let hash = customization_hash(DEFAULT_BASE_IMAGE, &exec.packages, None, Some(&content));
        let rt = ImgFake {
            exists: true,
            label: Some(hash),
            ..Default::default()
        };
        let tag = ensure_image(&rt, &fs, Path::new("/repo"), "h", "claude", &exec).unwrap();
        assert_eq!(tag, "localhost/flightdeck-h-claude:local");
        assert!(
            rt.builds.lock().unwrap().is_empty(),
            "current image → no rebuild"
        );
    }

    #[test]
    fn ensure_image_returns_explicit_image_without_building() {
        let rt = ImgFake::default();
        let fs = FakeFs::new();
        let exec = ContainersConfig {
            enabled: true,
            image: Some("localhost/custom:1".to_string()),
            ..Default::default()
        };
        let tag = ensure_image(&rt, &fs, Path::new("/repo"), "h", "claude", &exec).unwrap();
        assert_eq!(tag, "localhost/custom:1");
        assert!(rt.builds.lock().unwrap().is_empty());
    }
}
