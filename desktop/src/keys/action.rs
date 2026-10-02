//! One GPUI action per keymap entry, and the key bindings for all of them.
//!
//! GPUI dispatches actions by Rust type, and its `actions!` macro wants one
//! type per action named in source — a hand-written list, which is exactly
//! what the one-table design forbids. So there is a single action type,
//! [`KeymapAction`], carrying the entry id, with a hand-written `Action` impl
//! whose [`Action::name`] is the entry's own
//! [`gpui_action_name`](flightdeck::app::keymap::KeymapEntry::gpui_action_name)
//! (`flightdeck::OpenPalette`). One `on_action::<KeymapAction>` handler then
//! serves every entry, and GPUI's per-action queries (`keystroke_text_for`,
//! `bindings_for_action`) still tell entries apart through
//! [`Action::partial_eq`], which compares ids.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use flightdeck::app::keymap::{Context, Keymap, KeymapEntry};
use gpui::{Action, KeyBinding};

use super::keystroke::chord_to_gpui;

/// The GPUI action for one keymap entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeymapAction {
    id: &'static str,
    name: &'static str,
}

impl KeymapAction {
    /// The action performing `entry`.
    pub fn for_entry(entry: &KeymapEntry) -> KeymapAction {
        KeymapAction {
            id: entry.id,
            name: action_name(entry),
        }
    }

    /// The [`KeymapEntry::id`] this action performs.
    pub fn id(&self) -> &'static str {
        self.id
    }

    /// The entry this action performs, in `keymap`.
    pub fn entry<'k>(&self, keymap: &'k Keymap) -> Option<&'k KeymapEntry> {
        keymap.entry(self.id)
    }
}

/// `flightdeck::<id>` as a `&'static str`, which [`Action::name`] requires.
///
/// Interned: each id's name is allocated once and kept for the life of the
/// process. The table has a few dozen fixed ids, so this is bounded.
fn action_name(entry: &KeymapEntry) -> &'static str {
    static NAMES: OnceLock<Mutex<HashMap<&'static str, &'static str>>> = OnceLock::new();
    let mut names = NAMES
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    names
        .entry(entry.id)
        .or_insert_with(|| Box::leak(entry.gpui_action_name().into_boxed_str()))
}

impl Action for KeymapAction {
    fn boxed_clone(&self) -> Box<dyn Action> {
        Box::new(*self)
    }

    fn partial_eq(&self, action: &dyn Action) -> bool {
        action
            .as_any()
            .downcast_ref::<KeymapAction>()
            .is_some_and(|other| other.id == self.id)
    }

    fn name(&self) -> &'static str {
        self.name
    }

    fn name_for_type() -> &'static str {
        // The type-level name, for GPUI's registry; instances report the
        // entry-specific name above. Same namespace as theirs
        // (`keymap::ACTION_NAMESPACE`; a test holds the two together).
        "flightdeck::KeymapAction"
    }

    /// Build from `{"id": "OpenPalette"}`. Ids are the same in every variant
    /// of the table (the options only change chords), so any table resolves.
    fn build(value: serde_json::Value) -> anyhow::Result<Box<dyn Action>> {
        let id = value
            .get("id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("KeymapAction needs {{\"id\": \"<entry id>\"}}"))?;
        let entry = Keymap::for_this_platform(false)
            .entry(id)
            .ok_or_else(|| anyhow::anyhow!("no keymap entry with id {id:?}"))?;
        Ok(Box::new(KeymapAction::for_entry(entry)))
    }
}

/// One GPUI binding the table produces, before it becomes a [`KeyBinding`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BindingSpec {
    /// The [`KeymapEntry::id`].
    pub entry_id: &'static str,
    /// The GPUI keystroke string (`ctrl-g`).
    pub keystroke: String,
    /// The GPUI key context predicate: the table context's name.
    pub context: &'static str,
}

/// Every binding in `keymap`, in table order per context (Global, Terminal,
/// App).
///
/// Each trigger is bound as its exact chord. A trigger's `tolerate` set (the
/// TUI's leniency, e.g. Ctrl-Shift-g still opening the palette) is not
/// expanded into extra bindings; the key-down fallbacks
/// ([`super::terminal_key_down`], [`super::app_key_down`]) apply it instead,
/// so Cmd chords are never claimed by accident.
///
/// A chord with no GPUI spelling (Hyper/Meta) is skipped; the table has none.
pub fn binding_specs(keymap: &Keymap) -> Vec<BindingSpec> {
    [Context::Global, Context::Terminal, Context::App]
        .into_iter()
        .flat_map(|context| {
            keymap
                .bindings_in(context)
                .filter_map(move |(entry, trigger)| {
                    Some(BindingSpec {
                        entry_id: entry.id,
                        keystroke: chord_to_gpui(trigger.chord)?,
                        context: context.name(),
                    })
                })
        })
        .collect()
}

/// [`binding_specs`] as GPUI key bindings, ready for `cx.bind_keys`.
pub fn key_bindings(keymap: &Keymap) -> Vec<KeyBinding> {
    binding_specs(keymap)
        .into_iter()
        .filter_map(|spec| {
            let entry = keymap.entry(spec.entry_id)?;
            Some(KeyBinding::new(
                &spec.keystroke,
                KeymapAction::for_entry(entry),
                Some(spec.context),
            ))
        })
        .collect()
}
