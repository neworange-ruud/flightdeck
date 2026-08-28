//! The tests that keep the browser's command surface honest as the palette
//! grows. See the module doc for what each one is defending.

use super::*;
use crate::tui::palette::{all_entries, REQUIRED_ACTION_COUNT};
use crate::web::protocol::command as names;

/// Requirement: **no palette action is silently unreachable from a browser.**
///
/// The compiler already forces every [`PaletteAction`] variant to be classified
/// ([`exposure_of`] has no wildcard arm). This is the other half: the name it
/// claims must really be in [`INVENTORY`], so classifying an action and then
/// forgetting to add its row cannot pass.
#[test]
fn every_palette_row_is_reachable_by_name() {
    for entry in all_entries() {
        match exposure_of(&entry.action) {
            Exposure::Wire(name) => {
                let spec = lookup(name).unwrap_or_else(|| {
                    panic!(
                        "palette row '{}' claims wire name '{name}', which is not in \
                         INVENTORY — add a CommandSpec for it",
                        entry.label
                    )
                });
                assert_eq!(
                    spec.label, entry.label,
                    "the browser's palette must read like the desktop's for '{name}'"
                );
                assert_eq!(
                    spec.group, entry.group,
                    "group mismatch for '{name}': the two palettes would disagree"
                );
            }
            Exposure::NotExposed(reason) => panic!(
                "palette row '{}' is not exposed to the browser ({reason}) — a row \
                 the desktop offers must be reachable by name, even if the host \
                 then refuses it",
                entry.label
            ),
        }
    }
}

/// The palette is the §22 inventory; the wire surface is that plus the browser's
/// own plumbing. Pinned so growing one without thinking about the other shows up
/// here rather than as a missing row in a browser.
#[test]
fn the_wire_surface_is_the_palette_plus_the_browsers_own_rows() {
    // Every §22 row, plus: three D3 selection templates, `edit_in_editor`
    // (D16 names it but the palette does not offer it), the three rows the
    // browser needs for itself (snapshot, seat, read-marking), and D13's two
    // dialog answers (`dialog_confirm` / `dialog_cancel`) — which the desktop
    // answers with its keyboard, so they have no palette row on either side.
    assert_eq!(all_entries().len(), REQUIRED_ACTION_COUNT);
    assert_eq!(INVENTORY.len(), REQUIRED_ACTION_COUNT + 9);
}

/// Two commands sharing a name would make one of them unreachable — the exact
/// failure this module exists to prevent, one step further along.
#[test]
fn wire_names_are_unique() {
    let mut seen = std::collections::HashSet::new();
    for spec in INVENTORY {
        assert!(
            seen.insert(spec.name),
            "duplicate wire name '{}' in INVENTORY",
            spec.name
        );
    }
    let mut claimed = std::collections::HashSet::new();
    for entry in all_entries() {
        if let Exposure::Wire(name) = exposure_of(&entry.action) {
            assert!(
                claimed.insert(name),
                "two palette rows both claim '{name}'; one of them is unreachable"
            );
        }
    }
}

/// The table and the classifier must agree in both directions: a row that
/// forwards must forward the action whose name it carries.
#[test]
fn forwarding_rows_carry_the_action_that_names_them() {
    for spec in INVENTORY {
        if let Route::Palette(action) = &spec.route {
            assert_eq!(
                exposure_of(action),
                Exposure::Wire(spec.name),
                "'{}' forwards an action whose wire name is not '{}'",
                spec.name,
                spec.name
            );
        }
    }
}

/// The commands with no wire name of their own are listed here with the reason,
/// so "deliberately not exposed" is a decision on the record rather than an
/// omission. Each is the payload-carrying second phase of a flow whose first
/// phase *is* a row, or is hidden from the palette outright.
#[test]
fn the_commands_with_no_wire_name_say_why() {
    let deliberately_not_exposed = [
        Command::NewAgentTab {
            name: String::new(),
            agent_key: None,
        },
        Command::RenameAgentTab {
            new_name: String::new(),
        },
        Command::CloseAgentTab { action: None },
        Command::NewAgentTerminal { agent_key: None },
        Command::SetManualStatus(None),
        Command::CopyEnvFile,
    ];
    for cmd in &deliberately_not_exposed {
        match exposure_of_command(cmd) {
            Exposure::NotExposed(reason) => assert!(
                reason.len() > 20,
                "{cmd:?} needs a real reason, not '{reason}'"
            ),
            Exposure::Wire(name) => {
                panic!("{cmd:?} is exposed as '{name}'; update this test if that is intended")
            }
        }
    }
}

// ---------------------------------------------------------------------------
// D16 and the two-step confirmation
// ---------------------------------------------------------------------------

/// D16: desktop-only actions stay visible and are honest about where their
/// effect lands. Exactly two carry the badge, and both refuse with the same
/// sentence rather than reporting a success that happened on another machine.
#[test]
fn the_two_desktop_only_actions_are_badged_and_refuse() {
    let badged: Vec<&str> = INVENTORY
        .iter()
        .filter(|spec| spec.host_only)
        .map(|spec| spec.name)
        .collect();
    assert_eq!(
        badged,
        vec![names::OPEN_WORKTREE_IN_FILE_MANAGER, names::EDIT_IN_EDITOR],
        "D16 names exactly these two"
    );
    for name in badged {
        let spec = lookup(name).expect("badged commands are in the inventory");
        assert_eq!(spec.route, Route::Rejected(HOST_ONLY_REFUSAL));
        assert_eq!(spec.view().refusal.as_deref(), Some(HOST_ONLY_REFUSAL));
        assert!(spec.view().host_only, "the badge must reach the browser");
    }
}

