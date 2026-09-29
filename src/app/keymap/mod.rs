//! The one keymap table (SPECS §23), shared by every front-end.
//!
//! ## What this is
//!
//! Every key FlightDeck binds lives in [`Keymap`], once. The table is the only
//! source for:
//!
//! - the TUI's dispatch ([`crate::tui::input`] lifts a crossterm event into a
//!   [`Chord`] and calls [`Keymap::lookup`]);
//! - the help screen ([`crate::tui::help::help_doc`] renders
//!   [`Keymap::help_sections`], which the browser receives verbatim);
//! - the palette's keycap hints ([`KeymapEntry::keycap`]);
//! - the native desktop app's bindings (a separate crate enumerates
//!   [`Keymap::entries`] and registers each [`KeymapEntry::id`] as an action).
//!
//! Nothing here names a crossterm, GPUI or browser type. Front-ends adapt their
//! own events to [`Chord`] at their edge.
//!
//! ## Contexts
//!
//! A binding applies in a [`Context`]: [`Context::Global`] in both input modes,
//! or only in [`InputMode::Terminal`] / [`InputMode::App`] (see
//! [`crate::app::modes`]). In Terminal mode a chord that matches no binding is
//! typed into the PTY via [`encode_pty`]; in App mode it does nothing.
//!
//! ## Not in the table
//!
//! Modal overlays (prompts, the palette, the web access overlay's
//! [`crate::web::access::AccessKey`]) own their keys while they capture input;
//! they are not mode-level bindings. The help screen's mouse-gesture rows are
//! help text only ([`HelpRowSpec::entry_ids`] is empty for them).
//!
//! ## Example: another crate enumerating bindings and typing into a PTY
//!
//! ```
//! use flightdeck::app::keymap::{encode_pty, Chord, Context, Key, Keymap, KeymapOptions, Mods};
//! use flightdeck::app::modes::InputMode;
//!
//! let keymap = Keymap::new(KeymapOptions::for_this_platform(false));
//! for context in [Context::Global, Context::App, Context::Terminal] {
//!     for (entry, trigger) in keymap.bindings_in(context) {
//!         // e.g. register `flightdeck::OpenPalette` bound to Ctrl-g, globally.
//!         let _ = (entry.gpui_action_name(), trigger.chord, entry.description);
//!     }
//! }
//!
//! // An unbound chord in Terminal mode goes to the PTY, with the TUI's bytes.
//! let up = Chord::bare(Key::Up);
//! assert!(keymap.lookup(InputMode::Terminal, up).is_none());
//! assert_eq!(encode_pty(up), b"\x1b[A");
//! let ctrl_g = Chord::new(Key::Char('g'), Mods::CTRL);
//! assert_eq!(keymap.lookup(InputMode::Terminal, ctrl_g).unwrap().id, "OpenPalette");
//! ```

mod chord;
mod pty;

use std::sync::OnceLock;

pub use chord::{Chord, Key, Mods};
pub use pty::{encode_paste, encode_pty};

use crate::app::commands::{Command, Selector};
use crate::app::modes::InputMode;

/// The namespace [`KeymapEntry::gpui_action_name`] puts ids under.
pub const ACTION_NAMESPACE: &str = "flightdeck";

/// Where a binding applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Context {
    /// Both input modes: a focused terminal does not swallow it.
    Global,
    /// Only while a terminal has input focus ([`InputMode::Terminal`]).
    Terminal,
    /// Only in app-command mode ([`InputMode::App`]).
    App,
}

impl Context {
    /// Whether a binding in this context is live in `mode`.
    pub fn applies_in(self, mode: InputMode) -> bool {
        match self {
            Context::Global => true,
            Context::Terminal => mode == InputMode::Terminal,
            Context::App => mode == InputMode::App,
        }
    }

    /// A stable name, usable as a key-context identifier.
    pub fn name(self) -> &'static str {
        match self {
            Context::Global => "Global",
            Context::Terminal => "Terminal",
            Context::App => "App",
        }
    }
}

