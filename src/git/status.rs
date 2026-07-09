//! Status collection, base-drift calculation, local merge-back precondition
//! checks, and the guarded worktree rebase (SPECS §5, §12, §13, §15, §21).

use crate::contracts::{GitExecutor, MergeOutcome, RebaseOutcome, Result};
use std::path::{Path, PathBuf};

/// Number of commits the base branch has moved since the stored base SHA
/// (SPECS §12 "Base moved: N commits ahead since tab creation").
pub fn base_drift(git: &dyn GitExecutor, base_branch: &str, base_commit_sha: &str) -> Result<u32> {
    // How far `base_branch` is ahead of the stored SHA = the `ahead` component
    // of ahead_behind(base_commit_sha, base_branch).
    Ok(git.ahead_behind(base_commit_sha, base_branch)?.0)
}

/// A count of working-tree changes by category, parsed from
/// `git status --porcelain` (SPECS §21). Each changed path is counted once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WorktreeChanges {
    /// New files: untracked (`??`) plus staged additions (`A`).
    pub added: u32,
    /// Modified, renamed, copied, or type-changed tracked files.
    pub modified: u32,
    /// Deleted tracked files.
    pub deleted: u32,
}

impl WorktreeChanges {
    /// Total number of changed paths.
    pub fn total(&self) -> u32 {
        self.added + self.modified + self.deleted
    }

    /// Whether the worktree is clean (no changes of any kind).
    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }
}

/// Classify the lines of `git status --porcelain` (v1) into [`WorktreeChanges`].
///
/// Each line is `XY <path>` where `X` is the staged status and `Y` the
/// worktree status. A path is counted once, by its most significant status:
/// deletion > addition > modification (rename/copy/type-change count as
/// modifications). Untracked `??` entries count as additions.
pub fn parse_porcelain_changes(lines: &[String]) -> WorktreeChanges {
    let mut changes = WorktreeChanges::default();
    for line in lines {
        let line = line.trim_end_matches(['\n', '\r']);
        let bytes = line.as_bytes();
        if bytes.len() < 2 {
            continue;
        }
        let x = bytes[0] as char;
        let y = bytes[1] as char;
        if x == '?' && y == '?' {
            changes.added += 1;
        } else if x == 'D' || y == 'D' {
            changes.deleted += 1;
        } else if x == 'A' {
            changes.added += 1;
        } else {
            // M, R, C, T, and worktree-only modifications.
            changes.modified += 1;
        }
    }
    changes
}

/// Lightweight git status for an Agent Tab's worktree (SPECS §21).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeStatus {
    pub branch: String,
    pub base_branch: String,
    pub dirty: bool,
    pub changes: WorktreeChanges,
    pub ahead: u32,
    pub behind: u32,
    pub upstream: Option<String>,
    pub base_drift: u32,
    pub worktree_path: PathBuf,
}

/// Collect the status panel data for an Agent Tab (SPECS §21).
pub fn collect_status(
    git: &dyn GitExecutor,
    branch: &str,
    base_branch: &str,
    base_commit_sha: &str,
    worktree_path: &Path,
) -> Result<WorktreeStatus> {
    // One porcelain call yields both the dirty flag and the per-category counts.
    let porcelain = git.status_porcelain(worktree_path)?;
    let changes = parse_porcelain_changes(&porcelain);
    let dirty = !changes.is_empty();
    let upstream = git.upstream_of(branch)?;
    // Ahead/behind vs the upstream, only meaningful when an upstream is known.
    let (ahead, behind) = match &upstream {
        Some(up) => git.ahead_behind(up, branch)?,
        None => (0, 0),
    };
    let drift = base_drift(git, base_branch, base_commit_sha)?;
    Ok(WorktreeStatus {
        branch: branch.to_string(),
        base_branch: base_branch.to_string(),
        dirty,
        changes,
        ahead,
        behind,
        upstream,
        base_drift: drift,
        worktree_path: worktree_path.to_path_buf(),
    })
}

/// Whether a guarded local merge-back is allowed, or why it is refused
/// (SPECS §13, §15).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeDecision {
    Allowed,
    Refused(String),
}

/// Inputs to a local merge-back precondition check (SPECS §15).
#[derive(Debug, Clone)]
pub struct MergeRequest<'a> {
    pub base_branch: &'a str,
    pub agent_branch: &'a str,
    pub base_worktree: &'a Path,
    pub agent_worktree: &'a Path,
}

