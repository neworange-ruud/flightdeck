//! Branch naming and create-vs-attach decisions (SPECS §11).

use crate::contracts::{GitExecutor, Result};

/// Generate a task slug from a free-form tab name (lowercase, hyphenated,
/// alphanumeric-only) (SPECS §11, §26 "Slug generation").
pub fn slugify(name: &str) -> String {
    let _ = name;
    todo!("T3: slug generation")
}

/// Build the full branch name `<prefix><slug>` (SPECS §11).
pub fn branch_name(prefix: &str, slug: &str) -> String {
    let _ = (prefix, slug);
    todo!("T3")
}

/// Enforce that a generated branch carries the configured prefix (SPECS §11,
/// §26 "prefix enforcement").
pub fn enforce_prefix(prefix: &str, branch: &str) -> Result<()> {
    let _ = (prefix, branch);
    todo!("T3: refuse branches that do not carry the prefix")
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
    let _ = (git, branch);
    todo!("T3: branch_exists -> AttachExisting else Create")
}