/// What a binding does. The front-end performs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Dispatch this command via `AppState::dispatch`. Commands with an empty
    /// payload (a new tab's name, the manual status) are the front-end's cue to
    /// prompt first.
    Dispatch(Command),
    /// Switch the active project (workspace-level, not an `AppState` command).
    SwitchProject(Selector),
    /// Paste from the system clipboard into the active terminal.
    Paste,
    /// Open the command palette.
    OpenPalette,
    /// Open the help overlay.
    OpenHelp,
    /// Leave terminal focus (`AppState::focus_app`).
    FocusApp,
    /// Focus the active terminal (`AppState::focus_terminal`).
    FocusTerminal,
    /// Quit FlightDeck.
    Quit,
}

/// One chord bound to an action, in one context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Trigger {
    /// The chord, exactly. A native front-end binds this and nothing looser.
    pub chord: Chord,
    /// Where it applies.
    pub context: Context,
    /// Extra modifiers that may also be held without breaking the match.
    ///
    /// This preserves the TUI's historical leniency (Ctrl-g still opens the
    /// palette with Shift held; F1 only when bare). It is part of the TUI's
    /// behaviour, not of the chord, and a front-end that binds exact chords can
    /// ignore it.
    pub tolerate: Mods,
}

impl Trigger {
    /// Whether `chord` fires this trigger: same key, every required modifier
    /// held, and nothing else held beyond [`Trigger::tolerate`].
    pub fn matches(&self, chord: Chord) -> bool {
        chord.key == self.chord.key
            && chord.mods.contains(self.chord.mods)
            && self.tolerate.contains(chord.mods.without(self.chord.mods))
    }
}

/// One bindable action and every chord that triggers it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeymapEntry {
    /// Stable identifier, PascalCase (`OpenPalette`). Never reused or renamed:
    /// front-ends key actions, menus and user overrides on it.
    pub id: &'static str,
    /// What happens.
    pub action: Action,
    /// What it does, for one action on its own (menus, tooltips). The help
    /// screen may word a grouped row differently (`Previous / Next …`).
    pub description: &'static str,
    /// Its chords, in the order a keycap hint prefers them.
    pub triggers: Vec<Trigger>,
    /// The help section it is listed under, or `None` for a binding the help
    /// screen deliberately does not list. Filled from the help layout, so it
    /// cannot disagree with it.
    pub help_section: Option<&'static str>,
}

impl KeymapEntry {
    /// `flightdeck::<id>`, the shape GPUI action names take.
    pub fn gpui_action_name(&self) -> String {
        format!("{ACTION_NAMESPACE}::{}", self.id)
    }

    /// The label a hint shows for this action: its first chord (`Ctrl-g`).
    pub fn keycap(&self) -> Option<String> {
        self.triggers.first().map(|t| t.chord.to_string())
    }
}

/// The facts that change the table: one config choice and two platform ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeymapOptions {
    /// `[ui] use_f2_to_leave_terminal_focus`: F2 instead of modified Esc.
    pub use_f2_to_leave_focus: bool,
    /// The default leave-focus key is Shift+Esc (Windows, Linux) rather than
    /// Alt+Esc (macOS), because the former reserve Alt+Esc for the window
    /// manager.
    pub leave_focus_uses_shift: bool,
    /// Command-V (reported as Super) pastes as well as Ctrl-V (macOS).
    pub command_v_pastes: bool,
}

impl KeymapOptions {
    /// The options for the OS this binary was built for.
    pub fn for_this_platform(use_f2_to_leave_focus: bool) -> Self {
        KeymapOptions {
            use_f2_to_leave_focus,
            leave_focus_uses_shift: crate::tui::platform::LEAVE_FOCUS_USES_SHIFT,
            command_v_pastes: crate::tui::platform::IS_MACOS,
        }
    }
}

/// The user-facing label for the leave-terminal-focus key under `options`.
///
/// Spelled with `+` (`Alt+Esc`) as it always has been on the status bar and in
/// help, unlike the `-` of [`Chord`]'s display.
pub fn leave_focus_label(options: KeymapOptions) -> &'static str {
    if options.use_f2_to_leave_focus {
        "F2"
    } else if options.leave_focus_uses_shift {
        "Shift+Esc"
    } else {
        "Alt+Esc"
    }
}