/// The §13 message shown when the base worktree is dirty and local merge is
/// therefore disabled.
const BASE_DIRTY_MESSAGE: &str = "Base worktree has uncommitted changes. Local merge is disabled.\nRecommended action: push this branch and create a PR instead.";

/// Check the technical local merge-back preconditions (SPECS §15): both
/// worktrees clean, the base worktree actually has `base_branch` checked out,
/// and both branches exist. Refuses with a clear reason otherwise; base-dirty
/// disables merge entirely (SPECS §13). A running primary agent does NOT block
/// the merge — "Finish" stops the agent and removes the worktree after
/// merging. User confirmation is handled at the command layer.
pub fn check_merge_preconditions(
    git: &dyn GitExecutor,
    req: &MergeRequest<'_>,
) -> Result<MergeDecision> {
    // Base worktree clean (SPECS §13: base dirty disables merge entirely).
    if git.is_dirty(req.base_worktree)? {
        return Ok(MergeDecision::Refused(BASE_DIRTY_MESSAGE.to_string()));
    }
    // Agent worktree clean.
    if git.is_dirty(req.agent_worktree)? {
        return Ok(MergeDecision::Refused(format!(
            "Agent worktree '{}' has uncommitted changes; commit or discard them before merging.",
            req.agent_worktree.display()
        )));
    }
    // Base worktree must actually have base_branch checked out — merging
    // merges into whatever is currently HEAD there, not the named branch.
    let current = git.current_branch(req.base_worktree)?;
    if current != req.base_branch {
        return Ok(MergeDecision::Refused(format!(
            "Base worktree is on '{}', not the base branch '{}'.",
            current, req.base_branch
        )));
    }
    // Base branch exists.
    if !git.branch_exists(req.base_branch)? {
        return Ok(MergeDecision::Refused(format!(
            "Base branch '{}' does not exist.",
            req.base_branch
        )));
    }
    // Agent branch exists.
    if !git.branch_exists(req.agent_branch)? {
        return Ok(MergeDecision::Refused(format!(
            "Agent branch '{}' does not exist.",
            req.agent_branch
        )));
    }
    Ok(MergeDecision::Allowed)
}

/// Perform a guarded local merge-back: re-checks preconditions, then merges
/// `--no-ff`. Stops and explains on conflict (no auto conflict resolution,
/// SPECS §15).
pub fn merge_back(git: &dyn GitExecutor, req: &MergeRequest<'_>) -> Result<MergeOutcome> {
    // Re-check immediately before mutating, never trust an earlier check.
    match check_merge_preconditions(git, req)? {
        MergeDecision::Refused(reason) => {
            return Ok(MergeOutcome {
                merged: false,
                conflicted: false,
                message: reason,
            });
        }
        MergeDecision::Allowed => {}
    }
    // Merge the agent branch into the base worktree (which has base_branch
    // checked out).
    let outcome = git.merge_no_ff(req.agent_branch, req.base_worktree)?;
    if outcome.conflicted {
        // Never auto-resolve: surface that manual git intervention is required.
        return Ok(MergeOutcome {
            merged: false,
            conflicted: true,
            message: format!(
                "Merge of '{}' hit conflicts. Manual git intervention is required; FlightDeck will not resolve conflicts automatically.\n{}",
                req.agent_branch, outcome.message
            ),
        });
    }
    Ok(outcome)
}

/// Whether a guarded worktree rebase is allowed, or why it is refused
/// (SPECS §5 carve-out).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RebaseDecision {
    Allowed,
    Refused(String),
}

/// Inputs to a worktree-rebase precondition check (SPECS §5 carve-out).
#[derive(Debug, Clone)]
pub struct RebaseRequest<'a> {
    pub base_branch: &'a str,
    pub agent_branch: &'a str,
    pub agent_worktree: &'a Path,
}

