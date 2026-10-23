//! The remote configuration manager: the card's input alphabet, staging, and
//! the save payload.

use super::*;

fn row(
    key: &str,
    kind: ConfigFieldKind,
    value: ConfigValue,
    from: ConfigOrigin,
    choices: &[&str],
    inherited: ConfigValue,
) -> ConfigRowView {
    ConfigRowView {
        key: key.to_string(),
        label: key.to_string(),
        kind,
        value,
        choices: choices.iter().map(|c| c.to_string()).collect(),
        origin: from,
        inherited,
        inherited_origin: ConfigOrigin::Default,
    }
}

fn rows(notify: bool) -> Vec<ConfigRowView> {
    vec![
        row(
            "notifications.enabled",
            ConfigFieldKind::Bool,
            ConfigValue::Bool(notify),
            ConfigOrigin::SetHere,
            &[],
            ConfigValue::Bool(true),
        ),
        row(
            "ui.mode_border",
            ConfigFieldKind::Choice,
            ConfigValue::Text("blue".to_string()),
            ConfigOrigin::Default,
            &["blue", "green", "red"],
            ConfigValue::Text("blue".to_string()),
        ),
        row(
            "remote.relay_url",
            ConfigFieldKind::Text,
            ConfigValue::Text("wss://relay".to_string()),
            ConfigOrigin::Global,
            &[],
            ConfigValue::Text("wss://default".to_string()),
        ),
    ]
}

fn view() -> wire::ConfigView {
    wire::ConfigView {
        seq: 4,
        project_name: "app".to_string(),
        global_path: Some("/home/u/.flightdeck/config.toml".to_string()),
        project_path: "/repo/app/.flightdeck/config.toml".to_string(),
        project_rows: rows(false),
        global_rows: rows(true),
    }
}

fn editor() -> RemoteConfigEditor {
    RemoteConfigEditor::new(view(), Some("/repo/app".into()))
}

fn shown(editor: &RemoteConfigEditor) -> Vec<(String, Origin)> {
    editor
        .overlay()
        .rows
        .into_iter()
        .map(|r| (r.value, r.origin))
        .collect()
}

#[test]
fn it_draws_the_hosts_rows_for_the_scope_on_show() {
    let editor = editor();
    let overlay = editor.overlay();
    assert_eq!(overlay.scope, ConfigScope::Project);
    assert_eq!(overlay.project_name, "app");
    assert_eq!(
        overlay.path,
        Some(PathBuf::from("/repo/app/.flightdeck/config.toml"))
    );
    assert!(!overlay.dirty);
    assert_eq!(
        shown(&editor),
        vec![
            ("off".to_string(), Origin::SetHere),
            ("blue".to_string(), Origin::Default),
            ("wss://relay".to_string(), Origin::Global),
        ]
    );
    assert!(overlay.rows[0].is_bool && overlay.rows[0].selected);
    assert!(overlay.rows[2].is_text);
    assert_eq!(overlay.rows[1].choices, vec!["blue", "green", "red"]);
    assert_eq!(overlay.inherited[2].value, "wss://default");
}

#[test]
fn tab_switches_scope_and_the_arrows_and_clicks_move_the_highlight() {
    let mut editor = editor();
    assert_eq!(
        editor.input(OverlayInput::Key(OverlayKey::Tab)),
        EditorEffect::None
    );
    let overlay = editor.overlay();
    assert_eq!(overlay.scope, ConfigScope::Global);
    assert_eq!(
        overlay.path,
        Some(PathBuf::from("/home/u/.flightdeck/config.toml"))
    );
    assert_eq!(overlay.rows[0].value, "on");

    editor.input(OverlayInput::Key(OverlayKey::Up));
    assert_eq!(editor.overlay().selected, 2, "wraps upwards");
    editor.input(OverlayInput::Key(OverlayKey::Down));
    assert_eq!(editor.overlay().selected, 0, "wraps downwards");
    editor.input(OverlayInput::SelectRow(1));
    assert_eq!(editor.overlay().selected, 1);
    editor.input(OverlayInput::SelectRow(7));
    assert_eq!(editor.overlay().selected, 1, "a row it does not have");
    assert!(editor.overlay().status.unwrap().contains("row 7"));
}