/// A help-screen section, in display order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelpSectionSpec {
    pub title: &'static str,
    pub rows: Vec<HelpRowSpec>,
}

/// One help-screen row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelpRowSpec {
    /// What to press, as printed.
    pub keys: String,
    /// What it does, as printed.
    pub description: &'static str,
    /// The [`KeymapEntry::id`]s this row documents. Empty for a mouse gesture
    /// row (`Mouse click`, `Drag`), which is help text but not a binding.
    pub entry_ids: Vec<&'static str>,
}

/// The ids the help screen deliberately does not list.
///
/// `Paste` is Ctrl-V (and Command-V on macOS) with a terminal focused: the key
/// every user already expects, so listing it would cost a line of a fixed-size
/// overlay that clips its tail.
pub const HIDDEN_FROM_HELP: &[&str] = &["Paste"];

/// Ids for Alt-1 .. Alt-9, one action per digit.
const JUMP_IDS: [&str; 9] = [
    "JumpToAgentTab1",
    "JumpToAgentTab2",
    "JumpToAgentTab3",
    "JumpToAgentTab4",
    "JumpToAgentTab5",
    "JumpToAgentTab6",
    "JumpToAgentTab7",
    "JumpToAgentTab8",
    "JumpToAgentTab9",
];

/// The keymap: every binding, plus the help screen's layout of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keymap {
    options: KeymapOptions,
    entries: Vec<KeymapEntry>,
    help: Vec<HelpSectionSpec>,
}

impl Keymap {
    /// Build the table for `options`.
    pub fn new(options: KeymapOptions) -> Keymap {
        let mut entries = build_entries(options);
        let help = build_help(options, &entries);
        for entry in &mut entries {
            entry.help_section = help
                .iter()
                .find(|s| s.rows.iter().any(|r| r.entry_ids.contains(&entry.id)))
                .map(|s| s.title);
        }
        Keymap {
            options,
            entries,
            help,
        }
    }

    /// The table for this platform, built once per `use_f2` value.
    pub fn for_this_platform(use_f2_to_leave_focus: bool) -> &'static Keymap {
        static KEYMAPS: [OnceLock<Keymap>; 2] = [OnceLock::new(), OnceLock::new()];
        KEYMAPS[usize::from(use_f2_to_leave_focus)]
            .get_or_init(|| Keymap::new(KeymapOptions::for_this_platform(use_f2_to_leave_focus)))
    }

    /// The options this table was built for.
    pub fn options(&self) -> KeymapOptions {
        self.options
    }

    /// Every entry, in help order.
    pub fn entries(&self) -> &[KeymapEntry] {
        &self.entries
    }

    /// The entry with this id.
    pub fn entry(&self, id: &str) -> Option<&KeymapEntry> {
        self.entries.iter().find(|e| e.id == id)
    }

    /// The entry that performs `action`, if any key does.
    pub fn entry_for_action(&self, action: &Action) -> Option<&KeymapEntry> {
        self.entries.iter().find(|e| &e.action == action)
    }

    /// Every `(entry, trigger)` bound in exactly `context`.
    pub fn bindings_in(
        &self,
        context: Context,
    ) -> impl Iterator<Item = (&KeymapEntry, &Trigger)> + '_ {
        self.entries.iter().flat_map(move |e| {
            e.triggers
                .iter()
                .filter(move |t| t.context == context)
                .map(move |t| (e, t))
        })
    }

    /// The entry `chord` triggers in `mode`, or `None` when it is unbound there.
    ///
    /// Global bindings are consulted before mode-specific ones. (No chord
    /// matches two bindings live in the same mode — a test proves it — so the
    /// order is belt and braces.)
    pub fn lookup(&self, mode: InputMode, chord: Chord) -> Option<&KeymapEntry> {
        let specific = match mode {
            InputMode::Terminal => Context::Terminal,
            InputMode::App => Context::App,
        };
        [Context::Global, specific].into_iter().find_map(|context| {
            self.bindings_in(context)
                .find(|(_, t)| t.matches(chord))
                .map(|(e, _)| e)
        })
    }

    /// The help screen's sections, in display order.
    pub fn help_sections(&self) -> &[HelpSectionSpec] {
        &self.help
    }
}