/// Check the technical preconditions for rebasing the agent worktree onto its
/// base branch (SPECS §5 carve-out): the agent worktree must be clean (a rebase
/// refuses on uncommitted changes anyway, and we never stash or discard), the
/// agent worktree must actually have `agent_branch` checked out, and both
/// branches must exist. Unlike merge-back this does not touch the base
/// worktree, so the base's dirty state is irrelevant. User confirmation is
/// handled at the command layer.
pub fn check_rebase_preconditions(
    git: &dyn GitExecutor,
    req: &RebaseRequest<'_>,
) -> Result<RebaseDecision> {
    if git.is_dirty(req.agent_worktree)? {
        return Ok(RebaseDecision::Refused(format!(
            "Agent worktree '{}' has uncommitted changes; commit or discard them before rebasing.",
            req.agent_worktree.display()
        )));
    }
    // Agent worktree must actually have agent_branch checked out — a rebase
    // rewrites whatever is currently HEAD there, not the named branch.
    let current = git.current_branch(req.agent_worktree)?;
    if current != req.agent_branch {
        return Ok(RebaseDecision::Refused(format!(
            "Agent worktree '{}' is on '{}', not the agent branch '{}'.",
            req.agent_worktree.display(),
            current,
            req.agent_branch
        )));
    }
    if !git.branch_exists(req.base_branch)? {
        return Ok(RebaseDecision::Refused(format!(
            "Base branch '{}' does not exist.",
            req.base_branch
        )));
    }
    if !git.branch_exists(req.agent_branch)? {
        return Ok(RebaseDecision::Refused(format!(
            "Agent branch '{}' does not exist.",
            req.agent_branch
        )));
    }
    Ok(RebaseDecision::Allowed)
}

