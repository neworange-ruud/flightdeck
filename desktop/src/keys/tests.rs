//! The chord matrix (beads remote-control-bmej.1.3): every keymap entry, in
//! every context, through GPUI's real key dispatch on its headless test
//! platform, against the bytes the TUI sends.
//!
//! Three layers:
//!
//! - **Pure spelling**: the GPUI string each table chord is bound as, written
//!   out below by hand ([`expected_matrix`]) so a change to the table fails
//!   here until the matrix — and the matrix in `desktop/NOTES-M0.md` — is
//!   updated; plus a round-trip sweep (chord → GPUI string → `Keystroke::parse`
//!   → chord).
//! - **Dispatch**: a test window shaped like the app — a `"Global"` root with
//!   sibling `"App"` and `"Terminal"` panes — with the generated bindings, the
//!   key-down fallbacks and an `EntityInputHandler` wired exactly as the
//!   terminal element will wire them. `simulate_keystrokes` goes through
//!   GPUI's keymap, action dispatch, key-down listeners and then the input
//!   handler, in the order the platform does.
//! - **Bytes**: what the Terminal pane writes equals
//!   [`encode_pty`](flightdeck::app::keymap::encode_pty) and, for a set of
//!   representative keys, the TUI's own `tui::input::encode_key` on the
//!   crossterm event a terminal would report.

use std::collections::BTreeSet;
use std::ops::Range;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use flightdeck::app::keymap::{
    encode_pty, Chord, Key, Keymap, KeymapOptions, Mods, ACTION_NAMESPACE,
};
use flightdeck::app::modes::InputMode;
use flightdeck::tui::input::{encode_key, map_key_with_f2, KeyAction};
use gpui::{
    canvas, div, Action, Bounds, ClipboardItem, Context, ElementInputHandler, Entity,
    EntityInputHandler, FocusHandle, InputHandler, InteractiveElement, IntoElement, Keystroke,
    ParentElement, Pixels, Point, Render, Styled, TestAppContext, UTF16Selection,
    VisualTestContext, Window,
};

use super::*;

// ---------------------------------------------------------------------------
// The matrix, spelled out
// ---------------------------------------------------------------------------

/// Every combination of the keymap options, so each OS's table and both
/// leave-focus settings are covered on whichever OS runs the tests.
fn all_options() -> Vec<KeymapOptions> {
    let mut out = Vec::new();
    for use_f2 in [false, true] {
        for shift in [false, true] {
            for cmd_v in [false, true] {
                out.push(KeymapOptions {
                    use_f2_to_leave_focus: use_f2,
                    leave_focus_uses_shift: shift,
                    command_v_pastes: cmd_v,
                });
            }
        }
    }
    out
}

/// The expected `(entry id, GPUI context, GPUI keystroke)` rows for `options`.
fn expected_matrix(options: KeymapOptions) -> BTreeSet<(&'static str, &'static str, String)> {
    let mut rows: Vec<(&'static str, &'static str, String)> = vec![
        ("OpenPalette", "Global", "ctrl-g".into()),
        ("Quit", "Global", "ctrl-q".into()),
        ("OpenHelp", "Global", "f1".into()),
        ("OpenHelp", "Global", "alt-h".into()),
        ("SwitchProjectPrev", "Global", "shift-left".into()),
        ("SwitchProjectNext", "Global", "shift-right".into()),
        ("AgentTabPrev", "Global", "alt-up".into()),
        ("AgentTabNext", "Global", "alt-down".into()),
        ("TerminalTabPrev", "Global", "alt-left".into()),
        ("TerminalTabNext", "Global", "alt-right".into()),
        ("JumpToAgentTab1", "Global", "alt-1".into()),
        ("JumpToAgentTab2", "Global", "alt-2".into()),
        ("JumpToAgentTab3", "Global", "alt-3".into()),
        ("JumpToAgentTab4", "Global", "alt-4".into()),
        ("JumpToAgentTab5", "Global", "alt-5".into()),
        ("JumpToAgentTab6", "Global", "alt-6".into()),
        ("JumpToAgentTab7", "Global", "alt-7".into()),
        ("JumpToAgentTab8", "Global", "alt-8".into()),
        ("JumpToAgentTab9", "Global", "alt-9".into()),
        ("OpenWorktreeInFileManager", "Global", "alt-o".into()),
        ("AgentTabPrev", "App", "up".into()),
        ("AgentTabNext", "App", "down".into()),
        ("TerminalTabPrev", "App", "left".into()),
        ("TerminalTabNext", "App", "right".into()),
        ("NewAgentTab", "App", "ctrl-n".into()),
        ("PushBranch", "App", "ctrl-p".into()),
        ("PullBase", "App", "ctrl-u".into()),
        ("FinishLocalMerge", "App", "ctrl-f".into()),
        ("CloseAgentTab", "App", "ctrl-k".into()),
        ("NewChildTerminal", "App", "ctrl-t".into()),
        ("CloseChildTerminal", "App", "ctrl-w".into()),
        ("ToggleSplitView", "App", "ctrl-b".into()),
        ("FocusTerminal", "App", "enter".into()),
        ("SetManualStatus", "App", "ctrl-s".into()),
        ("RestartAgent", "App", "ctrl-r".into()),
        ("ToggleMissionControl", "App", "alt-m".into()),
        ("Paste", "Terminal", "ctrl-v".into()),
    ];
    let leave_focus = if options.use_f2_to_leave_focus {
        "f2"
    } else if options.leave_focus_uses_shift {
        "shift-escape"
    } else {
        "alt-escape"
    };
    rows.push(("FocusApp", "Terminal", leave_focus.into()));
    if options.command_v_pastes {
        rows.push(("Paste", "Terminal", format!("{PLATFORM_MODIFIER}-v")));
    }
    rows.into_iter().collect()
}

