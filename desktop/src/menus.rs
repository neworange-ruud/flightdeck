//! The macOS menu bar, generated from the keymap table.
//!
//! Every chord FlightDeck binds is a GPUI action carrying a keymap entry id
//! (`flightdeck_desktop::keys::KeymapAction`). A menu item is that same
//! action: its label is the entry's own description, its shortcut is drawn by
//! the platform from the key bindings registered from the table, and choosing
//! it dispatches the action to the focused window, where the shell performs
//! it through [`crate::commands::perform_entry`] like the chord, every button
//! and the context menu. There is no second mapping to drift.
//!
//! ## What is not in the table
//!
//! A handful of items are the desktop's own, not chords: About, Settings…,
//! New window, Open project…, Connect to remote… and the GitHub link
//! ([`AppCommand`]). Each is a small
//! action whose handler asks the host for the same thing the palette row
//! does (`Command::ShowAbout`, `PaletteAction::OpenConfig`, the native folder
//! picker answering `HostEvent::OpenProject`).
//!
//! ## Layout
//!
//! ```text
//! FlightDeck  About · Settings… ⌘, · Services · Quit ⌘Q
//! File        New window ⌘N · New agent · New shell · Open project… ⌘O · Connect to remote… · Close session · Close shell · Open worktree
//! View        Toggle split · Command palette · Projects / Agent sessions / Terminals ▸ · Focus
//! Git         Push · Pull base · Finish
//! Agent       Set manual status · Restart
//! Help        Help · FlightDeck on GitHub
//! ```
//!
//! [`LAYOUT`] names table ids; [`menu_model`] resolves them (a test holds
//! every help-listed entry to appearing somewhere, so a new chord cannot be
//! forgotten). Mission control joins View when its view exists.
//!
//! ## Windows and Linux
//!
//! They have no global menu bar, and the window already has three ways to
//! everything: the command palette (Ctrl-g, or the titlebar's Command field)
//! lists every action with its keycap and includes Open Project, Open
//! Configuration and About; F1 is the help screen; the buttons carry the
//! common ones. A hamburger menu in the titlebar would be a fourth copy that
//! also needs titlebar surgery on every platform's layout, so it is
//! deliberately not built: [`install`] sets the menu bar on macOS only, and
//! the model is platform-independent so a later in-window menu reuses it.
//!
//! ## Shortcuts that are platform conventions, not table chords
//!
//! Cmd-, (Settings), Cmd-N (New window), Cmd-O (Open project) and Cmd-Q
//! (Quit, the table's `Quit` action). The Dock icon's menu offers New window
//! too, the usual way to get a second window of a Mac app. Cmd-V pastes and Cmd-C copies in the terminal (the table
//! and the terminal view). **Cmd-W is left unbound**: it must never close a
//! session by muscle memory (that is Ctrl-k), and the window has its own
//! close button.

use flightdeck::app::commands::Command;
use flightdeck::app::keymap::{Keymap, KeymapEntry};
use flightdeck::host::HostEvent;
use flightdeck::tui::palette::{FrontEndAction, PaletteAction};
use flightdeck::tui::platform;
use flightdeck_desktop::keys::KeymapAction;
use gpui::{actions, Action, App, KeyBinding, Menu, MenuItem, SystemMenuType};

use crate::commands::disabled_in;

actions!(
    flightdeck,
    [
        About,
        OpenSettings,
        OpenProjectFolder,
        NewWindow,
        ConnectToRemote,
        OpenGithub
    ]
);

/// The project's home, for the Help menu's link.
pub const GITHUB_URL: &str = "https://github.com/neworange-ruud/flightdeck";

/// An item that is the desktop's own rather than a table chord.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppCommand {
    About,
    Settings,
    OpenProject,
    /// Another instance of the app, on its launcher
    /// ([`flightdeck_desktop::instance`]).
    NewWindow,
    /// FlightDeck Desktop as a remote control (`crate::remote::connect`).
    ConnectRemote,
    GitHub,
}

