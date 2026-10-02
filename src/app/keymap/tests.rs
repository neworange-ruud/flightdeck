use std::collections::HashSet;

use super::*;

/// Every combination of the three options, so each platform's table and both
/// leave-focus settings are covered on whichever OS runs the tests.
fn all_options() -> Vec<KeymapOptions> {
    let mut out = Vec::new();
    for use_f2 in [false, true] {
        for shift in [false, true] {
            for cmd_v in [false, true] {
                for desktop in [false, true] {
                    out.push(KeymapOptions {
                        use_f2_to_leave_focus: use_f2,
                        leave_focus_uses_shift: shift,
                        command_v_pastes: cmd_v,
                        desktop,
                    });
                }
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
        desktop: false,
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

/// Alt-m (the Projects / Mission control switch) is new, so prove it took
/// nothing: in App mode it was unbound under every option set and every
/// modifier combination that could reach it, and in Terminal mode it is still
/// unbound, so the PTY keeps receiving Meta-m (`ESC m`) — zsh's
/// copy-prev-shell-word, and Claude Code's Shift+Tab fallback on Windows.
#[test]
fn alt_m_switches_views_in_app_mode_and_stays_the_terminals() {
    let alt_m = Chord::new(Key::Char('m'), Mods::ALT);
    for options in all_options() {
        let keymap = Keymap::new(options);
        assert_eq!(
            keymap.lookup(InputMode::App, alt_m).map(|e| e.id),
            Some("ToggleMissionControl")
        );
        // No other entry is bound to any m chord, in any context.
        for entry in keymap.entries() {
            for t in &entry.triggers {
                if t.chord.key == Key::Char('m') || t.chord.key == Key::Char('M') {
                    assert_eq!(entry.id, "ToggleMissionControl", "{options:?}");
                    assert_eq!(t.context, Context::App, "never Global or Terminal");
                    assert_eq!(t.tolerate, Mods::NONE, "exact: Cmd-Alt-m is the OS's");
                }
            }
        }
        for mods in all_mods() {
            let chord = Chord::new(Key::Char('m'), mods);
            assert!(
                keymap.lookup(InputMode::Terminal, chord).is_none(),
                "{chord} must reach the PTY ({options:?})"
            );
            if mods != Mods::ALT {
                assert!(
                    keymap.lookup(InputMode::App, chord).is_none(),
                    "{chord} is not the switch"
                );
            }
        }
    }
    assert_eq!(encode_pty(alt_m), b"\x1bm".to_vec());
    let keymap = Keymap::new(KeymapOptions::for_this_platform(false));
    let entry = keymap.entry("ToggleMissionControl").unwrap();
    assert_eq!(entry.keycap().as_deref(), Some("Alt-m"));
    assert_eq!(entry.help_section, Some("Projects"));
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

// --- bracketed paste encoding ---------------------------------------------

#[test]
fn encode_paste_wraps_when_app_enabled_bracketed_mode() {
    // A multi-line paste must reach a bracketed-paste-aware agent as one
    // atomic insert (guarded by ESC[200~/ESC[201~), not line-by-line, so it
    // does not execute the first line and queue the rest as prompts.
    let bytes = encode_paste("line one\nline two", true);
    assert_eq!(bytes, b"\x1b[200~line one\rline two\x1b[201~".to_vec());
}

#[test]
fn encode_paste_passes_raw_when_app_disabled_bracketed_mode() {
    // Without bracketed paste mode the app gets the raw text, exactly as a
    // real terminal forwards a paste — no guards inserted.
    let bytes = encode_paste("line one\nline two", false);
    assert_eq!(bytes, b"line one\rline two".to_vec());
}

#[test]
fn encode_paste_normalises_crlf_and_lf_to_cr() {
    // Both CRLF (Windows clipboard) and bare LF collapse to a single CR.
    assert_eq!(encode_paste("a\r\nb\nc", false), b"a\rb\rc".to_vec());
}

// --- isolated runs -------------------------------------------------------------

#[test]
fn an_isolated_run_refuses_exactly_new_agent_and_project_switching() {
    // SPECS §32: one session, one project. The palette hides the same actions;
    // a native front-end draws these entries' controls disabled.
    let keymap = Keymap::new(KeymapOptions::for_this_platform(false));
    let refused: Vec<&str> = keymap
        .entries()
        .iter()
        .filter(|e| e.refused_when_isolated())
        .map(|e| e.id)
        .collect();
    assert_eq!(
        refused,
        ["SwitchProjectPrev", "SwitchProjectNext", "NewAgentTab"]
    );
}

// --- desktop-only help rows ---------------------------------------------------

/// Every help row's keys and description, flattened, for comparing two tables.
fn help_rows(options: KeymapOptions) -> Vec<(String, &'static str)> {
    Keymap::new(options)
        .help_sections()
        .iter()
        .flat_map(|s| s.rows.iter().map(|r| (r.keys.clone(), r.description)))
        .collect()
}

#[test]
fn desktop_shortcut_rows_appear_only_for_the_macos_desktop() {
    let base = KeymapOptions::for_this_platform(false);
    let with = |desktop, command_v_pastes| {
        help_rows(KeymapOptions {
            desktop,
            command_v_pastes,
            ..base
        })
    };
    let has = |rows: &[(String, &str)], keys: &str| rows.iter().any(|(k, _)| k == keys);

    let desktop_macos = with(true, true);
    assert!(has(&desktop_macos, "Cmd-C"));
    assert!(has(&desktop_macos, "Cmd-= / Cmd-- / Cmd-0"));

    // The TUI (even on macOS) and the desktop off macOS have neither chord, so
    // help must not claim them; both are exactly the table without the rows.
    let tui_macos = with(false, true);
    let desktop_elsewhere = with(true, false);
    for rows in [&tui_macos, &desktop_elsewhere] {
        assert!(!has(rows, "Cmd-C"));
        assert!(!has(rows, "Cmd-= / Cmd-- / Cmd-0"));
    }
    let without: Vec<_> = desktop_macos
        .iter()
        .filter(|(k, _)| k != "Cmd-C" && k != "Cmd-= / Cmd-- / Cmd-0")
        .cloned()
        .collect();
    assert_eq!(without, tui_macos);
}

// --- bracketed paste sanitising -----------------------------------------------

const START: &str = "\x1b[200~";
const END: &str = "\x1b[201~";

#[test]
fn bracketed_paste_drops_an_embedded_end_marker() {
    // The injection: without stripping, `rm -rf ~` would arrive after the
    // bracket closed, as typed input.
    let bytes = encode_paste("safe\x1b[201~rm -rf ~\n", true);
    assert_eq!(bytes, b"\x1b[200~saferm -rf ~\r\x1b[201~".to_vec());
    let text = String::from_utf8(bytes).unwrap();
    assert_eq!(text.matches(END).count(), 1, "only the closing guard");
}

#[test]
fn bracketed_paste_drops_a_marker_that_reassembles_after_one_pass() {
    // Removing the inner marker glues `ESC[20` and `1~` into a new one.
    let text = String::from_utf8(encode_paste("a\x1b[20\x1b[201~1~b", true)).unwrap();
    assert_eq!(text, format!("{START}ab{END}"));
    // Nested three deep.
    let nested = "\x1b[\x1b[\x1b[201~201~201~";
    let text = String::from_utf8(encode_paste(nested, true)).unwrap();
    assert_eq!(text, format!("{START}{END}"));
}

#[test]
fn bracketed_paste_drops_the_start_marker_too() {
    let text = String::from_utf8(encode_paste("x\x1b[200~y", true)).unwrap();
    assert_eq!(text, format!("{START}xy{END}"));
}

#[test]
fn bracketed_paste_keeps_unicode_and_other_escapes() {
    // Only the two markers go: a pasted colour code and non-ASCII text are
    // byte-identical inside the guards.
    let text = "héllo \u{1f680} \x1b[31mred\x1b[0m 日本語";
    let bytes = encode_paste(text, true);
    assert_eq!(bytes, format!("{START}{text}{END}").into_bytes());
}

#[test]
fn bracketed_paste_of_nothing_is_just_the_guards() {
    assert_eq!(encode_paste("", true), format!("{START}{END}").into_bytes());
    assert_eq!(
        encode_paste("\x1b[201~", true),
        format!("{START}{END}").into_bytes()
    );
}

#[test]
fn unbracketed_paste_is_forwarded_unchanged() {
    // No bracket to escape, so nothing is stripped (see `encode_paste`).
    assert_eq!(
        encode_paste("a\x1b[201~b\x1b[200~c", false),
        b"a\x1b[201~b\x1b[200~c".to_vec()
    );
}