#[test]
fn every_entry_binds_the_matrix_spelling_in_every_context() {
    for options in all_options() {
        let keymap = Keymap::new(options);
        let got: BTreeSet<_> = binding_specs(&keymap)
            .into_iter()
            .map(|s| (s.entry_id, s.context, s.keystroke))
            .collect();
        assert_eq!(got, expected_matrix(options), "{options:?}");
        // Every entry has at least one binding: nothing in the table is
        // unreachable from the keyboard in the GUI.
        for entry in keymap.entries() {
            assert!(
                got.iter().any(|(id, _, _)| *id == entry.id),
                "{} has no GPUI binding",
                entry.id
            );
        }
        // No trigger was dropped for want of a GPUI spelling.
        let triggers: usize = keymap.entries().iter().map(|e| e.triggers.len()).sum();
        assert_eq!(got.len(), triggers, "{options:?}");
    }
}

#[test]
fn table_chords_round_trip_through_gpui_parsing() {
    for options in all_options() {
        let keymap = Keymap::new(options);
        for entry in keymap.entries() {
            for trigger in &entry.triggers {
                let spelled = chord_to_gpui(trigger.chord).expect("table chord has a spelling");
                let parsed = Keystroke::parse(&spelled).expect("GPUI parses our spelling");
                // Our spelling is GPUI's canonical one on this OS.
                assert_eq!(parsed.unparse(), spelled);
                assert_eq!(
                    chord_from_keystroke(&parsed),
                    Some(trigger.chord),
                    "{spelled}"
                );
            }
        }
    }
}

/// What a chord becomes after a trip through GPUI: the spelling cannot carry
/// "lowercase letter + Shift" or "Tab + Shift" separately, so they come back
/// in the form a front-end reports (see `chord_from_keystroke`).
fn normalised(chord: Chord) -> Chord {
    let shift = chord.mods.contains(Mods::SHIFT);
    let ctrl = chord.mods.contains(Mods::CTRL);
    match chord.key {
        Key::Char(c) if c.is_ascii_uppercase() && ctrl => {
            Chord::new(Key::Char(c.to_ascii_lowercase()), chord.mods | Mods::SHIFT)
        }
        Key::Char(c) if c.is_ascii_uppercase() => Chord::new(chord.key, chord.mods | Mods::SHIFT),
        Key::Char(c) if shift && !ctrl && c.is_ascii_lowercase() => {
            Chord::new(Key::Char(c.to_ascii_uppercase()), chord.mods)
        }
        Key::Tab if shift => Chord::new(Key::BackTab, chord.mods),
        Key::BackTab => Chord::new(Key::BackTab, chord.mods | Mods::SHIFT),
        _ => chord,
    }
}

fn sweep_keys() -> Vec<Key> {
    let mut keys = vec![
        Key::Enter,
        Key::Esc,
        Key::Tab,
        Key::BackTab,
        Key::Backspace,
        Key::Delete,
        Key::Up,
        Key::Down,
        Key::Left,
        Key::Right,
        Key::Home,
        Key::End,
        Key::PageUp,
        Key::PageDown,
        Key::Char(' '),
        Key::Char('-'),
        Key::Char(','),
        Key::Char('/'),
        Key::Char('['),
    ];
    keys.extend(('a'..='z').map(Key::Char));
    keys.extend(('A'..='Z').map(Key::Char));
    keys.extend(('0'..='9').map(Key::Char));
    keys.extend((1..=24).map(Key::F));
    keys
}

/// Every subset of the four modifiers GPUI has.
fn gpui_mods() -> impl Iterator<Item = Mods> {
    (0u8..16).map(|bits| {
        [Mods::SHIFT, Mods::CTRL, Mods::ALT, Mods::SUPER]
            .into_iter()
            .enumerate()
            .filter(|(i, _)| bits & (1 << i) != 0)
            .fold(Mods::NONE, |acc, (_, m)| acc | m)
    })
}

#[test]
fn every_chord_survives_the_gpui_round_trip_with_the_same_bytes() {
    for key in sweep_keys() {
        for mods in gpui_mods() {
            let chord = Chord::new(key, mods);
            let spelled = chord_to_gpui(chord).expect("GPUI can spell it");
            let parsed = Keystroke::parse(&spelled).expect("GPUI parses it");
            let back = chord_from_keystroke(&parsed).expect("and we lift it back");
            assert_eq!(back, normalised(chord), "{spelled}");
            // A chord in the reported form survives unchanged.
            assert_eq!(normalised(back), back, "{spelled}");
        }
    }
}

