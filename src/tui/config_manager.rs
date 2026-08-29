//! Configuration manager model (SPECS §8): a small, pure editor for the common
//! settings, layered over the global base and per-project overrides.
//!
//! This is a headless data model — no I/O, no rendering. The wiring layer reads
//! the raw config files, builds a [`ConfigManager`], mutates it in response to
//! keys, and writes back the strings it produces ([`ConfigManager::outputs`]).
//! Rendering lives in `render.rs`.
//!
//! The manager exposes a curated set of frequently-changed toggles/choices. The
//! full surface (containers, agents, git, …) is edited by opening the raw
//! `config.toml` in `$EDITOR`. Two scopes are editable: the per-user **Global**
//! base (`~/.flightdeck/config.toml`) and the active **Project** override
//! (`<repo>/.flightdeck/config.toml`). A project value only needs to store what
//! it changes, so editing in Project scope writes a single override key and
//! leaves everything else inherited (SPECS §8).

use crate::config::load::{serialize_global_table, serialize_project_table};
use crate::contracts::{Config, Result};
use std::path::PathBuf;

/// Which config layer the manager is currently editing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigScope {
    /// The per-user global base (`~/.flightdeck/config.toml`).
    Global,
    /// The active project's override (`<repo>/.flightdeck/config.toml`).
    Project,
}

/// The kind of a curated field: a boolean toggle, a fixed set of choices, or a
/// free-text value the user types (e.g. the relay URL).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldKind {
    Bool,
    Choice(Vec<String>),
    Text,
}

/// One curated, editable setting: a label plus the TOML `section.key` it maps to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CuratedField {
    pub label: &'static str,
    pub section: &'static str,
    pub key: &'static str,
    pub kind: FieldKind,
}

/// Where a displayed value comes from, given the current scope (SPECS §8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Explicitly set in the scope currently being edited (an override).
    SetHere,
    /// Inherited from the global base (only possible in Project scope).
    Global,
    /// Falling back to the shipped default (no global or project value).
    Default,
}

impl Origin {
    /// Short label shown next to a value.
    pub fn label(self) -> &'static str {
        match self {
            Origin::SetHere => "set here",
            Origin::Global => "from global",
            Origin::Default => "default",
        }
    }
}

/// A value a caller wants to put in a field: a boolean or a string, which is
/// every shape [`FieldKind`] admits.
///
/// The desktop never builds one — its keys *cycle* a field rather than naming a
/// value — but FlightDeck Web's configuration manager does, because a browser
/// stages its edits and sends the values it staged
/// (`specs/WEB_INTERFACE.md` §6.5 R22). [`ConfigManager::set_selected`] is the
/// one door both surfaces write through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldValue {
    Bool(bool),
    Text(String),
}

/// A render-ready view of one curated field for the current scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigRow {
    /// The TOML path this row writes, as `section.key` (`notifications.enabled`).
    ///
    /// The desktop addresses a row by its cursor position and never needs this;
    /// a browser addresses one by name, and this is the name — read off the
    /// same [`CuratedField`] the write goes to, so the two cannot drift
    /// (`specs/WEB_INTERFACE.md` §6.5 R22).
    pub key: String,
    pub label: String,
    /// Display value (`on`/`off` for a bool, the choice string otherwise).
    pub value: String,
    pub origin: Origin,
    pub selected: bool,
    /// True for a boolean toggle (rendered as a checkbox).
    pub is_bool: bool,
    /// The boolean state when `is_bool` (ignored otherwise).
    pub bool_value: bool,
    /// True for a free-text field (rendered as an editable value).
    pub is_text: bool,
    /// True when this text field is currently being edited (`value` holds the
    /// in-progress buffer, which a renderer may show with a cursor).
    pub editing: bool,
    /// For a [`FieldKind::Choice`] field, the legal values in cycle order;
    /// empty for a bool or a text field.
    ///
    /// The desktop cycles them itself and never reads this. A browser cannot:
    /// the agent list is built from the live config, so a browser that shipped
    /// its own would be offering agents this host does not have.
    pub choices: Vec<String>,
}

/// The configuration manager model (SPECS §8).
#[derive(Debug, Clone)]
pub struct ConfigManager {
    scope: ConfigScope,
    project_name: String,
    global_path: Option<PathBuf>,
    project_path: PathBuf,
    /// Raw global table (the full documented base as read from disk).
    global: toml::Table,
    /// Raw project table (only the values this project overrides).
    project: toml::Table,
    /// Shipped defaults, as a table, for showing inherited fallbacks.
    defaults: toml::Table,
    fields: Vec<CuratedField>,
    selected: usize,
    global_dirty: bool,
    project_dirty: bool,
    /// When a [`FieldKind::Text`] field is being edited, its in-progress buffer.
    /// `None` means no text field is currently open for editing.
    editing: Option<String>,
    /// Transient status line (e.g. "Saved.").
    status: Option<String>,
}

