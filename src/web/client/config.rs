//! The host's configuration manager, edited from a remote window
//! (`remote-control-qlyd.15`; design A1: every host kind).
//!
//! The host answers `open_configuration` with a [`wire::ConfigView`]: every
//! curated field resolved in both scopes, with what clearing it would leave
//! behind. [`RemoteConfigEditor`] stages edits against that answer exactly as
//! the browser's manager does (`webui/src/state/config.ts`) — a staged set
//! reads as `(set here)`, a staged clear as the host's own `inherited` value —
//! and draws it as the very [`ConfigView`] the desktop's local manager card
//! renders, so one card serves both. It answers the card's
//! [`OverlayInput`] alphabet itself; only a save leaves the window, as one
//! [`ConfigSaveRequest`] naming the project this window is looking at.
//!
//! It owns no field, no key and no layer: all of that is the host's, and the
//! host validates every staged value against its own field table on save.

use std::path::PathBuf;

use crate::host::{ConfigView, OverlayInput, OverlayKey};
use crate::tui::config_manager::{ConfigRow, ConfigScope, FieldValue, Origin};
use crate::web::protocol::{
    self as wire, ConfigChange, ConfigFieldKind, ConfigOrigin, ConfigRowView, ConfigSaveRequest,
    ConfigScopeName, ConfigValue, ProjectId,
};

/// Why the manager's `e` does nothing here.
pub const RAW_EDIT_UNAVAILABLE: &str =
    "Raw config.toml editing is not available from a remote window.";

/// What one input asks of the window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditorEffect {
    /// Nothing beyond repainting.
    None,
    /// Close the manager (staged edits are dropped, as the desktop's `Esc`).
    Close,
    /// Send these edits as `open_configuration`'s args: save, then read back.
    Send(ConfigSaveRequest),
}

/// The configuration manager of a remote host, staged locally.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteConfigEditor {
    view: wire::ConfigView,
    project: Option<ProjectId>,
    scope: ConfigScope,
    selected: usize,
    /// The inline text edit's buffer, while one is open.
    editing: Option<String>,
    /// Staged edits, oldest first, at most one per scope and key (the last
    /// edit to a field is the one that stands).
    staged: Vec<ConfigChange>,
    status: Option<String>,
    /// A save went out and its answer has not come back yet.
    saving: bool,
}

impl RemoteConfigEditor {
    /// An editor over the host's answer, saving into `project`'s file (the
    /// project this window is looking at). Opens in Project scope, like the
    /// local manager.
    pub fn new(view: wire::ConfigView, project: Option<ProjectId>) -> RemoteConfigEditor {
        RemoteConfigEditor {
            view,
            project,
            scope: ConfigScope::Project,
            selected: 0,
            editing: None,
            staged: Vec::new(),
            status: None,
            saving: false,
        }
    }

    /// The host answered again — after a save, or a fresh read. Its answer is
    /// the new truth: staged edits it now reflects are dropped, and the scope
    /// and highlighted row stay where they were.
    pub fn on_view(&mut self, view: wire::ConfigView) {
        self.view = view;
        self.editing = None;
        if std::mem::take(&mut self.saving) {
            self.staged.clear();
            self.status = Some("Saved.".to_string());
        }
        self.selected = self.selected.min(self.rows().len().saturating_sub(1));
    }

    /// The host refused the save (`detail` is its reason). The staged edits
    /// stay, so the person can fix one and save again.
    pub fn on_refused(&mut self, detail: impl Into<String>) {
        self.saving = false;
        self.status = Some(detail.into());
    }

    /// Whether anything is staged and unsaved.
    pub fn dirty(&self) -> bool {
        !self.staged.is_empty()
    }

    /// The staged edits, oldest first.
    pub fn staged(&self) -> &[ConfigChange] {
        &self.staged
    }

    /// The project saves land in.
    pub fn project(&self) -> Option<&ProjectId> {
        self.project.as_ref()
    }

    /// Whether a save is waiting for the host's answer.
    pub fn is_saving(&self) -> bool {
        self.saving
    }

    // -- drawing ------------------------------------------------------------

    /// The manager as the desktop's config card draws it.
    pub fn overlay(&self) -> ConfigView {
        let rows = self.rows();
        let scope = scope_name(self.scope);
        let mut drawn = Vec::with_capacity(rows.len());
        let mut inherited = Vec::with_capacity(rows.len());
        for (i, row) in rows.iter().enumerate() {
            let selected = i == self.selected;
            let (value, origin) = self.resolved(scope, row);
            let editing = selected && self.editing.is_some();
            let text = match (&self.editing, editing) {
                (Some(buffer), true) => Some(buffer.as_str()),
                _ => None,
            };
            drawn.push(config_row(row, &value, origin, selected, editing, text));
            inherited.push(config_row(
                row,
                &row.inherited,
                row.inherited_origin,
                selected,
                false,
                None,
            ));
        }
        ConfigView {
            project_name: self.view.project_name.clone(),
            scope: self.scope,
            path: match self.scope {
                ConfigScope::Global => self.view.global_path.as_ref().map(PathBuf::from),
                ConfigScope::Project => Some(PathBuf::from(&self.view.project_path)),
            },
            rows: drawn,
            inherited,
            selected: self.selected,
            editing: self.editing.is_some(),
            dirty: self.dirty(),
            status: self.status.clone(),
        }
    }

