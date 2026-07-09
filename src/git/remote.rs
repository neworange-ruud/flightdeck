//! Remote parsing, push planning, and GitHub PR compare URLs (SPECS §14).

use crate::contracts::{GitExecutor, Result};
use std::path::Path;

/// Parse a GitHub remote URL into `(owner, repo)` for both SSH and HTTPS forms
/// (SPECS §14):
/// - `git@github.com:owner/repo.git`
/// - `https://github.com/owner/repo.git`
pub fn parse_github_remote(url: &str) -> Option<(String, String)> {
    let url = url.trim();
    // Accept SSH (`git@github.com:owner/repo`) and HTTP(S)/ssh URL forms,
    // reducing each to the `owner/repo[.git]` path portion.
    let prefixes = [
        "git@github.com:",
        "https://github.com/",
        "http://github.com/",
        "ssh://git@github.com/",
    ];
    let path = prefixes.iter().find_map(|p| url.strip_prefix(p))?;

    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let path = path.trim_matches('/');
    let mut parts = path.splitn(2, '/');
    let owner = parts.next()?.trim();
    let repo = parts.next()?.trim();
    if owner.is_empty() || repo.is_empty() || repo.contains('/') {
        return None;
    }
    Some((owner.to_string(), repo.to_string()))
}

/// Build the GitHub PR compare URL (SPECS §14):
/// `https://github.com/<owner>/<repo>/compare/<base>...<branch>`.
pub fn pr_compare_url(owner: &str, repo: &str, base: &str, branch: &str) -> String {
    format!("https://github.com/{owner}/{repo}/compare/{base}...{branch}")
}

/// Push planning: whether the worktree is ready to push or has uncommitted
/// changes that warrant a warning (SPECS §14).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushPlan {
    /// Clean worktree; push will include all committed work.
    Ready,
    /// Worktree has uncommitted changes; warn that push only includes commits.
    UncommittedChanges,
}

/// Inspect the worktree to plan a push (SPECS §14).
pub fn plan_push(git: &dyn GitExecutor, worktree: &Path) -> Result<PushPlan> {
    if git.is_dirty(worktree)? {
        Ok(PushPlan::UncommittedChanges)
    } else {
        Ok(PushPlan::Ready)
    }
}

/// Push `branch` to `remote` from `worktree` after confirmation (SPECS §14).
pub fn push_branch(
    git: &dyn GitExecutor,
    remote: &str,
    branch: &str,
    worktree: &Path,
) -> Result<()> {
    git.push(remote, branch, worktree)
}

/// Compute the PR compare URL for `branch` if `remote` is a GitHub remote
/// (SPECS §14). Returns `None` when no remote is configured or it is not GitHub.
pub fn github_pr_url(
    git: &dyn GitExecutor,
    remote: &str,
    base: &str,
    branch: &str,
) -> Result<Option<String>> {
    let Some(url) = git.remote_url(remote)? else {
        return Ok(None);
    };
    let Some((owner, repo)) = parse_github_remote(&url) else {
        return Ok(None);
    };
    Ok(Some(pr_compare_url(&owner, &repo, base, branch)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeGit;

    #[test]
    fn parse_ssh_remote() {
        assert_eq!(
            parse_github_remote("git@github.com:owner/repo.git"),
            Some(("owner".to_string(), "repo".to_string()))
        );
    }

    #[test]
    fn parse_https_remote() {
        assert_eq!(
            parse_github_remote("https://github.com/owner/repo.git"),
            Some(("owner".to_string(), "repo".to_string()))
        );
    }

    #[test]
    fn parse_remote_without_git_suffix() {
        assert_eq!(
            parse_github_remote("https://github.com/owner/repo"),
            Some(("owner".to_string(), "repo".to_string()))
        );
        assert_eq!(
            parse_github_remote("git@github.com:owner/repo"),
            Some(("owner".to_string(), "repo".to_string()))
        );
    }

    #[test]
    fn parse_non_github_remote_is_none() {
        assert_eq!(parse_github_remote("git@gitlab.com:owner/repo.git"), None);
        assert_eq!(parse_github_remote("https://example.com/x/y.git"), None);
        assert_eq!(parse_github_remote("not a url"), None);
    }

    #[test]
    fn pr_compare_url_format() {
        assert_eq!(
            pr_compare_url("owner", "repo", "main", "flightdeck/feat"),
            "https://github.com/owner/repo/compare/main...flightdeck/feat"
        );
    }

    #[test]
    fn plan_push_ready_when_clean() {
        let git = FakeGit::new();
        let wt = Path::new("/repo/.flightdeck/worktrees/feat");
        git.set_dirty_at(wt, false);
        assert_eq!(plan_push(&git, wt).unwrap(), PushPlan::Ready);
    }

    #[test]
    fn plan_push_warns_when_uncommitted() {
        let git = FakeGit::new();
        let wt = Path::new("/repo/.flightdeck/worktrees/feat");
        git.set_dirty_at(wt, true);
        assert_eq!(plan_push(&git, wt).unwrap(), PushPlan::UncommittedChanges);
    }

    #[test]
    fn push_branch_delegates_to_executor() {
        let git = FakeGit::new();
        let wt = Path::new("/repo/.flightdeck/worktrees/feat");
        push_branch(&git, "origin", "flightdeck/feat", wt).unwrap();
        assert_eq!(
            git.pushes(),
            vec![(
                "origin".to_string(),
                "flightdeck/feat".to_string(),
                wt.to_path_buf()
            )]
        );
    }

    #[test]
    fn github_pr_url_for_github_remote() {
        let git = FakeGit::new();
        git.set_remote("origin", "git@github.com:owner/repo.git");
        let url = github_pr_url(&git, "origin", "main", "flightdeck/feat").unwrap();
        assert_eq!(
            url,
            Some("https://github.com/owner/repo/compare/main...flightdeck/feat".to_string())
        );
    }

    #[test]
    fn github_pr_url_none_for_non_github() {
        let git = FakeGit::new();
        git.set_remote("origin", "git@gitlab.com:owner/repo.git");
        assert_eq!(
            github_pr_url(&git, "origin", "main", "flightdeck/feat").unwrap(),
            None
        );
    }

    #[test]
    fn github_pr_url_none_when_no_remote() {
        let git = FakeGit::new();
        assert_eq!(
            github_pr_url(&git, "origin", "main", "flightdeck/feat").unwrap(),
            None
        );
    }
}
