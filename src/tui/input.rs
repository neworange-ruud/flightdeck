//! Key mapping for both input modes (T8, SPECS §23).
//!
//! [`map_key`] is the single entry point: it takes the current [`InputMode`]
//! and a [`crossterm::event::KeyEvent`] and returns a [`KeyAction`] describing
//! what the wiring layer (T9) should do.
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
use crate::app::modes::InputMode;
use crate::tui::platform;

/// The result of mapping a key event (SPECS §23).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyAction {
    /// Dispatch the given [`Command`] via `AppState::dispatch`.
    Dispatch(Command),
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
    /// Quit FlightDeck (wiring layer cleans up).
    Quit,
    /// No action.
    None,
}

/// Map a key event to a [`KeyAction`] based on the current input mode (SPECS §23).
///
/// In [`InputMode::Terminal`] most keys produce `Passthrough`; the global
/// shortcuts (`Ctrl-g`, `Ctrl-q`) and the leave-terminal-focus key (`Alt+Esc`,
/// or `Shift+Esc` on Windows/Linux) are intercepted first. Bare `Esc` passes
/// through to the PTY.
///
/// In [`InputMode::App`] all keys are interpreted as FlightDeck commands.
pub fn map_key(mode: InputMode, key: KeyEvent) -> KeyAction {
    match mode {
        InputMode::Terminal => map_terminal_mode(key),
        InputMode::App => map_app_mode(key),
    }
}

// ---------------------------------------------------------------------------
// Terminal Focus mode (SPECS §23)
// ---------------------------------------------------------------------------

fn map_terminal_mode(key: KeyEvent) -> KeyAction {
    // Global intercepts work in both modes.
    if let Some(global) = map_global(key) {
        return global;
    }
    // Alt+Esc: leave terminal focus (SPECS §23 "Leave terminal input focus").
    // Bare Esc (and double-Esc) must pass through to the PTY so hosted agents
    // like Claude Code / OpenCode can use it — e.g. their 2×Esc "abort prompt"
    // gesture, vim/readline cancel, fzf dismiss, etc. We therefore require Alt
    // to leave terminal focus rather than swallowing every Esc.
    if key.code == KeyCode::Esc && key.modifiers == KeyModifiers::ALT {
        return KeyAction::FocusApp;
    }
    // Windows and Linux: Alt+Esc is a reserved shortcut that cycles windows —
    // the OS/window manager (e.g. GNOME) grabs it before FlightDeck ever sees
    // it. Offer Shift+Esc as the leave-terminal-focus key on those platforms
    // instead. Bare Esc (and 2×Esc) still pass through to hosted agents.
    if platform::LEAVE_FOCUS_USES_SHIFT
        && key.code == KeyCode::Esc
        && key.modifiers == KeyModifiers::SHIFT
    {
        return KeyAction::FocusApp;
    }
    // Ctrl-V: paste. Hosted agents (e.g. Claude Code) accept images via Ctrl-V
    // but cannot see the host clipboard's image flavour through the PTY, so the
    // wiring layer reads it and sends a temp-file path instead. With no image on
    // the clipboard it falls back to a literal Ctrl-V (0x16) passthrough.
    if key.code == KeyCode::Char('v')
        && key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::ALT)
    {
        return KeyAction::Paste;
    }
    // Everything else passes through to the PTY.
    KeyAction::Passthrough(encode_key(key))
}

// ---------------------------------------------------------------------------
// App Command mode (SPECS §23)
// ---------------------------------------------------------------------------

fn map_app_mode(key: KeyEvent) -> KeyAction {
    // Global intercepts.
    if let Some(global) = map_global(key) {
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
        KeyCode::Char('p') if ctrl => KeyAction::Dispatch(Command::PushBranch { confirm: None }),
        // Ctrl-f: Finish / Local Merge.
        KeyCode::Char('f') if ctrl => {
            KeyAction::Dispatch(Command::FinishLocalMerge { confirm: false })
        }
        // Ctrl-u: Pull base (git pull --rebase on the base folder).
        KeyCode::Char('u') if ctrl => KeyAction::Dispatch(Command::PullBase),
        // Ctrl-k: Close Agent Tab.
        KeyCode::Char('k') if ctrl => KeyAction::Dispatch(Command::CloseAgentTab { action: None }),
        // ?: Help / keybindings.
        KeyCode::Char('?') if no_mod => KeyAction::OpenHelp,

        // --- Agent Tab Navigation (SPECS §23) ----------------------------
        // Bare Up/Down: previous / next Agent Tab. The Alt-modified variants are
        // handled in `map_global` so they also work in Terminal mode; the bare
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
        // Alt-Left/Right are handled in `map_global` for Terminal mode.
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

fn map_global(key: KeyEvent) -> Option<KeyAction> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);

    match key.code {
        // Ctrl-g: Command palette (both modes).
        KeyCode::Char('g') if ctrl => Some(KeyAction::OpenPalette),
        // Ctrl-q: Quit.
        KeyCode::Char('q') if ctrl => Some(KeyAction::Quit),

        // --- Agent + child-terminal navigation (SPECS §23) ---------------
        // Alt-based navigation is global so it works while a terminal is
        // focused (Terminal mode) as well as in App mode; otherwise these keys
        // would be swallowed by the PTY passthrough and tabs would never switch.
        // Alt-Up: previous Agent Tab.
        KeyCode::Up if alt => Some(KeyAction::Dispatch(Command::SwitchAgentTab(Selector::Prev))),
        // Alt-Down: next Agent Tab.
        KeyCode::Down if alt => Some(KeyAction::Dispatch(Command::SwitchAgentTab(Selector::Next))),
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
pub fn encode_key(key: KeyEvent) -> Vec<u8> {
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
        KeyCode::Tab => {
            if key.modifiers.contains(KeyModifiers::SHIFT) {
                vec![0x1b, b'[', b'Z']
            } else {
                vec![b'\t']
            }
        }
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
    fn app_mode_question_mark_help() {
        assert_eq!(
            map_key(InputMode::App, key(KeyCode::Char('?'))),
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
    fn terminal_mode_alt_esc_focuses_app() {
        // Alt+Esc leaves terminal focus → app-command mode (SPECS §23).
        assert_eq!(
            map_key(InputMode::Terminal, alt(KeyCode::Esc)),
            KeyAction::FocusApp
        );
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
            // Windows and Linux reserve Alt+Esc (cycles windows), so Shift+Esc
            // is the leave-terminal-focus key there.
            assert_eq!(action, KeyAction::FocusApp);
        } else {
            // On macOS, Shift+Esc is not a focus key — it stays a PTY passthrough.
            assert_eq!(
                action,
                KeyAction::Passthrough(encode_key(shift(KeyCode::Esc)))
            );
        }
    }

    #[test]
    fn terminal_mode_bare_esc_passes_through() {
        // Bare Esc must reach the PTY so hosted agents can use it (e.g. Claude
        // Code / OpenCode 2×Esc abort). Only Alt+Esc leaves terminal focus.
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
        let k = KeyEvent {
            code: KeyCode::Tab,
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
