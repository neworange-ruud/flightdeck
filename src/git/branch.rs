//! Branch naming and create-vs-attach decisions (SPECS §11).

use crate::contracts::{FlightDeckError, GitExecutor, Result};

/// Generate a task slug from a free-form tab name (lowercase, hyphenated,
/// alphanumeric-only) (SPECS §11, §26 "Slug generation").
///
/// Rules: lowercase; every run of non-alphanumeric characters collapses to a
/// single hyphen; leading and trailing hyphens are trimmed.
pub fn slugify(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut prev_hyphen = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_hyphen = false;
        } else if ch.is_alphanumeric() {
            // Non-ASCII alphanumerics: lowercase but keep.
            for lc in ch.to_lowercase() {
                out.push(lc);
            }
            prev_hyphen = false;
        } else {
            // Any non-alphanumeric run → a single hyphen.
            if !prev_hyphen {
                out.push('-');
                prev_hyphen = true;
            }
        }
    }
    out.trim_matches('-').to_string()
}

/// Build the full branch name `<prefix><slug>` (SPECS §11).
pub fn branch_name(prefix: &str, slug: &str) -> String {
    format!("{prefix}{slug}")
}

/// Enforce that a generated branch carries the configured prefix (SPECS §11,
/// §26 "prefix enforcement").
pub fn enforce_prefix(prefix: &str, branch: &str) -> Result<()> {
    if branch.starts_with(prefix) {
        Ok(())
    } else {
        Err(FlightDeckError::Refused(format!(
            "branch '{branch}' does not carry required prefix '{prefix}'"
        )))
    }
}

/// Whether a generated branch should be created fresh or attached-to because it
/// already exists. FlightDeck must never silently attach (SPECS §11) — the
/// caller surfaces the attach to the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchDecision {
    /// Branch does not exist; create it from the base branch.
    Create,
    /// Branch already exists; attach (must be surfaced to the user).
    AttachExisting,
}

/// Decide whether to create or attach for `branch` (SPECS §11).
pub fn decide_branch(git: &dyn GitExecutor, branch: &str) -> Result<BranchDecision> {
    if git.branch_exists(branch)? {
        Ok(BranchDecision::AttachExisting)
    } else {
        Ok(BranchDecision::Create)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeGit;

    #[test]
    fn slugify_lowercases_and_hyphenates_spaces_and_punctuation() {
        assert_eq!(slugify("Fix the Login Bug!"), "fix-the-login-bug");
        assert_eq!(slugify("Add OAuth2 support"), "add-oauth2-support");
    }

    #[test]
    fn slugify_collapses_runs_and_trims() {
        assert_eq!(slugify("  Hello___World  "), "hello-world");
        assert_eq!(slugify("a // b -- c"), "a-b-c");
        assert_eq!(slugify("---trim---"), "trim");
        assert_eq!(slugify("UPPER.CASE"), "upper-case");
    }

    #[test]
    fn slugify_empty_and_all_punct() {
        assert_eq!(slugify(""), "");
        assert_eq!(slugify("!!!"), "");
    }

    #[test]
    fn branch_name_concatenates_prefix_and_slug() {
        assert_eq!(
            branch_name("flightdeck/", "fix-login"),
            "flightdeck/fix-login"
        );
    }

    #[test]
    fn enforce_prefix_accepts_prefixed() {
        assert!(enforce_prefix("flightdeck/", "flightdeck/fix-login").is_ok());
    }

    #[test]
    fn enforce_prefix_rejects_non_prefixed() {
        let err = enforce_prefix("flightdeck/", "fix-login").unwrap_err();
        assert!(matches!(err, FlightDeckError::Refused(_)));
    }

    #[test]
    fn decide_branch_create_when_absent() {
        let git = FakeGit::new().with_branches(["main"]);
        assert_eq!(
            decide_branch(&git, "flightdeck/new").unwrap(),
            BranchDecision::Create
        );
    }

    #[test]
    fn decide_branch_attach_when_present() {
        let git = FakeGit::new().with_branches(["main", "flightdeck/existing"]);
        assert_eq!(
            decide_branch(&git, "flightdeck/existing").unwrap(),
            BranchDecision::AttachExisting
        );
    }

    #[test]
    fn tab_rename_does_not_rename_branch() {
        // Branch name is derived from the ORIGINAL slug at creation time. A
        // later tab rename produces a different slug, but the branch must stay
        // stable: callers keep the original branch_name, not a recomputed one.
        let original_slug = slugify("Implement Login");
        let branch = branch_name("flightdeck/", &original_slug);
        assert_eq!(branch, "flightdeck/implement-login");

        // User renames the tab later.
        let renamed_slug = slugify("Login work (WIP)");
        let recomputed = branch_name("flightdeck/", &renamed_slug);
        // The recomputed name differs, demonstrating the rename would change it
        // IF used — so callers must keep `branch`, which is unchanged.
        assert_ne!(recomputed, branch);
        assert_eq!(branch, "flightdeck/implement-login");
    }
}
