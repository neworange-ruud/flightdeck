//! What a keymap entry does in the GUI, in ONE place: the chord's action
//! handler, every button, and every context-menu item go through
//! [`perform_entry`], so a button cannot drift from its chord.
//!
//! The mapping ([`intent_for`]) is the TUI's own: `Action::Dispatch(cmd)` is
//! `HostEvent::Command(cmd)` (a command with an empty payload opens its
//! prompt, as in the TUI), project switching and the overlays are their
//! `HostEvent`s, and Paste reads the system clipboard first.

use flightdeck::app::keymap::{Action, Chord, Key, Keymap, KeymapEntry, Mods};
use flightdeck::host::HostEvent;
use flightdeck::tui::platform;
use flightdeck::view::HintAction;
use gpui::{App, Entity};

use crate::host::HostModel;

/// The keymap table the app binds and its buttons label themselves from.
///
/// The `[ui] use_f2_to_leave_terminal_focus` setting is not read yet, so this
/// is the default table (`remote-control-9diy`).
pub fn keymap() -> &'static Keymap {
    Keymap::for_this_platform(false)
}

/// What performing a table entry means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intent {
    /// Hand this to the host.
    Host(HostEvent),
    /// Read the system clipboard and hand it over as `HostEvent::Paste`.
    PasteClipboard,
}

/// The intent behind a keymap action. Total: every action has one.
pub fn intent_for(action: &Action) -> Intent {
    match action {
        Action::Dispatch(command) => Intent::Host(HostEvent::Command(command.clone())),
        Action::SwitchProject(selector) => Intent::Host(HostEvent::SwitchProject(*selector)),
        Action::Paste => Intent::PasteClipboard,
        Action::OpenPalette => Intent::Host(HostEvent::OpenPalette),
        Action::OpenHelp => Intent::Host(HostEvent::OpenHelp),
        Action::FocusApp => Intent::Host(HostEvent::FocusApp),
        Action::FocusTerminal => Intent::Host(HostEvent::FocusTerminal),
        Action::Quit => Intent::Host(HostEvent::Quit),
    }
}

/// Perform `entry` against the host: the one path for chords, buttons and
/// menu items.
pub fn perform_entry(entry: &KeymapEntry, host: &Entity<HostModel>, cx: &mut App) {
    match intent_for(&entry.action) {
        Intent::Host(event) => host.update(cx, |model, cx| model.dispatch(event, cx)),
        Intent::PasteClipboard => {
            let text = cx.read_from_clipboard().and_then(|item| item.text());
            if let Some(text) = text {
                host.update(cx, |model, cx| model.dispatch(HostEvent::Paste(text), cx));
            }
        }
    }
}

/// Perform the entry with this id. A missing id is a programming error in a
/// view (the table's ids are fixed); it does nothing rather than panic.
pub fn perform_id(id: &str, host: &Entity<HostModel>, cx: &mut App) {
    if let Some(entry) = keymap().entry(id) {
        perform_entry(entry, host, cx);
    }
}

/// The keymap entry a mode-bar hint stands for, so its keycap and its click
/// come from the table like every other control's.
pub fn hint_entry_id(action: HintAction) -> &'static str {
    match action {
        HintAction::FocusApp => "FocusApp",
        HintAction::FocusTerminal => "FocusTerminal",
        HintAction::OpenPalette => "OpenPalette",
        HintAction::OpenHelp => "OpenHelp",
    }
}

/// The keycap a control shows for entry `id`: its first chord, spelled the
/// platform's way (`⌃N`, `⌥Esc` on macOS; `Ctrl+N`, `Alt+Esc` elsewhere).
/// `None` for an id the table does not have.
pub fn keycap(id: &str) -> Option<String> {
    let entry = keymap().entry(id)?;
    entry.triggers.first().map(|t| keycap_text(t.chord))
}