#[test]
fn hyper_and_meta_have_no_gpui_spelling() {
    assert_eq!(chord_to_gpui(Chord::new(Key::Char('a'), Mods::HYPER)), None);
    assert_eq!(chord_to_gpui(Chord::new(Key::Up, Mods::META)), None);
}

#[test]
fn keys_with_no_chord_lift_to_none() {
    for key in ["insert", "menu", "shift", "f0", "fx", "", "ab"] {
        let keystroke = Keystroke {
            key: key.into(),
            ..Default::default()
        };
        assert_eq!(chord_from_keystroke(&keystroke), None, "{key:?}");
    }
}

#[test]
fn option_composed_characters_do_not_change_the_chord() {
    // macOS reports Option+1 as key "1", alt, key_char "¡": the chord is Alt-1.
    let keystroke = Keystroke::parse("alt-1->¡").unwrap();
    assert_eq!(keystroke.key_char.as_deref(), Some("¡"));
    assert_eq!(
        chord_from_keystroke(&keystroke),
        Some(Chord::new(Key::Char('1'), Mods::ALT))
    );
}

#[test]
fn actions_carry_the_entry_gpui_names() {
    let keymap = Keymap::for_this_platform(false);
    for entry in keymap.entries() {
        let action = KeymapAction::for_entry(entry);
        assert_eq!(action.name(), entry.gpui_action_name());
        assert_eq!(action.id(), entry.id);
        assert_eq!(action.entry(keymap), Some(entry));
        assert!(action.partial_eq(&action));
        // Rebuilt from keymap JSON, it is the same action.
        let built = KeymapAction::build(serde_json::json!({ "id": entry.id })).unwrap();
        assert!(built.partial_eq(&action));
    }
    let open = KeymapAction::for_entry(keymap.entry("OpenPalette").unwrap());
    let quit = KeymapAction::for_entry(keymap.entry("Quit").unwrap());
    assert!(!open.partial_eq(&quit));
    assert!(KeymapAction::name_for_type().starts_with(&format!("{ACTION_NAMESPACE}::")));
    assert!(KeymapAction::build(serde_json::json!({ "id": "NoSuchEntry" })).is_err());
    assert!(KeymapAction::build(serde_json::json!({})).is_err());
}

#[test]
fn paste_bytes_is_the_tui_paste_encoding() {
    assert_eq!(
        paste_bytes("a\nb", true),
        b"\x1b[200~a\rb\x1b[201~".to_vec()
    );
    assert_eq!(paste_bytes("a\r\nb", false), b"a\rb".to_vec());
}

#[test]
fn option_key_defaults_to_compose_only_on_macos() {
    let expected = if flightdeck::tui::platform::IS_MACOS {
        OptionKey::Compose
    } else {
        OptionKey::Meta
    };
    assert_eq!(OptionKey::for_this_platform(), expected);
}

// ---------------------------------------------------------------------------
// The test window: root "Global" > ("App" pane, "Terminal" pane)
// ---------------------------------------------------------------------------

/// A stand-in for the app: the root carries the Global context and performs
/// actions; the App pane has the App-mode fallback; the Terminal pane has the
/// Terminal-mode fallback and the text input handler, wired as the terminal
/// element will be.
struct Harness {
    keymap: Keymap,
    option: OptionKey,
    bracketed: bool,
    app_focus: FocusHandle,
    terminal_focus: FocusHandle,
    ime: ImeState,
    /// Entry ids performed, in order.
    performed: Vec<&'static str>,
    /// Bytes written to the "PTY".
    pty: Vec<u8>,
}

/// What a fallback decided, with the keymap borrow released.
enum Outcome {
    Perform(&'static str),
    Write(Vec<u8>),
    Propagate,
}

impl Harness {
    fn new(keymap: Keymap, option: OptionKey, cx: &mut Context<Self>) -> Self {
        Harness {
            keymap,
            option,
            bracketed: false,
            app_focus: cx.focus_handle(),
            terminal_focus: cx.focus_handle(),
            ime: ImeState::default(),
            performed: Vec::new(),
            pty: Vec::new(),
        }
    }

    fn take(&mut self) -> (Vec<&'static str>, Vec<u8>) {
        (
            std::mem::take(&mut self.performed),
            std::mem::take(&mut self.pty),
        )
    }
}

impl Render for Harness {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let terminal_focus = self.terminal_focus.clone();
        div()
            .key_context("Global")
            .size_full()
            .on_action(cx.listener(|this, action: &KeymapAction, _, _| {
                this.performed.push(action.id());
            }))
            .child(
                div()
                    .key_context("App")
                    .track_focus(&self.app_focus)
                    .size_full()
                    .on_key_down(cx.listener(|this, event, _, cx| {
                        if let Some(entry) = app_key_down(&this.keymap, event) {
                            this.performed.push(entry.id);
                            cx.stop_propagation();
                        }
                    })),
            )
            .child(
                div()
                    .key_context("Terminal")
                    .track_focus(&self.terminal_focus)
                    .size_full()
                    .on_key_down(cx.listener(|this, event, _, cx| {
                        let outcome = match terminal_key_down(&this.keymap, event, this.option) {
                            TerminalKey::Action(entry) => Outcome::Perform(entry.id),
                            TerminalKey::Pty(bytes) => Outcome::Write(bytes),
                            TerminalKey::Text | TerminalKey::Ignore => Outcome::Propagate,
                        };
                        match outcome {
                            Outcome::Perform(id) => {
                                this.performed.push(id);
                                cx.stop_propagation();
                            }
                            Outcome::Write(bytes) => {
                                this.pty.extend(bytes);
                                cx.stop_propagation();
                            }
                            Outcome::Propagate => {}
                        }
                    }))
                    .child(
                        canvas(
                            |_, _, _| {},
                            move |bounds, (), window, cx| {
                                window.handle_input(
                                    &terminal_focus,
                                    ElementInputHandler::new(bounds, entity),
                                    cx,
                                );
                            },
                        )
                        .size_full(),
                    ),
            )
    }
}

