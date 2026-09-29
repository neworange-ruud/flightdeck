//! Key mapping for both input modes (T8, SPECS §23).
//!
//! [`map_key`] is the single entry point: it takes the current [`InputMode`]
//! and a [`crossterm::event::KeyEvent`] and returns a [`KeyAction`] describing
//! what the wiring layer (T9) should do.
//!
//! This module owns no bindings. It lifts a crossterm event into the
//! front-end-neutral [`Chord`] ([`chord_from_key_event`]), looks it up in the
//! one keymap table ([`crate::app::keymap::Keymap`]) and, for an unbound chord
//! in Terminal mode, encodes it with [`crate::app::keymap::encode_pty`]. Every
//! binding, and the help screen that documents them, lives in that table.
//!
//! T9 integration note:
//! - `KeyAction::Dispatch(cmd)` → call `AppState::dispatch(cmd, &services)`.
//! - `KeyAction::Passthrough(bytes)` → write `bytes` to the active PTY.
//! - `KeyAction::OpenPalette` → open the [`crate::tui::palette::CommandPalette`].
//! - `KeyAction::Quit` → clean teardown (terminate sessions, restore terminal).
//! - `KeyAction::OpenHelp` → show the help overlay.
//! - `KeyAction::FocusApp` → call `AppState::focus_app()` (leave terminal focus).
//! - `KeyAction::FocusTerminal` → call `AppState::focus_terminal()`.
//! - `KeyAction::None` → no-op.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::commands::{Command, Selector};
use crate::app::keymap::{encode_pty, Action, Chord, Key, Keymap, Mods};
use crate::app::modes::InputMode;
#[cfg(test)]
use crate::tui::platform;

/// The result of mapping a key event (SPECS §23).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyAction {
    /// Dispatch the given [`Command`] via `AppState::dispatch`.
    Dispatch(Command),
    /// Switch the active project (workspace-level; handled by the wiring layer,
    /// not `AppState`). `Prev`/`Next` cycle the project tab row.
    SwitchProject(Selector),
    /// Forward these raw bytes to the active PTY (Terminal mode passthrough).
    Passthrough(Vec<u8>),
    /// Paste from the system clipboard into the active terminal. The wiring
    /// layer (T9) reads the clipboard: an image is written to a temp file and
    /// its path sent to the agent; otherwise a literal Ctrl-V passes through.
    Paste,
    /// Open the command palette.
    OpenPalette,
    /// Open the help / keybindings overlay.
    OpenHelp,
    /// Leave terminal-input focus → app-command mode (`AppState::focus_app`).
    FocusApp,
    /// Focus the active terminal → terminal mode (`AppState::focus_terminal`).
    FocusTerminal,
    /// Switch Projects ⇄ Mission control: a desktop-app view, which the TUI
    /// answers by saying where to find it.
    ToggleMissionControl,
    /// Quit FlightDeck (wiring layer cleans up).
    Quit,
    /// No action.
    None,
}

impl From<Action> for KeyAction {
    fn from(action: Action) -> Self {
        match action {
            Action::Dispatch(cmd) => KeyAction::Dispatch(cmd),
            Action::SwitchProject(sel) => KeyAction::SwitchProject(sel),
            Action::Paste => KeyAction::Paste,
            Action::OpenPalette => KeyAction::OpenPalette,
            Action::OpenHelp => KeyAction::OpenHelp,
            Action::FocusApp => KeyAction::FocusApp,
            Action::FocusTerminal => KeyAction::FocusTerminal,
            Action::ToggleMissionControl => KeyAction::ToggleMissionControl,
            Action::Quit => KeyAction::Quit,
        }
    }
}

/// Map a key event to a [`KeyAction`] based on the current input mode (SPECS §23).
///
/// In [`InputMode::Terminal`] most keys produce `Passthrough`; the global
/// shortcuts (`Ctrl-g`, `Ctrl-q`) and the configured leave-terminal-focus key
/// are intercepted first. Bare `Esc` passes through to the PTY.
///
/// In [`InputMode::App`] all keys are interpreted as FlightDeck commands.
pub fn map_key(mode: InputMode, key: KeyEvent) -> KeyAction {
    map_key_with_f2(mode, key, false)
}

/// Map a key event with the optional F2 leave-focus binding enabled or disabled.
pub fn map_key_with_f2(mode: InputMode, key: KeyEvent, use_f2: bool) -> KeyAction {
    map_key_in(Keymap::for_this_platform(use_f2), mode, key)
}

/// Map a key event against an explicit keymap.
fn map_key_in(keymap: &Keymap, mode: InputMode, key: KeyEvent) -> KeyAction {
    let chord = chord_from_key_event(key);
    if let Some(entry) = chord.and_then(|c| keymap.lookup(mode, c)) {
        return entry.action.clone().into();
    }
    match mode {
        // Everything unbound passes through to the PTY — including bare Esc,
        // which hosted agents use for their 2xEsc "abort prompt" gesture. A key
        // with no chord encodes to nothing, as it always has.
        InputMode::Terminal => KeyAction::Passthrough(chord.map(encode_pty).unwrap_or_default()),
        // Unrecognised key in App mode: no-op.
        InputMode::App => KeyAction::None,
    }
}

/// Lift a crossterm key event into the front-end-neutral [`Chord`].
///
/// `None` for keys the keymap and the PTY encoder have no name for (Insert,
/// media keys, bare modifiers, …): those match no binding and send no bytes.
/// Shift+Tab stays [`Key::BackTab`], as crossterm reports it.
pub fn chord_from_key_event(key: KeyEvent) -> Option<Chord> {
    let code = match key.code {
        KeyCode::Char(c) => Key::Char(c),
        KeyCode::Enter => Key::Enter,
        KeyCode::Esc => Key::Esc,
        KeyCode::Tab => Key::Tab,
        KeyCode::BackTab => Key::BackTab,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Delete => Key::Delete,
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        KeyCode::F(n) => Key::F(n),
        _ => return None,
    };
    Some(Chord::new(code, mods_from_crossterm(key.modifiers)))
}

