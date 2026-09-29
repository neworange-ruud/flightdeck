//! The project tab row view model.

use crate::app::state::AppState;
use crate::contracts::InterpretedStatus;

/// A project's aggregate status: the most urgent state among its agents.
///
/// The variants are declared in increasing urgency and `Ord` follows that
/// order, so aggregating is just taking the maximum — attention beats working
/// beats idle. A project with no agents is [`ProjectStatus::Idle`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ProjectStatus {
    /// Nothing is running or waiting (also: the project has no agents).
    Idle,
    /// At least one agent is starting or working, and none needs attention.
    Working,
    /// At least one agent is waiting for input, needs attention or has failed.
    /// Wins over [`ProjectStatus::Working`] so a blocked agent is never hidden
    /// behind a busy sibling.
    NeedsAttention,
}

impl ProjectStatus {
    /// The status one agent contributes to its project.
    ///
    /// `SessionLost` is deliberately idle here: it shows as an error on the
    /// agent's own row, but a lost session is not something a project-level
    /// badge should shout about.
    pub fn of_agent(status: InterpretedStatus) -> Self {
        use InterpretedStatus::*;
        match status {
            Starting | Running | Working => ProjectStatus::Working,
            WaitingForInput | NeedsAttention | Failed => ProjectStatus::NeedsAttention,
            Idle | Completed | Stopped | SessionLost | Recovered | Unknown => ProjectStatus::Idle,
        }
    }

    /// Aggregate over a project's agents: precedence NEEDS ATTENTION > WORKING >
    /// IDLE, and IDLE for an empty project.
    pub fn aggregate(statuses: impl IntoIterator<Item = InterpretedStatus>) -> Self {
        statuses
            .into_iter()
            .map(Self::of_agent)
            .max()
            .unwrap_or(ProjectStatus::Idle)
    }

    /// The upper-case label front-ends show for this status.
    pub fn label(self) -> &'static str {
        match self {
            ProjectStatus::Idle => "IDLE",
            ProjectStatus::Working => "WORKING",
            ProjectStatus::NeedsAttention => "NEEDS ATTENTION",
        }
    }
}

/// One project's entry on the project tab row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectTabView {
    /// Display name (the project's folder name).
    pub name: String,
    /// Aggregate status over the project's agents.
    pub status: ProjectStatus,
    /// How many agents (Agent Tabs) the project has.
    pub agent_count: usize,
    /// Whether this is the project currently in view.
    pub active: bool,
}

/// Build the project tab view for one project's [`AppState`].
///
/// `name` and `active` come from the workspace that owns the project (the state
/// itself knows neither); `now_ms` is threaded through to the display-status
/// computation the rest of the app shares.
pub fn project_tab_view(name: &str, state: &AppState, active: bool, now_ms: u64) -> ProjectTabView {
    ProjectTabView {
        name: name.to_string(),
        status: ProjectStatus::aggregate(
            state
                .tabs
                .iter()
                .map(|tab| tab.display_status(now_ms).interpreted),
        ),
        agent_count: state.tabs.len(),
        active,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use InterpretedStatus::*;

    #[test]
    fn empty_project_is_idle() {
        assert_eq!(ProjectStatus::aggregate([]), ProjectStatus::Idle);
    }

    #[test]
    fn all_idle_agents_are_idle() {
        assert_eq!(
            ProjectStatus::aggregate([Idle, Completed, Stopped, Unknown, Recovered]),
            ProjectStatus::Idle
        );
    }

    #[test]
    fn working_beats_idle() {
        for busy in [Starting, Running, Working] {
            assert_eq!(
                ProjectStatus::aggregate([Idle, busy, Idle]),
                ProjectStatus::Working,
                "{busy:?}"
            );
        }
    }

    #[test]
    fn attention_beats_working_and_idle() {
        for needy in [WaitingForInput, NeedsAttention, Failed] {
            assert_eq!(
                ProjectStatus::aggregate([Working, needy, Idle]),
                ProjectStatus::NeedsAttention,
                "{needy:?}"
            );
            // Order of the agents must not matter.
            assert_eq!(
                ProjectStatus::aggregate([needy, Working]),
                ProjectStatus::NeedsAttention
            );
        }
    }

    #[test]
    fn lost_session_does_not_raise_the_project_badge() {
        assert_eq!(ProjectStatus::aggregate([SessionLost]), ProjectStatus::Idle);
    }

    #[test]
    fn precedence_is_reflected_in_ord() {
        assert!(ProjectStatus::NeedsAttention > ProjectStatus::Working);
        assert!(ProjectStatus::Working > ProjectStatus::Idle);
    }

    #[test]
    fn labels_are_upper_case() {
        assert_eq!(ProjectStatus::Idle.label(), "IDLE");
        assert_eq!(ProjectStatus::Working.label(), "WORKING");
        assert_eq!(ProjectStatus::NeedsAttention.label(), "NEEDS ATTENTION");
    }
}
