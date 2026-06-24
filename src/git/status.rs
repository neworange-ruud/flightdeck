//! Status collection, base-drift calculation, and local merge-back precondition
//! checks (SPECS §12, §13, §15, §21).

use crate::contracts::{GitExecutor, MergeOutcome, Result};
use std::path::{Path, PathBuf};

/// Number of commits the base branch has moved since the stored base SHA
/// (SPECS §12 "Base moved: N commits ahead since tab creation").
pub fn base_drift(git: &dyn GitExecutor, base_branch: &str, base_commit_sha: &str) -> Result<u32> {
    // How far `base_branch` is ahead of the stored SHA = the `ahead` component
    // of ahead_behind(base_commit_sha, base_branch).
    Ok(git.ahead_behind(base_commit_sha, base_branch)?.0)
}

/// Lightweight git status for an Agent Tab's worktree (SPECS §21).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeStatus {
    pub branch: String,
    pub base_branch: String,
    pub dirty: bool,
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
    let dirty = git.is_dirty(worktree_path)?;
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
    pub primary_running: bool,
    pub user_confirmed: bool,
}

/// The §13 message shown when the base worktree is dirty and local merge is
/// therefore disabled.
const BASE_DIRTY_MESSAGE: &str = "Base worktree has uncommitted changes. Local merge is disabled.\nRecommended action: push this branch and create a PR instead.";

/// Check all local merge-back preconditions (SPECS §15): both worktrees clean,
/// both branches exist, no running primary agent unless explicitly stopped,
/// user confirmed. Refuses with a clear reason otherwise; base-dirty disables
/// merge entirely (SPECS §13).
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
    // No running primary agent unless explicitly stopped.
    if req.primary_running {
        return Ok(MergeDecision::Refused(
            "Primary agent is still running; stop it before merging.".to_string(),
        ));
    }
    // User explicitly confirmed.
    if !req.user_confirmed {
        return Ok(MergeDecision::Refused(
            "Merge not confirmed by the user.".to_string(),
        ));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::MergeOutcome;
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
            primary_running: false,
            user_confirmed: true,
        }
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
    fn merge_refused_when_primary_running() {
        let git = FakeGit::new().with_branches(["main", "flightdeck/feat"]);
        let base_wt = Path::new("/repo");
        let agent_wt = Path::new("/repo/.flightdeck/worktrees/feat");
        let mut r = req("main", "flightdeck/feat", base_wt, agent_wt);
        r.primary_running = true;
        let decision = check_merge_preconditions(&git, &r).unwrap();
        match decision {
            MergeDecision::Refused(msg) => assert!(msg.contains("Primary agent")),
            MergeDecision::Allowed => panic!("expected refusal"),
        }
    }

    #[test]
    fn merge_refused_when_not_confirmed() {
        let git = FakeGit::new().with_branches(["main", "flightdeck/feat"]);
        let base_wt = Path::new("/repo");
        let agent_wt = Path::new("/repo/.flightdeck/worktrees/feat");
        let mut r = req("main", "flightdeck/feat", base_wt, agent_wt);
        r.user_confirmed = false;
        let decision = check_merge_preconditions(&git, &r).unwrap();
        match decision {
            MergeDecision::Refused(msg) => assert!(msg.contains("confirm")),
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
}
