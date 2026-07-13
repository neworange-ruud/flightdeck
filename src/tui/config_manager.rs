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

/// The kind of a curated field: a boolean toggle or a fixed set of choices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldKind {
    Bool,
    Choice(Vec<String>),
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

/// A render-ready view of one curated field for the current scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigRow {
    pub label: String,
    /// Display value (`on`/`off` for a bool, the choice string otherwise).
    pub value: String,
    pub origin: Origin,
    pub selected: bool,
    /// True for a boolean toggle (rendered as a checkbox).
    pub is_bool: bool,
    /// The boolean state when `is_bool` (ignored otherwise).
    pub bool_value: bool,
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

    /// Move the selection down one row (wraps).
    pub fn select_next(&mut self) {
        if !self.fields.is_empty() {
            self.selected = (self.selected + 1) % self.fields.len();
        }
    }

    /// Move the selection up one row (wraps).
    pub fn select_prev(&mut self) {
        let len = self.fields.len();
        if len > 0 {
            self.selected = (self.selected + len - 1) % len;
        }
    }

    /// Switch between Global and Project scope, clamping the selection.
    pub fn switch_scope(&mut self) {
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
                let (display, is_bool, bool_value) = match &f.kind {
                    FieldKind::Bool => {
                        let b = value.as_bool().unwrap_or(false);
                        ((if b { "on" } else { "off" }).to_string(), true, b)
                    }
                    FieldKind::Choice(_) => {
                        (value.as_str().unwrap_or("").to_string(), false, false)
                    }
                };
                ConfigRow {
                    label: f.label.to_string(),
                    value: display,
                    origin,
                    selected: i == self.selected,
                    is_bool,
                    bool_value,
                }
            })
            .collect()
    }

    /// Toggle a boolean or advance a choice for the selected field, writing the
    /// new value into the current scope as an explicit override.
    pub fn toggle_selected(&mut self) {
        let Some(field) = self.fields.get(self.selected).cloned() else {
            return;
        };
        let (current, _) = self.effective(&field);
        let new_value = match &field.kind {
            FieldKind::Bool => toml::Value::Boolean(!current.as_bool().unwrap_or(false)),
            FieldKind::Choice(options) if !options.is_empty() => {
                let cur = current.as_str().unwrap_or("");
                let idx = options.iter().position(|o| o == cur).unwrap_or(0);
                let next = options[(idx + 1) % options.len()].clone();
                toml::Value::String(next)
            }
            FieldKind::Choice(_) => return,
        };
        set_value(self.scope_table_mut(), field.section, field.key, new_value);
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
        // Move to "Agent tab position" (index 6).
        for _ in 0..6 {
            m.select_next();
        }
        let before = m.rows()[6].value.clone();
        assert_eq!(before, "left");
        m.toggle_selected();
        assert_eq!(m.rows()[6].value, "right");
        m.toggle_selected();
        assert_eq!(m.rows()[6].value, "left");
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
}