/// The terminal element's text input, delegating to [`ImeState`].
impl EntityInputHandler for Harness {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        self.ime.text_for_range(range, adjusted_range)
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(self.ime.selected_text_range())
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.ime.marked_text_range()
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.ime.unmark();
    }

    fn paste(&mut self, item: ClipboardItem, _: &mut Window, _: &mut Context<Self>) {
        if let Some(text) = item.text() {
            self.pty.extend(paste_bytes(&text, self.bracketed));
        }
    }

    fn replace_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        _: &mut Context<Self>,
    ) {
        let bytes = self.ime.commit(text);
        self.pty.extend(bytes);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        new_text: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) {
        self.ime.mark(new_text);
    }

    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        Some(element_bounds)
    }

    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pane {
    App,
    Terminal,
}

/// Open the test window with `keymap` bound, as `keys::register` binds it.
fn open(
    app: &mut TestAppContext,
    keymap: Keymap,
    option: OptionKey,
) -> (Entity<Harness>, &mut VisualTestContext) {
    let bindings = key_bindings(&keymap);
    app.update(|cx| cx.bind_keys(bindings));
    app.add_window_view(|_, cx| Harness::new(keymap, option, cx))
}

fn focus(view: &Entity<Harness>, cx: &mut VisualTestContext, pane: Pane) {
    cx.update(|window, cx| {
        let handle = match pane {
            Pane::App => view.read(cx).app_focus.clone(),
            Pane::Terminal => view.read(cx).terminal_focus.clone(),
        };
        handle.focus(window, cx);
    });
    cx.run_until_parked();
}

/// Press `keystrokes` in `pane`; what was performed and what reached the PTY.
fn press(
    view: &Entity<Harness>,
    cx: &mut VisualTestContext,
    pane: Pane,
    keystrokes: &str,
) -> (Vec<&'static str>, Vec<u8>) {
    focus(view, cx, pane);
    view.update(cx, |h, _| h.take());
    cx.simulate_keystrokes(keystrokes);
    view.update(cx, |h, _| h.take())
}

// ---------------------------------------------------------------------------
// Dispatch through real key contexts
// ---------------------------------------------------------------------------

#[test]
fn every_binding_dispatches_in_its_contexts_and_only_there() {
    for options in all_options() {
        let keymap = Keymap::new(options);
        let mut app = TestAppContext::single();
        let (view, cx) = open(&mut app, keymap.clone(), OptionKey::Meta);

        for (id, context, keystroke) in expected_matrix(options) {
            let chord = chord_from_keystroke(&Keystroke::parse(&keystroke).unwrap()).unwrap();
            let in_app = press(&view, cx, Pane::App, &keystroke);
            let in_terminal = press(&view, cx, Pane::Terminal, &keystroke);
            let label = format!("{keystroke} ({id}, {context}, {options:?})");
            match context {
                "Global" => {
                    assert_eq!(in_app, (vec![id], vec![]), "App mode: {label}");
                    assert_eq!(in_terminal, (vec![id], vec![]), "Terminal mode: {label}");
                }
                "App" => {
                    assert_eq!(in_app, (vec![id], vec![]), "App mode: {label}");
                    // With a terminal focused an App-only chord is typed, with
                    // the TUI's bytes.
                    assert_eq!(
                        in_terminal,
                        (vec![], encode_pty(chord)),
                        "Terminal mode: {label}"
                    );
                }
                "Terminal" => {
                    assert_eq!(in_terminal, (vec![id], vec![]), "Terminal mode: {label}");
                    // In App mode an unbound chord does nothing.
                    assert_eq!(in_app, (vec![], vec![]), "App mode: {label}");
                }
                other => panic!("unexpected context {other}"),
            }
            // The TUI resolves the same chord to the same entry in the same
            // modes (checked on the table: the TUI's crossterm adapter is
            // covered by its own tests).
            for (mode, bound) in [
                (InputMode::App, context != "Terminal"),
                (InputMode::Terminal, context != "App"),
            ] {
                assert_eq!(
                    keymap.lookup(mode, chord).map(|e| e.id),
                    bound.then_some(id),
                    "TUI lookup {mode:?}: {label}"
                );
            }
        }
    }
}

