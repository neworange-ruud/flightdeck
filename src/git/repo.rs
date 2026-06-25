//! Real [`GitExecutor`] over the `git` binary, plus repo-root / base-branch
//! detection (SPECS §5, §27).

use crate::contracts::{
    FlightDeckError, GitExecutor, MergeOutcome, RebaseOutcome, Result, WorktreeInfo,
};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// `git`-binary-backed [`GitExecutor`]. Constructed for a specific repository
/// root; methods without a `cwd` argument run against that root.
#[derive(Debug, Clone)]
pub struct GitCli {
    root: PathBuf,
}

impl GitCli {
    /// Construct bound to a known repository root.
    pub fn new(root: PathBuf) -> Self {
        GitCli { root }
    }

    /// Discover the repository root from `cwd` and construct a [`GitCli`].
    pub fn discover(cwd: &Path) -> Result<Self> {
        let out = run_git_in(cwd, &["rev-parse", "--show-toplevel"])?;
        let root = stdout_trimmed(&out);
        if root.is_empty() {
            return Err(FlightDeckError::Git(
                "could not determine repository root".to_string(),
            ));
        }
        Ok(GitCli {
            root: PathBuf::from(root),
        })
    }

    /// The repository root this executor is bound to.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Run a `git -C <root> ...` command, returning its captured output on
    /// success or a [`FlightDeckError::Git`] describing the failure.
    fn run(&self, args: &[&str]) -> Result<Output> {
        run_git_in(&self.root, args)
    }

    /// Run a `git -C <cwd> ...` command for an explicit working directory.
    fn run_in(&self, cwd: &Path, args: &[&str]) -> Result<Output> {
        run_git_in(cwd, args)
    }
}

/// Execute `git -C <dir> <args...>` and capture output. Maps a non-zero exit or
/// a spawn failure to [`FlightDeckError::Git`].
fn run_git_in(dir: &Path, args: &[&str]) -> Result<Output> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| FlightDeckError::Git(format!("failed to run git {}: {e}", args.join(" "))))?;
    Ok(out)
}

/// Require a successful exit; otherwise produce a `Git` error with stderr.
fn require_success(out: &Output, what: &str) -> Result<()> {
    if out.status.success() {
        Ok(())
    } else {
        Err(FlightDeckError::Git(format!(
            "{what} failed: {}",
            stderr_trimmed(out)
        )))
    }
}

fn stdout_trimmed(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn stderr_trimmed(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).trim().to_string()
}

impl GitExecutor for GitCli {
    fn repo_root(&self, cwd: &Path) -> Result<PathBuf> {
        let out = self.run_in(cwd, &["rev-parse", "--show-toplevel"])?;
        require_success(&out, "rev-parse --show-toplevel")?;
        Ok(PathBuf::from(stdout_trimmed(&out)))
    }

    fn current_branch(&self, cwd: &Path) -> Result<String> {
        let out = self.run_in(cwd, &["rev-parse", "--abbrev-ref", "HEAD"])?;
        require_success(&out, "rev-parse --abbrev-ref HEAD")?;
        Ok(stdout_trimmed(&out))
    }

    fn is_dirty(&self, cwd: &Path) -> Result<bool> {
        let out = self.run_in(cwd, &["status", "--porcelain"])?;
        require_success(&out, "status --porcelain")?;
        Ok(!out.stdout.is_empty() && !stdout_trimmed(&out).is_empty())
    }