/// Perform a guarded rebase of the agent worktree onto its base branch:
/// re-checks preconditions, then rebases. On conflict the rebase is aborted by
/// the executor and reported — never auto-resolved, never left half-finished
/// (SPECS §5 carve-out / §15 conflict policy).
pub fn rebase_onto_base(git: &dyn GitExecutor, req: &RebaseRequest<'_>) -> Result<RebaseOutcome> {
    // Re-check immediately before mutating; never trust an earlier check.
    match check_rebase_preconditions(git, req)? {
        RebaseDecision::Refused(reason) => {
            return Ok(RebaseOutcome {
                rebased: false,
                conflicted: false,
                message: reason,
            });
        }
        RebaseDecision::Allowed => {}
    }
    let outcome = git.rebase_onto(req.base_branch, req.agent_worktree)?;
    if outcome.conflicted {
        return Ok(RebaseOutcome {
            rebased: false,
            conflicted: true,
            message: format!(
                "Rebase of '{}' onto '{}' hit conflicts and was aborted; the worktree is unchanged. Resolve manually — FlightDeck will not rebase through conflicts.\n{}",
                req.agent_branch, req.base_branch, outcome.message
            ),
        });
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::{MergeOutcome, RebaseOutcome};
    use crate::testing::FakeGit;

    fn req<'a>(
        base_branch: &'a str,
        agent_branch: &'a str,
        base_wt: &'a Path,
        agent_wt: &'a Path,
    ) -> MergeRequest<'a> {
        MergeRequest {
            base_branch,
            agent_branch,
            base_worktree: base_wt,
            agent_worktree: agent_wt,
        }
    }

    fn lines(raw: &[&str]) -> Vec<String> {
        raw.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parse_changes_classifies_each_category() {
        // Untracked + staged-add → added; modified/renamed → modified; del → deleted.
        let porcelain = lines(&[
            "?? new_untracked.rs",
            "A  staged_new.rs",
            " M worktree_modified.rs",
            "M  staged_modified.rs",
            "R  old.rs -> renamed.rs",
            " D deleted.rs",
            "D  staged_deleted.rs",
        ]);
        let ch = parse_porcelain_changes(&porcelain);
        assert_eq!(ch.added, 2, "untracked + staged add");
        assert_eq!(ch.modified, 3, "worktree-mod + staged-mod + rename");
        assert_eq!(ch.deleted, 2, "worktree-del + staged-del");
        assert_eq!(ch.total(), 7);
        assert!(!ch.is_empty());
    }

    #[test]
    fn parse_changes_empty_is_clean() {
        let ch = parse_porcelain_changes(&[]);
        assert!(ch.is_empty());
        assert_eq!(ch.total(), 0);
    }

    #[test]
    fn collect_status_reports_change_counts() {
        let git = FakeGit::new().with_branches(["main", "flightdeck/feat"]);
        let wt = Path::new("/repo/.flightdeck/worktrees/feat");
        git.set_porcelain_at(wt, ["?? a.rs", " M b.rs", "M  c.rs", " D d.rs"]);
        let status = collect_status(&git, "flightdeck/feat", "main", "sha-base", wt).unwrap();
        assert!(status.dirty);
        assert_eq!(status.changes.added, 1);
        assert_eq!(status.changes.modified, 2);
        assert_eq!(status.changes.deleted, 1);
        assert_eq!(status.changes.total(), 4);
    }

    #[test]
    fn collect_status_clean_worktree_has_no_changes() {
        let git = FakeGit::new().with_branches(["main", "flightdeck/feat"]);
        let wt = Path::new("/repo/.flightdeck/worktrees/feat");
        let status = collect_status(&git, "flightdeck/feat", "main", "sha-base", wt).unwrap();
        assert!(!status.dirty);
        assert!(status.changes.is_empty());
    }

    #[test]
    fn base_drift_uses_ahead_count() {
        let git = FakeGit::new();
        git.set_ahead_behind("sha-old", "main", 12, 0);
        assert_eq!(base_drift(&git, "main", "sha-old").unwrap(), 12);
    }

    #[test]
    fn base_drift_zero_when_unchanged() {
        let git = FakeGit::new();
        assert_eq!(base_drift(&git, "main", "sha-current").unwrap(), 0);
    }

    #[test]
    fn collect_status_reports_dirty_ahead_behind_drift() {
        let git = FakeGit::new().with_branches(["main", "flightdeck/feat"]);
        let wt = Path::new("/repo/.flightdeck/worktrees/feat");
        git.set_dirty_at(wt, true);
        git.set_upstream(
            "flightdeck/feat",
            Some("origin/flightdeck/feat".to_string()),
        );
        git.set_ahead_behind("origin/flightdeck/feat", "flightdeck/feat", 3, 1);
        git.set_ahead_behind("sha-base", "main", 5, 0);
        let status = collect_status(&git, "flightdeck/feat", "main", "sha-base", wt).unwrap();
        assert!(status.dirty);
        assert_eq!(status.ahead, 3);
        assert_eq!(status.behind, 1);
        assert_eq!(status.upstream.as_deref(), Some("origin/flightdeck/feat"));
        assert_eq!(status.base_drift, 5);
    }

    #[test]
    fn collect_status_no_upstream_yields_zero_ahead_behind() {
        let git = FakeGit::new();
        let wt = Path::new("/repo/.flightdeck/worktrees/feat");
        let status = collect_status(&git, "flightdeck/feat", "main", "sha-base", wt).unwrap();
        assert_eq!(status.upstream, None);
        assert_eq!(status.ahead, 0);
        assert_eq!(status.behind, 0);
    }

    #[test]
    fn merge_refused_when_base_dirty() {
        let git = FakeGit::new().with_branches(["main", "flightdeck/feat"]);
        let base_wt = Path::new("/repo");
        let agent_wt = Path::new("/repo/.flightdeck/worktrees/feat");
        git.set_dirty_at(base_wt, true);
        let decision =
            check_merge_preconditions(&git, &req("main", "flightdeck/feat", base_wt, agent_wt))
                .unwrap();
        match decision {
            MergeDecision::Refused(msg) => {
                assert!(msg.contains("Local merge is disabled"));
            }
            MergeDecision::Allowed => panic!("expected refusal"),
        }
    }

    #[test]
    fn merge_refused_when_agent_dirty() {
        let git = FakeGit::new().with_branches(["main", "flightdeck/feat"]);
        let base_wt = Path::new("/repo");
        let agent_wt = Path::new("/repo/.flightdeck/worktrees/feat");
        git.set_dirty_at(agent_wt, true);
        let decision =
            check_merge_preconditions(&git, &req("main", "flightdeck/feat", base_wt, agent_wt))
                .unwrap();
        assert!(matches!(decision, MergeDecision::Refused(_)));
    }

    #[test]
    fn merge_allowed_when_primary_running() {
        // A running primary agent no longer blocks the technical preconditions;
        // "Finish" stops the agent and removes the worktree after merging.
        let git = FakeGit::new().with_branches(["main", "flightdeck/feat"]);
        let base_wt = Path::new("/repo");
        let agent_wt = Path::new("/repo/.flightdeck/worktrees/feat");
        let decision =
            check_merge_preconditions(&git, &req("main", "flightdeck/feat", base_wt, agent_wt))
                .unwrap();
        assert_eq!(decision, MergeDecision::Allowed);
    }

    #[test]
    fn merge_refused_when_base_worktree_on_wrong_branch() {
        // Both worktrees are clean and both branches exist, but the base
        // worktree has some other branch checked out — merging into it
        // would silently land on the wrong branch.
        let git = FakeGit::new()
            .with_branches(["main", "flightdeck/feat"])
            .with_current_branch("some-other-branch");
        let base_wt = Path::new("/repo");
        let agent_wt = Path::new("/repo/.flightdeck/worktrees/feat");
        let decision =
            check_merge_preconditions(&git, &req("main", "flightdeck/feat", base_wt, agent_wt))
                .unwrap();
        match decision {
            MergeDecision::Refused(msg) => {
                assert!(msg.contains("some-other-branch"));
                assert!(msg.contains("main"));
            }
            MergeDecision::Allowed => panic!("expected refusal"),
        }
    }

    #[test]
    fn merge_refused_when_branch_missing() {
        let git = FakeGit::new().with_branches(["main"]);
        let base_wt = Path::new("/repo");
        let agent_wt = Path::new("/repo/.flightdeck/worktrees/feat");
        let decision =
            check_merge_preconditions(&git, &req("main", "flightdeck/feat", base_wt, agent_wt))
                .unwrap();
        assert!(matches!(decision, MergeDecision::Refused(_)));
    }

    #[test]
    fn merge_allowed_when_all_preconditions_pass() {
        let git = FakeGit::new().with_branches(["main", "flightdeck/feat"]);
        let base_wt = Path::new("/repo");
        let agent_wt = Path::new("/repo/.flightdeck/worktrees/feat");
        let decision =
            check_merge_preconditions(&git, &req("main", "flightdeck/feat", base_wt, agent_wt))
                .unwrap();
        assert_eq!(decision, MergeDecision::Allowed);
    }

    #[test]
    fn merge_back_performs_merge_on_allowed() {
        let git = FakeGit::new().with_branches(["main", "flightdeck/feat"]);
        let base_wt = Path::new("/repo");
        let agent_wt = Path::new("/repo/.flightdeck/worktrees/feat");
        let outcome = merge_back(&git, &req("main", "flightdeck/feat", base_wt, agent_wt)).unwrap();
        assert!(outcome.merged);
        assert!(!outcome.conflicted);
        assert_eq!(
            git.merges(),
            vec![("flightdeck/feat".to_string(), base_wt.to_path_buf())]
        );
    }

    #[test]
    fn merge_back_does_not_merge_when_refused() {
        let git = FakeGit::new().with_branches(["main", "flightdeck/feat"]);
        let base_wt = Path::new("/repo");
        let agent_wt = Path::new("/repo/.flightdeck/worktrees/feat");
        git.set_dirty_at(base_wt, true);
        let outcome = merge_back(&git, &req("main", "flightdeck/feat", base_wt, agent_wt)).unwrap();
        assert!(!outcome.merged);
        assert!(git.merges().is_empty());
    }

    #[test]
    fn merge_back_surfaces_conflicts_without_resolving() {
        let git = FakeGit::new().with_branches(["main", "flightdeck/feat"]);
        let base_wt = Path::new("/repo");
        let agent_wt = Path::new("/repo/.flightdeck/worktrees/feat");
        git.set_merge_outcome(MergeOutcome {
            merged: false,
            conflicted: true,
            message: "CONFLICT in file.rs".to_string(),
        });
        let outcome = merge_back(&git, &req("main", "flightdeck/feat", base_wt, agent_wt)).unwrap();
        assert!(!outcome.merged);
        assert!(outcome.conflicted);
        assert!(outcome
            .message
            .contains("Manual git intervention is required"));
    }

    // --- rebase preconditions / workflow (SPECS §5 carve-out) -------------

    fn rebase_req<'a>(
        base_branch: &'a str,
        agent_branch: &'a str,
        agent_wt: &'a Path,
    ) -> RebaseRequest<'a> {
        RebaseRequest {
            base_branch,
            agent_branch,
            agent_worktree: agent_wt,
        }
    }

    #[test]
    fn rebase_allowed_when_clean_and_branches_exist() {
        let git = FakeGit::new()
            .with_branches(["main", "flightdeck/feat"])
            .with_current_branch("flightdeck/feat");
        let agent_wt = Path::new("/repo/.flightdeck/worktrees/feat");
        let decision =
            check_rebase_preconditions(&git, &rebase_req("main", "flightdeck/feat", agent_wt))
                .unwrap();
        assert_eq!(decision, RebaseDecision::Allowed);
    }

    #[test]
    fn rebase_refused_when_agent_worktree_on_wrong_branch() {
        // The agent worktree is clean and both branches exist, but the
        // checked-out branch there is not agent_branch — rebasing would
        // silently rewrite whatever is actually checked out.
        let git = FakeGit::new()
            .with_branches(["main", "flightdeck/feat"])
            .with_current_branch("some-other-branch");
        let agent_wt = Path::new("/repo/.flightdeck/worktrees/feat");
        let decision =
            check_rebase_preconditions(&git, &rebase_req("main", "flightdeck/feat", agent_wt))
                .unwrap();
        match decision {
            RebaseDecision::Refused(msg) => {
                assert!(msg.contains("some-other-branch"));
                assert!(msg.contains("flightdeck/feat"));
            }
            RebaseDecision::Allowed => panic!("expected refusal"),
        }
    }

    #[test]
    fn rebase_refused_when_agent_dirty() {
        // A dirty agent worktree blocks the rebase; we never stash or discard.
        let git = FakeGit::new().with_branches(["main", "flightdeck/feat"]);
        let agent_wt = Path::new("/repo/.flightdeck/worktrees/feat");
        git.set_dirty_at(agent_wt, true);
        let decision =
            check_rebase_preconditions(&git, &rebase_req("main", "flightdeck/feat", agent_wt))
                .unwrap();
        assert!(matches!(decision, RebaseDecision::Refused(_)));
    }

    #[test]
    fn rebase_refused_when_base_branch_missing() {
        let git = FakeGit::new()
            .with_branches(["flightdeck/feat"])
            .with_current_branch("flightdeck/feat");
        let agent_wt = Path::new("/repo/.flightdeck/worktrees/feat");
        let decision =
            check_rebase_preconditions(&git, &rebase_req("main", "flightdeck/feat", agent_wt))
                .unwrap();
        assert!(matches!(decision, RebaseDecision::Refused(_)));
    }

    #[test]
    fn rebase_onto_base_rebases_on_allowed() {
        let git = FakeGit::new()
            .with_branches(["main", "flightdeck/feat"])
            .with_current_branch("flightdeck/feat");
        let agent_wt = Path::new("/repo/.flightdeck/worktrees/feat");
        let outcome =
            rebase_onto_base(&git, &rebase_req("main", "flightdeck/feat", agent_wt)).unwrap();
        assert!(outcome.rebased);
        assert!(!outcome.conflicted);
        // Rebased onto the base branch, in the agent worktree.
        assert_eq!(
            git.rebases(),
            vec![("main".to_string(), agent_wt.to_path_buf())]
        );
    }

    #[test]
    fn rebase_onto_base_does_not_rebase_when_refused() {
        let git = FakeGit::new().with_branches(["main", "flightdeck/feat"]);
        let agent_wt = Path::new("/repo/.flightdeck/worktrees/feat");
        git.set_dirty_at(agent_wt, true);
        let outcome =
            rebase_onto_base(&git, &rebase_req("main", "flightdeck/feat", agent_wt)).unwrap();
        assert!(!outcome.rebased);
        assert!(git.rebases().is_empty(), "must not touch git when refused");
    }

    #[test]
    fn rebase_onto_base_surfaces_aborted_conflict() {
        let git = FakeGit::new()
            .with_branches(["main", "flightdeck/feat"])
            .with_current_branch("flightdeck/feat");
        let agent_wt = Path::new("/repo/.flightdeck/worktrees/feat");
        git.set_rebase_outcome(RebaseOutcome {
            rebased: false,
            conflicted: true,
            message: "CONFLICT in file.rs".to_string(),
        });
        let outcome =
            rebase_onto_base(&git, &rebase_req("main", "flightdeck/feat", agent_wt)).unwrap();
        assert!(!outcome.rebased);
        assert!(outcome.conflicted);
        assert!(outcome.message.contains("aborted"));
        assert!(outcome
            .message
            .contains("will not rebase through conflicts"));
    }
}
