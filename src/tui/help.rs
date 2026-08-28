//! What FlightDeck's help and About screens say, as data rather than as
//! drawing code (SPECS §23; `specs/WEB_INTERFACE.md` §6.5 R16).
//!
//! ## Why this module exists
//!
//! Until `remote-control-ll5.8` the help overlay's content *was* its ratatui
//! drawing code: forty-odd `Line::from(...)` calls inside `draw_help_overlay`.
//! That is fine while one surface renders it, and it is exactly the shape that
//! rots the moment a second one does — the browser would have had to copy the
//! list, and the copy would have been wrong on the first keybinding anybody
//! changed. §1's "the browser mirrors the desktop" is not satisfied by two
//! lists that happen to agree today.
//!
//! So the words live here, once. [`crate::tui::render::draw_help_overlay`]
//! turns them into ratatui lines, and
//! [`crate::web::server::HostState::help`] puts the very same values on the
//! wire for the browser to draw. This is R7's ruling about the command
//! inventory applied to the other thing both surfaces have to agree about:
//! **the host owns it and sends it**, rather than each surface holding a
//! private copy.
//!
//! ## Why the types are also the wire types
//!
//! [`crate::web::protocol`] re-exports these structs rather than restating
//! them. That module's own doc explains the principle for
//! [`crate::contracts::InterpretedStatus`] — *"it borrows the vocabulary by
//! reusing the domain types rather than restating them"* — and the argument is
//! stronger here, because a restated `HelpRow` would be a second place for the
//! same sentence to live, which is the failure this module was written to
//! remove.
//!
//! ## What is deliberately *not* here
//!
//! The **browser's own** keyboard. `Ctrl-g`, `Esc Esc`, `a`, `?` and
//! click-outside are facts about a tab, not about this process, and a host that
//! claimed to know them would be guessing about software it does not run. The
//! SPA states its own half (`webui/src/state/help.ts`) and labels the host's
//! half as the host's, which is D16's "honest about where the effect lands"
//! applied to keys instead of to actions.

use serde::{Deserialize, Serialize};

/// Everything the help overlay shows, for one running FlightDeck.
///
/// Built by [`help_doc`] from the two facts that change it: the leave-focus
/// keybinding (a config choice, and platform-dependent) and whether this is an
/// isolated run. Nothing else about it varies at runtime, which is why it can
/// ride on a snapshot rather than needing a delta.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelpDoc {
    /// The overlay's own title, so neither surface invents one.
    pub title: String,
    /// Notes that must be read **before** the shortcut list, not after it.
    /// Empty in an ordinary run; see [`isolated_note`].
    #[serde(default)]
    pub notes: Vec<HelpNote>,
    /// The shortcut list, in the order both surfaces render it.
    pub sections: Vec<HelpSection>,
}

/// A short block of prose above the shortcuts, with its own heading.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelpNote {
    /// The heading, e.g. `Isolated run (--isolated)`.
    pub title: String,
    /// One line each, already worded as complete sentences.
    pub lines: Vec<String>,
}

/// One group of shortcuts under a heading (`Global`, `Projects`, …).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelpSection {
    /// The group heading.
    pub title: String,
    /// Its rows, in display order.
    pub rows: Vec<HelpRow>,
}

/// One shortcut: what you press, and what it does.
///
/// `keys` is not always a key — `Mouse click`, `Drag past edge` and `+ project`
/// are all rows here, because SPECS §22's interaction model is not only the
/// keyboard and a help screen that pretended otherwise would be missing the
/// gestures users actually reach for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelpRow {
    /// The keys or the gesture, unindented. The desktop adds its own leading
    /// spaces when it draws; the browser lays the column out in CSS.
    pub keys: String,
    /// What it does, in the imperative.
    pub description: String,
}

impl HelpRow {
    fn new(keys: &str, description: &str) -> Self {
        HelpRow {
            keys: keys.to_string(),
            description: description.to_string(),
        }
    }
}