    fn status_porcelain(&self, cwd: &Path) -> Result<Vec<String>> {
        let out = self.run_in(cwd, &["status", "--porcelain"])?;
        require_success(&out, "status --porcelain")?;
        Ok(String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.to_string())
            .collect())
    }

    fn branch_exists(&self, name: &str) -> Result<bool> {
        // `git branch --list <name>` prints the branch if it exists locally.
        let out = self.run(&["branch", "--list", name])?;
        require_success(&out, "branch --list")?;
        Ok(!stdout_trimmed(&out).is_empty())
    }

    fn create_branch(&self, name: &str, from: &str) -> Result<()> {
        // `git branch <name> <from>` creates without checkout and never
        // rewrites history (SPECS §5).
        let out = self.run(&["branch", name, from])?;
        require_success(&out, &format!("branch {name} {from}"))
    }

    fn rev_parse(&self, refname: &str) -> Result<String> {
        let out = self.run(&["rev-parse", refname])?;
        require_success(&out, &format!("rev-parse {refname}"))?;
        Ok(stdout_trimmed(&out))
    }

    fn add_worktree(&self, path: &Path, branch: &str) -> Result<()> {
        let path_str = path.to_string_lossy();
        let out = self.run(&["worktree", "add", &path_str, branch])?;
        require_success(&out, &format!("worktree add {path_str} {branch}"))
    }

    fn list_worktrees(&self) -> Result<Vec<WorktreeInfo>> {
        let out = self.run(&["worktree", "list", "--porcelain"])?;
        require_success(&out, "worktree list --porcelain")?;
        Ok(parse_worktree_list(&String::from_utf8_lossy(&out.stdout)))
    }

    fn remove_worktree(&self, path: &Path, force: bool) -> Result<()> {
        let path_str = path.to_string_lossy();
        let args: &[&str] = if force {
            &["worktree", "remove", "--force", &path_str]
        } else {
            &["worktree", "remove", &path_str]
        };
        let out = self.run(args)?;
        require_success(&out, &format!("worktree remove {path_str}"))
    }

    fn ahead_behind(&self, base: &str, branch: &str) -> Result<(u32, u32)> {
        // `git rev-list --left-right --count base...branch` prints
        // "<left>\t<right>" where left = commits in base not in branch (behind)
        // and right = commits in branch not in base (ahead). We report
        // (ahead, behind) per the trait contract.
        let range = format!("{base}...{branch}");
        let out = self.run(&["rev-list", "--left-right", "--count", &range])?;
        require_success(&out, &format!("rev-list --left-right --count {range}"))?;
        let text = stdout_trimmed(&out);
        let mut parts = text.split_whitespace();
        let left: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        let right: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        // left = behind (in base, not branch); right = ahead (in branch, not base).
        Ok((right, left))
    }

    fn upstream_of(&self, branch: &str) -> Result<Option<String>> {
        let spec = format!("{branch}@{{upstream}}");
        let out = self.run(&["rev-parse", "--abbrev-ref", &spec])?;
        if out.status.success() {
            let up = stdout_trimmed(&out);
            if up.is_empty() {
                Ok(None)
            } else {
                Ok(Some(up))
            }
        } else {
            // No upstream configured is not an error condition.
            Ok(None)
        }
    }

    fn push(&self, remote: &str, branch: &str, cwd: &Path) -> Result<()> {
        let out = self.run_in(cwd, &["push", remote, branch])?;
        require_success(&out, &format!("push {remote} {branch}"))
    }

    fn remote_url(&self, remote: &str) -> Result<Option<String>> {
        let out = self.run(&["remote", "get-url", remote])?;
        if out.status.success() {
            let url = stdout_trimmed(&out);
            if url.is_empty() {
                Ok(None)
            } else {
                Ok(Some(url))
            }
        } else {
            // Unknown remote is reported as "no URL", not a hard error.
            Ok(None)
        }
    }

    fn merge_no_ff(&self, branch: &str, cwd: &Path) -> Result<MergeOutcome> {
        // Guarded merge: a `--no-ff` merge in `cwd`. Conflicts are reported, not
        // resolved (SPECS §15). This must only be reached after precondition
        // checks in the workflow layer.
        let out = self.run_in(cwd, &["merge", "--no-ff", branch])?;
        if out.status.success() {
            Ok(MergeOutcome {
                merged: true,
                conflicted: false,
                message: format!("merged {branch} (--no-ff)"),
            })
        } else {
            let combined = format!("{}\n{}", stdout_trimmed(&out), stderr_trimmed(&out));
            let conflicted = combined.to_lowercase().contains("conflict");
            Ok(MergeOutcome {
                merged: false,
                conflicted,
                message: combined.trim().to_string(),
            })
        }
    }

    fn rebase_onto(&self, onto: &str, cwd: &Path) -> Result<RebaseOutcome> {
        // Guarded rebase (SPECS §5 carve-out): only reached after preconditions
        // and explicit confirmation in the workflow layer. On any failure we
        // abort so the worktree is left exactly as it was — never resolve
        // conflicts, never leave a half-finished rebase.
        let out = self.run_in(cwd, &["rebase", onto])?;
        if out.status.success() {
            return Ok(RebaseOutcome {
                rebased: true,
                conflicted: false,
                message: format!("rebased onto {onto}"),
            });
        }
        let combined = format!("{}\n{}", stdout_trimmed(&out), stderr_trimmed(&out));
        let conflicted = combined.to_lowercase().contains("conflict");
        // Abort any rebase left in progress (no-op/harmless if none is). We
        // ignore its result: the worktree state is reported via the outcome.
        let _ = self.run_in(cwd, &["rebase", "--abort"]);
        Ok(RebaseOutcome {
            rebased: false,
            conflicted,
            message: combined.trim().to_string(),
        })
    }
}