/// Spell a chord for a keycap. macOS uses the menu-bar glyphs with no
/// separator; Windows and Linux spell modifiers out, joined by `+`, as their
/// menus do.
pub fn keycap_text(chord: Chord) -> String {
    let mut out = String::new();
    let mods: [(Mods, &str, &str); 4] = [
        (Mods::CTRL, "⌃", "Ctrl+"),
        (Mods::ALT, "⌥", "Alt+"),
        (Mods::SHIFT, "⇧", "Shift+"),
        (Mods::SUPER, "⌘", "Super+"),
    ];
    for (m, mac, other) in mods {
        if chord.mods.contains(m) {
            out.push_str(if platform::IS_MACOS { mac } else { other });
        }
    }
    let key = match chord.key {
        Key::Char(c) => c.to_uppercase().to_string(),
        Key::Up => "↑".to_string(),
        Key::Down => "↓".to_string(),
        Key::Left => "←".to_string(),
        Key::Right => "→".to_string(),
        Key::Enter => if platform::IS_MACOS { "↩" } else { "Enter" }.to_string(),
        other => other.to_string(),
    };
    out.push_str(&key);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use flightdeck::app::commands::{Command, Selector};

    #[test]
    fn every_entry_maps_to_the_tui_meaning() {
        for entry in keymap().entries() {
            let intent = intent_for(&entry.action);
            match (&entry.action, &intent) {
                (Action::Dispatch(c), Intent::Host(HostEvent::Command(d))) => assert_eq!(c, d),
                (Action::SwitchProject(a), Intent::Host(HostEvent::SwitchProject(b))) => {
                    assert_eq!(a, b)
                }
                (Action::Paste, Intent::PasteClipboard)
                | (Action::OpenPalette, Intent::Host(HostEvent::OpenPalette))
                | (Action::OpenHelp, Intent::Host(HostEvent::OpenHelp))
                | (Action::FocusApp, Intent::Host(HostEvent::FocusApp))
                | (Action::FocusTerminal, Intent::Host(HostEvent::FocusTerminal))
                | (Action::Quit, Intent::Host(HostEvent::Quit)) => {}
                (action, intent) => panic!("{}: {action:?} became {intent:?}", entry.id),
            }
        }
    }

    #[test]
    fn the_buttons_ids_exist_in_the_table() {
        for id in [
            "NewAgentTab",
            "NewChildTerminal",
            "PushBranch",
            "PullBase",
            "FinishLocalMerge",
            "SetManualStatus",
            "RestartAgent",
            "OpenWorktreeInFileManager",
            "CloseAgentTab",
            "OpenPalette",
            "OpenHelp",
            "FocusApp",
            "FocusTerminal",
        ] {
            assert!(keymap().entry(id).is_some(), "{id}");
            assert!(keycap(id).is_some(), "{id} has a keycap");
        }
        assert_eq!(
            keymap().entry("PushBranch").map(|e| intent_for(&e.action)),
            Some(Intent::Host(HostEvent::Command(Command::PushBranch {
                confirm: None
            })))
        );
        assert_eq!(
            keymap()
                .entry("SwitchProjectNext")
                .map(|e| intent_for(&e.action)),
            Some(Intent::Host(HostEvent::SwitchProject(Selector::Next)))
        );
    }

    #[test]
    fn keycaps_use_the_platforms_spelling() {
        let ctrl_n = Chord::new(Key::Char('n'), Mods::CTRL);
        let alt_esc = Chord::new(Key::Esc, Mods::ALT);
        if platform::IS_MACOS {
            assert_eq!(keycap_text(ctrl_n), "⌃N");
            assert_eq!(keycap_text(alt_esc), "⌥Esc");
        } else {
            assert_eq!(keycap_text(ctrl_n), "Ctrl+N");
            assert_eq!(keycap_text(alt_esc), "Alt+Esc");
        }
        assert_eq!(keycap_text(Chord::bare(Key::F(1))), "F1");
    }
}
