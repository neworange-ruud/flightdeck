//! Terminal text size: the `[ui] desktop_terminal_font_size` setting plus a
//! session zoom (beads `remote-control-bmej.3.2`).
//!
//! The setting is the base, read every frame from the effective config (so a
//! save in the configuration manager applies at once). Cmd-= / Cmd-+ and Cmd--
//! step the zoom a point at a time and Cmd-0 drops it; the zoom is a GPUI
//! global, so every terminal in the window (a split view's two) grows
//! together, and it is not saved: the next launch starts at the setting, as a
//! browser's zoom does not rewrite its default font size.
//!
//! **macOS only.** On Linux and Windows the natural chords are Ctrl +/-/0,
//! and those belong to the terminal: Ctrl-- is readline's undo (0x1f), and
//! FlightDeck takes no Ctrl chord from a focused terminal that the keymap
//! table does not list (the table has none of these, and adding them would
//! change the TUI's keymap too). There the size is the setting alone. Cmd is
//! never sent to a PTY (see `keys::terminal_key_down`), so on macOS the zoom
//! costs an agent nothing.

use gpui::{App, Global, Keystroke};

use flightdeck::contracts::UiConfig;
use flightdeck::tui::platform;

/// The session zoom, in whole points added to the configured size.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TerminalZoom {
    steps: i32,
}

impl Global for TerminalZoom {}

/// What a zoom chord asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZoomChord {
    In,
    Out,
    Reset,
}

impl ZoomChord {
    /// The zoom chord `keystroke` is, if any: Cmd with `=` or `+` (the same
    /// key with or without Shift), `-`, or `0`, and nothing else held. `None`
    /// off macOS (see the module docs).
    pub fn of(keystroke: &Keystroke) -> Option<ZoomChord> {
        Self::of_on(platform::IS_MACOS, keystroke)
    }

    /// [`ZoomChord::of`] for a given platform (the tests cover both).
    pub fn of_on(is_macos: bool, keystroke: &Keystroke) -> Option<ZoomChord> {
        let m = &keystroke.modifiers;
        if !is_macos || !m.platform || m.control || m.alt || m.function {
            return None;
        }
        match keystroke.key.as_str() {
            "=" | "+" => Some(ZoomChord::In),
            "-" if !m.shift => Some(ZoomChord::Out),
            "0" if !m.shift => Some(ZoomChord::Reset),
            _ => None,
        }
    }
}

impl TerminalZoom {
    /// The size to draw at, in points, for a configured `base`: the base plus
    /// the zoom, clamped to what the setting itself accepts. An out-of-range
    /// base (config validation refuses one, but a caller may be handed
    /// anything) clamps too.
    pub fn size_for(&self, base: u16) -> f32 {
        let sizes = UiConfig::DESKTOP_TERMINAL_FONT_SIZES;
        let (lo, hi) = (i32::from(*sizes.start()), i32::from(*sizes.end()));
        (i32::from(base) + self.steps).clamp(lo, hi) as f32
    }

    /// Apply `chord` for a configured `base`. A step past either end of the
    /// range is not counted, so pressing Cmd-+ at the largest size and then
    /// Cmd-- shrinks at once. Returns whether the drawn size changed.
    pub fn apply(&mut self, chord: ZoomChord, base: u16) -> bool {
        let before = self.size_for(base);
        match chord {
            ZoomChord::In => self.steps += 1,
            ZoomChord::Out => self.steps -= 1,
            ZoomChord::Reset => self.steps = 0,
        }
        let after = self.size_for(base);
        if chord != ZoomChord::Reset && after == before {
            // Undo a step that went nowhere.
            match chord {
                ZoomChord::In => self.steps -= 1,
                _ => self.steps += 1,
            }
        }
        after != before
    }

    /// The zoom in effect (the default before any chord).
    pub fn current(cx: &App) -> TerminalZoom {
        cx.try_global::<TerminalZoom>().copied().unwrap_or_default()
    }
}