    fn rows(&self) -> &[ConfigRowView] {
        match self.scope {
            ConfigScope::Global => &self.view.global_rows,
            ConfigScope::Project => &self.view.project_rows,
        }
    }

    /// A row's value and origin after any staged edit in `scope`.
    fn resolved(&self, scope: ConfigScopeName, row: &ConfigRowView) -> (ConfigValue, ConfigOrigin) {
        match self
            .staged
            .iter()
            .find(|c| c.scope == scope && c.key == row.key)
        {
            Some(ConfigChange {
                value: Some(value), ..
            }) => (value.clone(), ConfigOrigin::SetHere),
            Some(ConfigChange { value: None, .. }) => (row.inherited.clone(), row.inherited_origin),
            None => (row.value.clone(), row.origin),
        }
    }

    // -- input --------------------------------------------------------------

    /// Apply one input from the config card.
    pub fn input(&mut self, input: OverlayInput) -> EditorEffect {
        if self.editing.is_some() {
            return self.edit_input(input);
        }
        match input {
            OverlayInput::Cancel | OverlayInput::Key(OverlayKey::Esc) => EditorEffect::Close,
            OverlayInput::Key(OverlayKey::Tab) => {
                self.scope = match self.scope {
                    ConfigScope::Global => ConfigScope::Project,
                    ConfigScope::Project => ConfigScope::Global,
                };
                self.selected = self.selected.min(self.rows().len().saturating_sub(1));
                self.status = None;
                EditorEffect::None
            }
            OverlayInput::Key(OverlayKey::Up) => {
                let len = self.rows().len();
                if len > 0 {
                    self.selected = (self.selected + len - 1) % len;
                }
                EditorEffect::None
            }
            OverlayInput::Key(OverlayKey::Down) => {
                let len = self.rows().len();
                if len > 0 {
                    self.selected = (self.selected + 1) % len;
                }
                EditorEffect::None
            }
            OverlayInput::SelectRow(index) => {
                let len = self.rows().len();
                if index < len {
                    self.selected = index;
                } else {
                    self.status = Some(format!(
                        "The configuration manager has {len} rows, so row {index} names none of them."
                    ));
                }
                EditorEffect::None
            }
            OverlayInput::Submit
            | OverlayInput::Key(OverlayKey::Enter)
            | OverlayInput::Key(OverlayKey::Char(' ')) => {
                self.activate();
                EditorEffect::None
            }
            OverlayInput::Key(OverlayKey::Char('c')) => {
                if let Some(key) = self.rows().get(self.selected).map(|r| r.key.clone()) {
                    self.stage(self.scope, key, None);
                }
                EditorEffect::None
            }
            OverlayInput::ConfigSet { scope, key, value } => {
                self.set(scope, key, value.map(wire_value));
                EditorEffect::None
            }
            OverlayInput::ConfigSave | OverlayInput::Key(OverlayKey::Char('s')) => self.save(),
            OverlayInput::ConfigEditRaw | OverlayInput::Key(OverlayKey::Char('e')) => {
                self.status = Some(RAW_EDIT_UNAVAILABLE.to_string());
                EditorEffect::None
            }
            _ => EditorEffect::None,
        }
    }

    /// While a text field is open, keys belong to it: Enter commits, Esc
    /// cancels the edit (not the manager), text replaces the buffer.
    fn edit_input(&mut self, input: OverlayInput) -> EditorEffect {
        match input {
            OverlayInput::Submit | OverlayInput::Key(OverlayKey::Enter) => {
                self.commit_edit();
                EditorEffect::None
            }
            OverlayInput::Cancel | OverlayInput::Key(OverlayKey::Esc) => {
                self.editing = None;
                EditorEffect::None
            }
            OverlayInput::SetText(text) => {
                self.editing = Some(text);
                EditorEffect::None
            }
            OverlayInput::Key(OverlayKey::Backspace) => {
                if let Some(buffer) = self.editing.as_mut() {
                    buffer.pop();
                }
                EditorEffect::None
            }
            OverlayInput::Key(OverlayKey::Char(c)) => {
                if let Some(buffer) = self.editing.as_mut() {
                    buffer.push(c);
                }
                EditorEffect::None
            }
            OverlayInput::ConfigSave => {
                self.commit_edit();
                self.save()
            }
            // A click elsewhere on the card resolves the edit first, as the
            // desktop's own manager discards it on navigation.
            other @ (OverlayInput::SelectRow(_) | OverlayInput::ConfigSet { .. }) => {
                self.editing = None;
                self.input(other)
            }
            _ => EditorEffect::None,
        }
    }

