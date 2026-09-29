use std::collections::HashSet;

use super::*;

/// Every combination of the three options, so each platform's table and both
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

/// A broad sample of keys: every binding's key plus neighbours.
fn sample_keys() -> Vec<Key> {
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
    ];
    keys.extend((0..=24).map(Key::F));
    keys.extend((0x20u8..0x7f).map(|b| Key::Char(char::from(b))));
    keys
}

fn all_mods() -> impl Iterator<Item = Mods> {
    (0..=Mods::ALL.bits()).map(Mods::from_bits_truncate)
}

/// Within one context no chord is bound twice, and no chord is bound both
/// globally and in a mode-specific context (which would shadow it).
#[test]
fn every_chord_is_unique_within_its_context() {
    for options in all_options() {
        let keymap = Keymap::new(options);
        for mode in [InputMode::Terminal, InputMode::App] {
            let mut seen = HashSet::new();
            for entry in keymap.entries() {
                for t in entry.triggers.iter().filter(|t| t.context.applies_in(mode)) {
                    assert!(
                        seen.insert(t.chord),
                        "{} is bound twice in {mode:?} ({options:?})",
                        t.chord
                    );
                }
            }
        }
    }
}

/// Stronger than uniqueness: with tolerances applied, no key press fires two
/// bindings live in the same mode. This is what makes lookup order irrelevant.
#[test]
fn no_key_press_matches_two_bindings() {
    for options in all_options() {
        let keymap = Keymap::new(options);
        for mode in [InputMode::Terminal, InputMode::App] {
            for key in sample_keys() {
                for mods in all_mods() {
                    let chord = Chord::new(key, mods);
                    let hits: Vec<_> = keymap
                        .entries()
                        .iter()
                        .flat_map(|e| e.triggers.iter().map(move |t| (e.id, t)))
                        .filter(|(_, t)| t.context.applies_in(mode) && t.matches(chord))
                        .map(|(id, _)| id)
                        .collect();
                    assert!(hits.len() <= 1, "{chord} in {mode:?} hits {hits:?}");
                }
            }
        }
    }
}

/// Every entry's own chord looks up to itself.
#[test]
fn each_trigger_looks_up_to_its_entry() {
    for options in all_options() {
        let keymap = Keymap::new(options);
        for entry in keymap.entries() {
            for t in &entry.triggers {
                for mode in [InputMode::Terminal, InputMode::App] {
                    if t.context.applies_in(mode) {
                        assert_eq!(keymap.lookup(mode, t.chord).map(|e| e.id), Some(entry.id));
                    }
                }
            }
        }
    }
}

#[test]
fn ids_are_unique_pascal_case_action_names() {
    let keymap = Keymap::new(KeymapOptions::for_this_platform(false));
    let mut seen = HashSet::new();
    for entry in keymap.entries() {
        assert!(seen.insert(entry.id), "duplicate id {}", entry.id);
        let mut chars = entry.id.chars();
        assert!(chars.next().is_some_and(|c| c.is_ascii_uppercase()));
        assert!(chars.all(|c| c.is_ascii_alphanumeric()), "{}", entry.id);
        assert_eq!(
            entry.gpui_action_name(),
            format!("flightdeck::{}", entry.id)
        );
        assert!(!entry.description.is_empty());
        assert!(!entry.triggers.is_empty());
    }
    // One action per entry, so a front-end can map an action back to its id.
    let actions: Vec<_> = keymap.entries().iter().map(|e| &e.action).collect();
    for (i, a) in actions.iter().enumerate() {
        assert!(!actions[..i].contains(a), "{a:?} appears twice");
    }
}