/// crossterm's modifier set as [`Mods`], all six bits.
fn mods_from_crossterm(m: KeyModifiers) -> Mods {
    [
        (KeyModifiers::SHIFT, Mods::SHIFT),
        (KeyModifiers::CONTROL, Mods::CTRL),
        (KeyModifiers::ALT, Mods::ALT),
        (KeyModifiers::SUPER, Mods::SUPER),
        (KeyModifiers::HYPER, Mods::HYPER),
        (KeyModifiers::META, Mods::META),
    ]
    .into_iter()
    .filter(|(ct, _)| m.contains(*ct))
    .fold(Mods::NONE, |acc, (_, ours)| acc | ours)
}

/// Whether a terminal-focused key event is FlightDeck's image-aware paste
/// shortcut. macOS terminals that report Command as `SUPER` get Command-V;
/// all platforms retain Ctrl-V. Answered by the keymap table.
#[cfg(test)]
fn is_paste_shortcut(key: KeyEvent, is_macos: bool) -> bool {
    use crate::app::keymap::KeymapOptions;
    let keymap = Keymap::new(KeymapOptions {
        command_v_pastes: is_macos,
        ..KeymapOptions::for_this_platform(false)
    });
    chord_from_key_event(key)
        .and_then(|c| keymap.lookup(InputMode::Terminal, c))
        .is_some_and(|e| e.action == Action::Paste)
}

// ---------------------------------------------------------------------------
// Key-to-bytes encoding for PTY passthrough (Terminal mode)
// ---------------------------------------------------------------------------