/// The credits and version the About screen shows.
///
/// Version comes from `CARGO_PKG_VERSION` — the build's own, so a browser
/// attached to a host that was updated under it reports the *host's* version
/// and not the one its JavaScript was compiled with. That is the same reason
/// [`crate::web::protocol::Snapshot::host_version`] exists, and it is why the
/// browser must not fill this in from a constant of its own.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AboutDoc {
    /// The product name.
    pub name: String,
    /// This build's version string, without a leading `v`.
    pub version: String,
    /// One sentence saying what FlightDeck is.
    pub tagline: String,
    /// Who made it, in the order the About screen lists them.
    pub credits: Vec<AboutCredit>,
    /// The project's home page.
    pub url: String,
}

/// One credit line: the relationship, then the person.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AboutCredit {
    /// `Built by`, `with collaboration from`.
    pub role: String,
    /// The person's name.
    pub name: String,
}

/// The About screen's content for this build.
pub fn about_doc() -> AboutDoc {
    AboutDoc {
        name: "FlightDeck".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        tagline: "A terminal UI for orchestrating parallel AI coding agents.".to_string(),
        credits: vec![
            AboutCredit {
                role: "Built by".to_string(),
                name: "Ruud van Falier".to_string(),
            },
            AboutCredit {
                role: "with collaboration from".to_string(),
                name: "Sander Langhorst".to_string(),
            },
        ],
        url: "https://flightdeckai.app".to_string(),
    }
}

/// The key that leaves terminal focus, as this build and this platform have it
/// (SPECS §23).
///
/// Three answers, one of them chosen by `[ui] use_f2_to_leave_terminal_focus`
/// and the other two by the platform. The help screen has to state the one that
/// is actually bound, not the one that usually is — which is why this is a
/// function of the config rather than a constant.
pub fn leave_focus_key(use_f2: bool) -> &'static str {
    if use_f2 {
        "F2"
    } else if crate::tui::platform::LEAVE_FOCUS_USES_SHIFT {
        "Shift+Esc"
    } else {
        "Alt+Esc"
    }
}

/// SPECS §32's isolated-run note, or `None` for an ordinary run.
///
/// Kept to a heading and two lines on purpose. The desktop's overlay is a fixed
/// 64×40 box with no scrolling, so every line this costs is a line of the
/// shortcut list pushed into the clipped tail — and leading with it is what
/// guarantees it survives the clip at all.
fn isolated_note(isolated: bool) -> Option<HelpNote> {
    if !isolated {
        return None;
    }
    Some(HelpNote {
        title: "Isolated run (--isolated)".to_string(),
        lines: vec![
            "Nothing is saved and nothing was continued.".to_string(),
            "One session here; no other projects, no new session tabs.".to_string(),
        ],
    })
}