impl ConfigManager {
    /// Build a manager. `global`/`project` are the raw tables read from disk
    /// (either may be empty); `agent_keys` are the effective agent keys used to
    /// populate the "default agent" choice. Opens in Project scope — the most
    /// common target — falling back to Global when there is no project file yet.
    pub fn new(
        project_name: impl Into<String>,
        global_path: Option<PathBuf>,
        project_path: impl Into<PathBuf>,
        global: toml::Table,
        project: toml::Table,
        agent_keys: Vec<String>,
    ) -> Self {
        let defaults = toml::Value::try_from(Config::default())
            .ok()
            .and_then(|v| v.as_table().cloned())
            .unwrap_or_default();
        ConfigManager {
            scope: ConfigScope::Project,
            project_name: project_name.into(),
            global_path,
            project_path: project_path.into(),
            global,
            project,
            defaults,
            fields: build_fields(agent_keys),
            selected: 0,
            global_dirty: false,
            project_dirty: false,
            editing: None,
            status: None,
        }
    }

    pub fn scope(&self) -> ConfigScope {
        self.scope
    }

    pub fn project_name(&self) -> &str {
        &self.project_name
    }

    pub fn selected_index(&self) -> usize {
        self.selected
    }

    pub fn status(&self) -> Option<&str> {
        self.status.as_deref()
    }

    /// Whether either scope has unsaved edits.
    pub fn dirty(&self) -> bool {
        self.global_dirty || self.project_dirty
    }

    /// The path of the file for the current scope (for the header / `$EDITOR`).
    /// `None` only for Global scope when there is no home dir.
    pub fn current_path(&self) -> Option<PathBuf> {
        match self.scope {
            ConfigScope::Global => self.global_path.clone(),
            ConfigScope::Project => Some(self.project_path.clone()),
        }
    }

    /// Move the selection down one row (wraps). Discards any open text edit.
    pub fn select_next(&mut self) {
        self.editing = None;
        if !self.fields.is_empty() {
            self.selected = (self.selected + 1) % self.fields.len();
        }
    }

    /// Move the selection up one row (wraps). Discards any open text edit.
    pub fn select_prev(&mut self) {
        self.editing = None;
        let len = self.fields.len();
        if len > 0 {
            self.selected = (self.selected + len - 1) % len;
        }
    }

    /// Switch between Global and Project scope, clamping the selection.
    pub fn switch_scope(&mut self) {
        self.editing = None;
        self.scope = match self.scope {
            ConfigScope::Global => ConfigScope::Project,
            ConfigScope::Project => ConfigScope::Global,
        };
        self.status = None;
    }