fn trigger(context: Context, key: Key, mods: Mods, tolerate: Mods) -> Trigger {
    Trigger {
        chord: Chord::new(key, mods),
        context,
        tolerate,
    }
}

fn entry(
    id: &'static str,
    action: Action,
    description: &'static str,
    triggers: Vec<Trigger>,
) -> KeymapEntry {
    KeymapEntry {
        id,
        action,
        description,
        triggers,
        help_section: None,
    }
}

/// The bindings. Tolerances reproduce the TUI's historical matching exactly.
fn build_entries(options: KeymapOptions) -> Vec<KeymapEntry> {
    use Context::{App, Global, Terminal};
    let any = Mods::ALL;
    let exact = Mods::NONE;
    let ctrl = Mods::CTRL;
    let alt = Mods::ALT;
    let dispatch = Action::Dispatch;
    let ctrl_key = |c: char| trigger(App, Key::Char(c), ctrl, any);

    let mut entries = vec![
        entry(
            "OpenPalette",
            Action::OpenPalette,
            "Command palette",
            vec![trigger(Global, Key::Char('g'), ctrl, any)],
        ),
        entry(
            "Quit",
            Action::Quit,
            "Quit / close app",
            vec![trigger(Global, Key::Char('q'), ctrl, any)],
        ),
        // F1 is claimed only bare; Alt-h exists because Apple keyboards
        // reserve F1 as a media key. Option+h needs "Use Option as Meta" on
        // macOS, the same requirement Alt-o and Alt-1..9 impose.
        entry(
            "OpenHelp",
            Action::OpenHelp,
            "Help / keybindings",
            vec![
                trigger(Global, Key::F(1), exact, exact),
                trigger(
                    Global,
                    Key::Char('h'),
                    alt,
                    Mods::ALL.without(Mods::CTRL | Mods::SHIFT),
                ),
            ],
        ),
        entry(
            "SwitchProjectPrev",
            Action::SwitchProject(Selector::Prev),
            "Previous project",
            vec![trigger(
                Global,
                Key::Left,
                Mods::SHIFT,
                Mods::ALL.without(Mods::ALT | Mods::CTRL),
            )],
        ),
        entry(
            "SwitchProjectNext",
            Action::SwitchProject(Selector::Next),
            "Next project",
            vec![trigger(
                Global,
                Key::Right,
                Mods::SHIFT,
                Mods::ALL.without(Mods::ALT | Mods::CTRL),
            )],
        ),
        // Alt-arrows are global so they work with a terminal focused; the bare
        // arrows are an App-mode fallback for terminals (e.g. Warp) that
        // capture Option/Alt+arrows themselves.
        entry(
            "AgentTabPrev",
            dispatch(Command::SwitchAgentTab(Selector::Prev)),
            "Previous Agent Session Tab",
            vec![
                trigger(Global, Key::Up, alt, any),
                trigger(App, Key::Up, exact, exact),
            ],
        ),
        entry(
            "AgentTabNext",
            dispatch(Command::SwitchAgentTab(Selector::Next)),
            "Next Agent Session Tab",
            vec![
                trigger(Global, Key::Down, alt, any),
                trigger(App, Key::Down, exact, exact),
            ],
        ),
        entry(
            "TerminalTabPrev",
            dispatch(Command::SwitchChildTerminal(Selector::Prev)),
            "Previous terminal tab",
            vec![
                trigger(Global, Key::Left, alt, any),
                trigger(App, Key::Left, exact, exact),
            ],
        ),
        entry(
            "TerminalTabNext",
            dispatch(Command::SwitchChildTerminal(Selector::Next)),
            "Next terminal tab",
            vec![
                trigger(Global, Key::Right, alt, any),
                trigger(App, Key::Right, exact, exact),
            ],
        ),
    ];

    for (idx, id) in JUMP_IDS.into_iter().enumerate() {
        let digit = char::from(b'1' + idx as u8);
        entries.push(entry(
            id,
            dispatch(Command::SwitchAgentTab(Selector::Index(idx))),
            "Jump to Agent Session Tab by index",
            vec![trigger(Global, Key::Char(digit), alt, any)],
        ));
    }

    let leave_focus = if options.use_f2_to_leave_focus {
        // F2 has always matched with any modifier held.
        trigger(Terminal, Key::F(2), exact, any)
    } else if options.leave_focus_uses_shift {
        trigger(Terminal, Key::Esc, Mods::SHIFT, exact)
    } else {
        trigger(Terminal, Key::Esc, alt, exact)
    };

    let mut paste = vec![trigger(
        Terminal,
        Key::Char('v'),
        ctrl,
        Mods::ALL.without(Mods::ALT),
    )];
    if options.command_v_pastes {
        paste.push(trigger(
            Terminal,
            Key::Char('v'),
            Mods::SUPER,
            Mods::ALL.without(Mods::ALT | Mods::CTRL),
        ));
    }

    entries.extend([
        // Alt-o is not a standard readline/agent binding, so the PTY loses
        // nothing by it being global.
        entry(
            "OpenWorktreeInFileManager",
            dispatch(Command::OpenWorktreeInFileManager),
            "Open worktree in file manager",
            vec![trigger(
                Global,
                Key::Char('o'),
                alt,
                Mods::ALL.without(Mods::CTRL | Mods::SHIFT),
            )],
        ),
        entry(
            "NewAgentTab",
            dispatch(Command::NewAgentTab {
                name: String::new(),
                agent_key: None,
            }),
            "New Agent Session Tab",
            vec![ctrl_key('n')],
        ),
        entry(
            "PushBranch",
            dispatch(Command::PushBranch { confirm: None }),
            "Push current branch",
            vec![ctrl_key('p')],
        ),
        entry(
            "PullBase",
            dispatch(Command::PullBase),
            "Pull base (git pull --rebase)",
            vec![ctrl_key('u')],
        ),
        entry(
            "FinishLocalMerge",
            dispatch(Command::FinishLocalMerge { confirm: false }),
            "Finish current Agent Session Tab",
            vec![ctrl_key('f')],
        ),
        entry(
            "CloseAgentTab",
            dispatch(Command::CloseAgentTab { action: None }),
            "Close current Agent Session Tab",
            vec![ctrl_key('k')],
        ),
        entry(
            "NewChildTerminal",
            dispatch(Command::NewChildTerminal),
            "New child terminal",
            vec![ctrl_key('t')],
        ),
        entry(
            "CloseChildTerminal",
            dispatch(Command::CloseChildTerminal),
            "Close active child terminal",
            vec![ctrl_key('w')],
        ),
        entry(
            "ToggleSplitView",
            dispatch(Command::ToggleSplitView),
            "Toggle split view (terminals side by side)",
            vec![ctrl_key('b')],
        ),
        // Bare Esc is never claimed: hosted agents use Esc / Esc Esc.
        entry(
            "FocusApp",
            Action::FocusApp,
            "Leave terminal focus / focus app",
            vec![leave_focus],
        ),
        entry(
            "FocusTerminal",
            Action::FocusTerminal,
            "Focus active terminal",
            vec![trigger(App, Key::Enter, exact, exact)],
        ),
        entry(
            "SetManualStatus",
            dispatch(Command::SetManualStatus(None)),
            "Set manual status",
            vec![ctrl_key('s')],
        ),
        entry(
            "RestartAgent",
            dispatch(Command::RestartAgent),
            "Restart primary agent",
            vec![ctrl_key('r')],
        ),
        // The wiring layer turns a clipboard image into a file path for the
        // agent; with no image it falls back to a literal Ctrl-V.
        entry("Paste", Action::Paste, "Paste into the terminal", paste),
    ]);
    entries
}

