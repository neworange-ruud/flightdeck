//! Pure resolution of input-mode visual cues (SPECS §23): per-mode colors,
//! border brightness, and which pane is "live". No I/O, no rendering.

use crate::app::modes::InputMode;
use crate::contracts::UiConfig;
use ratatui::style::{Color, Modifier, Style};

/// The two framed regions of the layout (SPECS §23).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    /// The left Agent-Tabs sidebar — live in APP mode.
    Sidebar,
    /// The right terminal viewport — live in TERMINAL mode.
    Terminal,
}

/// Border brightness levels (SPECS §23).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BorderLevel {
    Off,
    Dim,
    Normal,
    Bright,
}

fn border_level(ui: &UiConfig) -> BorderLevel {
    match ui.mode_border.as_str() {
        "dim" => BorderLevel::Dim,
        "normal" => BorderLevel::Normal,
        "bright" => BorderLevel::Bright,
        // "off" and any unexpected value (validation rejects the latter at load).
        _ => BorderLevel::Off,
    }
}

/// Whether a live-pane border is drawn at all.
pub fn border_enabled(ui: &UiConfig) -> bool {
    border_level(ui) != BorderLevel::Off
}

/// Cells reserved on each side of a framed pane: 1 when the border is on, else 0.
pub fn border_inset(ui: &UiConfig) -> u16 {
    if border_enabled(ui) {
        1
    } else {
        0
    }
}

/// Base ratatui color for a validated mode-color name. Falls back to white for
/// any unexpected value (config validation rejects those at load).
fn base_color(name: &str) -> Color {
    match name {
        "green" => Color::Green,
        "cyan" => Color::Cyan,
        "blue" => Color::Blue,
        "magenta" => Color::Magenta,
        "yellow" => Color::Yellow,
        "red" => Color::Red,
        "white" => Color::White,
        _ => Color::White,
    }
}

/// The bright (Light*) variant of a mode-color name. White is already brightest.
fn bright_color(name: &str) -> Color {
    match name {
        "green" => Color::LightGreen,
        "cyan" => Color::LightCyan,
        "blue" => Color::LightBlue,
        "magenta" => Color::LightMagenta,
        "yellow" => Color::LightYellow,
        "red" => Color::LightRed,
        "white" => Color::White,
        _ => Color::White,
    }
}

/// The configured color name for a mode.
fn mode_color_name(ui: &UiConfig, mode: InputMode) -> &str {
    match mode {
        InputMode::Terminal => &ui.terminal_mode_color,
        InputMode::App => &ui.app_mode_color,
    }
}

/// The mode chip's background color.
pub fn chip_color(ui: &UiConfig, mode: InputMode) -> Color {
    base_color(mode_color_name(ui, mode))
}

/// Is `pane` the one receiving keystrokes in `mode`?
fn is_live(mode: InputMode, pane: Pane) -> bool {
    matches!(
        (mode, pane),
        (InputMode::Terminal, Pane::Terminal) | (InputMode::App, Pane::Sidebar)
    )
}

/// The border style for `pane` given the current `mode`. The live pane uses the
/// mode's configured color at the configured brightness; the inactive pane
/// recedes to dark gray so the contrast points at the live pane.
pub fn pane_border_style(ui: &UiConfig, mode: InputMode, pane: Pane) -> Style {
    if !is_live(mode, pane) {
        return Style::default().fg(Color::DarkGray);
    }
    let name = mode_color_name(ui, mode);
    match border_level(ui) {
        BorderLevel::Bright => Style::default().fg(bright_color(name)),
        BorderLevel::Dim => Style::default()
            .fg(base_color(name))
            .add_modifier(Modifier::DIM),
        // Normal, and Off (never reached while a border is drawn).
        _ => Style::default().fg(base_color(name)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::modes::InputMode;
    use crate::contracts::UiConfig;
    use ratatui::style::{Color, Modifier};

    fn ui(term: &str, app: &str, border: &str) -> UiConfig {
        UiConfig {
            terminal_mode_color: term.to_string(),
            app_mode_color: app.to_string(),
            mode_border: border.to_string(),
            ..UiConfig::default()
        }
    }

    #[test]
    fn border_enabled_follows_setting() {
        assert!(!border_enabled(&ui("green", "cyan", "off")));
        assert!(border_enabled(&ui("green", "cyan", "dim")));
        assert!(border_enabled(&ui("green", "cyan", "normal")));
        assert!(border_enabled(&ui("green", "cyan", "bright")));
    }

    #[test]
    fn border_inset_is_one_when_enabled() {
        assert_eq!(border_inset(&ui("green", "cyan", "off")), 0);
        assert_eq!(border_inset(&ui("green", "cyan", "normal")), 1);
    }

    #[test]
    fn chip_color_uses_mode_color() {
        let u = ui("magenta", "yellow", "off");
        assert_eq!(chip_color(&u, InputMode::Terminal), Color::Magenta);
        assert_eq!(chip_color(&u, InputMode::App), Color::Yellow);
    }

    #[test]
    fn live_pane_uses_mode_color_terminal() {
        let u = ui("green", "cyan", "normal");
        // In terminal mode, the terminal pane is live → green.
        let s = pane_border_style(&u, InputMode::Terminal, Pane::Terminal);
        assert_eq!(s.fg, Some(Color::Green));
        // The sidebar is inactive → recedes to dark gray.
        let s2 = pane_border_style(&u, InputMode::Terminal, Pane::Sidebar);
        assert_eq!(s2.fg, Some(Color::DarkGray));
    }

    #[test]
    fn live_pane_uses_mode_color_app() {
        let u = ui("green", "cyan", "normal");
        // In app mode, the sidebar is live → cyan.
        let s = pane_border_style(&u, InputMode::App, Pane::Sidebar);
        assert_eq!(s.fg, Some(Color::Cyan));
        let s2 = pane_border_style(&u, InputMode::App, Pane::Terminal);
        assert_eq!(s2.fg, Some(Color::DarkGray));
    }

    #[test]
    fn dim_level_adds_dim_modifier() {
        let u = ui("green", "cyan", "dim");
        let s = pane_border_style(&u, InputMode::Terminal, Pane::Terminal);
        assert_eq!(s.fg, Some(Color::Green));
        assert!(s.add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn bright_level_uses_light_variant() {
        let u = ui("green", "cyan", "bright");
        let s = pane_border_style(&u, InputMode::Terminal, Pane::Terminal);
        assert_eq!(s.fg, Some(Color::LightGreen));
    }
}
