//! The front-end-neutral key chord: a key plus the modifiers held with it.
//!
//! Nothing here names a crossterm, GPUI or browser type. Each front-end lifts
//! its native key event into a [`Chord`] at its own edge (the TUI does it in
//! [`crate::tui::input`]) and from then on speaks only this vocabulary.

use std::fmt;
use std::ops::BitOr;

/// A key, independent of any front-end's event type.
///
/// The set is exactly the keys FlightDeck binds or encodes for a PTY today. A
/// front-end key with no variant here (Insert, media keys, a bare modifier…)
/// has no chord: it matches no binding and encodes to no bytes, which is what
/// the TUI has always done with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    /// A character as the front-end reports it, **case-sensitive**: Shift+g is
    /// `Char('G')` with [`Mods::SHIFT`] on the terminals crossterm reads. The
    /// table binds lowercase letters.
    Char(char),
    Enter,
    Esc,
    Tab,
    /// Shift+Tab. crossterm reports it as its own key (with SHIFT set) rather
    /// than as `Tab` + SHIFT, and only this key encodes to `ESC [ Z`; a
    /// front-end that sees `Tab` + SHIFT must normalise it to `BackTab` to get
    /// the same bytes.
    BackTab,
    Backspace,
    Delete,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    /// A function key, `F(1)` for F1.
    F(u8),
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Key::Char(c) => write!(f, "{c}"),
            Key::F(n) => write!(f, "F{n}"),
            Key::Enter => f.write_str("Enter"),
            Key::Esc => f.write_str("Esc"),
            Key::Tab => f.write_str("Tab"),
            Key::BackTab => f.write_str("BackTab"),
            Key::Backspace => f.write_str("Backspace"),
            Key::Delete => f.write_str("Delete"),
            Key::Up => f.write_str("Up"),
            Key::Down => f.write_str("Down"),
            Key::Left => f.write_str("Left"),
            Key::Right => f.write_str("Right"),
            Key::Home => f.write_str("Home"),
            Key::End => f.write_str("End"),
            Key::PageUp => f.write_str("PageUp"),
            Key::PageDown => f.write_str("PageDown"),
        }
    }
}

/// A set of held modifiers.
///
/// The six crossterm can report, so a TUI event round-trips losslessly. On
/// macOS, Option arrives as [`Mods::ALT`] only when the terminal is set to use
/// Option as Meta; Command arrives as [`Mods::SUPER`] only on terminals that
/// report it at all. A native front-end should map Option to `ALT` and
/// Command to `SUPER` so the same table applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Mods(u8);

impl Mods {
    /// No modifier held.
    pub const NONE: Mods = Mods(0);
    pub const SHIFT: Mods = Mods(1);
    pub const CTRL: Mods = Mods(1 << 1);
    /// Alt, or Option on macOS.
    pub const ALT: Mods = Mods(1 << 2);
    /// Command on macOS, the Windows/Super key elsewhere.
    pub const SUPER: Mods = Mods(1 << 3);
    pub const HYPER: Mods = Mods(1 << 4);
    pub const META: Mods = Mods(1 << 5);
    /// Every modifier.
    pub const ALL: Mods = Mods(0b11_1111);

    /// Whether every modifier in `other` is held.
    pub const fn contains(self, other: Mods) -> bool {
        self.0 & other.0 == other.0
    }

    /// Whether any modifier in `other` is held.
    pub const fn intersects(self, other: Mods) -> bool {
        self.0 & other.0 != 0
    }

    /// Whether no modifier is held.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// These modifiers without the ones in `other`.
    pub const fn without(self, other: Mods) -> Mods {
        Mods(self.0 & !other.0)
    }

    /// The raw bits, stable across releases (bit 0 Shift … bit 5 Meta).
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Build from raw bits; unknown bits are dropped.
    pub const fn from_bits_truncate(bits: u8) -> Mods {
        Mods(bits & Mods::ALL.0)
    }
}

impl BitOr for Mods {
    type Output = Mods;
    fn bitor(self, rhs: Mods) -> Mods {
        Mods(self.0 | rhs.0)
    }
}

/// One key press: a [`Key`] and the [`Mods`] held with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Chord {
    pub key: Key,
    pub mods: Mods,
}

impl Chord {
    /// A chord.
    pub const fn new(key: Key, mods: Mods) -> Chord {
        Chord { key, mods }
    }

    /// The key with no modifiers.
    pub const fn bare(key: Key) -> Chord {
        Chord::new(key, Mods::NONE)
    }
}

/// The keycap label FlightDeck prints, e.g. `Ctrl-g`, `Alt-Up`, `F1`.
///
/// Modifiers come first in the fixed order Ctrl, Alt, Shift, Super, Hyper,
/// Meta, each followed by `-`. This is the spelling the help screen uses.
impl fmt::Display for Chord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (m, name) in [
            (Mods::CTRL, "Ctrl"),
            (Mods::ALT, "Alt"),
            (Mods::SHIFT, "Shift"),
            (Mods::SUPER, "Super"),
            (Mods::HYPER, "Hyper"),
            (Mods::META, "Meta"),
        ] {
            if self.mods.contains(m) {
                write!(f, "{name}-")?;
            }
        }
        write!(f, "{}", self.key)
    }
}