/// A row documenting `ids`, its keys spelled from their chords (`F1 / Alt-h`).
fn keys_row(
    entries: &[KeymapEntry],
    ids: &[&'static str],
    description: &'static str,
) -> HelpRowSpec {
    let keys = ids
        .iter()
        .flat_map(|id| {
            entries
                .iter()
                .find(|e| e.id == *id)
                .into_iter()
                .flat_map(|e| e.triggers.iter().map(|t| t.chord.to_string()))
        })
        .collect::<Vec<_>>()
        .join(" / ");
    HelpRowSpec {
        keys,
        description,
        entry_ids: ids.to_vec(),
    }
}

/// A row documenting `ids` under a hand-written label, for groups whose chords
/// read better summarised (`Up / Down (or Alt)`, `Alt-1 .. Alt-9`).
fn labelled_row(label: &str, ids: &[&'static str], description: &'static str) -> HelpRowSpec {
    HelpRowSpec {
        keys: label.to_string(),
        description,
        entry_ids: ids.to_vec(),
    }
}

/// A mouse-gesture row: help text, not a binding.
fn gesture_row(gesture: &str, description: &'static str) -> HelpRowSpec {
    HelpRowSpec {
        keys: gesture.to_string(),
        description,
        entry_ids: Vec::new(),
    }
}

/// The help screen's layout. Section titles and wording are the help screen's
/// as shipped; note that "Global" lists some App-mode-only keys (Ctrl-n, …).
fn build_help(options: KeymapOptions, entries: &[KeymapEntry]) -> Vec<HelpSectionSpec> {
    let row = |ids: &[&'static str], description| keys_row(entries, ids, description);
    vec![
        HelpSectionSpec {
            title: "Global",
            rows: vec![
                row(&["OpenPalette"], "Command palette"),
                row(&["Quit"], "Quit / close app"),
                row(&["NewAgentTab"], "New Agent Session Tab"),
                row(&["PushBranch"], "Push current branch"),
                row(&["PullBase"], "Pull base (git pull --rebase)"),
                row(&["FinishLocalMerge"], "Finish current Agent Session Tab"),
                row(&["CloseAgentTab"], "Close current Agent Session Tab"),
                row(
                    &["OpenWorktreeInFileManager"],
                    "Open worktree in file manager",
                ),
                row(&["OpenHelp"], "Help / keybindings"),
            ],
        },
        HelpSectionSpec {
            title: "Projects",
            rows: vec![
                row(
                    &["SwitchProjectPrev", "SwitchProjectNext"],
                    "Previous / Next project",
                ),
                gesture_row("Mouse click", "Switch project (top tab row)"),
                gesture_row("+ project", "Open another project folder"),
            ],
        },
        HelpSectionSpec {
            title: "Agent Session Tab Navigation",
            rows: vec![
                labelled_row(
                    "Up / Down (or Alt)",
                    &["AgentTabPrev", "AgentTabNext"],
                    "Previous / Next Agent Session Tab",
                ),
                labelled_row(
                    "Alt-1 .. Alt-9",
                    &JUMP_IDS,
                    "Jump to Agent Session Tab by index",
                ),
                gesture_row("Mouse click", "Select Agent Session Tab"),
            ],
        },
        HelpSectionSpec {
            title: "Child Terminal Navigation",
            rows: vec![
                row(&["NewChildTerminal"], "New child terminal"),
                row(&["CloseChildTerminal"], "Close active child terminal"),
                labelled_row(
                    "Left / Right (or Alt)",
                    &["TerminalTabPrev", "TerminalTabNext"],
                    "Cycle terminal tabs (agent + shells)",
                ),
                row(
                    &["ToggleSplitView"],
                    "Toggle split view (terminals side by side)",
                ),
                gesture_row("Mouse click", "Select terminal tab"),
            ],
        },
        HelpSectionSpec {
            title: "Selection / Clipboard",
            rows: vec![
                gesture_row("Drag", "Select terminal text (copies on release)"),
                gesture_row("Drag past edge", "Auto-scrolls to reach offscreen text"),
                gesture_row("Shift-drag", "Force selection over a mouse-driven app"),
            ],
        },
        HelpSectionSpec {
            title: "Focus",
            rows: vec![
                labelled_row(
                    leave_focus_label(options),
                    &["FocusApp"],
                    "Leave terminal focus / focus app",
                ),
                row(&["FocusTerminal"], "Focus active terminal"),
            ],
        },
        HelpSectionSpec {
            title: "Status",
            rows: vec![
                row(&["SetManualStatus"], "Set manual status"),
                row(&["RestartAgent"], "Restart primary agent"),
            ],
        },
    ]
}

#[cfg(test)]
mod tests;