/// Encode a [`KeyEvent`] to the bytes that should be sent to the active PTY.
///
/// A thin adapter over [`crate::app::keymap::encode_pty`], which owns the
/// encoding (arrows always CSI, never SS3) so every front-end sends the same
/// bytes. A key with no [`Chord`] encodes to nothing.
pub fn encode_key(key: KeyEvent) -> Vec<u8> {
    chord_from_key_event(key)
        .map(encode_pty)
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Tests (SPECS §26)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    /// Construct a KeyEvent with no modifiers.
    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::empty(),
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        }
    }

    /// Construct a KeyEvent with Ctrl held.
    fn ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        }
    }

    fn super_key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::SUPER,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    /// Construct a KeyEvent with Alt held.
    fn alt(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::ALT,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        }
    }

    // --- Global shortcuts (both modes) ------------------------------------

    #[test]
    fn ctrl_g_opens_palette_in_app_mode() {
        assert_eq!(
            map_key(InputMode::App, ctrl(KeyCode::Char('g'))),
            KeyAction::OpenPalette
        );
    }

    #[test]
    fn ctrl_g_opens_palette_in_terminal_mode() {
        assert_eq!(
            map_key(InputMode::Terminal, ctrl(KeyCode::Char('g'))),
            KeyAction::OpenPalette
        );
    }

    #[test]
    fn ctrl_q_quits_in_app_mode() {
        assert_eq!(
            map_key(InputMode::App, ctrl(KeyCode::Char('q'))),
            KeyAction::Quit
        );
    }

    #[test]
    fn ctrl_q_quits_in_terminal_mode() {
        assert_eq!(
            map_key(InputMode::Terminal, ctrl(KeyCode::Char('q'))),
            KeyAction::Quit
        );
    }

    #[test]
    fn terminal_mode_alt_up_switches_agent_tab() {
        // Agent-tab navigation must work while a terminal is focused, not just
        // in App mode — otherwise Alt+Up is swallowed by the PTY passthrough.
        assert_eq!(
            map_key(InputMode::Terminal, alt(KeyCode::Up)),
            KeyAction::Dispatch(Command::SwitchAgentTab(Selector::Prev))
        );
    }

    #[test]
    fn terminal_mode_alt_down_switches_agent_tab() {
        assert_eq!(
            map_key(InputMode::Terminal, alt(KeyCode::Down)),
            KeyAction::Dispatch(Command::SwitchAgentTab(Selector::Next))
        );
    }

    #[test]
    fn terminal_mode_alt_left_right_switch_child_terminal() {
        assert_eq!(
            map_key(InputMode::Terminal, alt(KeyCode::Left)),
            KeyAction::Dispatch(Command::SwitchChildTerminal(Selector::Prev))
        );
        assert_eq!(
            map_key(InputMode::Terminal, alt(KeyCode::Right)),
            KeyAction::Dispatch(Command::SwitchChildTerminal(Selector::Next))
        );
    }

    #[test]
    fn shift_left_right_switch_project_in_both_modes() {
        // Project switching is global (works while a terminal is focused too).
        for mode in [InputMode::App, InputMode::Terminal] {
            assert_eq!(
                map_key(mode, shift(KeyCode::Left)),
                KeyAction::SwitchProject(Selector::Prev)
            );
            assert_eq!(
                map_key(mode, shift(KeyCode::Right)),
                KeyAction::SwitchProject(Selector::Next)
            );
        }
    }

    #[test]
    fn terminal_mode_alt_index_jumps_agent_tab() {
        assert_eq!(
            map_key(InputMode::Terminal, alt(KeyCode::Char('2'))),
            KeyAction::Dispatch(Command::SwitchAgentTab(Selector::Index(1)))
        );
    }

    #[test]
    fn terminal_mode_bare_up_passes_through() {
        // Without Alt, arrows still belong to the PTY in Terminal mode.
        assert_eq!(
            map_key(InputMode::Terminal, KeyEvent::from(KeyCode::Up)),
            KeyAction::Passthrough(vec![0x1b, b'[', b'A'])
        );
    }

    // --- App mode shortcuts (SPECS §23) -----------------------------------

    #[test]
    fn app_mode_ctrl_n_new_agent_tab() {
        let action = map_key(InputMode::App, ctrl(KeyCode::Char('n')));
        assert!(
            matches!(action, KeyAction::Dispatch(Command::NewAgentTab { .. })),
            "expected NewAgentTab, got {action:?}"
        );
    }

    #[test]
    fn app_mode_ctrl_p_push_branch() {
        assert_eq!(
            map_key(InputMode::App, ctrl(KeyCode::Char('p'))),
            KeyAction::Dispatch(Command::PushBranch { confirm: None })
        );
    }

    #[test]
    fn app_mode_ctrl_f_finish_merge() {
        assert_eq!(
            map_key(InputMode::App, ctrl(KeyCode::Char('f'))),
            KeyAction::Dispatch(Command::FinishLocalMerge { confirm: false })
        );
    }

    #[test]
    fn app_mode_ctrl_k_close_tab() {
        assert_eq!(
            map_key(InputMode::App, ctrl(KeyCode::Char('k'))),
            KeyAction::Dispatch(Command::CloseAgentTab { action: None })
        );
    }

    #[test]
    fn app_mode_question_mark_is_no_longer_help() {
        // Help used to be App-mode-only on '?'. It is now F1 / Alt-h, global in
        // both modes, so '?' is an ordinary unbound key in App mode.
        assert_eq!(
            map_key(InputMode::App, key(KeyCode::Char('?'))),
            KeyAction::None
        );
    }

    #[test]
    fn f1_opens_help_in_app_mode() {
        assert_eq!(
            map_key(InputMode::App, key(KeyCode::F(1))),
            KeyAction::OpenHelp
        );
    }

    #[test]
    fn f1_opens_help_in_terminal_mode() {
        // F1 is global: a focused terminal must not swallow it as passthrough,
        // otherwise help is unreachable exactly when a user reaches for it.
        assert_eq!(
            map_key(InputMode::Terminal, key(KeyCode::F(1))),
            KeyAction::OpenHelp
        );
    }

    #[test]
    fn modified_f1_still_passes_through() {
        // Only bare F1 is claimed; Ctrl/Alt/Shift-F1 stay the PTY's.
        assert_eq!(
            map_key(InputMode::Terminal, ctrl(KeyCode::F(1))),
            KeyAction::Passthrough(encode_key(ctrl(KeyCode::F(1))))
        );
    }

    #[test]
    fn f3_still_passes_through() {
        assert_eq!(
            map_key(InputMode::Terminal, key(KeyCode::F(3))),
            KeyAction::Passthrough(encode_key(key(KeyCode::F(3))))
        );
    }

    #[test]
    fn alt_h_opens_help_in_both_modes() {
        // The reachable-everywhere companion to F1: macOS reserves F1 as a
        // media key on Apple keyboards, so a non-function-key route matters.
        assert_eq!(
            map_key(InputMode::App, alt(KeyCode::Char('h'))),
            KeyAction::OpenHelp
        );
        assert_eq!(
            map_key(InputMode::Terminal, alt(KeyCode::Char('h'))),
            KeyAction::OpenHelp
        );
    }

    #[test]
    fn bare_h_still_passes_through_to_the_pty() {
        // Only the Alt-modified 'h' is claimed; typing 'h' must reach the agent.
        assert_eq!(
            map_key(InputMode::Terminal, key(KeyCode::Char('h'))),
            KeyAction::Passthrough(encode_key(key(KeyCode::Char('h'))))
        );
    }

    #[test]
    fn ctrl_alt_h_is_not_the_help_key() {
        assert_ne!(
            map_key(InputMode::Terminal, ctrl(KeyCode::Char('h'))),
            KeyAction::OpenHelp
        );
    }

    #[test]
    fn app_mode_alt_up_prev_tab() {
        assert_eq!(
            map_key(InputMode::App, alt(KeyCode::Up)),
            KeyAction::Dispatch(Command::SwitchAgentTab(Selector::Prev))
        );
    }

    #[test]
    fn app_mode_alt_down_next_tab() {
        assert_eq!(
            map_key(InputMode::App, alt(KeyCode::Down)),
            KeyAction::Dispatch(Command::SwitchAgentTab(Selector::Next))
        );
    }

    #[test]
    fn app_mode_plain_up_down_switch_agent_tab() {
        // Bare arrows work in App mode (terminals may swallow Alt+Up/Down).
        assert_eq!(
            map_key(InputMode::App, key(KeyCode::Up)),
            KeyAction::Dispatch(Command::SwitchAgentTab(Selector::Prev))
        );
        assert_eq!(
            map_key(InputMode::App, key(KeyCode::Down)),
            KeyAction::Dispatch(Command::SwitchAgentTab(Selector::Next))
        );
    }

    #[test]
    fn app_mode_plain_left_right_switch_terminal() {
        assert_eq!(
            map_key(InputMode::App, key(KeyCode::Left)),
            KeyAction::Dispatch(Command::SwitchChildTerminal(Selector::Prev))
        );
        assert_eq!(
            map_key(InputMode::App, key(KeyCode::Right)),
            KeyAction::Dispatch(Command::SwitchChildTerminal(Selector::Next))
        );
    }

    #[test]
    fn app_mode_alt_left_prev_child() {
        assert_eq!(
            map_key(InputMode::App, alt(KeyCode::Left)),
            KeyAction::Dispatch(Command::SwitchChildTerminal(Selector::Prev))
        );
    }

    #[test]
    fn app_mode_alt_right_next_child() {
        assert_eq!(
            map_key(InputMode::App, alt(KeyCode::Right)),
            KeyAction::Dispatch(Command::SwitchChildTerminal(Selector::Next))
        );
    }

    #[test]
    fn app_mode_alt_1_jump_to_index_0() {
        assert_eq!(
            map_key(InputMode::App, alt(KeyCode::Char('1'))),
            KeyAction::Dispatch(Command::SwitchAgentTab(Selector::Index(0)))
        );
    }

    #[test]
    fn app_mode_alt_9_jump_to_index_8() {
        assert_eq!(
            map_key(InputMode::App, alt(KeyCode::Char('9'))),
            KeyAction::Dispatch(Command::SwitchAgentTab(Selector::Index(8)))
        );
    }

    #[test]
    fn app_mode_ctrl_t_new_child_terminal() {
        assert_eq!(
            map_key(InputMode::App, ctrl(KeyCode::Char('t'))),
            KeyAction::Dispatch(Command::NewChildTerminal)
        );
    }

    #[test]
    fn app_mode_ctrl_w_close_child_terminal() {
        assert_eq!(
            map_key(InputMode::App, ctrl(KeyCode::Char('w'))),
            KeyAction::Dispatch(Command::CloseChildTerminal)
        );
    }

    #[test]
    fn app_mode_ctrl_tab_is_unbound() {
        // Child-terminal switching moved to Alt-Left/Right; Ctrl-Tab is unbound.
        assert_eq!(map_key(InputMode::App, ctrl(KeyCode::Tab)), KeyAction::None);
    }

    #[test]
    fn app_mode_ctrl_s_set_manual_status() {
        assert_eq!(
            map_key(InputMode::App, ctrl(KeyCode::Char('s'))),
            KeyAction::Dispatch(Command::SetManualStatus(None))
        );
    }

    #[test]
    fn app_mode_ctrl_r_restart_agent() {
        assert_eq!(
            map_key(InputMode::App, ctrl(KeyCode::Char('r'))),
            KeyAction::Dispatch(Command::RestartAgent)
        );
    }

    #[test]
    fn app_mode_ctrl_b_toggles_split_view() {
        assert_eq!(
            map_key(InputMode::App, ctrl(KeyCode::Char('b'))),
            KeyAction::Dispatch(Command::ToggleSplitView)
        );
    }

    #[test]
    fn app_mode_unrecognised_key_is_none() {
        assert_eq!(
            map_key(InputMode::App, key(KeyCode::Char('x'))),
            KeyAction::None
        );
    }

    // --- Terminal mode passthrough ----------------------------------------

    #[test]
    fn terminal_mode_f2_is_opt_in() {
        assert_eq!(
            map_key(InputMode::Terminal, key(KeyCode::F(2))),
            KeyAction::Passthrough(encode_key(key(KeyCode::F(2))))
        );
        assert_eq!(
            map_key_with_f2(InputMode::Terminal, key(KeyCode::F(2)), true),
            KeyAction::FocusApp
        );
    }

    #[test]
    fn terminal_mode_alt_esc_focus_depends_on_platform() {
        let action = map_key(InputMode::Terminal, alt(KeyCode::Esc));
        if platform::LEAVE_FOCUS_USES_SHIFT {
            assert_eq!(
                action,
                KeyAction::Passthrough(encode_key(alt(KeyCode::Esc)))
            );
        } else {
            assert_eq!(action, KeyAction::FocusApp);
        }
    }

    /// Construct a KeyEvent with Shift held.
    fn shift(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::SHIFT,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        }
    }

    #[test]
    fn terminal_mode_shift_esc_focus_depends_on_platform() {
        let action = map_key(InputMode::Terminal, shift(KeyCode::Esc));
        if platform::LEAVE_FOCUS_USES_SHIFT {
            assert_eq!(action, KeyAction::FocusApp);
        } else {
            assert_eq!(
                action,
                KeyAction::Passthrough(encode_key(shift(KeyCode::Esc)))
            );
        }
    }

    #[test]
    fn terminal_mode_modified_esc_passes_through_when_f2_is_enabled() {
        let leave_key = if platform::LEAVE_FOCUS_USES_SHIFT {
            shift(KeyCode::Esc)
        } else {
            alt(KeyCode::Esc)
        };
        assert_eq!(
            map_key_with_f2(InputMode::Terminal, leave_key, true),
            KeyAction::Passthrough(encode_key(leave_key))
        );
    }

    #[test]
    fn terminal_mode_bare_esc_passes_through() {
        // Bare Esc must reach the PTY so hosted agents can use it (e.g. Claude
        // Code / OpenCode 2×Esc abort).
        assert_eq!(
            map_key(InputMode::Terminal, key(KeyCode::Esc)),
            KeyAction::Passthrough(vec![0x1b])
        );
    }

    #[test]
    fn app_mode_enter_focuses_terminal() {
        // Enter focuses the active terminal (SPECS §23).
        assert_eq!(
            map_key(InputMode::App, key(KeyCode::Enter)),
            KeyAction::FocusTerminal
        );
    }

    #[test]
    fn alt_o_opens_the_file_manager_in_both_modes() {
        // Global binding: the common case is hitting it while the agent
        // terminal has focus, so it must not be App-mode only.
        assert_eq!(
            map_key(InputMode::App, alt(KeyCode::Char('o'))),
            KeyAction::Dispatch(Command::OpenWorktreeInFileManager)
        );
        assert_eq!(
            map_key(InputMode::Terminal, alt(KeyCode::Char('o'))),
            KeyAction::Dispatch(Command::OpenWorktreeInFileManager)
        );
    }

    #[test]
    fn plain_o_still_passes_through_to_the_terminal() {
        assert_eq!(
            map_key(InputMode::Terminal, key(KeyCode::Char('o'))),
            KeyAction::Passthrough(vec![b'o'])
        );
    }

    #[test]
    fn terminal_mode_regular_char_passes_through() {
        let action = map_key(InputMode::Terminal, key(KeyCode::Char('a')));
        assert_eq!(action, KeyAction::Passthrough(vec![b'a']));
    }

    #[test]
    fn terminal_mode_enter_passes_cr() {
        let action = map_key(InputMode::Terminal, key(KeyCode::Enter));
        assert_eq!(action, KeyAction::Passthrough(vec![b'\r']));
    }

    #[test]
    fn terminal_mode_ctrl_a_passes_0x01() {
        let action = map_key(InputMode::Terminal, ctrl(KeyCode::Char('a')));
        assert_eq!(action, KeyAction::Passthrough(vec![0x01]));
    }

    #[test]
    fn terminal_mode_ctrl_v_maps_to_paste() {
        // Ctrl-V is intercepted as a paste so the wiring layer can turn a
        // clipboard image into a file-path reference for the agent.
        assert_eq!(
            map_key(InputMode::Terminal, ctrl(KeyCode::Char('v'))),
            KeyAction::Paste
        );
    }

    #[test]
    fn command_v_is_paste_on_macos_when_the_terminal_reports_it() {
        assert!(is_paste_shortcut(super_key(KeyCode::Char('v')), true));
        assert!(!is_paste_shortcut(super_key(KeyCode::Char('v')), false));
    }

    #[test]
    fn app_mode_ctrl_v_is_unbound() {
        // Paste only applies while a terminal is focused (the agent chat).
        assert_eq!(
            map_key(InputMode::App, ctrl(KeyCode::Char('v'))),
            KeyAction::None
        );
    }

    #[test]
    fn terminal_mode_ctrl_c_passes_through() {
        // Ctrl-C in terminal mode is 0x03 (ETX), passed to PTY — the PTY
        // decides whether to forward SIGINT. It is NOT mapped to CloseAgentTab.
        let action = map_key(InputMode::Terminal, ctrl(KeyCode::Char('c')));
        assert_eq!(action, KeyAction::Passthrough(vec![0x03]));
    }

    #[test]
    fn terminal_mode_arrow_up_passes_escape_sequence() {
        let action = map_key(InputMode::Terminal, key(KeyCode::Up));
        assert_eq!(action, KeyAction::Passthrough(vec![0x1b, b'[', b'A']));
    }

    #[test]
    fn encode_key_backspace() {
        assert_eq!(encode_key(key(KeyCode::Backspace)), vec![0x7f]);
    }

    #[test]
    fn encode_key_tab() {
        assert_eq!(encode_key(key(KeyCode::Tab)), vec![b'\t']);
    }

    #[test]
    fn encode_key_shift_tab_backtab() {
        // crossterm delivers Shift+Tab as `KeyCode::BackTab` (with SHIFT set),
        // never as `KeyCode::Tab` + SHIFT — this is the event it actually emits.
        let k = KeyEvent {
            code: KeyCode::BackTab,
            modifiers: KeyModifiers::SHIFT,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        };
        assert_eq!(encode_key(k), vec![0x1b, b'[', b'Z']);
    }

    #[test]
    fn encode_key_f1() {
        let k = KeyEvent {
            code: KeyCode::F(1),
            modifiers: KeyModifiers::empty(),
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        };
        assert_eq!(encode_key(k), vec![0x1b, b'O', b'P']);
    }
}

