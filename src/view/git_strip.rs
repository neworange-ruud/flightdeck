//! The git strip view model: the selected agent's branch, base and git actions.

use std::collections::HashMap;

use crate::app::state::{AppState, RuntimeTab, TabPhase};
use crate::git::status::WorktreeStatus;
use crate::view::agent::{ChangeSummary, UpstreamState};

/// Which git actions the strip offers for the selected agent.
///
/// These are the *static* preconditions AppState can decide on its own. Actions
/// that also depend on live repository state (a dirty base folder, the base
/// folder being on another branch, a push with uncommitted changes) are still
/// checked when the command runs and answer with a refusal or a confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GitActions {
    /// Push the agent's branch. Needs a materialized worktree.
    pub push: bool,
    /// Pull the base branch in the base folder. Needs a materialized worktree
    /// so there is a selected, live agent to act for.
    pub pull_base: bool,
    /// Finish (local merge-back). Needs a materialized worktree and an agent
    /// that has its own branch: a tab running directly on the base branch has
    /// nothing to merge back.
    pub finish: bool,
}

/// The available git actions for `tab`.
pub fn git_actions(tab: &RuntimeTab) -> GitActions {
    let ready = tab.phase == TabPhase::Ready;
    GitActions {
        push: ready,
        pull_base: ready,
        finish: ready && !tab.meta.runs_on_base,
    }
}

/// The selected agent's part of the git strip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitStripAgent {
    /// The agent's tab name.
    pub name: String,
    /// The branch: freshly collected when git status is known, else stored.
    pub branch: String,
    /// The branch this agent merges back into (its target/base branch).
    pub target_branch: String,
    /// Whether the target is the project's configured default base.
    pub target_is_default: bool,
    /// Uncommitted changes, or `None` when no git status is collected yet.
    pub changes: Option<ChangeSummary>,
    /// Upstream tracking state.
    pub upstream: UpstreamState,
    /// Commits the target branch has advanced since the agent last synced.
    pub base_drift: u32,
    /// Which of Push / Pull base / Finish are available.
    pub actions: GitActions,
}

/// The git strip: the project's default base plus, when an agent is selected,
/// that agent's git state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitStripView {
    /// The project's default base branch. When the configured one is not a
    /// local branch this is the configured (invalid) name, so it can be shown
    /// with a warning.
    pub default_branch: String,
    /// Whether the default base branch exists locally. `false` means the name
    /// in [`GitStripView::default_branch`] is the misconfigured one.
    pub default_branch_valid: bool,
    /// The selected agent, or `None` when no Agent Tab is selected.
    pub agent: Option<GitStripAgent>,
}