#[test]
fn submit_toggles_a_bool_cycles_a_choice_and_edits_text() {
    let mut editor = editor();
    editor.input(OverlayInput::Submit);
    assert_eq!(
        shown(&editor)[0],
        ("on".to_string(), Origin::SetHere),
        "a staged set reads as set here"
    );
    assert!(editor.overlay().dirty);

    editor.input(OverlayInput::SelectRow(1));
    editor.input(OverlayInput::Key(OverlayKey::Char(' ')));
    editor.input(OverlayInput::Submit);
    assert_eq!(shown(&editor)[1], ("red".to_string(), Origin::SetHere));
    editor.input(OverlayInput::Submit);
    assert_eq!(shown(&editor)[1].0, "blue", "the cycle wraps");

    editor.input(OverlayInput::SelectRow(2));
    editor.input(OverlayInput::Submit);
    let overlay = editor.overlay();
    assert!(overlay.editing && overlay.rows[2].editing);
    assert_eq!(
        overlay.rows[2].value, "wss://relay",
        "seeded with the value"
    );
    // While editing, the arrows and Tab are the field's, not the manager's.
    editor.input(OverlayInput::Key(OverlayKey::Tab));
    assert_eq!(editor.overlay().scope, ConfigScope::Project);
    editor.input(OverlayInput::SetText("wss://mine".to_string()));
    assert_eq!(editor.overlay().rows[2].value, "wss://mine");
    editor.input(OverlayInput::Submit);
    assert!(!editor.overlay().editing);
    assert_eq!(
        shown(&editor)[2],
        ("wss://mine".to_string(), Origin::SetHere)
    );

    // Esc while editing drops the edit, not the manager.
    editor.input(OverlayInput::Submit);
    editor.input(OverlayInput::SetText("discarded".to_string()));
    assert_eq!(editor.input(OverlayInput::Cancel), EditorEffect::None);
    assert_eq!(shown(&editor)[2].0, "wss://mine");
    assert_eq!(editor.input(OverlayInput::Cancel), EditorEffect::Close);
}

#[test]
fn two_edits_to_one_field_leave_the_last_and_a_clear_reads_as_inherited() {
    let mut editor = editor();
    let set = |value: Option<FieldValue>| OverlayInput::ConfigSet {
        scope: ConfigScope::Project,
        key: "notifications.enabled".to_string(),
        value,
    };
    editor.input(set(Some(FieldValue::Bool(true))));
    editor.input(set(Some(FieldValue::Bool(false))));
    assert_eq!(
        editor.staged(),
        &[ConfigChange {
            scope: ConfigScopeName::Project,
            key: "notifications.enabled".to_string(),
            value: Some(ConfigValue::Bool(false)),
        }]
    );

    // A clear stages `value: None` and shows what the host says is left.
    editor.input(set(None));
    assert_eq!(editor.staged().len(), 1);
    assert_eq!(editor.staged()[0].value, None);
    assert_eq!(shown(&editor)[0], ("on".to_string(), Origin::Default));

    // `c` clears the highlighted row the same way.
    editor.input(OverlayInput::SelectRow(2));
    editor.input(OverlayInput::Key(OverlayKey::Char('c')));
    assert_eq!(shown(&editor)[2].0, "wss://default");

    // A staged edit shows in its own scope only.
    editor.input(OverlayInput::Key(OverlayKey::Tab));
    assert_eq!(shown(&editor)[2].0, "wss://relay");

    // A key the host did not send is refused by name and stages nothing.
    editor.input(OverlayInput::ConfigSet {
        scope: ConfigScope::Global,
        key: "web.port".to_string(),
        value: Some(FieldValue::Text("1".to_string())),
    });
    assert_eq!(editor.staged().len(), 2);
    assert!(editor.overlay().status.unwrap().contains("web.port"));
}

#[test]
fn save_sends_every_staged_edit_for_this_windows_project() {
    let mut editor = editor();
    editor.input(OverlayInput::Submit);
    editor.input(OverlayInput::Key(OverlayKey::Tab));
    editor.input(OverlayInput::SelectRow(1));
    editor.input(OverlayInput::Submit);
    let EditorEffect::Send(request) = editor.input(OverlayInput::ConfigSave) else {
        panic!("a save is sent");
    };
    assert_eq!(request.project_id, Some("/repo/app".into()));
    assert_eq!(
        request.changes,
        vec![
            ConfigChange {
                scope: ConfigScopeName::Project,
                key: "notifications.enabled".to_string(),
                value: Some(ConfigValue::Bool(true)),
            },
            ConfigChange {
                scope: ConfigScopeName::Global,
                key: "ui.mode_border".to_string(),
                value: Some(ConfigValue::Text("green".to_string())),
            },
        ]
    );
    // It is the payload `open_configuration` takes.
    let json = serde_json::to_value(&request).unwrap();
    assert_eq!(json["project_id"], "/repo/app");
    assert!(editor.is_saving());

    // The host's answer is the new truth; the staged edits are spent.
    let mut answer = view();
    answer.project_rows = rows(true);
    editor.on_view(answer);
    let overlay = editor.overlay();
    assert!(!overlay.dirty);
    assert_eq!(overlay.status.as_deref(), Some("Saved."));
    assert_eq!(overlay.scope, ConfigScope::Global, "the scope stays");
    assert_eq!(overlay.selected, 1, "so does the highlight");
}

#[test]
fn a_refused_save_keeps_the_edits_and_raw_editing_is_not_offered() {
    let mut editor = editor();
    editor.input(OverlayInput::Submit);
    assert!(matches!(
        editor.input(OverlayInput::Key(OverlayKey::Char('s'))),
        EditorEffect::Send(_)
    ));
    editor.on_refused("`ui.mode_border` is one of: blue, green, red.");
    assert!(editor.dirty());
    assert!(!editor.is_saving());
    assert_eq!(
        editor.overlay().status.as_deref(),
        Some("`ui.mode_border` is one of: blue, green, red.")
    );

    assert_eq!(
        editor.input(OverlayInput::ConfigEditRaw),
        EditorEffect::None
    );
    assert_eq!(
        editor.overlay().status.as_deref(),
        Some(RAW_EDIT_UNAVAILABLE)
    );
}