    /// Render-ready rows for the current scope.
    pub fn rows(&self) -> Vec<ConfigRow> {
        self.fields
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let (value, origin) = self.effective(f);
                let editing_this = i == self.selected && self.editing.is_some();
                let (display, is_bool, bool_value, is_text) = match &f.kind {
                    FieldKind::Bool => {
                        let b = value.as_bool().unwrap_or(false);
                        ((if b { "on" } else { "off" }).to_string(), true, b, false)
                    }
                    FieldKind::Choice(_) => (
                        value.as_str().unwrap_or("").to_string(),
                        false,
                        false,
                        false,
                    ),
                    FieldKind::Text => {
                        let display = if editing_this {
                            self.editing.clone().unwrap_or_default()
                        } else {
                            value.as_str().unwrap_or("").to_string()
                        };
                        (display, false, false, true)
                    }
                };
                ConfigRow {
                    key: format!("{}.{}", f.section, f.key),
                    label: f.label.to_string(),
                    value: display,
                    origin,
                    selected: i == self.selected,
                    is_bool,
                    bool_value,
                    is_text,
                    editing: editing_this,
                    choices: match &f.kind {
                        FieldKind::Choice(options) => options.clone(),
                        FieldKind::Bool | FieldKind::Text => Vec::new(),
                    },
                }
            })
            .collect()
    }

    /// The rows again, as they would read if every override in the current
    /// scope were cleared — field by field, exactly what `c` leaves behind.
    ///
    /// The desktop never needs this: it clears and re-renders. FlightDeck Web
    /// stages a clear locally and has to show the result *before* it is saved,
    /// and the honest way to do that is to ask this model rather than to walk
    /// the layers a second time in a browser
    /// (`specs/WEB_INTERFACE.md` §6.5 R22). So it is one `rows()` call over a
    /// probe copy with the curated keys removed: no second resolution order,
    /// and nothing here that could disagree with [`Self::effective`].
    pub fn inherited_rows(&self) -> Vec<ConfigRow> {
        let mut probe = self.clone();
        probe.editing = None;
        for field in &self.fields {
            if let Some(toml::Value::Table(section)) =
                probe.scope_table_mut().get_mut(field.section)
            {
                section.remove(field.key);
            }
        }
        probe.rows()
    }

    /// Toggle a boolean or advance a choice for the selected field, writing the
    /// new value into the current scope as an explicit override.
    ///
    /// The *choosing* is here; the *writing* is [`Self::set_selected`], which
    /// both surfaces go through — so a value a browser names and a value the
    /// desktop cycles to land in the config file by the same route.
    pub fn toggle_selected(&mut self) {
        let Some(field) = self.fields.get(self.selected).cloned() else {
            return;
        };
        let (current, _) = self.effective(&field);
        let next = match &field.kind {
            FieldKind::Bool => FieldValue::Bool(!current.as_bool().unwrap_or(false)),
            FieldKind::Choice(options) if !options.is_empty() => {
                let cur = current.as_str().unwrap_or("");
                let idx = options.iter().position(|o| o == cur).unwrap_or(0);
                FieldValue::Text(options[(idx + 1) % options.len()].clone())
            }
            FieldKind::Choice(_) => return,
            // A text field is not toggled — activating it opens an inline editor
            // seeded with the current effective value. The edit is committed by
            // [`Self::commit_edit`] and discarded by [`Self::cancel_edit`].
            FieldKind::Text => {
                self.editing = Some(current.as_str().unwrap_or("").to_string());
                self.status = None;
                return;
            }
        };
        // Cycling can only produce a value the field's own kind admits, so this
        // cannot fail; the `Result` is for the caller that names a value.
        let _ = self.set_selected(next);
    }

    /// Write an explicit value into the selected field as an override in the
    /// current scope.
    ///
    /// Refuses, in words naming what the field accepts, when the value does not
    /// fit the field's kind. That check is here rather than at the caller
    /// because this model owns the field table: a `[ui] mode_border = "purple"`
    /// written from a browser would be a config file the desktop then fails to
    /// load, and the only place that knows `mode_border`'s four options is the
    /// same place that knows its TOML path.
    pub fn set_selected(&mut self, value: FieldValue) -> std::result::Result<(), String> {
        let Some(field) = self.fields.get(self.selected).cloned() else {
            return Err("no field is selected.".to_string());
        };
        let written = match (&field.kind, value) {
            (FieldKind::Bool, FieldValue::Bool(b)) => toml::Value::Boolean(b),
            (FieldKind::Bool, FieldValue::Text(_)) => {
                return Err(format!(
                    "`{}` is a toggle: it takes true or false.",
                    field.label
                ));
            }
            (FieldKind::Choice(options), FieldValue::Text(s)) if options.contains(&s) => {
                toml::Value::String(s)
            }
            (FieldKind::Choice(options), _) => {
                return Err(format!(
                    "`{}` is one of: {}.",
                    field.label,
                    options.join(", ")
                ));
            }
            (FieldKind::Text, FieldValue::Text(s)) => toml::Value::String(s),
            (FieldKind::Text, FieldValue::Bool(_)) => {
                return Err(format!("`{}` is a text value.", field.label));
            }
        };
        set_value(self.scope_table_mut(), field.section, field.key, written);
        self.mark_dirty();
        Ok(())
    }

    /// Move the selection onto the field with this `section.key` path, if this
    /// build has one. `false` means it does not — which is how a browser built
    /// against a different FlightDeck is told so rather than silently writing
    /// the wrong row.
    pub fn select_key(&mut self, key: &str) -> bool {
        let found = self
            .fields
            .iter()
            .position(|f| format!("{}.{}", f.section, f.key) == key);
        match found {
            Some(i) => {
                self.editing = None;
                self.selected = i;
                true
            }
            None => false,
        }
    }

    /// Edit `scope` from now on. [`Self::switch_scope`] is the desktop's `Tab`;
    /// this is for a caller that already knows which scope it means.
    pub fn set_scope(&mut self, scope: ConfigScope) {
        if scope != self.scope {
            self.switch_scope();
        }
    }

    /// Whether an inline text edit is currently open.
    pub fn is_editing(&self) -> bool {
        self.editing.is_some()
    }

    /// Append a typed character to the open text edit buffer (no-op if not
    /// editing).
    pub fn edit_push_char(&mut self, c: char) {
        if let Some(buf) = self.editing.as_mut() {
            buf.push(c);
        }
    }

    /// Delete the last character of the open text edit buffer (no-op if not
    /// editing or already empty).
    pub fn edit_backspace(&mut self) {
        if let Some(buf) = self.editing.as_mut() {
            buf.pop();
        }
    }

    /// Discard the open text edit without changing any value.
    pub fn cancel_edit(&mut self) {
        self.editing = None;
    }

    /// Commit the open text edit into the current scope as an explicit override.
    pub fn commit_edit(&mut self) {
        let Some(buf) = self.editing.take() else {
            return;
        };
        let Some(field) = self.fields.get(self.selected).cloned() else {
            return;
        };
        set_value(
            self.scope_table_mut(),
            field.section,
            field.key,
            toml::Value::String(buf),
        );
        self.mark_dirty();
    }

    /// Clear the selected field's override in the current scope, reverting it to
    /// the inherited (global, then default) value. Prunes a section left empty
    /// (except the project-identity `[project]` section).
    pub fn clear_selected(&mut self) {
        let Some(field) = self.fields.get(self.selected).cloned() else {
            return;
        };
        let table = self.scope_table_mut();
        if let Some(toml::Value::Table(section)) = table.get_mut(field.section) {
            section.remove(field.key);
            if section.is_empty() && field.section != "project" {
                table.remove(field.section);
            }
        }
        self.mark_dirty();
    }

    /// The files to write for the dirty scopes, as `(path, contents)` pairs. A
    /// Global scope with no home dir is skipped.
    pub fn outputs(&self) -> Result<Vec<(PathBuf, String)>> {
        let mut out = Vec::new();
        if self.global_dirty {
            if let Some(path) = &self.global_path {
                out.push((path.clone(), serialize_global_table(&self.global)?));
            }
        }
        if self.project_dirty {
            out.push((
                self.project_path.clone(),
                serialize_project_table(&self.project)?,
            ));
        }
        Ok(out)
    }

    /// Mark both scopes clean and record a status message after a successful save.
    pub fn mark_saved(&mut self) {
        self.global_dirty = false;
        self.project_dirty = false;
        self.status = Some("Saved.".to_string());
    }

    // --- internals ---------------------------------------------------------

    fn scope_table(&self) -> &toml::Table {
        match self.scope {
            ConfigScope::Global => &self.global,
            ConfigScope::Project => &self.project,
        }
    }

    fn scope_table_mut(&mut self) -> &mut toml::Table {
        match self.scope {
            ConfigScope::Global => &mut self.global,
            ConfigScope::Project => &mut self.project,
        }
    }

    fn mark_dirty(&mut self) {
        match self.scope {
            ConfigScope::Global => self.global_dirty = true,
            ConfigScope::Project => self.project_dirty = true,
        }
        self.status = None;
    }

    /// The effective value of `field` for the current scope, and where it comes
    /// from: the scope's own override, else (Project scope) the global base,
    /// else the shipped default.
    fn effective(&self, field: &CuratedField) -> (toml::Value, Origin) {
        if let Some(v) = get_value(self.scope_table(), field.section, field.key) {
            return (v.clone(), Origin::SetHere);
        }
        if self.scope == ConfigScope::Project {
            if let Some(v) = get_value(&self.global, field.section, field.key) {
                return (v.clone(), Origin::Global);
            }
        }
        let fallback = get_value(&self.defaults, field.section, field.key)
            .cloned()
            .unwrap_or(toml::Value::Boolean(false));
        (fallback, Origin::Default)
    }
}