/// Build the git strip for the selected agent (if any).
pub fn git_strip_view(state: &AppState, git: &HashMap<String, WorktreeStatus>) -> GitStripView {
    let default_branch = state
        .invalid_base_branch
        .as_deref()
        .unwrap_or(&state.base_branch)
        .to_string();
    let agent = state.selected().map(|tab| {
        let ws = git.get(&tab.meta.id);
        GitStripAgent {
            name: tab.meta.name.clone(),
            branch: ws
                .map(|w| w.branch.clone())
                .unwrap_or_else(|| tab.meta.branch.clone()),
            target_branch: tab.meta.base_branch.clone(),
            target_is_default: tab.meta.base_branch == state.base_branch,
            changes: ws.map(|w| ChangeSummary::of(w.changes, w.lines)),
            upstream: UpstreamState::of(ws),
            base_drift: ws.map(|w| w.base_drift).unwrap_or(0),
            actions: git_actions(tab),
        }
    });
    GitStripView {
        default_branch,
        default_branch_valid: state.invalid_base_branch.is_none(),
        agent,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::Config;
    use crate::git::status::WorktreeChanges;
    use crate::persistence::project_state::default_state;
    use std::path::PathBuf;

    fn state_with_tabs(n: usize) -> AppState {
        let mut ps = default_state("main");
        for i in 0..n {
            ps.tabs.push(crate::contracts::TabState {
                id: format!("t{i}"),
                name: format!("tab{i}"),
                slug: format!("tab{i}"),
                agent: "opencode".to_string(),
                branch: format!("flightdeck/tab{i}"),
                worktree_path_relative: format!(".flightdeck/worktrees/tab{i}"),
                base_branch: "main".to_string(),
                base_commit_sha: "sha".to_string(),
                created_at: "t".to_string(),
                attached_existing_branch: false,
                recovered: false,
                last_known_status: "unknown".to_string(),
                manual_status: None,
                containerized: false,
                container_image: None,
                runs_on_base: false,
                resume_args: Vec::new(),
                activity: Default::default(),
            });
        }
        AppState::new(Config::default(), ps, "/repo", "/repo/state.json")
    }

    fn status(upstream: Option<&str>) -> WorktreeStatus {
        WorktreeStatus {
            lines: Default::default(),
            branch: "flightdeck/live".to_string(),
            base_branch: "main".to_string(),
            dirty: true,
            changes: WorktreeChanges {
                added: 1,
                modified: 1,
                deleted: 0,
            },
            ahead: 3,
            behind: 0,
            upstream: upstream.map(str::to_string),
            base_drift: 2,
            worktree_path: PathBuf::from("/wt"),
        }
    }

    #[test]
    fn ready_agent_on_its_own_branch_offers_all_three_actions() {
        let state = state_with_tabs(1);
        let actions = git_actions(&state.tabs[0]);
        assert_eq!(
            actions,
            GitActions {
                push: true,
                pull_base: true,
                finish: true
            }
        );
    }

    #[test]
    fn creating_agent_offers_nothing() {
        let mut state = state_with_tabs(1);
        state.tabs[0].phase = TabPhase::Creating;
        assert_eq!(
            git_actions(&state.tabs[0]),
            GitActions {
                push: false,
                pull_base: false,
                finish: false
            }
        );
    }

    #[test]
    fn agent_running_on_base_cannot_finish() {
        let mut state = state_with_tabs(1);
        state.tabs[0].meta.runs_on_base = true;
        let actions = git_actions(&state.tabs[0]);
        assert!(actions.push);
        assert!(actions.pull_base);
        assert!(!actions.finish);
    }

    #[test]
    fn no_selected_agent_means_no_agent_strip() {
        let mut state = state_with_tabs(1);
        state.selected_tab = None;
        let view = git_strip_view(&state, &HashMap::new());
        assert_eq!(view.agent, None);
        assert_eq!(view.default_branch, "main");
        assert!(view.default_branch_valid);
    }

    #[test]
    fn strip_without_git_status_uses_stored_data() {
        let state = state_with_tabs(1);
        let agent = git_strip_view(&state, &HashMap::new()).agent.unwrap();
        assert_eq!(agent.name, "tab0");
        assert_eq!(agent.branch, "flightdeck/tab0");
        assert_eq!(agent.target_branch, "main");
        assert!(agent.target_is_default);
        assert_eq!(agent.changes, None);
        assert_eq!(agent.upstream, UpstreamState::Unknown);
        assert_eq!(agent.base_drift, 0);
    }

    #[test]
    fn strip_with_git_status_reports_changes_upstream_and_drift() {
        let state = state_with_tabs(1);
        let git = HashMap::from([("t0".to_string(), status(Some("origin/x")))]);
        let agent = git_strip_view(&state, &git).agent.unwrap();
        assert_eq!(agent.branch, "flightdeck/live");
        assert_eq!(agent.changes.unwrap().files, 2);
        assert_eq!(
            agent.upstream,
            UpstreamState::Tracking {
                upstream: "origin/x".to_string(),
                ahead: 3,
                behind: 0
            }
        );
        assert_eq!(agent.base_drift, 2);

        let git = HashMap::from([("t0".to_string(), status(None))]);
        let agent = git_strip_view(&state, &git).agent.unwrap();
        assert_eq!(agent.upstream, UpstreamState::None);
    }

    #[test]
    fn non_default_target_is_flagged() {
        let mut state = state_with_tabs(1);
        state.tabs[0].meta.base_branch = "develop".to_string();
        let agent = git_strip_view(&state, &HashMap::new()).agent.unwrap();
        assert_eq!(agent.target_branch, "develop");
        assert!(!agent.target_is_default);
    }

    #[test]
    fn invalid_default_branch_reports_the_configured_name() {
        let mut state = state_with_tabs(0);
        state.invalid_base_branch = Some("trunk".to_string());
        let view = git_strip_view(&state, &HashMap::new());
        assert_eq!(view.default_branch, "trunk");
        assert!(!view.default_branch_valid);
    }
}