impl AppCommand {
    fn label(self) -> &'static str {
        match self {
            AppCommand::About => "About FlightDeck",
            AppCommand::Settings => "Settings…",
            AppCommand::OpenProject => "Open project…",
            AppCommand::NewWindow => "New window",
            AppCommand::ConnectRemote => "Connect to remote…",
            AppCommand::GitHub => "FlightDeck on GitHub",
        }
    }

    /// The host input this item stands for, when it is one.
    pub fn host_event(self) -> Option<HostEvent> {
        match self {
            AppCommand::About => Some(HostEvent::Command(Command::ShowAbout)),
            AppCommand::Settings => Some(HostEvent::RunPaletteAction(PaletteAction::OpenConfig)),
            AppCommand::OpenProject
            | AppCommand::NewWindow
            | AppCommand::ConnectRemote
            | AppCommand::GitHub => None,
        }
    }

    fn action(self) -> Box<dyn Action> {
        match self {
            AppCommand::About => Box::new(About),
            AppCommand::Settings => Box::new(OpenSettings),
            AppCommand::OpenProject => Box::new(OpenProjectFolder),
            AppCommand::NewWindow => Box::new(NewWindow),
            AppCommand::ConnectRemote => Box::new(ConnectToRemote),
            AppCommand::GitHub => Box::new(OpenGithub),
        }
    }

    /// The item a palette front-end row stands for: the host queues the row
    /// and the window performs it (`crate::host::HostModel::dispatch`).
    pub fn for_front_end(action: FrontEndAction) -> AppCommand {
        match action {
            FrontEndAction::ConnectToRemote => AppCommand::ConnectRemote,
            FrontEndAction::NewWindow => AppCommand::NewWindow,
        }
    }

    /// It as a menu item, labelled `name`.
    fn item_named(self, name: impl Into<gpui::SharedString>) -> MenuItem {
        MenuItem::Action {
            name: name.into(),
            action: self.action(),
            os_action: None,
            checked: false,
            disabled: false,
        }
    }

    /// It as a menu item under its own label (the Dock menu's).
    fn menu_item(self) -> MenuItem {
        self.item_named(self.label())
    }

    /// Perform it: the same thing wherever it was asked from.
    pub fn perform(self, cx: &mut App) {
        match self {
            AppCommand::OpenProject => crate::root::pick_folder(cx),
            AppCommand::NewWindow => {
                if let Err(e) = flightdeck_desktop::instance::spawn_new_instance() {
                    crate::root::report(format!("Could not open a new window: {e}"), cx);
                }
            }
            AppCommand::ConnectRemote => crate::remote::connect::open_connect_window(None, cx),
            AppCommand::GitHub => cx.open_url(GITHUB_URL),
            other => {
                if let Some(event) = other.host_event() {
                    crate::root::dispatch(event, cx);
                }
            }
        }
    }
}

/// What choosing an item does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// The table entry with this id.
    Entry(&'static str),
    App(AppCommand),
}

/// One resolved menu item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemModel {
    Separator,
    /// The platform's Services submenu (macOS).
    Services,
    Submenu {
        name: &'static str,
        items: Vec<ItemModel>,
    },
    Item {
        label: String,
        target: Target,
        disabled: bool,
    },
}

/// One resolved top-level menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuModel {
    pub name: &'static str,
    pub items: Vec<ItemModel>,
}