/// Help documents exactly the table: every help row's ids exist, every entry
/// is on exactly one row (except the documented hidden ones), and each entry's
/// `help_section` is the section that row sits in.
#[test]
fn help_lists_exactly_the_tables_entries() {
    for options in all_options() {
        let keymap = Keymap::new(options);
        let mut listed = Vec::new();
        for section in keymap.help_sections() {
            for row in &section.rows {
                for id in &row.entry_ids {
                    let entry = keymap
                        .entry(id)
                        .unwrap_or_else(|| panic!("help row names unknown id {id}"));
                    assert_eq!(entry.help_section, Some(section.title));
                    listed.push(*id);
                }
            }
        }
        let unique: HashSet<_> = listed.iter().collect();
        assert_eq!(unique.len(), listed.len(), "an entry is on two help rows");
        for entry in keymap.entries() {
            let hidden = HIDDEN_FROM_HELP.contains(&entry.id);
            assert_eq!(
                listed.contains(&entry.id),
                !hidden,
                "{} listed/hidden mismatch",
                entry.id
            );
            assert_eq!(entry.help_section.is_none(), hidden);
        }
    }
}

#[test]
fn derived_help_keys_are_spelled_from_the_chords() {
    let keymap = Keymap::new(KeymapOptions::for_this_platform(false));
    let keys = |id: &str| {
        keymap
            .help_sections()
            .iter()
            .flat_map(|s| &s.rows)
            .find(|r| r.entry_ids.first() == Some(&id))
            .map(|r| r.keys.clone())
            .unwrap()
    };
    assert_eq!(keys("OpenPalette"), "Ctrl-g");
    assert_eq!(keys("OpenHelp"), "F1 / Alt-h");
    assert_eq!(keys("SwitchProjectPrev"), "Shift-Left / Shift-Right");
    assert_eq!(keys("FocusTerminal"), "Enter");
}

#[test]
fn leave_focus_binding_follows_the_options() {
    let esc = |mods| Chord::new(Key::Esc, mods);
    let base = KeymapOptions {
        use_f2_to_leave_focus: false,
        leave_focus_uses_shift: false,
        command_v_pastes: false,
    };
    let focus = |o: KeymapOptions, c: Chord| {
        Keymap::new(o).lookup(InputMode::Terminal, c).map(|e| e.id) == Some("FocusApp")
    };
    assert!(focus(base, esc(Mods::ALT)));
    assert!(!focus(base, esc(Mods::SHIFT)));
    let shift = KeymapOptions {
        leave_focus_uses_shift: true,
        ..base
    };
    assert!(focus(shift, esc(Mods::SHIFT)));
    assert!(!focus(shift, esc(Mods::ALT)));
    let f2 = KeymapOptions {
        use_f2_to_leave_focus: true,
        ..base
    };
    assert!(focus(f2, Chord::bare(Key::F(2))));
    assert!(!focus(f2, esc(Mods::ALT)));
    // Bare Esc is never claimed.
    for o in all_options() {
        assert!(Keymap::new(o)
            .lookup(InputMode::Terminal, Chord::bare(Key::Esc))
            .is_none());
    }
    assert_eq!(leave_focus_label(base), "Alt+Esc");
    assert_eq!(leave_focus_label(shift), "Shift+Esc");
    assert_eq!(leave_focus_label(f2), "F2");
}

#[test]
fn command_v_pastes_only_when_enabled() {
    let cmd_v = Chord::new(Key::Char('v'), Mods::SUPER);
    for o in all_options() {
        let hit = Keymap::new(o)
            .lookup(InputMode::Terminal, cmd_v)
            .map(|e| e.id);
        assert_eq!(hit == Some("Paste"), o.command_v_pastes);
    }
}

#[test]
fn chord_display_matches_the_help_spelling() {
    assert_eq!(Chord::new(Key::Char('g'), Mods::CTRL).to_string(), "Ctrl-g");
    assert_eq!(Chord::new(Key::Up, Mods::ALT).to_string(), "Alt-Up");
    assert_eq!(Chord::new(Key::Left, Mods::SHIFT).to_string(), "Shift-Left");
    assert_eq!(Chord::bare(Key::F(1)).to_string(), "F1");
    assert_eq!(
        Chord::new(Key::Char('x'), Mods::CTRL | Mods::ALT | Mods::SHIFT).to_string(),
        "Ctrl-Alt-Shift-x"
    );
}

#[test]
fn keycap_is_the_first_chord() {
    let keymap = Keymap::new(KeymapOptions::for_this_platform(false));
    assert_eq!(
        keymap.entry("AgentTabNext").and_then(|e| e.keycap()),
        Some("Alt-Down".to_string())
    );
    assert_eq!(
        keymap.entry("PushBranch").and_then(|e| e.keycap()),
        Some("Ctrl-p".to_string())
    );
}

