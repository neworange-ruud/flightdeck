//! Command palette model (SPECS §22): every §22 action, filterable, selectable.
//!
//! This is a pure data model — no I/O, no rendering. It is testable standalone
//! and is rendered by `render.rs`. The wiring layer (T9) passes the selected
//! item's [`PaletteAction`] to the appropriate [`Command`] builder.
//!
//! T9 integration note: when the palette is confirmed, T9 must convert the
//! returned [`PaletteAction`] into the matching [`crate::app::commands::Command`]
//! (possibly opening a secondary prompt for payloads like the tab name or the
//! manual status choice) and call `AppState::dispatch`.

use crate::app::commands::{Command, Selector};

/// A single entry in the command palette (SPECS §22).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaletteEntry {
    /// Short human-readable label (the exact string the user sees and filters on).
    pub label: &'static str,
    /// The action this entry maps to.
    pub action: PaletteAction,
}

/// What the palette entry does when confirmed (SPECS §22). Most map directly to
/// a `Command`; some require additional user input that T9 must collect first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaletteAction {
    /// Dispatch this command directly (no additional input required).
    Dispatch(Command),
    /// T9 must prompt for a tab name then dispatch `NewAgentTab`.
    NewAgentTab,
    /// T9 must prompt for a new name then dispatch `RenameAgentTab`.
    RenameAgentTab,
    /// T9 must present the close-action menu then dispatch `CloseAgentTab`.
    CloseAgentTab,
    /// T9 must prompt for a manual status choice then dispatch `SetManualStatus`.
    SetManualStatus,
}

/// All §22 command-palette entries, in display order.
const ALL_ENTRIES: &[PaletteEntry] = &[
    PaletteEntry {
        label: "New Agent Tab",
        action: PaletteAction::NewAgentTab,
    },
    PaletteEntry {
        label: "Rename Agent Tab",
        action: PaletteAction::RenameAgentTab,
    },
    PaletteEntry {
        label: "Close Agent Tab",
        action: PaletteAction::CloseAgentTab,
    },
    PaletteEntry {
        label: "Push Branch",
        action: PaletteAction::Dispatch(Command::PushBranch { confirm: None }),
    },
    PaletteEntry {
        label: "Finish / Local Merge",
        action: PaletteAction::Dispatch(Command::FinishLocalMerge { confirm: false }),
    },
    PaletteEntry {
        label: "Abandon Worktree",
        action: PaletteAction::Dispatch(Command::AbandonWorktree),
    },
    PaletteEntry {
        label: "New Child Terminal",
        action: PaletteAction::Dispatch(Command::NewChildTerminal),
    },
    PaletteEntry {
        label: "Close Child Terminal",
        action: PaletteAction::Dispatch(Command::CloseChildTerminal),
    },
    PaletteEntry {
        label: "Switch Agent Tab",
        action: PaletteAction::Dispatch(Command::SwitchAgentTab(Selector::Next)),
    },
    PaletteEntry {
        label: "Switch Child Terminal",
        action: PaletteAction::Dispatch(Command::SwitchChildTerminal(Selector::Next)),
    },
    PaletteEntry {
        label: "Set Manual Status",
        action: PaletteAction::SetManualStatus,
    },
    PaletteEntry {
        label: "Restart Agent",
        action: PaletteAction::Dispatch(Command::RestartAgent),
    },
    PaletteEntry {
        label: "Open Shell",
        action: PaletteAction::Dispatch(Command::OpenShell),
    },
    PaletteEntry {
        label: "Show Git Status",
        action: PaletteAction::Dispatch(Command::ShowGitStatus),
    },
    PaletteEntry {
        label: "Show Help",
        action: PaletteAction::Dispatch(Command::ShowHelp),
    },
    PaletteEntry {
        label: "Quit",
        action: PaletteAction::Dispatch(Command::Quit),
    },
];

/// The number of required §22 command-palette actions.
pub const REQUIRED_ACTION_COUNT: usize = 16;

/// The command palette model (SPECS §22).
///
/// Holds the filter text and the current selection index into the filtered list.
/// All state changes are in-place — no I/O.
#[derive(Debug, Clone, Default)]
pub struct CommandPalette {
    /// The current filter string typed by the user.
    filter: String,
    /// The selected index within the current filtered results.
    selected: usize,
}

impl CommandPalette {
    /// Create an empty, unfiltered palette with no selection.
    pub fn new() -> Self {
        CommandPalette::default()
    }

    /// The current filter text.
    pub fn filter(&self) -> &str {
        &self.filter
    }

    /// Set the filter text and reset the selection to the first item.
    pub fn set_filter(&mut self, text: impl Into<String>) {
        self.filter = text.into();
        self.selected = 0;
    }

    /// Append a character to the filter text and reset selection.
    pub fn push_char(&mut self, c: char) {
        self.filter.push(c);
        self.selected = 0;
    }

    /// Remove the last character from the filter text and reset selection.
    pub fn pop_char(&mut self) {
        self.filter.pop();
        self.selected = 0;
    }

    /// Clear the filter text and reset selection.
    pub fn clear_filter(&mut self) {
        self.filter.clear();
        self.selected = 0;
    }