/// Behaviour-preservation proof for the keymap refactor (remote-control-bmej.2.2).
///
/// `legacy_*` below is the pre-refactor mapper and encoder, verbatim apart from
/// the `legacy_` prefix. The tests drive both over every key code the old code
/// named (and more) under every modifier combination, in both modes and both
/// leave-focus settings, and require identical answers.
#[cfg(test)]
mod legacy_equivalence_tests {
    use super::*;
    use crate::app::keymap::KeymapOptions;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn legacy_map_key_with_f2(mode: InputMode, key: KeyEvent, use_f2: bool) -> KeyAction {
        match mode {
            InputMode::Terminal => legacy_map_terminal_mode(key, use_f2),
            InputMode::App => legacy_map_app_mode(key),
        }
    }

    // ---------------------------------------------------------------------------
    // Terminal Focus mode (SPECS §23)
    // ---------------------------------------------------------------------------

    fn legacy_map_terminal_mode(key: KeyEvent, use_f2: bool) -> KeyAction {
        // Global intercepts work in both modes.
        if let Some(global) = legacy_map_global(key) {
            return global;
        }
        // Leave terminal focus (SPECS §23). Bare Esc must still reach the PTY for
        // hosted-agent gestures, vim/readline cancel, fzf dismiss, etc. The default
        // is Alt+Esc on macOS and Shift+Esc on Windows/Linux; users whose terminal
        // cannot distinguish modified Esc can opt into the unambiguous F2 binding.
        let modified_esc = key.code == KeyCode::Esc
            && key.modifiers
                == if platform::LEAVE_FOCUS_USES_SHIFT {
                    KeyModifiers::SHIFT
                } else {
                    KeyModifiers::ALT
                };
        if (use_f2 && key.code == KeyCode::F(2)) || (!use_f2 && modified_esc) {
            return KeyAction::FocusApp;
        }

        // Bare Esc (and double-Esc) must pass through to the PTY so hosted agents
        // like Claude Code / OpenCode can use their 2xEsc "abort prompt" gesture.
        // Ctrl-V / Cmd-V on macOS: paste. The wiring layer gives local Codex CLI
        // the literal key so it can read its native clipboard image; other agents,
        // and containerized Codex, receive a temporary file path instead. With no
        // image on the clipboard every agent falls back to Ctrl-V passthrough.
        if legacy_is_paste_shortcut(key, platform::IS_MACOS) {
            return KeyAction::Paste;
        }
        // Everything else passes through to the PTY.
        KeyAction::Passthrough(legacy_encode_key(key))
    }