#[test]
fn app_mode_ctrl_chords_are_flightdeck_actions() {
    let keymap = Keymap::for_this_platform(false).clone();
    let mut app = TestAppContext::single();
    let (view, cx) = open(&mut app, keymap.clone(), OptionKey::for_this_platform());
    for letter in 'a'..='z' {
        let keystroke = format!("ctrl-{letter}");
        let chord = Chord::new(Key::Char(letter), Mods::CTRL);
        let expected: Vec<_> = keymap
            .lookup(InputMode::App, chord)
            .map(|e| e.id)
            .into_iter()
            .collect();
        let (performed, pty) = press(&view, cx, Pane::App, &keystroke);
        assert_eq!(performed, expected, "{keystroke}");
        // App mode never types.
        assert!(pty.is_empty(), "{keystroke}");
    }
}

#[test]
fn lenient_matches_follow_the_tui_tolerance() {
    // GPUI binds exact chords; the fallbacks apply the table's `tolerate`, so
    // extra modifiers the TUI ignores are ignored here too.
    let keymap = Keymap::for_this_platform(false).clone();
    let mut app = TestAppContext::single();
    let (view, cx) = open(&mut app, keymap, OptionKey::Meta);
    let cases = [
        (Pane::Terminal, "ctrl-shift-g", "OpenPalette"),
        (Pane::App, "ctrl-shift-g", "OpenPalette"),
        (Pane::Terminal, "alt-shift-up", "AgentTabPrev"),
        (Pane::Terminal, "ctrl-alt-1", "JumpToAgentTab1"),
        (Pane::App, "ctrl-alt-n", "NewAgentTab"),
        (Pane::Terminal, "ctrl-shift-v", "Paste"),
    ];
    for (pane, keystroke, id) in cases {
        assert_eq!(
            press(&view, cx, pane, keystroke),
            (vec![id], vec![]),
            "{keystroke} in {pane:?}"
        );
        // …exactly as the TUI resolves the crossterm event for it.
        let mode = match pane {
            Pane::App => InputMode::App,
            Pane::Terminal => InputMode::Terminal,
        };
        let chord = chord_from_keystroke(&Keystroke::parse(keystroke).unwrap()).unwrap();
        let tui = map_key_with_f2(mode, crossterm_event(chord), false);
        let entry = Keymap::for_this_platform(false).entry(id).unwrap();
        assert_eq!(
            tui,
            KeyAction::from(entry.action.clone()),
            "TUI {keystroke}"
        );
    }
}

