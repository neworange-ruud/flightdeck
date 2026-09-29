//! The mode pill and context hints view model (the TUI status bar).

use crate::app::modes::InputMode;

/// What activating a hint (or the mode pill) does. Every action is exactly what
/// the hint's own shortcut does, so a front-end never offers something the
/// keyboard cannot reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HintAction {
    /// Leave terminal focus for the app chrome.
    FocusApp,
    /// Give focus back to the terminal.
    FocusTerminal,
    /// Open the command palette.
    OpenPalette,
    /// Open the help screen.
    OpenHelp,
}

/// One key hint: a key label and what it does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HintView {
    /// The key label, e.g. `Ctrl-g`.
    pub key: String,
    /// What the key does, e.g. `palette`.
    pub label: String,
    /// The action activating the hint performs.
    pub action: HintAction,
}

/// The mode pill plus the context hints and badges beside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModeBarView {
    /// The current input mode.
    pub mode: InputMode,
    /// The mode pill text: `MODE: TERMINAL` or `MODE: APP`.
    pub pill: &'static str,
    /// What clicking the pill does: it leaves the current mode, like the first
    /// hint does.
    pub pill_action: HintAction,
    /// The hints in display order: leave the current mode, palette, help.
    pub hints: Vec<HintView>,
    /// Who holds the shared input lock, when a browser writer contests it.
    pub input_holder: Option<String>,
    /// Whether this is an isolated run.
    pub isolated: bool,
    /// A newer release's version, when one is available.
    pub update_available: Option<String>,
}

/// Build the mode bar.
///
/// `leave_key` is the label of the key that leaves terminal focus (it varies by
/// platform and config, so the caller supplies it) and `help_keys` the label of
/// the help keys, which the help screen also shows. In Terminal mode the first
/// hint is `leave_key: app mode`; in App mode it is `Enter: focus terminal`.
pub fn mode_bar_view(
    mode: InputMode,
    leave_key: &str,
    help_keys: &str,
    update_available: Option<&str>,
    isolated: bool,
    input_holder: Option<&str>,
) -> ModeBarView {
    let (pill, leave, first) = match mode {
        InputMode::Terminal => (
            "MODE: TERMINAL",
            HintAction::FocusApp,
            HintView {
                key: leave_key.to_string(),
                label: "app mode".to_string(),
                action: HintAction::FocusApp,
            },
        ),
        InputMode::App => (
            "MODE: APP",
            HintAction::FocusTerminal,
            HintView {
                key: "Enter".to_string(),
                label: "focus terminal".to_string(),
                action: HintAction::FocusTerminal,
            },
        ),
    };
    ModeBarView {
        mode,
        pill,
        pill_action: leave,
        hints: vec![
            first,
            HintView {
                key: "Ctrl-g".to_string(),
                label: "palette".to_string(),
                action: HintAction::OpenPalette,
            },
            HintView {
                key: help_keys.to_string(),
                label: "help".to_string(),
                action: HintAction::OpenHelp,
            },
        ],
        input_holder: input_holder.map(str::to_string),
        isolated,
        update_available: update_available.map(str::to_string),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys_labels(view: &ModeBarView) -> Vec<(String, String)> {
        view.hints
            .iter()
            .map(|h| (h.key.clone(), h.label.clone()))
            .collect()
    }

    #[test]
    fn terminal_mode_leads_with_the_leave_key() {
        let v = mode_bar_view(
            InputMode::Terminal,
            "Alt-Space",
            "F1 / Alt-h",
            None,
            false,
            None,
        );
        assert_eq!(v.pill, "MODE: TERMINAL");
        assert_eq!(v.pill_action, HintAction::FocusApp);
        assert_eq!(
            keys_labels(&v),
            vec![
                ("Alt-Space".to_string(), "app mode".to_string()),
                ("Ctrl-g".to_string(), "palette".to_string()),
                ("F1 / Alt-h".to_string(), "help".to_string()),
            ]
        );
        assert_eq!(v.hints[0].action, HintAction::FocusApp);
    }

    #[test]
    fn app_mode_leads_with_enter() {
        let v = mode_bar_view(InputMode::App, "Alt-Space", "F1 / Alt-h", None, false, None);
        assert_eq!(v.pill, "MODE: APP");
        assert_eq!(v.pill_action, HintAction::FocusTerminal);
        assert_eq!(
            v.hints[0],
            HintView {
                key: "Enter".to_string(),
                label: "focus terminal".to_string(),
                action: HintAction::FocusTerminal
            }
        );
        assert_eq!(v.hints[1].action, HintAction::OpenPalette);
        assert_eq!(v.hints[2].action, HintAction::OpenHelp);
    }

    #[test]
    fn badges_pass_through() {
        let v = mode_bar_view(InputMode::App, "x", "y", Some("1.2.3"), true, Some("alice"));
        assert_eq!(v.update_available.as_deref(), Some("1.2.3"));
        assert!(v.isolated);
        assert_eq!(v.input_holder.as_deref(), Some("alice"));
        let v = mode_bar_view(InputMode::App, "x", "y", None, false, None);
        assert_eq!(v.update_available, None);
        assert!(!v.isolated);
        assert_eq!(v.input_holder, None);
    }
}