    /// The filtered list of matching entries (case-insensitive substring match).
    pub fn filtered(&self) -> Vec<&'static PaletteEntry> {
        let needle = self.filter.to_lowercase();
        ALL_ENTRIES
            .iter()
            .filter(|e| needle.is_empty() || e.label.to_lowercase().contains(&needle))
            .collect()
    }

    /// The currently selected index within the filtered list.
    pub fn selected_index(&self) -> usize {
        self.selected
    }

    /// Move selection down by one (wraps around).
    pub fn select_next(&mut self) {
        let len = self.filtered().len();
        if len > 0 {
            self.selected = (self.selected + 1) % len;
        }
    }

    /// Move selection up by one (wraps around).
    pub fn select_prev(&mut self) {
        let len = self.filtered().len();
        if len > 0 {
            self.selected = (self.selected + len - 1) % len;
        }
    }

    /// The currently selected [`PaletteAction`], if any filtered results exist.
    pub fn selected_action(&self) -> Option<&'static PaletteAction> {
        let items = self.filtered();
        items.get(self.selected).map(|e| &e.action)
    }

    /// The total number of §22 actions (unfiltered). Used in tests to assert
    /// completeness.
    pub fn total_actions() -> usize {
        ALL_ENTRIES.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_all_required_actions() {
        // SPECS §22 mandates exactly 16 palette actions.
        assert_eq!(
            CommandPalette::total_actions(),
            REQUIRED_ACTION_COUNT,
            "palette must list all §22 actions"
        );
    }

    #[test]
    fn all_action_labels_present() {
        let required = [
            "New Agent Tab",
            "Rename Agent Tab",
            "Close Agent Tab",
            "Push Branch",
            "Finish / Local Merge",
            "Abandon Worktree",
            "New Child Terminal",
            "Close Child Terminal",
            "Switch Agent Tab",
            "Switch Child Terminal",
            "Set Manual Status",
            "Restart Agent",
            "Open Shell",
            "Show Git Status",
            "Show Help",
            "Quit",
        ];
        let labels: Vec<&str> = ALL_ENTRIES.iter().map(|e| e.label).collect();
        for req in &required {
            assert!(
                labels.contains(req),
                "missing required palette action: '{req}'"
            );
        }
    }

    #[test]
    fn filter_narrows_list() {
        let mut palette = CommandPalette::new();
        palette.set_filter("agent tab");
        let results = palette.filtered();
        // Should match "New Agent Tab", "Rename Agent Tab", "Close Agent Tab",
        // "Switch Agent Tab"
        assert!(
            results.len() >= 3,
            "expected at least 3 results for 'agent tab', got {}",
            results.len()
        );
        for entry in &results {
            assert!(
                entry.label.to_lowercase().contains("agent tab"),
                "unexpected match: {}",
                entry.label
            );
        }
    }

    #[test]
    fn filter_empty_shows_all() {
        let palette = CommandPalette::new();
        assert_eq!(palette.filtered().len(), REQUIRED_ACTION_COUNT);
    }

    #[test]
    fn filter_case_insensitive() {
        let mut palette = CommandPalette::new();
        palette.set_filter("QUIT");
        let results = palette.filtered();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].label, "Quit");
    }

    #[test]
    fn filter_no_match_returns_empty() {
        let mut palette = CommandPalette::new();
        palette.set_filter("xyzzy_no_match");
        assert!(palette.filtered().is_empty());
        assert!(palette.selected_action().is_none());
    }

    #[test]
    fn navigation_wraps() {
        let mut palette = CommandPalette::new();
        // At end, next wraps to 0.
        palette.selected = REQUIRED_ACTION_COUNT - 1;
        palette.select_next();
        assert_eq!(palette.selected_index(), 0);

        // At start, prev wraps to last.
        palette.select_prev();
        assert_eq!(palette.selected_index(), REQUIRED_ACTION_COUNT - 1);
    }

    #[test]
    fn selected_action_quit() {
        let mut palette = CommandPalette::new();
        palette.set_filter("quit");
        let action = palette.selected_action().expect("should match Quit");
        assert_eq!(action, &PaletteAction::Dispatch(Command::Quit));
    }

    #[test]
    fn selected_action_new_agent_tab() {
        let mut palette = CommandPalette::new();
        palette.set_filter("New Agent Tab");
        let results = palette.filtered();
        // First result should be "New Agent Tab"
        let first_label = results.first().map(|e| e.label).unwrap_or("");
        assert_eq!(first_label, "New Agent Tab");
        let action = palette.selected_action().unwrap();
        assert_eq!(action, &PaletteAction::NewAgentTab);
    }

    #[test]
    fn push_and_pop_char_updates_filter() {
        let mut palette = CommandPalette::new();
        palette.push_char('q');
        palette.push_char('u');
        palette.push_char('i');
        palette.push_char('t');
        assert_eq!(palette.filter(), "quit");
        assert_eq!(palette.filtered().len(), 1);

        palette.pop_char();
        assert_eq!(palette.filter(), "qui");
        // "qui" still matches "Quit"
        assert_eq!(palette.filtered().len(), 1);

        palette.clear_filter();
        assert_eq!(palette.filter(), "");
        assert_eq!(palette.filtered().len(), REQUIRED_ACTION_COUNT);
    }

    #[test]
    fn select_next_and_prev_basic() {
        let mut palette = CommandPalette::new();
        assert_eq!(palette.selected_index(), 0);
        palette.select_next();
        assert_eq!(palette.selected_index(), 1);
        palette.select_prev();
        assert_eq!(palette.selected_index(), 0);
    }

    #[test]
    fn filter_reset_selection_to_zero() {
        let mut palette = CommandPalette::new();
        palette.select_next();
        palette.select_next();
        assert_eq!(palette.selected_index(), 2);
        palette.set_filter("quit");
        assert_eq!(palette.selected_index(), 0);
    }
}