#[test]
fn cmd_chords_are_left_to_the_platform() {
    // Cmd-Q, Cmd-comma, Cmd-C, Cmd-A: nothing performed, nothing typed, in
    // either mode (the event propagates to the app menu / platform).
    let keymap = Keymap::for_this_platform(false).clone();
    let mut app = TestAppContext::single();
    let (view, cx) = open(&mut app, keymap, OptionKey::for_this_platform());
    for keystroke in ["cmd-q", "cmd-,", "cmd-c", "cmd-a", "cmd-shift-g", "cmd-up"] {
        for pane in [Pane::App, Pane::Terminal] {
            assert_eq!(
                press(&view, cx, pane, keystroke),
                (vec![], vec![]),
                "{keystroke} in {pane:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Terminal-mode bytes versus the TUI
// ---------------------------------------------------------------------------

/// The crossterm event a terminal reports for `chord` (what the TUI sees).
fn crossterm_event(chord: Chord) -> KeyEvent {
    let code = match chord.key {
        Key::Char(c) => KeyCode::Char(c),
        Key::Enter => KeyCode::Enter,
        Key::Esc => KeyCode::Esc,
        Key::Tab => KeyCode::Tab,
        Key::BackTab => KeyCode::BackTab,
        Key::Backspace => KeyCode::Backspace,
        Key::Delete => KeyCode::Delete,
        Key::Up => KeyCode::Up,
        Key::Down => KeyCode::Down,
        Key::Left => KeyCode::Left,
        Key::Right => KeyCode::Right,
        Key::Home => KeyCode::Home,
        Key::End => KeyCode::End,
        Key::PageUp => KeyCode::PageUp,
        Key::PageDown => KeyCode::PageDown,
        Key::F(n) => KeyCode::F(n),
    };
    let mut modifiers = KeyModifiers::NONE;
    for (ours, theirs) in [
        (Mods::SHIFT, KeyModifiers::SHIFT),
        (Mods::CTRL, KeyModifiers::CONTROL),
        (Mods::ALT, KeyModifiers::ALT),
        (Mods::SUPER, KeyModifiers::SUPER),
    ] {
        if chord.mods.contains(ours) {
            modifiers |= theirs;
        }
    }
    KeyEvent {
        code,
        modifiers,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

/// Representative unbound keys: GPUI keystroke, the crossterm event a
/// terminal reports for the same key, and the bytes both must send.
fn representative_keys() -> Vec<(&'static str, KeyCode, KeyModifiers, &'static [u8])> {
    let none = KeyModifiers::NONE;
    let shift = KeyModifiers::SHIFT;
    let ctrl = KeyModifiers::CONTROL;
    let alt = KeyModifiers::ALT;
    vec![
        // Text, delivered through the input handler.
        ("a", KeyCode::Char('a'), none, b"a"),
        ("shift-a", KeyCode::Char('A'), shift, b"A"),
        ("space", KeyCode::Char(' '), none, b" "),
        ("1", KeyCode::Char('1'), none, b"1"),
        ("h", KeyCode::Char('h'), none, b"h"),
        ("o", KeyCode::Char('o'), none, b"o"),
        // Named keys, encoded on key-down.
        ("enter", KeyCode::Enter, none, b"\r"),
        ("escape", KeyCode::Esc, none, b"\x1b"),
        ("tab", KeyCode::Tab, none, b"\t"),
        ("shift-tab", KeyCode::BackTab, shift, b"\x1b[Z"),
        ("backspace", KeyCode::Backspace, none, b"\x7f"),
        ("delete", KeyCode::Delete, none, b"\x1b[3~"),
        ("up", KeyCode::Up, none, b"\x1b[A"),
        ("down", KeyCode::Down, none, b"\x1b[B"),
        ("right", KeyCode::Right, none, b"\x1b[C"),
        ("left", KeyCode::Left, none, b"\x1b[D"),
        ("shift-up", KeyCode::Up, shift, b"\x1b[A"),
        ("ctrl-up", KeyCode::Up, ctrl, b"\x1b[A"),
        ("home", KeyCode::Home, none, b"\x1b[H"),
        ("end", KeyCode::End, none, b"\x1b[F"),
        ("pageup", KeyCode::PageUp, none, b"\x1b[5~"),
        ("pagedown", KeyCode::PageDown, none, b"\x1b[6~"),
        ("f2", KeyCode::F(2), none, b"\x1bOQ"),
        ("f3", KeyCode::F(3), none, b"\x1bOR"),
        ("f4", KeyCode::F(4), none, b"\x1bOS"),
        ("f5", KeyCode::F(5), none, b"\x1b[15~"),
        ("f12", KeyCode::F(12), none, b"\x1b[24~"),
        ("f13", KeyCode::F(13), none, b""),
        // Ctrl chords (App-mode bindings included: they are not bound in
        // Terminal mode, so the agent gets them).
        ("ctrl-a", KeyCode::Char('a'), ctrl, b"\x01"),
        ("ctrl-c", KeyCode::Char('c'), ctrl, b"\x03"),
        ("ctrl-d", KeyCode::Char('d'), ctrl, b"\x04"),
        ("ctrl-n", KeyCode::Char('n'), ctrl, b"\x0e"),
        ("ctrl-r", KeyCode::Char('r'), ctrl, b"\x12"),
        ("ctrl-z", KeyCode::Char('z'), ctrl, b"\x1a"),
        ("ctrl-alt-a", KeyCode::Char('a'), ctrl | alt, b"\x1b\x01"),
        // Alt (Meta policy): ESC + key.
        ("alt-b", KeyCode::Char('b'), alt, b"\x1bb"),
        ("alt-f", KeyCode::Char('f'), alt, b"\x1bf"),
        ("alt-shift-a", KeyCode::Char('A'), alt | shift, b"\x1bA"),
        ("alt-backspace", KeyCode::Backspace, alt, b"\x7f"),
        ("alt-pageup", KeyCode::PageUp, alt, b"\x1b[5~"),
    ]
}

#[test]
fn unbound_terminal_keys_send_the_tui_bytes() {
    // The macOS-shaped table without F2, so F2 and the others above are free.
    let keymap = Keymap::new(KeymapOptions {
        use_f2_to_leave_focus: false,
        leave_focus_uses_shift: false,
        command_v_pastes: true,
    });
    let mut app = TestAppContext::single();
    let (view, cx) = open(&mut app, keymap.clone(), OptionKey::Meta);
    for (keystroke, code, modifiers, bytes) in representative_keys() {
        let (performed, pty) = press(&view, cx, Pane::Terminal, keystroke);
        assert!(performed.is_empty(), "{keystroke} performed {performed:?}");
        assert_eq!(pty, bytes, "GUI bytes for {keystroke}");

        let chord = chord_from_keystroke(&Keystroke::parse(keystroke).unwrap()).unwrap();
        assert!(keymap.lookup(InputMode::Terminal, chord).is_none());
        assert_eq!(encode_pty(chord), bytes, "encode_pty for {keystroke}");
        let event = KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        };
        assert_eq!(encode_key(event), bytes, "TUI encode_key for {keystroke}");
    }
}

#[test]
fn unbound_terminal_chords_send_encode_pty_bytes_across_the_sweep() {
    // Every key x every Shift/Ctrl/Alt combination that is unbound in Terminal
    // mode: the GUI writes exactly `encode_pty` (text keys via the input
    // handler, the rest on key-down).
    let keymap = Keymap::for_this_platform(false).clone();
    let mut app = TestAppContext::single();
    let (view, cx) = open(&mut app, keymap.clone(), OptionKey::Meta);
    for key in sweep_keys() {
        for mods in gpui_mods().filter(|m| !m.contains(Mods::SUPER)) {
            let chord = normalised(Chord::new(key, mods));
            if keymap.lookup(InputMode::Terminal, chord).is_some() {
                continue;
            }
            let keystroke = chord_to_gpui(chord).unwrap();
            let (performed, pty) = press(&view, cx, Pane::Terminal, &keystroke);
            assert!(performed.is_empty(), "{keystroke}");
            assert_eq!(pty, encode_pty(chord), "{keystroke}");
            assert_eq!(pty, encode_key(crossterm_event(chord)), "TUI {keystroke}");
        }
    }
}

// ---------------------------------------------------------------------------
// macOS Option, F2, IME, paste
// ---------------------------------------------------------------------------

#[test]
fn option_chords_bind_as_alt_under_either_policy() {
    // Keystrokes as macOS reports them: the keycap plus the composed glyph.
    let cases = [
        ("alt-1->¡", "JumpToAgentTab1"),
        ("alt-9->ª", "JumpToAgentTab9"),
        ("alt-o->ø", "OpenWorktreeInFileManager"),
        ("alt-h->˙", "OpenHelp"),
        ("alt-up", "AgentTabPrev"),
        ("alt-down", "AgentTabNext"),
        ("alt-left", "TerminalTabPrev"),
        ("alt-right", "TerminalTabNext"),
        ("alt-escape", "FocusApp"),
    ];
    let keymap = Keymap::new(KeymapOptions {
        use_f2_to_leave_focus: false,
        leave_focus_uses_shift: false,
        command_v_pastes: true,
    });
    for option in [OptionKey::Meta, OptionKey::Compose] {
        let mut app = TestAppContext::single();
        let (view, cx) = open(&mut app, keymap.clone(), option);
        for (keystroke, id) in cases {
            assert_eq!(
                press(&view, cx, Pane::Terminal, keystroke),
                (vec![id], vec![]),
                "{keystroke} under {option:?}"
            );
        }
    }
}

#[test]
fn unbound_option_keys_follow_the_policy() {
    let keymap = Keymap::for_this_platform(false).clone();
    let cases: [(&str, OptionKey, &[u8]); 7] = [
        // Compose: the composed character, as UTF-8.
        ("alt-b->∫", OptionKey::Compose, "∫".as_bytes()),
        ("alt-l->@", OptionKey::Compose, b"@"),
        // A dead key's accent, as macOS reports Option-e before the vowel.
        ("alt-e->´", OptionKey::Compose, "´".as_bytes()),
        // Compose without a composed character (what Linux reports): Meta.
        ("alt-b", OptionKey::Compose, b"\x1bb"),
        // Meta: ESC + the keycap, whatever was composed.
        ("alt-b->∫", OptionKey::Meta, b"\x1bb"),
        ("alt-l->@", OptionKey::Meta, b"\x1bl"),
        // Ctrl+Option is never text.
        ("ctrl-alt-b", OptionKey::Compose, b"\x1b\x02"),
    ];
    for (keystroke, option, bytes) in cases {
        let mut app = TestAppContext::single();
        let (view, cx) = open(&mut app, keymap.clone(), option);
        let (performed, pty) = press(&view, cx, Pane::Terminal, keystroke);
        assert!(performed.is_empty(), "{keystroke}");
        assert_eq!(pty, bytes, "{keystroke} under {option:?}");
    }
}

#[test]
fn f2_leaves_focus_only_when_enabled() {
    for (use_f2, f2, alt_escape) in [
        (true, (vec!["FocusApp"], vec![]), (vec![], b"\x1b".to_vec())),
        (
            false,
            (vec![], b"\x1bOQ".to_vec()),
            (vec!["FocusApp"], vec![]),
        ),
    ] {
        let keymap = Keymap::new(KeymapOptions {
            use_f2_to_leave_focus: use_f2,
            leave_focus_uses_shift: false,
            command_v_pastes: true,
        });
        let mut app = TestAppContext::single();
        let (view, cx) = open(&mut app, keymap, OptionKey::Meta);
        assert_eq!(
            press(&view, cx, Pane::Terminal, "f2"),
            f2,
            "use_f2={use_f2}"
        );
        // With F2 enabled, F2 still matches with modifiers held (the TUI's
        // tolerance), and Alt-Esc is typed.
        if use_f2 {
            assert_eq!(
                press(&view, cx, Pane::Terminal, "shift-f2"),
                (vec!["FocusApp"], vec![])
            );
        }
        assert_eq!(
            press(&view, cx, Pane::Terminal, "alt-escape"),
            alt_escape,
            "use_f2={use_f2}"
        );
        // Bare Esc always reaches the agent (its Esc Esc gesture).
        assert_eq!(
            press(&view, cx, Pane::Terminal, "escape escape"),
            (vec![], b"\x1b\x1b".to_vec())
        );
    }
}

#[test]
fn ime_composition_types_only_the_committed_text() {
    let keymap = Keymap::for_this_platform(false).clone();
    let mut app = TestAppContext::single();
    let (view, cx) = open(&mut app, keymap, OptionKey::for_this_platform());
    focus(&view, cx, Pane::Terminal);
    view.update(cx, |h, _| h.take());

    // What the platform does for a Japanese IME typing "nihon" then choosing
    // 日本: marked-text updates, then one insert. Driven through
    // `ElementInputHandler`, the adapter GPUI hands the platform.
    cx.update(|window, cx| {
        let mut handler = ElementInputHandler::new(Bounds::default(), view.clone());
        for preview in ["n", "に", "にh", "にほ", "にほn", "にほん"] {
            handler.replace_and_mark_text_in_range(None, preview, None, window, cx);
        }
        assert_eq!(handler.marked_text_range(window, cx), Some(0..3));
        let mut adjusted = None;
        assert_eq!(
            handler.text_for_range(1..3, &mut adjusted, window, cx),
            Some("ほん".to_string())
        );
        assert_eq!(adjusted, Some(1..3));
        handler.replace_text_in_range(None, "日本", window, cx);
        assert_eq!(handler.marked_text_range(window, cx), None);
    });
    assert_eq!(
        view.update(cx, |h, _| h.take()),
        (vec![], "日本".as_bytes().to_vec())
    );

    // An abandoned composition types nothing.
    cx.update(|window, cx| {
        let mut handler = ElementInputHandler::new(Bounds::default(), view.clone());
        handler.replace_and_mark_text_in_range(None, "か", None, window, cx);
        handler.unmark_text(window, cx);
        assert_eq!(handler.marked_text_range(window, cx), None);
    });
    assert_eq!(view.update(cx, |h, _| h.take()), (vec![], vec![]));

    // Emptying the marked text ends the composition, also without typing.
    cx.update(|window, cx| {
        let mut handler = ElementInputHandler::new(Bounds::default(), view.clone());
        handler.replace_and_mark_text_in_range(None, "か", None, window, cx);
        handler.replace_and_mark_text_in_range(None, "", None, window, cx);
        assert_eq!(handler.marked_text_range(window, cx), None);
    });
    assert_eq!(view.update(cx, |h, _| h.take()), (vec![], vec![]));
}

#[test]
fn typed_text_reaches_the_pty_through_the_input_handler() {
    // `simulate_input` types each character as the platform would: key-down
    // first (which leaves printable text alone), then the input handler.
    let keymap = Keymap::for_this_platform(false).clone();
    let mut app = TestAppContext::single();
    let (view, cx) = open(&mut app, keymap, OptionKey::for_this_platform());
    focus(&view, cx, Pane::Terminal);
    view.update(cx, |h, _| h.take());
    cx.simulate_input("git status -sb");
    assert_eq!(
        view.update(cx, |h, _| h.take()),
        (vec![], b"git status -sb".to_vec())
    );
}

#[test]
fn paste_is_bracketed_when_the_app_asked_for_it() {
    let keymap = Keymap::for_this_platform(false).clone();
    let mut app = TestAppContext::single();
    let (view, cx) = open(&mut app, keymap, OptionKey::for_this_platform());
    for (bracketed, bytes) in [
        (true, b"\x1b[200~echo 1\recho 2\x1b[201~".to_vec()),
        (false, b"echo 1\recho 2".to_vec()),
    ] {
        view.update(cx, |h, _| {
            h.bracketed = bracketed;
            h.take();
        });
        cx.update(|window, cx| {
            let mut handler = ElementInputHandler::new(Bounds::default(), view.clone());
            handler.paste(
                ClipboardItem::new_string("echo 1\necho 2".into()),
                window,
                cx,
            );
        });
        assert_eq!(view.update(cx, |h, _| h.take()), (vec![], bytes));
    }
}

/// The `[ui] macos_option_as_meta` setting picks the policy on macOS only; on
/// Linux and Windows both values resolve to Meta, so the file cannot make the
/// three OSes differ.
#[test]
fn the_option_as_meta_setting_only_matters_on_macos() {
    assert_eq!(OptionKey::resolve(true, false), OptionKey::Compose);
    assert_eq!(OptionKey::resolve(true, true), OptionKey::Meta);
    for setting in [false, true] {
        assert_eq!(OptionKey::resolve(false, setting), OptionKey::Meta);
    }
}

/// End to end through the terminal element's key path, for each setting on
/// each kind of OS: the bytes a composing macOS keyboard (`alt-b` reporting
/// `∫`) and a Linux/Windows one (`alt-b`, no composed character) produce.
#[test]
fn the_option_as_meta_setting_decides_what_a_composing_option_key_sends() {
    let keymap = Keymap::for_this_platform(false).clone();
    // (is_macos, setting, keystroke, expected PTY bytes)
    let cases: [(bool, bool, &str, &[u8]); 6] = [
        (true, false, "alt-b->∫", "∫".as_bytes()),
        (true, true, "alt-b->∫", b"\x1bb"),
        // macOS keys that compose nothing are Meta either way.
        (true, false, "alt-b", b"\x1bb"),
        (true, true, "alt-b", b"\x1bb"),
        // Elsewhere the setting changes nothing.
        (false, false, "alt-b", b"\x1bb"),
        (false, true, "alt-b", b"\x1bb"),
    ];
    for (is_macos, setting, keystroke, bytes) in cases {
        let option = OptionKey::resolve(is_macos, setting);
        let mut app = TestAppContext::single();
        let (view, cx) = open(&mut app, keymap.clone(), option);
        let (performed, pty) = press(&view, cx, Pane::Terminal, keystroke);
        assert!(performed.is_empty(), "{keystroke}");
        assert_eq!(
            pty, bytes,
            "{keystroke} (macos={is_macos}, option_as_meta={setting})"
        );
    }
}