/// The curated field list. `agent_keys` populates the "default agent" choice.
fn build_fields(agent_keys: Vec<String>) -> Vec<CuratedField> {
    let b = |label, section, key| CuratedField {
        label,
        section,
        key,
        kind: FieldKind::Bool,
    };
    vec![
        b("OS notifications", "notifications", "enabled"),
        b("Notification sounds", "notifications", "sound"),
        b("Notify when finished", "notifications", "on_finish"),
        b("Notify when waiting", "notifications", "on_waiting"),
        b("Notify when failed", "notifications", "on_failed"),
        b("Check for updates", "update", "check"),
        b(
            "Use F2 to leave terminal focus",
            "ui",
            "use_f2_to_leave_terminal_focus",
        ),
        CuratedField {
            label: "Agent tab position",
            section: "ui",
            key: "agent_tab_position",
            kind: FieldKind::Choice(vec!["left".to_string(), "right".to_string()]),
        },
        CuratedField {
            label: "Default agent",
            section: "ui",
            key: "default_agent",
            kind: FieldKind::Choice(agent_keys),
        },
        CuratedField {
            label: "Terminal mode color",
            section: "ui",
            key: "terminal_mode_color",
            kind: FieldKind::Choice(
                ["green", "cyan", "blue", "magenta", "yellow", "red", "white"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
            ),
        },
        CuratedField {
            label: "App mode color",
            section: "ui",
            key: "app_mode_color",
            kind: FieldKind::Choice(
                ["green", "cyan", "blue", "magenta", "yellow", "red", "white"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
            ),
        },
        CuratedField {
            label: "Mode border",
            section: "ui",
            key: "mode_border",
            kind: FieldKind::Choice(
                ["off", "dim", "normal", "bright"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
            ),
        },
        b("Dim terminal in app mode", "ui", "dim_terminal_in_app_mode"),
        // FlightDeck Remote (phone link). The relay URL is free-text so a user
        // can point it at a relay they host themselves — the default relay is
        // restricted and not publicly usable (surfaced as a note in the UI).
        b("FlightDeck Remote (phone link)", "remote", "enabled"),
        CuratedField {
            label: "Relay URL",
            section: "remote",
            key: "relay_url",
            kind: FieldKind::Text,
        },
        // FlightDeck Web (embedded browser server, specs/WEB_INTERFACE.md D10),
        // sitting beside the FlightDeck Remote row above. Only the toggle and
        // the (string) bind address are curated here: `port` and
        // `replay_bytes` are numeric and this manager's `FieldKind::Text`
        // always commits a TOML *string* (see `commit_edit`), which is right
        // for `relay_url`/`bind` but would corrupt a `u16`/`usize` field on
        // save (`port = "7420"` fails to deserialize). Those two stay
        // raw-TOML-only, edited via `e` ($EDITOR), same as the rest of the
        // full surface (containers, agents, git, ...).
        b("Auto-start Web Interface", "web", "enabled"),
        CuratedField {
            label: "Web interface bind address",
            section: "web",
            key: "bind",
            kind: FieldKind::Text,
        },
    ]
}

/// Read `section.key` from a raw table, if present.
fn get_value<'a>(table: &'a toml::Table, section: &str, key: &str) -> Option<&'a toml::Value> {
    table.get(section)?.as_table()?.get(key)
}

/// Write `section.key = value` into a raw table, creating the section if needed.
fn set_value(table: &mut toml::Table, section: &str, key: &str, value: toml::Value) {
    let entry = table
        .entry(section.to_string())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    if let toml::Value::Table(t) = entry {
        t.insert(key.to_string(), value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agents() -> Vec<String> {
        vec!["opencode".to_string(), "claude".to_string()]
    }

    fn mgr(global: toml::Table, project: toml::Table) -> ConfigManager {
        ConfigManager::new(
            "demo",
            Some(PathBuf::from("/home/u/.flightdeck/config.toml")),
            PathBuf::from("/repo/.flightdeck/config.toml"),
            global,
            project,
            agents(),
        )
    }

    #[test]
    fn shows_defaults_when_nothing_overridden() {
        let m = mgr(toml::Table::new(), toml::Table::new());
        let rows = m.rows();
        let notif = rows.iter().find(|r| r.label == "OS notifications").unwrap();
        // Notifications default on; nothing set anywhere → Default origin.
        assert!(notif.bool_value);
        assert_eq!(notif.origin, Origin::Default);
        let f2 = rows
            .iter()
            .find(|r| r.label == "Use F2 to leave terminal focus")
            .unwrap();
        assert!(!f2.bool_value);
        assert_eq!(f2.origin, Origin::Default);
    }

    #[test]
    fn project_scope_reports_inherited_from_global() {
        let global: toml::Table = "[notifications]\nenabled = false\n".parse().unwrap();
        let mut m = mgr(global, toml::Table::new());
        assert_eq!(m.scope(), ConfigScope::Project);
        let notif = m
            .rows()
            .into_iter()
            .find(|r| r.label == "OS notifications")
            .unwrap();
        assert!(!notif.bool_value);
        assert_eq!(notif.origin, Origin::Global);
        // Toggling writes a project override (set here), flipping it back on.
        m.toggle_selected(); // row 0 is OS notifications
        let notif = m
            .rows()
            .into_iter()
            .find(|r| r.label == "OS notifications")
            .unwrap();
        assert!(notif.bool_value);
        assert_eq!(notif.origin, Origin::SetHere);
        assert!(m.dirty());
    }

    #[test]
    fn project_output_contains_only_overrides() {
        let mut m = mgr(toml::Table::new(), toml::Table::new());
        m.toggle_selected(); // override notifications.enabled in project scope
        let outputs = m.outputs().unwrap();
        assert_eq!(outputs.len(), 1);
        let (path, body) = &outputs[0];
        assert!(path.ends_with("config.toml"));
        assert!(body.contains("[notifications]"));
        assert!(body.contains("enabled"));
        // Only the one overridden section is present — not the whole config.
        assert!(!body.contains("[containers]"), "project output: {body}");
        assert!(!body.contains("[git]"), "project output: {body}");
    }

    #[test]
    fn clear_override_reverts_to_inherited() {
        let mut m = mgr(toml::Table::new(), toml::Table::new());
        m.toggle_selected(); // set an override
        assert_eq!(m.rows()[0].origin, Origin::SetHere);
        m.clear_selected();
        assert_eq!(m.rows()[0].origin, Origin::Default);
    }

    #[test]
    fn choice_cycles_through_options() {
        let mut m = mgr(toml::Table::new(), toml::Table::new());
        // Move to "Agent tab position" (index 7).
        for _ in 0..7 {
            m.select_next();
        }
        let before = m.rows()[7].value.clone();
        assert_eq!(before, "left");
        m.toggle_selected();
        assert_eq!(m.rows()[7].value, "right");
        m.toggle_selected();
        assert_eq!(m.rows()[7].value, "left");
    }

    #[test]
    fn f2_leave_focus_setting_writes_ui_override() {
        let mut m = mgr(toml::Table::new(), toml::Table::new());
        for _ in 0..6 {
            m.select_next();
        }
        m.toggle_selected();

        let body = &m.outputs().unwrap()[0].1;
        assert!(body.contains("[ui]"));
        assert!(body.contains("use_f2_to_leave_terminal_focus = true"));
    }

    #[test]
    fn switch_scope_changes_target_and_editing_target() {
        let mut m = mgr(toml::Table::new(), toml::Table::new());
        assert_eq!(m.scope(), ConfigScope::Project);
        m.switch_scope();
        assert_eq!(m.scope(), ConfigScope::Global);
        // Editing in Global scope marks the global file dirty.
        m.toggle_selected();
        let outputs = m.outputs().unwrap();
        assert_eq!(outputs.len(), 1);
        assert!(outputs[0].0.ends_with("config.toml"));
        assert!(outputs[0].0.to_string_lossy().contains(".flightdeck"));
    }

    /// Move the selection to the field with `label`, returning its row index.
    fn goto(m: &mut ConfigManager, label: &str) -> usize {
        let idx = m.rows().iter().position(|r| r.label == label).unwrap();
        while m.selected_index() != idx {
            m.select_next();
        }
        idx
    }

    #[test]
    fn exposes_remote_fields_with_relay_default() {
        let m = mgr(toml::Table::new(), toml::Table::new());
        let rows = m.rows();
        let enabled = rows
            .iter()
            .find(|r| r.label == "FlightDeck Remote (phone link)")
            .unwrap();
        assert!(enabled.is_bool);
        assert!(!enabled.bool_value, "remote is off by default");
        let relay = rows.iter().find(|r| r.label == "Relay URL").unwrap();
        assert!(relay.is_text);
        // The default relay URL is shown as the inherited fallback.
        assert_eq!(relay.value, "wss://relay.flightdeckai.app/ws");
        assert_eq!(relay.origin, Origin::Default);
    }

    #[test]
    fn editing_relay_url_writes_a_text_override() {
        let mut m = mgr(toml::Table::new(), toml::Table::new());
        goto(&mut m, "Relay URL");
        // Activating a text field opens the editor seeded with the current value.
        m.toggle_selected();
        assert!(m.is_editing());
        // Clear it and type a self-hosted URL.
        for _ in 0.."wss://relay.flightdeckai.app/ws".len() {
            m.edit_backspace();
        }
        for c in "wss://my-relay.example/ws".chars() {
            m.edit_push_char(c);
        }
        m.commit_edit();
        assert!(!m.is_editing());
        let relay = m
            .rows()
            .into_iter()
            .find(|r| r.label == "Relay URL")
            .unwrap();
        assert_eq!(relay.value, "wss://my-relay.example/ws");
        assert_eq!(relay.origin, Origin::SetHere);
        let body = &m.outputs().unwrap()[0].1;
        assert!(body.contains("[remote]"));
        assert!(body.contains("relay_url = \"wss://my-relay.example/ws\""));
    }

    #[test]
    fn cancel_edit_discards_changes() {
        let mut m = mgr(toml::Table::new(), toml::Table::new());
        goto(&mut m, "Relay URL");
        m.toggle_selected();
        m.edit_push_char('x');
        m.cancel_edit();
        assert!(!m.is_editing());
        assert!(!m.dirty(), "cancelled edit must not dirty the config");
    }

    #[test]
    fn navigating_away_discards_open_edit() {
        let mut m = mgr(toml::Table::new(), toml::Table::new());
        goto(&mut m, "Relay URL");
        m.toggle_selected();
        m.edit_push_char('z');
        m.select_next();
        assert!(!m.is_editing());
        assert!(!m.dirty());
    }

    #[test]
    fn toggle_remote_enabled_writes_bool_override() {
        let mut m = mgr(toml::Table::new(), toml::Table::new());
        goto(&mut m, "FlightDeck Remote (phone link)");
        m.toggle_selected();
        let body = &m.outputs().unwrap()[0].1;
        assert!(body.contains("[remote]"));
        assert!(body.contains("enabled = true"));
    }

    #[test]
    fn exposes_mode_cue_fields() {
        let m = mgr(toml::Table::new(), toml::Table::new());
        let rows = m.rows();
        let labels: Vec<&str> = rows.iter().map(|r| r.label.as_str()).collect();
        assert!(labels.contains(&"Terminal mode color"));
        assert!(labels.contains(&"App mode color"));
        assert!(labels.contains(&"Mode border"));
        assert!(labels.contains(&"Dim terminal in app mode"));
        // The dim field is a boolean toggle, defaulting on.
        let dim = rows
            .iter()
            .find(|r| r.label == "Dim terminal in app mode")
            .unwrap();
        assert!(dim.is_bool);
        assert!(dim.bool_value);
    }

    // --- [web] curated fields (specs/WEB_INTERFACE.md D10; beside FlightDeck Remote) ---

    #[test]
    fn exposes_web_fields_disabled_and_loopback_by_default() {
        let m = mgr(toml::Table::new(), toml::Table::new());
        let rows = m.rows();
        let enabled = rows
            .iter()
            .find(|r| r.label == "Auto-start Web Interface")
            .unwrap();
        assert!(enabled.is_bool);
        assert!(!enabled.bool_value, "web interface is off by default");
        assert_eq!(enabled.origin, Origin::Default);
        let bind = rows
            .iter()
            .find(|r| r.label == "Web interface bind address")
            .unwrap();
        assert!(bind.is_text);
        assert_eq!(bind.value, "127.0.0.1");
        assert_eq!(bind.origin, Origin::Default);
    }

    #[test]
    fn toggle_web_enabled_writes_bool_override() {
        let mut m = mgr(toml::Table::new(), toml::Table::new());
        goto(&mut m, "Auto-start Web Interface");
        m.toggle_selected();
        let row = m
            .rows()
            .into_iter()
            .find(|r| r.label == "Auto-start Web Interface")
            .unwrap();
        assert!(row.bool_value);
        assert_eq!(row.origin, Origin::SetHere);
        let body = &m.outputs().unwrap()[0].1;
        assert!(body.contains("[web]"));
        assert!(body.contains("enabled = true"));
    }

    #[test]
    fn editing_web_bind_writes_a_text_override() {
        let mut m = mgr(toml::Table::new(), toml::Table::new());
        goto(&mut m, "Web interface bind address");
        m.toggle_selected();
        assert!(m.is_editing());
        for _ in 0.."127.0.0.1".len() {
            m.edit_backspace();
        }
        for c in "0.0.0.0".chars() {
            m.edit_push_char(c);
        }
        m.commit_edit();
        let bind = m
            .rows()
            .into_iter()
            .find(|r| r.label == "Web interface bind address")
            .unwrap();
        assert_eq!(bind.value, "0.0.0.0");
        assert_eq!(bind.origin, Origin::SetHere);
        let body = &m.outputs().unwrap()[0].1;
        assert!(body.contains("[web]"));
        assert!(body.contains("bind = \"0.0.0.0\""));
    }

    #[test]
    fn web_enabled_from_global_shows_global_origin_in_project_scope() {
        // Global scope sets web.enabled = true; Project scope (the manager's
        // default) must show it inherited, not as a project override.
        let global: toml::Table = "[web]\nenabled = true\n".parse().unwrap();
        let m = mgr(global, toml::Table::new());
        assert_eq!(m.scope(), ConfigScope::Project);
        let row = m
            .rows()
            .into_iter()
            .find(|r| r.label == "Auto-start Web Interface")
            .unwrap();
        assert!(row.bool_value);
        assert_eq!(row.origin, Origin::Global);
    }

    // --- the browser's door onto this model (`specs/WEB_INTERFACE.md` R22) ---

    #[test]
    fn every_row_carries_the_toml_path_it_writes() {
        let m = mgr(toml::Table::new(), toml::Table::new());
        let keys: Vec<String> = m.rows().into_iter().map(|r| r.key).collect();
        // The real keys, not the plausible ones: `notifications.on_finish` and
        // `update.check` are what this build reads, and a browser that shipped
        // `on_finished` / `updates.check_for_updates` would be writing rows
        // FlightDeck never looks at.
        assert!(keys.contains(&"notifications.on_finish".to_string()));
        assert!(keys.contains(&"update.check".to_string()));
        assert!(keys.contains(&"web.bind".to_string()));
        assert!(!keys.iter().any(|k| k == "notifications.on_finished"));
        assert!(!keys.iter().any(|k| k == "updates.check_for_updates"));
    }

    #[test]
    fn a_choice_row_carries_its_options_and_a_bool_row_carries_none() {
        let m = mgr(toml::Table::new(), toml::Table::new());
        let rows = m.rows();
        let agent = rows.iter().find(|r| r.label == "Default agent").unwrap();
        // The live agent keys, so a browser cannot offer an agent this host
        // has not been configured with.
        assert_eq!(agent.choices, agents());
        let notif = rows.iter().find(|r| r.label == "OS notifications").unwrap();
        assert!(notif.choices.is_empty());
    }

    #[test]
    fn selecting_by_key_moves_the_cursor_and_reports_an_unknown_key() {
        let mut m = mgr(toml::Table::new(), toml::Table::new());
        assert!(m.select_key("ui.mode_border"));
        assert_eq!(m.rows()[m.selected_index()].label, "Mode border");
        assert!(!m.select_key("ui.no_such_setting"));
    }

    #[test]
    fn setting_an_explicit_value_writes_an_override_and_refuses_a_wrong_kind() {
        let mut m = mgr(toml::Table::new(), toml::Table::new());
        assert!(m.select_key("ui.mode_border"));
        m.set_selected(FieldValue::Text("bright".to_string()))
            .expect("`bright` is one of the four options");
        let row = m
            .rows()
            .into_iter()
            .find(|r| r.key == "ui.mode_border")
            .unwrap();
        assert_eq!(row.value, "bright");
        assert_eq!(row.origin, Origin::SetHere);

        // A value the field does not admit never reaches the file, and the
        // refusal names the options rather than saying "invalid".
        let err = m
            .set_selected(FieldValue::Text("purple".to_string()))
            .expect_err("`purple` is not an option");
        assert!(err.contains("off, dim, normal, bright"), "{err}");
        assert!(m
            .set_selected(FieldValue::Bool(true))
            .is_err_and(|e| e.contains("one of")));
        assert!(m.select_key("notifications.enabled"));
        assert!(m
            .set_selected(FieldValue::Text("yes".to_string()))
            .is_err_and(|e| e.contains("toggle")));
    }

    #[test]
    fn inherited_rows_are_what_clearing_this_scope_would_leave() {
        // Global says off, the project overrides it back on. Clearing the
        // project override re-inherits the global's `off` — and the *global*
        // scope's own clear would fall all the way to the shipped default.
        let global: toml::Table = "[notifications]\nenabled = false\n".parse().unwrap();
        let project: toml::Table = "[notifications]\nenabled = true\n".parse().unwrap();
        let mut m = mgr(global, project);

        let row = |rows: Vec<ConfigRow>| {
            rows.into_iter()
                .find(|r| r.key == "notifications.enabled")
                .unwrap()
        };
        assert_eq!(row(m.rows()).origin, Origin::SetHere);
        let inherited = row(m.inherited_rows());
        assert!(!inherited.bool_value);
        assert_eq!(inherited.origin, Origin::Global);

        m.set_scope(ConfigScope::Global);
        assert_eq!(row(m.rows()).origin, Origin::SetHere);
        let inherited = row(m.inherited_rows());
        assert!(inherited.bool_value, "the shipped default is on");
        assert_eq!(inherited.origin, Origin::Default);

        // Asking is not editing: the probe copy is thrown away.
        assert!(!m.dirty());
    }

    #[test]
    fn setting_a_scope_is_the_same_switch_tab_makes() {
        let mut m = mgr(toml::Table::new(), toml::Table::new());
        m.set_scope(ConfigScope::Project);
        assert_eq!(m.scope(), ConfigScope::Project, "already there: a no-op");
        m.set_scope(ConfigScope::Global);
        assert_eq!(m.scope(), ConfigScope::Global);
    }

    #[test]
    fn web_port_and_replay_bytes_are_not_curated() {
        // Numeric fields stay raw-TOML-only (see the comment in build_fields):
        // this manager's FieldKind::Text always commits a TOML string, which
        // would corrupt a u16/usize field on save.
        let m = mgr(toml::Table::new(), toml::Table::new());
        let labels: Vec<String> = m.rows().iter().map(|r| r.label.to_lowercase()).collect();
        assert!(!labels.iter().any(|l| l.contains("port")));
        assert!(!labels.iter().any(|l| l.contains("replay")));
    }
}