/// The whole help screen for this build, this config and this run.
///
/// `use_f2` is `[ui] use_f2_to_leave_terminal_focus`; `isolated` is SPECS §32's
/// `--isolated`. Both are read from the live [`crate::app::state::AppState`] at
/// the moment the screen is built, so neither surface can show a binding this
/// process does not have.
pub fn help_doc(use_f2: bool, isolated: bool) -> HelpDoc {
    HelpDoc {
        title: "FlightDeck Keyboard Shortcuts".to_string(),
        notes: isolated_note(isolated).into_iter().collect(),
        sections: vec![
            HelpSection {
                title: "Global".to_string(),
                rows: vec![
                    HelpRow::new("Ctrl-g", "Command palette"),
                    HelpRow::new("Ctrl-q", "Quit / close app"),
                    HelpRow::new("Ctrl-n", "New Agent Session Tab"),
                    HelpRow::new("Ctrl-p", "Push current branch"),
                    HelpRow::new("Ctrl-u", "Pull base (git pull --rebase)"),
                    HelpRow::new("Ctrl-f", "Finish current Agent Session Tab"),
                    HelpRow::new("Ctrl-k", "Close current Agent Session Tab"),
                    HelpRow::new("Alt-o", "Open worktree in file manager"),
                    HelpRow::new(crate::tui::render::HELP_KEYS, "Help / keybindings"),
                ],
            },
            HelpSection {
                title: "Projects".to_string(),
                rows: vec![
                    HelpRow::new("Shift-Left / Shift-Right", "Previous / Next project"),
                    HelpRow::new("Mouse click", "Switch project (top tab row)"),
                    HelpRow::new("+ project", "Open another project folder"),
                ],
            },
            HelpSection {
                title: "Agent Session Tab Navigation".to_string(),
                rows: vec![
                    HelpRow::new("Up / Down (or Alt)", "Previous / Next Agent Session Tab"),
                    HelpRow::new("Alt-1 .. Alt-9", "Jump to Agent Session Tab by index"),
                    HelpRow::new("Mouse click", "Select Agent Session Tab"),
                ],
            },
            HelpSection {
                title: "Child Terminal Navigation".to_string(),
                rows: vec![
                    HelpRow::new("Ctrl-t", "New child terminal"),
                    HelpRow::new("Ctrl-w", "Close active child terminal"),
                    HelpRow::new(
                        "Left / Right (or Alt)",
                        "Cycle terminal tabs (agent + shells)",
                    ),
                    HelpRow::new("Ctrl-b", "Toggle split view (terminals side by side)"),
                    HelpRow::new("Mouse click", "Select terminal tab"),
                ],
            },
            HelpSection {
                title: "Selection / Clipboard".to_string(),
                rows: vec![
                    HelpRow::new("Drag", "Select terminal text (copies on release)"),
                    HelpRow::new("Drag past edge", "Auto-scrolls to reach offscreen text"),
                    HelpRow::new("Shift-drag", "Force selection over a mouse-driven app"),
                ],
            },
            HelpSection {
                title: "Focus".to_string(),
                rows: vec![
                    HelpRow::new(leave_focus_key(use_f2), "Leave terminal focus / focus app"),
                    HelpRow::new("Enter", "Focus active terminal"),
                ],
            },
            HelpSection {
                title: "Status".to_string(),
                rows: vec![
                    HelpRow::new("Ctrl-s", "Set manual status"),
                    HelpRow::new("Ctrl-r", "Restart primary agent"),
                ],
            },
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The isolated note leads, because the desktop's overlay clips its tail
    /// (SPECS §32). Asserting the *position* rather than the presence is what
    /// keeps that reasoning true after an edit.
    #[test]
    fn the_isolated_note_leads_and_is_absent_otherwise() {
        assert!(help_doc(false, false).notes.is_empty());
        let isolated = help_doc(false, true);
        assert_eq!(isolated.notes.len(), 1);
        assert_eq!(isolated.notes[0].title, "Isolated run (--isolated)");
        // And it did not displace the shortcut list.
        assert_eq!(
            isolated.sections.len(),
            help_doc(false, false).sections.len()
        );
    }

    /// The one row that is a config answer rather than a constant. A help
    /// screen that printed `Alt+Esc` to a user who set `use_f2` would be
    /// documenting somebody else's FlightDeck.
    #[test]
    fn the_focus_row_states_the_binding_this_build_actually_has() {
        let f2 = help_doc(true, false);
        let focus = f2
            .sections
            .iter()
            .find(|s| s.title == "Focus")
            .expect("the Focus section");
        assert_eq!(focus.rows[0].keys, "F2");

        let plain = help_doc(false, false);
        let focus = plain
            .sections
            .iter()
            .find(|s| s.title == "Focus")
            .expect("the Focus section");
        assert_eq!(focus.rows[0].keys, leave_focus_key(false));
        assert_ne!(focus.rows[0].keys, "F2");
    }

    /// Every row says both halves. A row with an empty description is a key
    /// nobody can act on; a row with empty keys is a description of nothing.
    #[test]
    fn every_row_names_a_key_and_what_it_does() {
        for section in help_doc(false, true).sections {
            assert!(!section.title.is_empty());
            assert!(!section.rows.is_empty(), "'{}' has no rows", section.title);
            for row in section.rows {
                assert!(!row.keys.is_empty());
                assert!(!row.description.is_empty());
            }
        }
    }

    /// The About screen's version is this build's, never a literal that has to
    /// be remembered at release time.
    #[test]
    fn about_reports_this_builds_version() {
        assert_eq!(about_doc().version, env!("CARGO_PKG_VERSION"));
        assert!(!about_doc().credits.is_empty());
    }
}
