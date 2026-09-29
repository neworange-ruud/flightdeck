//! Presentation view models: pure functions from [`AppState`] to plain structs.
//!
//! Every front-end (the ratatui TUI today, the GPUI desktop app next) draws the
//! same things: a project tab row, an Agent Tab list, a git strip and a mode
//! bar. Deciding *what those show* — which status wins when a project has
//! several agents, which git buttons are enabled, which hints the mode bar lists
//! — is logic, not drawing, so it lives here once instead of being re-derived
//! inside each renderer where the two would drift apart.
//!
//! The rules for this module:
//!
//! - **Pure.** No I/O and no clock reads. Anything time-dependent (`now_ms`) or
//!   not held by [`AppState`] (git status) is a parameter.
//! - **Front-end neutral.** No `ratatui`, `crossterm` or GPUI types. Structs
//!   carry semantics (a badge, a label, a flag), never colours or glyphs; each
//!   renderer maps them to its own styling.
//! - **Plain data.** Every struct is `pub`, `Debug + Clone + PartialEq`.
//!
//! [`AppState`]: crate::app::state::AppState

pub mod agent;
pub mod git_strip;
pub mod mission;
pub mod mode_bar;
pub mod project;
pub mod prompt;

pub use agent::{
    agent_badge, agent_row_view, agent_row_views, agent_status_text, format_elapsed,
    terminal_views, AgentBadge, AgentRowView, ChangeSummary, TerminalRef, TerminalRole,
    TerminalView, UpstreamState,
};
pub use git_strip::{git_actions, git_strip_view, GitActions, GitStripAgent, GitStripView};
pub use mission::{
    grid_columns, grid_visible_rows, mission_view, move_selection, CardAction, GridMove, LastEvent,
    LastEventKind, MissionCard, MissionSource, MissionTile, MissionView, SessionKey,
};
pub use mode_bar::{mode_bar_view, HintAction, HintView, ModeBarView};
pub use project::{project_tab_view, ProjectStatus, ProjectTabView};
pub use prompt::{
    prompt_view, PromptAnswer, PromptButton, PromptReply, PromptShape, PromptView,
    MAX_INLINE_OPTIONS,
};