/// D16: `quit` kills FlightDeck and every agent, so a bare frame naming it must
/// not reach a dispatch at all. It is refused here, in the table, which is why
/// no code path exists that could run it.
#[test]
fn quit_is_refused_by_the_table_not_by_a_check() {
    let spec = lookup(names::QUIT).expect("quit is a name the host knows");
    assert_eq!(spec.route, Route::Rejected(QUIT_REFUSAL));
    assert!(
        dispatched_command(&spec.route).is_none(),
        "quit must not be on a dispatching route"
    );
    assert!(
        INVENTORY
            .iter()
            .all(|s| dispatched_command(&s.route) != Some(&Command::Quit)),
        "no row may dispatch Command::Quit"
    );
}

// ---------------------------------------------------------------------------
// The git-ownership boundary (SPECS §5)
// ---------------------------------------------------------------------------

/// SPECS §5: no browser-reachable path may rewrite history or create a pull
/// request. This holds by construction, not by a runtime check — the two
/// history-touching commands are simply not on a dispatching route, and the
/// dispatching routes carry their own payloads so a frame cannot supply one.
#[test]
fn no_browser_reachable_route_rewrites_history_or_opens_a_pr() {
    for spec in INVENTORY {
        let Some(cmd) = dispatched_command(&spec.route) else {
            continue;
        };
        assert!(
            !rewrites_history(cmd),
            "'{}' would dispatch {cmd:?}, which rewrites history (SPECS §5)",
            spec.name
        );
        assert!(
            !creates_pull_request(cmd),
            "'{}' would dispatch {cmd:?}, which creates a PR (SPECS §5)",
            spec.name
        );
    }

    // The two history-touching commands are named on the wire — the browser
    // shows the row — and refused, so the surface is honest in both directions.
    for name in [names::REBASE_WORKTREE, names::PULL_BASE] {
        let spec = lookup(name).expect("the row is offered");
        assert!(
            spec.refusal().is_some(),
            "'{name}' must refuse: it rewrites history"
        );
    }
}

/// A `Command` frame carries `args`, and a forwarding row ignores them: the
/// action — every `confirm` flag included — comes from this table. So no browser
/// frame can smuggle a confirmed destructive operation past the confirmation
/// the desktop would have asked for.
#[test]
fn no_forwarding_row_carries_a_confirmation() {
    for spec in INVENTORY {
        let Some(cmd) = dispatched_command(&spec.route) else {
            continue;
        };
        let confirmed = matches!(
            cmd,
            Command::AbandonWorktree { confirm: true }
                | Command::FinishLocalMerge { confirm: true }
                | Command::RebaseWorktree { confirm: true }
                | Command::PushBranch { confirm: Some(_) }
        );
        assert!(
            !confirmed,
            "'{}' carries a pre-confirmed {cmd:?}",
            spec.name
        );
    }
}

// ---------------------------------------------------------------------------
// Seats and the wire view
// ---------------------------------------------------------------------------

/// D14: only the two rows the server answers from published state are open to a
/// read-only observer. Everything else is a controller's frame — including the
/// ones the host would refuse, so an observer is told `read_only` rather than
/// being handed the reason a command it may not send would have failed.
#[test]
fn only_the_servers_own_rows_are_open_to_an_observer() {
    let open: Vec<&str> = INVENTORY
        .iter()
        .filter(|spec| !spec.requires_control())
        .map(|spec| spec.name)
        .collect();
    assert_eq!(open, vec![names::REQUEST_SNAPSHOT, names::RELEASE_SEAT]);
}

/// The template rows are the ones the browser has to fill an id into; every
/// other row is sendable exactly as it arrives.
#[test]
fn only_the_target_taking_rows_are_templates() {
    let templates: Vec<(&str, CommandTarget)> = INVENTORY
        .iter()
        .filter_map(|spec| spec.target().map(|t| (spec.name, t)))
        .collect();
    assert_eq!(
        templates,
        vec![
            (names::SELECT_SESSION, CommandTarget::Session),
            (names::SELECT_PROJECT, CommandTarget::Project),
            (names::SELECT_TERMINAL, CommandTarget::Terminal),
            (names::MARK_ACTIVITY_READ, CommandTarget::UnreadActivity),
        ]
    );
}

/// The row the browser receives carries everything its palette model needs, and
/// `run.name` is the name it sends back — the round trip a placeholder list
/// cannot make.
#[test]
fn a_view_round_trips_its_own_name() {
    for spec in INVENTORY {
        let view = spec.view();
        assert_eq!(view.run.name, spec.name);
        assert_eq!(view.id, spec.name);
        assert!(!view.label.is_empty());
        assert!(!view.group.is_empty());
        assert!(
            lookup(&view.run.name).is_some(),
            "a row the host sends must be a row the host accepts"
        );
    }
    assert_eq!(inventory().len(), INVENTORY.len());
}

/// Every refusal reads as a sentence a user can act on, because it is what the
/// browser shows verbatim.
#[test]
fn refusals_are_sentences() {
    for spec in INVENTORY {
        if let Some(reason) = spec.refusal() {
            assert!(
                reason.len() > 30 && reason.ends_with('.'),
                "'{}' refuses with '{reason}'",
                spec.name
            );
        }
    }
}

/// D13: exactly the two dialog answers say so on the wire, so the browser's
/// palette can leave them out without a hardcoded list of its own — and every
/// other row is a palette row.
#[test]
fn only_the_dialog_answers_are_flagged_as_answering_a_dialog() {
    let answering: Vec<&str> = INVENTORY
        .iter()
        .filter(|spec| spec.view().answers_dialog)
        .map(|spec| spec.name)
        .collect();
    assert_eq!(
        answering,
        vec![names::DIALOG_CONFIRM, names::DIALOG_CANCEL],
        "the palette on either surface is INVENTORY minus these two"
    );
}