/// A layout item, before it is resolved against the table.
enum Slot {
    Sep,
    Services,
    App(AppCommand),
    /// A table entry, by id.
    Entry(&'static str),
    Sub(&'static str, &'static [Slot]),
}

use Slot::{App as AppSlot, Entry, Sep, Services, Sub};

const JUMPS: [Slot; 9] = [
    Entry("JumpToAgentTab1"),
    Entry("JumpToAgentTab2"),
    Entry("JumpToAgentTab3"),
    Entry("JumpToAgentTab4"),
    Entry("JumpToAgentTab5"),
    Entry("JumpToAgentTab6"),
    Entry("JumpToAgentTab7"),
    Entry("JumpToAgentTab8"),
    Entry("JumpToAgentTab9"),
];

/// The menu bar. Ids are the table's; see the module docs.
const LAYOUT: &[(&str, &[Slot])] = &[
    (
        "FlightDeck",
        &[
            AppSlot(AppCommand::About),
            Sep,
            AppSlot(AppCommand::Settings),
            Sep,
            Services,
            Sep,
            Entry("Quit"),
        ],
    ),
    (
        "File",
        &[
            AppSlot(AppCommand::NewWindow),
            Sep,
            Entry("NewAgentTab"),
            Entry("NewChildTerminal"),
            AppSlot(AppCommand::OpenProject),
            AppSlot(AppCommand::ConnectRemote),
            Sep,
            Entry("CloseAgentTab"),
            Entry("CloseChildTerminal"),
            Sep,
            Entry("OpenWorktreeInFileManager"),
        ],
    ),
    (
        "View",
        &[
            Entry("ToggleMissionControl"),
            Entry("ToggleSplitView"),
            Entry("OpenPalette"),
            Sep,
            Sub(
                "Projects",
                &[Entry("SwitchProjectPrev"), Entry("SwitchProjectNext")],
            ),
            Sub(
                "Agent sessions",
                &[Entry("AgentTabPrev"), Entry("AgentTabNext"), Sep],
            ),
            Sub(
                "Terminals",
                &[Entry("TerminalTabPrev"), Entry("TerminalTabNext")],
            ),
            Sep,
            Entry("FocusApp"),
            Entry("FocusTerminal"),
        ],
    ),
    (
        "Git",
        &[
            Entry("PushBranch"),
            Entry("PullBase"),
            Entry("FinishLocalMerge"),
        ],
    ),
    ("Agent", &[Entry("SetManualStatus"), Entry("RestartAgent")]),
    (
        "Help",
        &[Entry("OpenHelp"), Sep, AppSlot(AppCommand::GitHub)],
    ),
];

/// The label for a table entry. Its description, except where the menu wants
/// the app's name or a shorter form.
fn label_for(entry: &KeymapEntry) -> String {
    match entry.id {
        "Quit" => "Quit FlightDeck".to_string(),
        id if id.starts_with("JumpToAgentTab") => {
            format!("Agent session {}", &id["JumpToAgentTab".len()..])
        }
        _ => entry.description.to_string(),
    }
}

fn resolve(slot: &Slot, keymap: &Keymap, isolated: bool) -> Option<ItemModel> {
    Some(match slot {
        Sep => ItemModel::Separator,
        Services => ItemModel::Services,
        AppSlot(command) => ItemModel::Item {
            label: command.label().to_string(),
            target: Target::App(*command),
            // An isolated run opens nothing else (SPECS §32).
            disabled: isolated && *command == AppCommand::OpenProject,
        },
        Entry(id) => {
            let entry = keymap.entry(id)?;
            ItemModel::Item {
                label: label_for(entry),
                target: Target::Entry(entry.id),
                disabled: disabled_in(entry, isolated),
            }
        }
        Sub(name, items) => {
            let mut resolved: Vec<ItemModel> = items
                .iter()
                .filter_map(|s| resolve(s, keymap, isolated))
                .collect();
            // "Agent sessions" ends with the nine jumps, generated once.
            if *name == "Agent sessions" {
                resolved.extend(JUMPS.iter().filter_map(|s| resolve(s, keymap, isolated)));
            }
            ItemModel::Submenu {
                name,
                items: resolved,
            }
        }
    })
}

/// The whole menu bar for `keymap`; `isolated` draws what an isolated run
/// refuses disabled. Platform-independent.
pub fn menu_model(keymap: &Keymap, isolated: bool) -> Vec<MenuModel> {
    LAYOUT
        .iter()
        .map(|(name, slots)| MenuModel {
            name,
            items: slots
                .iter()
                .filter_map(|s| resolve(s, keymap, isolated))
                .collect(),
        })
        .collect()
}

/// Every table entry id a menu item performs, in menu order.
#[cfg(test)]
pub fn entry_ids(menus: &[MenuModel]) -> Vec<&'static str> {
    fn walk(items: &[ItemModel], out: &mut Vec<&'static str>) {
        for item in items {
            match item {
                ItemModel::Item {
                    target: Target::Entry(id),
                    ..
                } => out.push(id),
                ItemModel::Submenu { items, .. } => walk(items, out),
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    for menu in menus {
        walk(&menu.items, &mut out);
    }
    out
}

fn to_gpui_item(item: ItemModel, keymap: &Keymap) -> Option<MenuItem> {
    Some(match item {
        ItemModel::Separator => MenuItem::separator(),
        ItemModel::Services => MenuItem::os_submenu("Services", SystemMenuType::Services),
        ItemModel::Submenu { name, items } => MenuItem::submenu(
            Menu::new(name).items(
                items
                    .into_iter()
                    .filter_map(|i| to_gpui_item(i, keymap))
                    .collect::<Vec<_>>(),
            ),
        ),
        ItemModel::Item {
            label,
            target,
            disabled,
        } => {
            let base = match target {
                Target::Entry(id) => {
                    MenuItem::action(label, KeymapAction::for_entry(keymap.entry(id)?))
                }
                Target::App(command) => command.item_named(label),
            };
            base.disabled(disabled)
        }
    })
}

/// The model as GPUI menus.
pub fn to_gpui(models: Vec<MenuModel>, keymap: &Keymap) -> Vec<Menu> {
    models
        .into_iter()
        .map(|menu| {
            Menu::new(menu.name).items(
                menu.items
                    .into_iter()
                    .filter_map(|i| to_gpui_item(i, keymap))
                    .collect::<Vec<_>>(),
            )
        })
        .collect()
}

/// The platform-convention key bindings (see the module docs): none outside
/// macOS, where Ctrl-q and the table's chords are the shortcuts.
pub fn platform_bindings(keymap: &Keymap) -> Vec<KeyBinding> {
    if !platform::IS_MACOS {
        return Vec::new();
    }
    let mut bindings = vec![
        KeyBinding::new("cmd-,", OpenSettings, None),
        KeyBinding::new("cmd-n", NewWindow, None),
        KeyBinding::new("cmd-o", OpenProjectFolder, None),
    ];
    if let Some(quit) = keymap.entry("Quit") {
        bindings.push(KeyBinding::new(
            "cmd-q",
            KeymapAction::for_entry(quit),
            None,
        ));
    }
    bindings
}

/// Register the desktop's own actions and the platform bindings, and set the
/// menu bar where the OS has one. Call once at start-up, after the table's
/// bindings.
pub fn install(cx: &mut App, keymap: &'static Keymap, isolated: bool) {
    register_actions(cx);
    cx.bind_keys(platform_bindings(keymap));
    if platform::IS_MACOS {
        cx.set_menus(to_gpui(menu_model(keymap, isolated), keymap));
        cx.set_dock_menu(vec![AppCommand::NewWindow.menu_item()]);
    }
}

/// Handlers for the desktop's own actions. App-level: they work with the
/// launcher up and no window focused on a project.
pub fn register_actions(cx: &mut App) {
    cx.on_action(|_: &About, cx| AppCommand::About.perform(cx));
    cx.on_action(|_: &OpenSettings, cx| AppCommand::Settings.perform(cx));
    cx.on_action(|_: &OpenProjectFolder, cx| AppCommand::OpenProject.perform(cx));
    cx.on_action(|_: &NewWindow, cx| AppCommand::NewWindow.perform(cx));
    cx.on_action(|_: &ConnectToRemote, cx| AppCommand::ConnectRemote.perform(cx));
    cx.on_action(|_: &OpenGithub, cx| AppCommand::GitHub.perform(cx));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{intent_for, keymap_for, Intent};
    use std::collections::HashSet;

    fn find(items: &[ItemModel], target: Target) -> Option<&ItemModel> {
        items.iter().find_map(|item| match item {
            ItemModel::Item { target: t, .. } if *t == target => Some(item),
            ItemModel::Submenu { items, .. } => find(items, target),
            _ => None,
        })
    }

    fn find_in(menus: &[MenuModel], target: Target) -> Option<&ItemModel> {
        menus.iter().find_map(|m| find(&m.items, target))
    }

    #[test]
    fn every_menu_item_names_a_table_entry() {
        let keymap = keymap_for(false);
        for id in entry_ids(&menu_model(keymap, false)) {
            assert!(keymap.entry(id).is_some(), "{id} is not in the table");
        }
        // Nothing was dropped for a missing id: every table slot resolved.
        fn entry_slots(list: &[Slot]) -> usize {
            list.iter()
                .map(|s| match s {
                    Slot::Entry(_) => 1,
                    Slot::Sub(_, inner) => entry_slots(inner),
                    _ => 0,
                })
                .sum()
        }
        let declared: usize = LAYOUT.iter().map(|(_, s)| entry_slots(s)).sum();
        let resolved = entry_ids(&menu_model(keymap, false)).len();
        assert_eq!(resolved, declared + JUMPS.len());
    }

    #[test]
    fn every_listed_action_is_in_a_menu() {
        // The help screen lists what a user can do; the menu must too.
        let keymap = keymap_for(false);
        let in_menu: HashSet<&str> = entry_ids(&menu_model(keymap, false)).into_iter().collect();
        for entry in keymap.entries().iter().filter(|e| e.help_section.is_some()) {
            assert!(in_menu.contains(entry.id), "{} is in no menu", entry.id);
        }
    }

    #[test]
    fn no_entry_is_in_two_menus() {
        let ids = entry_ids(&menu_model(keymap_for(false), false));
        let unique: HashSet<_> = ids.iter().collect();
        assert_eq!(ids.len(), unique.len());
    }

    #[test]
    fn labels_come_from_the_table() {
        let keymap = keymap_for(false);
        let menus = menu_model(keymap, false);
        let label = |id: &'static str| match find_in(&menus, Target::Entry(id)) {
            Some(ItemModel::Item { label, .. }) => label.clone(),
            other => panic!("{id}: {other:?}"),
        };
        for id in [
            "NewAgentTab",
            "PushBranch",
            "PullBase",
            "ToggleSplitView",
            "OpenHelp",
        ] {
            assert_eq!(label(id), keymap.entry(id).unwrap().description, "{id}");
        }
        assert_eq!(label("Quit"), "Quit FlightDeck");
        assert_eq!(label("JumpToAgentTab3"), "Agent session 3");
    }

    #[test]
    fn the_requested_menus_and_items_exist() {
        let menus = menu_model(keymap_for(false), false);
        let names: Vec<_> = menus.iter().map(|m| m.name).collect();
        assert_eq!(
            names,
            ["FlightDeck", "File", "View", "Git", "Agent", "Help"]
        );
        for target in [
            Target::App(AppCommand::About),
            Target::App(AppCommand::Settings),
            Target::Entry("Quit"),
            Target::Entry("NewAgentTab"),
            Target::Entry("NewChildTerminal"),
            Target::App(AppCommand::OpenProject),
            Target::App(AppCommand::NewWindow),
            Target::App(AppCommand::ConnectRemote),
            Target::Entry("CloseAgentTab"),
            Target::Entry("ToggleSplitView"),
            Target::Entry("OpenPalette"),
            Target::Entry("PushBranch"),
            Target::Entry("PullBase"),
            Target::Entry("FinishLocalMerge"),
            Target::Entry("OpenHelp"),
            Target::App(AppCommand::GitHub),
        ] {
            assert!(find_in(&menus, target).is_some(), "{target:?}");
        }
    }

    #[test]
    fn an_isolated_run_disables_new_agent_projects_and_open_project() {
        let keymap = keymap_for(false);
        let disabled = |menus: &[MenuModel]| -> Vec<Target> {
            fn walk(items: &[ItemModel], out: &mut Vec<Target>) {
                for item in items {
                    match item {
                        ItemModel::Item {
                            target,
                            disabled: true,
                            ..
                        } => out.push(*target),
                        ItemModel::Submenu { items, .. } => walk(items, out),
                        _ => {}
                    }
                }
            }
            let mut out = Vec::new();
            for m in menus {
                walk(&m.items, &mut out);
            }
            out
        };
        assert!(disabled(&menu_model(keymap, false)).is_empty());
        assert_eq!(
            disabled(&menu_model(keymap, true)),
            // In menu order.
            [
                Target::Entry("NewAgentTab"),
                Target::App(AppCommand::OpenProject),
                Target::Entry("SwitchProjectPrev"),
                Target::Entry("SwitchProjectNext"),
            ]
        );
    }

    #[test]
    fn the_desktops_own_items_map_to_the_palettes_rows() {
        assert_eq!(
            AppCommand::About.host_event(),
            Some(HostEvent::Command(Command::ShowAbout))
        );
        assert_eq!(
            AppCommand::Settings.host_event(),
            Some(HostEvent::RunPaletteAction(PaletteAction::OpenConfig))
        );
        assert!(AppCommand::OpenProject.host_event().is_none());
    }

    #[test]
    fn the_palettes_front_end_rows_are_the_desktops_own_items() {
        assert_eq!(
            AppCommand::for_front_end(FrontEndAction::ConnectToRemote),
            AppCommand::ConnectRemote
        );
        assert_eq!(
            AppCommand::for_front_end(FrontEndAction::NewWindow),
            AppCommand::NewWindow
        );
        // Performed by the window, never sent to the host.
        for row in flightdeck::tui::palette::front_end_entries() {
            let PaletteAction::FrontEnd(action) = row.action else {
                panic!("{} is not a front-end row", row.label);
            };
            assert!(AppCommand::for_front_end(action).host_event().is_none());
        }
    }

    #[test]
    fn new_window_is_cmd_n_on_macos() {
        let bindings = platform_bindings(keymap_for(false));
        if platform::IS_MACOS {
            let cmd_n = [gpui::Keystroke::parse("cmd-n").unwrap()];
            assert!(bindings.iter().any(|b| {
                // `Some(false)`: a complete match, not a prefix of a chord.
                b.match_keystrokes(&cmd_n) == Some(false) && b.action().name() == NewWindow.name()
            }));
        } else {
            assert!(bindings.is_empty());
        }
    }

    #[test]
    fn every_entry_item_performs_the_tables_intent() {
        // The mapping a menu item and a chord share.
        let keymap = keymap_for(false);
        for id in entry_ids(&menu_model(keymap, false)) {
            let entry = keymap.entry(id).unwrap();
            assert!(matches!(
                intent_for(&entry.action),
                Intent::Host(_) | Intent::PasteClipboard | Intent::ToggleMainView
            ));
        }
    }

    #[test]
    fn the_gpui_menus_carry_the_disabled_flag() {
        let keymap = keymap_for(false);
        let menus = to_gpui(menu_model(keymap, true), keymap);
        fn any_disabled(items: &[MenuItem]) -> bool {
            items.iter().any(|i| match i {
                MenuItem::Action { disabled, .. } => *disabled,
                MenuItem::Submenu(m) => any_disabled(&m.items),
                _ => false,
            })
        }
        assert!(menus.iter().any(|m| any_disabled(&m.items)));
        let live = to_gpui(menu_model(keymap, false), keymap);
        assert!(!live.iter().any(|m| any_disabled(&m.items)));
    }
}
