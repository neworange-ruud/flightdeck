//! Deterministic container naming + labels (SPECS §31).
//!
//! The container name is derived purely from the persisted `TabState.id`, so
//! child-shell `exec`, reattach, and teardown all reconstruct it without
//! capturing any runtime id at spawn.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;

/// Label key carrying the owning tab id.
pub const LABEL_TAB: &str = "flightdeck.tab";
/// Label key carrying the owning repo hash (for cross-tab discovery/cleanup).
pub const LABEL_REPO: &str = "flightdeck.repo";

/// Sanitize an arbitrary id into a valid Podman container-name component.
///
/// Podman requires names to match `[a-zA-Z0-9][a-zA-Z0-9_.-]*`; tab ids contain
/// characters like `:` (from ISO timestamps), so anything outside the allowed
/// set becomes `-`, and a leading non-alphanumeric is prefixed with `x`.
pub fn sanitize(s: &str) -> String {
    let mapped: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    match mapped.chars().next() {
        Some(c) if c.is_ascii_alphanumeric() => mapped,
        _ => format!("x{mapped}"),
    }
}

/// The container name for a tab: `flightdeck-<sanitized id>`.
pub fn container_name(tab_id: &str) -> String {
    format!("flightdeck-{}", sanitize(tab_id))
}

/// A short, stable hash of the repository root, used in image tags and the
/// `flightdeck.repo` label. `DefaultHasher` uses fixed keys, so this is
/// deterministic across runs.
pub fn repo_hash(repo_root: &Path) -> String {
    let mut h = DefaultHasher::new();
    repo_root.hash(&mut h);
    format!("{:016x}", h.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn container_name_is_prefixed() {
        assert_eq!(container_name("feature-x"), "flightdeck-feature-x");
    }

    #[test]
    fn sanitizes_colons_from_timestamped_ids() {
        // ids look like "slug-2026-01-01T00:00:00Z".
        let name = container_name("add-auth-2026-01-01T00:00:00Z");
        assert_eq!(name, "flightdeck-add-auth-2026-01-01T00-00-00Z");
        // No character outside the Podman-allowed set remains.
        assert!(name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-')));
    }

    #[test]
    fn leading_non_alnum_is_prefixed() {
        assert_eq!(sanitize("-weird"), "x-weird");
        assert_eq!(sanitize(":x"), "x-x");
    }

    #[test]
    fn repo_hash_is_stable_and_path_specific() {
        let a = repo_hash(Path::new("/repo/one"));
        let b = repo_hash(Path::new("/repo/one"));
        let c = repo_hash(Path::new("/repo/two"));
        assert_eq!(a, b, "same path → same hash");
        assert_ne!(a, c, "different path → different hash");
        assert_eq!(a.len(), 16);
    }
}