    /// Whether a terminal-focused key event is FlightDeck's image-aware paste
    /// shortcut. macOS terminals that report Command as `SUPER` get Command-V;
    /// all platforms retain Ctrl-V.
    fn legacy_is_paste_shortcut(key: KeyEvent, is_macos: bool) -> bool {
        if key.code != KeyCode::Char('v') || key.modifiers.contains(KeyModifiers::ALT) {
            return false;
        }
        key.modifiers.contains(KeyModifiers::CONTROL)
            || (is_macos
                && key.modifiers.contains(KeyModifiers::SUPER)
                && !key.modifiers.contains(KeyModifiers::CONTROL))
    }

    // ---------------------------------------------------------------------------
    // App Command mode (SPECS §23)
    // ---------------------------------------------------------------------------

    fn legacy_map_app_mode(key: KeyEvent) -> KeyAction {
        // Global intercepts.
        if let Some(global) = legacy_map_global(key) {
            return global;
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let no_mod = key.modifiers.is_empty();

        match key.code {
            // --- Focus -------------------------------------------------------
            // Enter: focus terminal (SPECS §23).
            KeyCode::Enter if no_mod => KeyAction::FocusTerminal,

            // --- Global shortcuts (App mode, non-global) ---------------------
            // Ctrl-n: New Agent Tab.
            KeyCode::Char('n') if ctrl => KeyAction::Dispatch(Command::NewAgentTab {
                name: String::new(), // T9 must prompt for name
                agent_key: None,
            }),
            // Ctrl-p: Push Branch.
            KeyCode::Char('p') if ctrl => {
                KeyAction::Dispatch(Command::PushBranch { confirm: None })
            }
            // Ctrl-f: Finish / Local Merge.
            KeyCode::Char('f') if ctrl => {
                KeyAction::Dispatch(Command::FinishLocalMerge { confirm: false })
            }
            // Ctrl-u: Pull base (git pull --rebase on the base folder).
            KeyCode::Char('u') if ctrl => KeyAction::Dispatch(Command::PullBase),
            // Ctrl-k: Close Agent Tab.
            KeyCode::Char('k') if ctrl => {
                KeyAction::Dispatch(Command::CloseAgentTab { action: None })
            }

            // --- Agent Tab Navigation (SPECS §23) ----------------------------
            // Bare Up/Down: previous / next Agent Tab. The Alt-modified variants are
            // handled in `legacy_map_global` so they also work in Terminal mode; the bare
            // arrows are an App-mode-only fallback because some terminals (e.g. Warp)
            // capture Option/Alt+Up/Down themselves, and in App mode the bare arrows
            // are otherwise unused.
            KeyCode::Up if no_mod => KeyAction::Dispatch(Command::SwitchAgentTab(Selector::Prev)),
            // Down: next Agent Tab.
            KeyCode::Down if no_mod => KeyAction::Dispatch(Command::SwitchAgentTab(Selector::Next)),

            // --- Child Terminal Navigation (SPECS §23) -----------------------
            // Ctrl-t: New child terminal.
            KeyCode::Char('t') if ctrl => KeyAction::Dispatch(Command::NewChildTerminal),
            // Ctrl-w: Close active child terminal.
            KeyCode::Char('w') if ctrl => KeyAction::Dispatch(Command::CloseChildTerminal),
            // Bare Left/Right: previous / next terminal tab (cycles agent + shells).
            // Alt-Left/Right are handled in `legacy_map_global` for Terminal mode.
            KeyCode::Left if no_mod => {
                KeyAction::Dispatch(Command::SwitchChildTerminal(Selector::Prev))
            }
            // Right: next terminal tab (cycles agent + shells).
            KeyCode::Right if no_mod => {
                KeyAction::Dispatch(Command::SwitchChildTerminal(Selector::Next))
            }

            // --- Status (SPECS §23) ------------------------------------------
            // Ctrl-s: Set manual status.
            KeyCode::Char('s') if ctrl => {
                KeyAction::Dispatch(Command::SetManualStatus(None)) // T9 prompts
            }
            // Ctrl-r: Restart primary agent.
            KeyCode::Char('r') if ctrl => KeyAction::Dispatch(Command::RestartAgent),

            // --- View (split layout) -----------------------------------------
            // Ctrl-b: Toggle split view (terminals side by side vs. tabs).
            KeyCode::Char('b') if ctrl => KeyAction::Dispatch(Command::ToggleSplitView),

            // Unrecognised key in App mode: no-op.
            _ => KeyAction::None,
        }
    }

    // ---------------------------------------------------------------------------
    // Global shortcuts active in BOTH modes (SPECS §23)
    // ---------------------------------------------------------------------------

    fn legacy_map_global(key: KeyEvent) -> Option<KeyAction> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);