    /// Enter / Space on the highlighted row: a toggle flips, a choice advances
    /// through the host's own options, a text field opens its inline editor
    /// seeded with the value in effect.
    fn activate(&mut self) {
        let scope = scope_name(self.scope);
        let Some(row) = self.rows().get(self.selected).cloned() else {
            return;
        };
        let (value, _) = self.resolved(scope, &row);
        let next = match row.kind {
            ConfigFieldKind::Bool => ConfigValue::Bool(!matches!(value, ConfigValue::Bool(true))),
            ConfigFieldKind::Choice if row.choices.is_empty() => return,
            ConfigFieldKind::Choice => {
                let current = text_of(&value);
                let next = match row.choices.iter().position(|c| *c == current) {
                    Some(i) => (i + 1) % row.choices.len(),
                    None => 0,
                };
                ConfigValue::Text(row.choices[next].clone())
            }
            ConfigFieldKind::Text => {
                self.editing = Some(text_of(&value));
                self.status = None;
                return;
            }
        };
        self.stage(self.scope, row.key, Some(next));
    }

    fn commit_edit(&mut self) {
        let Some(buffer) = self.editing.take() else {
            return;
        };
        if let Some(key) = self.rows().get(self.selected).map(|r| r.key.clone()) {
            self.stage(self.scope, key, Some(ConfigValue::Text(buffer)));
        }
    }

    /// Stage a value the card named (its checkbox, its choice menu, its reset
    /// button). A key this host did not send is refused by name.
    fn set(&mut self, scope: ConfigScope, key: String, value: Option<ConfigValue>) {
        let rows = match scope {
            ConfigScope::Global => &self.view.global_rows,
            ConfigScope::Project => &self.view.project_rows,
        };
        if !rows.iter().any(|r| r.key == key) {
            self.status = Some(format!("`{key}` is not a setting this host has."));
            return;
        }
        self.stage(scope, key, value);
    }

    fn stage(&mut self, scope: ConfigScope, key: String, value: Option<ConfigValue>) {
        let scope = scope_name(scope);
        self.staged.retain(|c| !(c.scope == scope && c.key == key));
        self.staged.push(ConfigChange { scope, key, value });
        self.status = None;
    }

    fn save(&mut self) -> EditorEffect {
        self.saving = true;
        self.status = Some("Saving…".to_string());
        EditorEffect::Send(ConfigSaveRequest {
            changes: self.staged.clone(),
            project_id: self.project.clone(),
        })
    }
}

fn scope_name(scope: ConfigScope) -> ConfigScopeName {
    match scope {
        ConfigScope::Global => ConfigScopeName::Global,
        ConfigScope::Project => ConfigScopeName::Project,
    }
}

fn origin(origin: ConfigOrigin) -> Origin {
    match origin {
        ConfigOrigin::SetHere => Origin::SetHere,
        ConfigOrigin::Global => Origin::Global,
        ConfigOrigin::Default => Origin::Default,
    }
}

fn wire_value(value: FieldValue) -> ConfigValue {
    match value {
        FieldValue::Bool(b) => ConfigValue::Bool(b),
        FieldValue::Text(s) => ConfigValue::Text(s),
    }
}

fn text_of(value: &ConfigValue) -> String {
    match value {
        ConfigValue::Text(s) => s.clone(),
        ConfigValue::Bool(b) => b.to_string(),
    }
}

/// One row as the local manager's [`ConfigRow`] reads: `on`/`off` for a
/// toggle, the text otherwise (the edit buffer while it is open).
fn config_row(
    row: &ConfigRowView,
    value: &ConfigValue,
    from: ConfigOrigin,
    selected: bool,
    editing: bool,
    buffer: Option<&str>,
) -> ConfigRow {
    let is_bool = row.kind == ConfigFieldKind::Bool;
    let bool_value = matches!(value, ConfigValue::Bool(true));
    let display = match (buffer, value) {
        (Some(buffer), _) => buffer.to_string(),
        (None, ConfigValue::Bool(b)) => (if *b { "on" } else { "off" }).to_string(),
        (None, ConfigValue::Text(s)) => s.clone(),
    };
    ConfigRow {
        key: row.key.clone(),
        label: row.label.clone(),
        value: display,
        origin: origin(from),
        selected,
        is_bool,
        bool_value,
        is_text: row.kind == ConfigFieldKind::Text,
        editing,
        choices: row.choices.clone(),
    }
}

#[cfg(test)]
mod tests;