/// The size the app's terminals draw at, in points: the active project's
/// effective `[ui] desktop_terminal_font_size` plus the zoom. The terminal
/// view and split view's read-only panes both read it, so a terminal keeps
/// its grid when it gains or loses focus.
pub fn app_font_size(host: &flightdeck::host::AppHost, cx: &App) -> f32 {
    let base = host.active_state().config.ui.desktop_terminal_font_size;
    TerminalZoom::current(cx).size_for(base)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::Modifiers;

    fn key(key: &str, m: Modifiers) -> Keystroke {
        Keystroke {
            modifiers: m,
            key: key.to_string(),
            key_char: None,
        }
    }

    fn cmd() -> Modifiers {
        Modifiers {
            platform: true,
            ..Modifiers::default()
        }
    }

    #[test]
    fn the_chords_are_cmd_plus_minus_zero_on_macos_only() {
        let cmd_shift = Modifiers {
            shift: true,
            ..cmd()
        };
        assert_eq!(
            ZoomChord::of_on(true, &key("=", cmd())),
            Some(ZoomChord::In)
        );
        assert_eq!(
            ZoomChord::of_on(true, &key("=", cmd_shift)),
            Some(ZoomChord::In)
        );
        assert_eq!(
            ZoomChord::of_on(true, &key("+", cmd_shift)),
            Some(ZoomChord::In)
        );
        assert_eq!(
            ZoomChord::of_on(true, &key("-", cmd())),
            Some(ZoomChord::Out)
        );
        assert_eq!(
            ZoomChord::of_on(true, &key("0", cmd())),
            Some(ZoomChord::Reset)
        );
        // Not a zoom: other keys, other modifiers, and every OS but macOS,
        // where the Ctrl forms would be bytes an agent can receive.
        assert_eq!(ZoomChord::of_on(true, &key("1", cmd())), None);
        let ctrl = Modifiers {
            control: true,
            ..Modifiers::default()
        };
        assert_eq!(ZoomChord::of_on(true, &key("-", ctrl)), None);
        let cmd_alt = Modifiers { alt: true, ..cmd() };
        assert_eq!(ZoomChord::of_on(true, &key("=", cmd_alt)), None);
        assert_eq!(ZoomChord::of_on(false, &key("=", cmd())), None);
        assert_eq!(ZoomChord::of_on(false, &key("-", ctrl)), None);
    }

    #[test]
    fn zoom_steps_a_point_clamps_to_the_setting_range_and_resets() {
        let mut zoom = TerminalZoom::default();
        assert_eq!(zoom.size_for(13), 13.0);
        assert!(zoom.apply(ZoomChord::In, 13));
        assert!(zoom.apply(ZoomChord::In, 13));
        assert_eq!(zoom.size_for(13), 15.0);
        // The zoom follows a changed base.
        assert_eq!(zoom.size_for(20), 22.0);
        assert!(zoom.apply(ZoomChord::Reset, 13));
        assert_eq!(zoom.size_for(13), 13.0);
        assert!(!zoom.apply(ZoomChord::Reset, 13), "already there");

        // At the top a step does nothing and is not banked.
        let mut zoom = TerminalZoom::default();
        assert!(!zoom.apply(ZoomChord::In, 32));
        assert_eq!(zoom.size_for(32), 32.0);
        assert!(zoom.apply(ZoomChord::Out, 32));
        assert_eq!(zoom.size_for(32), 31.0);
        // And at the bottom.
        let mut zoom = TerminalZoom::default();
        assert!(!zoom.apply(ZoomChord::Out, 8));
        assert!(zoom.apply(ZoomChord::In, 8));
        assert_eq!(zoom.size_for(8), 9.0);
        // A base out of range is clamped rather than drawn.
        assert_eq!(TerminalZoom::default().size_for(2), 8.0);
    }
}
