//! Image tags, staleness hashing, Containerfile templating, and the
//! build-if-needed flow (SPECS §31 image strategy).
//!
//! The tag/hash/template helpers are pure; [`ensure_image`] drives the control
//! plane through [`ContainerRuntime`] and is unit-tested with the fake.

use crate::contracts::{ContainerRuntime, ExecutionConfig, FileSystem, FlightDeckError, Result};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

/// Image label recording the customization hash, so a changed
/// packages/setup-script/Containerfile triggers a rebuild.
pub const BUILD_LABEL: &str = "flightdeck.build";

/// The FlightDeck-owned base image for an agent.
pub fn base_image_tag(agent: &str) -> String {
    format!("localhost/flightdeck-{agent}-base:latest")
}

/// The per-project, per-agent image built from base + customization.
pub fn project_image_tag(repo_hash: &str, agent: &str) -> String {
    format!("localhost/flightdeck-{repo_hash}-{agent}:local")
}

/// The image tag a launch will use: an explicit `execution.image`, else the
/// computed per-project tag.
pub fn resolve_image_tag(repo_hash: &str, agent: &str, exec: &ExecutionConfig) -> String {
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

/// Generate a Containerfile layering declarative customization on the base.
///
/// The base image is assumed Debian-family (the FlightDeck base contract), so
/// packages install via `apt-get`. `setup_script` is the build-context-relative
/// path the script is `COPY`'d from.
pub fn generate_containerfile(
    base: &str,
    packages: &[String],
    setup_script: Option<&str>,
) -> String {
    let mut s = String::new();
    s.push_str(&format!("FROM {base}\n"));
    s.push_str("USER root\n");
    if !packages.is_empty() {
        s.push_str(&format!(
            "RUN apt-get update && apt-get install -y --no-install-recommends {} \\\n    && rm -rf /var/lib/apt/lists/*\n",
            packages.join(" ")
        ));
    }
    if let Some(script) = setup_script {
        s.push_str(&format!("COPY {script} /tmp/flightdeck-setup\n"));
        s.push_str("RUN chmod +x /tmp/flightdeck-setup && /tmp/flightdeck-setup\n");
    }
    s.push_str("USER agent\n");
    s
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
/// - An explicit `execution.image` is returned as-is (BYO; existence is checked
///   at launch, not here).
/// - Otherwise the per-project tag is (re)built from base + customization when
///   the baked `flightdeck.build` label does not match the current hash.
pub fn ensure_image(
    runtime: &dyn ContainerRuntime,
    fs: &dyn FileSystem,
    repo_root: &Path,
    repo_hash: &str,
    agent: &str,
    exec: &ExecutionConfig,
) -> Result<String> {
    if let Some(tag) = &exec.image {
        return Ok(tag.clone());
    }

    let tag = project_image_tag(repo_hash, agent);
    let base = exec
        .base_image
        .clone()
        .unwrap_or_else(|| base_image_tag(agent));

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
        let content = generate_containerfile(&base, &exec.packages, exec.setup_script.as_deref());
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
        let mut exec = ExecutionConfig::default();
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
    fn generated_containerfile_installs_packages_and_runs_setup() {
        let cf = generate_containerfile(
            "localhost/flightdeck-claude-base:latest",
            &["postgresql-client".to_string(), "jq".to_string()],
            Some(".flightdeck/setup.sh"),
        );
        assert!(cf.starts_with("FROM localhost/flightdeck-claude-base:latest\n"));
        assert!(cf.contains("apt-get install -y --no-install-recommends postgresql-client jq"));
        assert!(cf.contains("COPY .flightdeck/setup.sh /tmp/flightdeck-setup"));
        assert!(cf.trim_end().ends_with("USER agent"));
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
        let exec = ExecutionConfig {
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
        let exec = ExecutionConfig {
            enabled: true,
            packages: vec!["jq".to_string()],
            ..Default::default()
        };
        // Pre-compute the hash the same way ensure_image will.
        let base = base_image_tag("claude");
        let content = generate_containerfile(&base, &exec.packages, None);
        let hash = customization_hash(&base, &exec.packages, None, Some(&content));
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
        let exec = ExecutionConfig {
            enabled: true,
            image: Some("localhost/custom:1".to_string()),
            ..Default::default()
        };
        let tag = ensure_image(&rt, &fs, Path::new("/repo"), "h", "claude", &exec).unwrap();
        assert_eq!(tag, "localhost/custom:1");
        assert!(rt.builds.lock().unwrap().is_empty());
    }
}