#[test]
fn bindings_in_partitions_the_triggers_by_context() {
    let keymap = Keymap::new(KeymapOptions::for_this_platform(false));
    let total: usize = keymap.entries().iter().map(|e| e.triggers.len()).sum();
    let by_context: usize = [Context::Global, Context::Terminal, Context::App]
        .into_iter()
        .map(|c| keymap.bindings_in(c).count())
        .sum();
    assert_eq!(total, by_context);
    assert!(keymap
        .bindings_in(Context::App)
        .any(|(e, t)| e.id == "AgentTabPrev" && t.chord == Chord::bare(Key::Up)));
}

/// The byte encoding, table-driven, as shipped. Arrows are CSI regardless of
/// modifiers (and of any DECCKM state, which the encoder never sees).
#[test]
fn encode_pty_matches_the_shipped_bytes() {
    let c = Chord::new;
    let cases: &[(Chord, &[u8])] = &[
        (Chord::bare(Key::Up), b"\x1b[A"),
        (Chord::bare(Key::Down), b"\x1b[B"),
        (Chord::bare(Key::Right), b"\x1b[C"),
        (Chord::bare(Key::Left), b"\x1b[D"),
        (c(Key::Up, Mods::CTRL), b"\x1b[A"),
        (c(Key::Left, Mods::ALT), b"\x1b[D"),
        (c(Key::Right, Mods::SHIFT), b"\x1b[C"),
        (Chord::bare(Key::Home), b"\x1b[H"),
        (Chord::bare(Key::End), b"\x1b[F"),
        (Chord::bare(Key::PageUp), b"\x1b[5~"),
        (Chord::bare(Key::PageDown), b"\x1b[6~"),
        (Chord::bare(Key::Delete), b"\x1b[3~"),
        (Chord::bare(Key::Enter), b"\r"),
        (Chord::bare(Key::Tab), b"\t"),
        (c(Key::Tab, Mods::SHIFT), b"\t"),
        (c(Key::BackTab, Mods::SHIFT), b"\x1b[Z"),
        (Chord::bare(Key::Backspace), b"\x7f"),
        (Chord::bare(Key::Esc), b"\x1b"),
        (c(Key::Esc, Mods::ALT), b"\x1b"),
        (Chord::bare(Key::F(1)), b"\x1bOP"),
        (Chord::bare(Key::F(4)), b"\x1bOS"),
        (Chord::bare(Key::F(5)), b"\x1b[15~"),
        (Chord::bare(Key::F(12)), b"\x1b[24~"),
        (Chord::bare(Key::F(13)), b""),
        (Chord::bare(Key::F(0)), b""),
        (Chord::bare(Key::Char('a')), b"a"),
        (c(Key::Char('A'), Mods::SHIFT), b"A"),
        (c(Key::Char('a'), Mods::CTRL), b"\x01"),
        (c(Key::Char('Z'), Mods::CTRL), b"\x1a"),
        (c(Key::Char('c'), Mods::CTRL), b"\x03"),
        (c(Key::Char('1'), Mods::CTRL), b"1"),
        (c(Key::Char('b'), Mods::ALT), b"\x1bb"),
        (c(Key::Char('b'), Mods::CTRL | Mods::ALT), b"\x1b\x02"),
        (Chord::bare(Key::Char('é')), "é".as_bytes()),
        (c(Key::Char('é'), Mods::ALT), "\x1bé".as_bytes()),
    ];
    for (chord, bytes) in cases {
        assert_eq!(encode_pty(*chord), bytes.to_vec(), "{chord:?}");
    }
}

#[test]
fn this_platform_table_is_cached_per_setting() {
    let a = Keymap::for_this_platform(false);
    let b = Keymap::for_this_platform(true);
    assert!(std::ptr::eq(a, Keymap::for_this_platform(false)));
    assert!(!a.options().use_f2_to_leave_focus);
    assert!(b.options().use_f2_to_leave_focus);
}