/// Parse the output of `git worktree list --porcelain` into [`WorktreeInfo`]s.
///
/// Records are separated by blank lines. Each starts with a `worktree <path>`
/// line, optionally followed by `HEAD <sha>` and `branch refs/heads/<name>`
/// (or `detached`).
fn parse_worktree_list(text: &str) -> Vec<WorktreeInfo> {
    let mut out = Vec::new();
    let mut cur: Option<WorktreeInfo> = None;

    let flush = |cur: &mut Option<WorktreeInfo>, out: &mut Vec<WorktreeInfo>| {
        if let Some(info) = cur.take() {
            out.push(info);
        }
    };

    for line in text.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            flush(&mut cur, &mut out);
            continue;
        }
        if let Some(rest) = line.strip_prefix("worktree ") {
            // Starting a new record.
            flush(&mut cur, &mut out);
            cur = Some(WorktreeInfo {
                path: PathBuf::from(rest),
                branch: None,
                head: None,
            });
        } else if let Some(rest) = line.strip_prefix("HEAD ") {
            if let Some(info) = cur.as_mut() {
                info.head = Some(rest.to_string());
            }
        } else if let Some(rest) = line.strip_prefix("branch ") {
            if let Some(info) = cur.as_mut() {
                let name = rest.strip_prefix("refs/heads/").unwrap_or(rest);
                info.branch = Some(name.to_string());
            }
        } else if line == "detached" {
            // Leave branch as None.
        }
        // Other attributes (bare, locked, prunable) are ignored.
    }
    flush(&mut cur, &mut out);
    out
}

/// Detect the base branch: the configured value if given and valid, else the
/// current branch (SPECS §7, §12).
pub fn detect_base_branch(
    git: &dyn GitExecutor,
    cwd: &Path,
    configured: Option<&str>,
) -> Result<String> {
    if let Some(name) = configured {
        if !name.is_empty() && git.branch_exists(name)? {
            return Ok(name.to_string());
        }
    }
    git.current_branch(cwd)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeGit;

    #[test]
    fn detect_base_uses_configured_when_it_exists() {
        let git = FakeGit::new().with_branches(["main", "develop"]);
        let base = detect_base_branch(&git, Path::new("/repo"), Some("develop")).unwrap();
        assert_eq!(base, "develop");
    }

    #[test]
    fn detect_base_falls_back_to_current_when_configured_missing() {
        let git = FakeGit::new()
            .with_branches(["main"])
            .with_current_branch("feature");
        let base = detect_base_branch(&git, Path::new("/repo"), Some("nonexistent")).unwrap();
        assert_eq!(base, "feature");
    }

    #[test]
    fn detect_base_falls_back_to_current_when_none_configured() {
        let git = FakeGit::new().with_current_branch("trunk");
        let base = detect_base_branch(&git, Path::new("/repo"), None).unwrap();
        assert_eq!(base, "trunk");
    }

    #[test]
    fn parse_worktree_list_extracts_path_branch_head() {
        let text = "\
worktree /repo
HEAD abc123
branch refs/heads/main

worktree /repo/.flightdeck/worktrees/feat
HEAD def456
branch refs/heads/flightdeck/feat

worktree /repo/detachedwt
HEAD 999fff
detached
";
        let wts = parse_worktree_list(text);
        assert_eq!(wts.len(), 3);
        assert_eq!(wts[0].path, PathBuf::from("/repo"));
        assert_eq!(wts[0].branch.as_deref(), Some("main"));
        assert_eq!(wts[0].head.as_deref(), Some("abc123"));
        assert_eq!(
            wts[1].path,
            PathBuf::from("/repo/.flightdeck/worktrees/feat")
        );
        assert_eq!(wts[1].branch.as_deref(), Some("flightdeck/feat"));
        assert_eq!(wts[2].branch, None);
        assert_eq!(wts[2].head.as_deref(), Some("999fff"));
    }

    #[test]
    fn parse_worktree_list_handles_empty() {
        assert!(parse_worktree_list("").is_empty());
    }
}