        match key.code {
            // Ctrl-g: Command palette (both modes).
            KeyCode::Char('g') if ctrl => Some(KeyAction::OpenPalette),
            // Ctrl-q: Quit.
            KeyCode::Char('q') if ctrl => Some(KeyAction::Quit),
            // F1 / Alt-h: Help / keybindings (both modes). Global so help is
            // reachable with a terminal focused, which is when a user actually
            // reaches for it; the cost is that hosted agents never see bare F1 or
            // Alt-h. Modified F1 and bare 'h' are left to the PTY.
            //
            // Alt-h exists because Apple keyboards reserve F1 as a media key
            // (brightness) unless the user enables standard function keys, so on a
            // Mac laptop F1 never reaches the terminal. Alt-h carries its own macOS
            // caveat — Option+letter composes a special character unless "Use
            // Option as Meta key" is on — but that is the same requirement Alt-o
            // and Alt-1..9 already impose, so it adds no new configuration burden.
            //
            // Pressing the same key again while the overlay is open opens the
            // FlightDeck repository; that lives in the overlay key handling
            // (`handle_key`), not here, since this map has no view of the overlay.
            KeyCode::F(1) if key.modifiers.is_empty() => Some(KeyAction::OpenHelp),
            KeyCode::Char('h') if alt && !ctrl && !shift => Some(KeyAction::OpenHelp),

            // --- Project navigation (multi-project) --------------------------
            // Shift-Left / Shift-Right cycle the open projects. Global so they work
            // while a terminal is focused too; distinct from the Alt/plain arrows
            // that switch agent tabs and child terminals. (`alt` takes precedence
            // when both are held, since those arms are matched first below.)
            KeyCode::Left if shift && !alt && !ctrl => {
                Some(KeyAction::SwitchProject(Selector::Prev))
            }
            KeyCode::Right if shift && !alt && !ctrl => {
                Some(KeyAction::SwitchProject(Selector::Next))
            }

            // --- Agent + child-terminal navigation (SPECS §23) ---------------
            // Alt-based navigation is global so it works while a terminal is
            // focused (Terminal mode) as well as in App mode; otherwise these keys
            // would be swallowed by the PTY passthrough and tabs would never switch.
            // Alt-Up: previous Agent Tab.
            KeyCode::Up if alt => {
                Some(KeyAction::Dispatch(Command::SwitchAgentTab(Selector::Prev)))
            }
            // Alt-Down: next Agent Tab.
            KeyCode::Down if alt => {
                Some(KeyAction::Dispatch(Command::SwitchAgentTab(Selector::Next)))
            }
            // Alt-Left: previous terminal tab (cycles agent + shells).
            KeyCode::Left if alt => Some(KeyAction::Dispatch(Command::SwitchChildTerminal(
                Selector::Prev,
            ))),
            // Alt-Right: next terminal tab (cycles agent + shells).
            KeyCode::Right if alt => Some(KeyAction::Dispatch(Command::SwitchChildTerminal(
                Selector::Next,
            ))),
            // Alt-1..Alt-9: jump to Agent Tab by index.
            KeyCode::Char(c @ '1'..='9') if alt => {
                let idx = (c as usize) - ('1' as usize);
                Some(KeyAction::Dispatch(Command::SwitchAgentTab(
                    Selector::Index(idx),
                )))
            }
            // Alt-o: open the selected worktree in the OS file manager. Global so it
            // works with a terminal focused (the common case). Alt-O is not a
            // standard readline/agent binding, so the PTY loses nothing.
            KeyCode::Char('o') if alt && !ctrl && !shift => {
                Some(KeyAction::Dispatch(Command::OpenWorktreeInFileManager))
            }
            _ => None,
        }
    }

    // ---------------------------------------------------------------------------
    // Key-to-bytes encoding for PTY passthrough (Terminal mode)
    // ---------------------------------------------------------------------------

    /// Encode a [`KeyEvent`] to the bytes that should be sent to the active PTY.
    ///
    /// This is a best-effort encoding of common keys to their VT100/ANSI byte
    /// sequences. The wiring layer (T9) should augment this with the full
    /// encoding table it uses for the portable-pty backend.
    fn legacy_encode_key(key: KeyEvent) -> Vec<u8> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);

        match key.code {
            KeyCode::Char(c) => {
                let mut bytes = Vec::new();
                if alt {
                    bytes.push(0x1b); // ESC prefix for Alt
                }
                if ctrl {
                    // Ctrl+letter → 0x01..0x1a
                    let b = c.to_ascii_uppercase() as u8;
                    if b.is_ascii_uppercase() {
                        bytes.push(b - b'A' + 1);
                    } else {
                        bytes.extend_from_slice(c.encode_utf8(&mut [0u8; 4]).as_bytes());
                    }
                } else {
                    bytes.extend_from_slice(c.encode_utf8(&mut [0u8; 4]).as_bytes());
                }
                bytes
            }
            KeyCode::Enter => vec![b'\r'],
            KeyCode::Backspace => vec![0x7f],
            KeyCode::Delete => vec![0x1b, b'[', b'3', b'~'],
            KeyCode::Tab => vec![b'\t'],
            // crossterm reports Shift+Tab as the dedicated `BackTab` variant (with
            // SHIFT set), never as `Tab` + SHIFT, on Unix and Windows alike.
            KeyCode::BackTab => vec![0x1b, b'[', b'Z'],
            KeyCode::Esc => vec![0x1b],
            KeyCode::Up => vec![0x1b, b'[', b'A'],
            KeyCode::Down => vec![0x1b, b'[', b'B'],
            KeyCode::Right => vec![0x1b, b'[', b'C'],
            KeyCode::Left => vec![0x1b, b'[', b'D'],
            KeyCode::Home => vec![0x1b, b'[', b'H'],
            KeyCode::End => vec![0x1b, b'[', b'F'],
            KeyCode::PageUp => vec![0x1b, b'[', b'5', b'~'],
            KeyCode::PageDown => vec![0x1b, b'[', b'6', b'~'],
            KeyCode::F(n) => {
                // F1-F4 use SS3; F5+ use CSI ~ sequences.
                match n {
                    1 => vec![0x1b, b'O', b'P'],
                    2 => vec![0x1b, b'O', b'Q'],
                    3 => vec![0x1b, b'O', b'R'],
                    4 => vec![0x1b, b'O', b'S'],
                    5 => vec![0x1b, b'[', b'1', b'5', b'~'],
                    6 => vec![0x1b, b'[', b'1', b'7', b'~'],
                    7 => vec![0x1b, b'[', b'1', b'8', b'~'],
                    8 => vec![0x1b, b'[', b'1', b'9', b'~'],
                    9 => vec![0x1b, b'[', b'2', b'0', b'~'],
                    10 => vec![0x1b, b'[', b'2', b'1', b'~'],
                    11 => vec![0x1b, b'[', b'2', b'3', b'~'],
                    12 => vec![0x1b, b'[', b'2', b'4', b'~'],
                    _ => vec![],
                }
            }
            _ => vec![],
        }
    }

    fn codes() -> Vec<KeyCode> {
        let mut codes = vec![
            KeyCode::Enter,
            KeyCode::Esc,
            KeyCode::Tab,
            KeyCode::BackTab,
            KeyCode::Backspace,
            KeyCode::Delete,
            KeyCode::Insert,
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Home,
            KeyCode::End,
            KeyCode::PageUp,
            KeyCode::PageDown,
            KeyCode::Null,
            KeyCode::CapsLock,
        ];
        codes.extend((0..=30).map(KeyCode::F));
        codes.extend((0u8..0x80).map(|b| KeyCode::Char(char::from(b))));
        // Non-ASCII, including one whose low byte is an ASCII capital ('Ł' is
        // U+0141): the old Ctrl encoding truncates with `as u8`, and must
        // still do so.
        codes.extend(['é', 'Ł', 'ß', '中', '😀'].map(KeyCode::Char));
        codes
    }

    fn all_modifiers() -> impl Iterator<Item = KeyModifiers> {
        (0u8..64).map(KeyModifiers::from_bits_truncate)
    }

    #[test]
    fn map_key_matches_the_pre_refactor_mapper_exhaustively() {
        for use_f2 in [false, true] {
            for mode in [InputMode::Terminal, InputMode::App] {
                for code in codes() {
                    for mods in all_modifiers() {
                        let key = KeyEvent::new(code, mods);
                        // The one binding added since the refactor: Alt-m,
                        // App mode only, exact. The pre-refactor mapper had
                        // nothing there.
                        if mode == InputMode::App
                            && code == KeyCode::Char('m')
                            && mods == KeyModifiers::ALT
                        {
                            assert_eq!(
                                map_key_with_f2(mode, key, use_f2),
                                KeyAction::ToggleMissionControl
                            );
                            assert_eq!(legacy_map_key_with_f2(mode, key, use_f2), KeyAction::None);
                            continue;
                        }
                        assert_eq!(
                            map_key_with_f2(mode, key, use_f2),
                            legacy_map_key_with_f2(mode, key, use_f2),
                            "{mode:?} use_f2={use_f2} {key:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn encode_key_matches_the_pre_refactor_encoder_exhaustively() {
        for code in codes() {
            for mods in all_modifiers() {
                let key = KeyEvent::new(code, mods);
                assert_eq!(encode_key(key), legacy_encode_key(key), "{key:?}");
            }
        }
    }

    /// The other platforms' tables too, not only this host's: the macOS
    /// Command-V paste binding, checked from any OS.
    #[test]
    fn paste_matches_the_pre_refactor_predicate_on_every_platform() {
        for is_macos in [false, true] {
            let keymap = Keymap::new(KeymapOptions {
                command_v_pastes: is_macos,
                ..KeymapOptions::for_this_platform(false)
            });
            for code in codes() {
                for mods in all_modifiers() {
                    let key = KeyEvent::new(code, mods);
                    let ours = chord_from_key_event(key)
                        .and_then(|c| keymap.lookup(InputMode::Terminal, c))
                        .is_some_and(|e| e.action == Action::Paste);
                    let old =
                        legacy_map_global(key).is_none() && legacy_is_paste_shortcut(key, is_macos);
                    assert_eq!(ours, old, "macos={is_macos} {key:?}");
                }
            }
        }
    }

    /// Leave-focus on the other platform family, checked from any OS: the
    /// legacy code read the compile-time constant, so compare against a
    /// restatement of its rule for the opposite value.
    #[test]
    fn leave_focus_matches_the_pre_refactor_rule_for_both_platform_families() {
        for uses_shift in [false, true] {
            let keymap = Keymap::new(KeymapOptions {
                leave_focus_uses_shift: uses_shift,
                ..KeymapOptions::for_this_platform(false)
            });
            for mods in all_modifiers() {
                let key = KeyEvent::new(KeyCode::Esc, mods);
                let want = mods
                    == if uses_shift {
                        KeyModifiers::SHIFT
                    } else {
                        KeyModifiers::ALT
                    };
                let got = chord_from_key_event(key)
                    .and_then(|c| keymap.lookup(InputMode::Terminal, c))
                    .is_some_and(|e| e.action == Action::FocusApp);
                assert_eq!(got, want, "shift={uses_shift} {mods:?}");
            }
        }
    }
}
