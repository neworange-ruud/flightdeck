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
// D16 and artboard 1g's two-step confirmation
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

/// **D16 + artboard 1g: a frame naming `quit` can only ever ask.**
///
/// `quit` stops FlightDeck and every agent in it, so the row carries the
/// *unconfirmed* value and nothing else — the first dispatch returns
/// `Effect::QuitConfirm`, which is D13's shared dialog, and a browser's answer
/// to that dialog has to pass the typed-name gate (`browser_confirm_gate` in
/// `src/lib.rs`). The property this pins is the one R7 stated for the refusal it
/// replaces: no row anywhere may hand `Command::Quit { confirm: true }` to a
/// dispatch, so no frame can reach the value that quits.
#[test]
fn no_row_can_dispatch_a_confirmed_quit() {
    let spec = lookup(names::QUIT).expect("quit is a name the host knows");
    assert_eq!(
        dispatched_command(&spec.route),
        Some(&Command::Quit { confirm: false }),
        "the row must carry the value that can only ask (D16)"
    );
    assert_eq!(
        confirmation_of(&Command::Quit { confirm: false }),
        Confirmation::Pending
    );
    assert!(
        INVENTORY
            .iter()
            .all(|s| dispatched_command(&s.route) != Some(&Command::Quit { confirm: true })),
        "no row may dispatch a confirmed Command::Quit"
    );
    // And the row still says what it is, so nobody meets it by accident.
    assert_eq!(spec.view().annotation.as_deref(), Some("destructive"));
    assert!(
        !spec.view().host_only,
        "D16: quit is not a `host only` badge — a badge is not enough for it"
    );
}

/// **The destructive pair carry their unconfirmed values, and only those.**
///
/// `abandon_worktree` joined `rebase_worktree` on a dispatching route in
/// `remote-control-ll5.4`. What makes that safe is unchanged from R7/R11: the
/// forwarding row's payload comes from this table, so the first dispatch can
/// only raise SPECS §5/§15's question, and 1g's typed-name step stands in front
/// of a browser's answer to it.
#[test]
fn the_destructive_rows_can_only_ask() {
    for (name, cmd) in [
        (
            names::ABANDON_WORKTREE,
            Command::AbandonWorktree { confirm: false },
        ),
        (names::QUIT, Command::Quit { confirm: false }),
    ] {
        let spec = lookup(name).expect("the row is offered");
        assert_eq!(dispatched_command(&spec.route), Some(&cmd));
        assert_eq!(confirmation_of(&cmd), Confirmation::Pending);
        assert!(
            spec.refusal().is_none(),
            "`{name}` runs now — it must not also claim it will be refused"
        );
        assert!(
            spec.view().run.args.is_none(),
            "`{name}` hands the browser no args to echo back"
        );
    }
}

// ---------------------------------------------------------------------------
// The git-ownership boundary (SPECS §5)
// ---------------------------------------------------------------------------

/// **The boundary invariant, restated for a browser that can now run git**
/// (SPECS §5, §5.1, §5.2).
///
/// `remote-control-ll5.5` put the git family on dispatching routes, so "no
/// browser-reachable route rewrites history" — true while every git row refused
/// — would now be false, and weakening it into a rubber stamp would throw away
/// the only thing making the surface safe. The invariant it becomes instead:
///
/// > No browser-reachable route may rewrite history **except** through a route
/// > whose dispatched command is [`Confirmation::Pending`] and therefore lands
/// > on §5.1's confirmation prompt; and no browser-reachable route may create a
/// > pull request, ever, with no exception.
///
/// Both halves hold by construction. The exception is checkable rather than
/// asserted in prose — a `Pending` value *cannot* perform the rewrite, because
/// the first dispatch returns the prompt — and it is narrow by construction too:
/// [`Command::PullBase`] has no confirmation step at all
/// ([`Confirmation::None`]), so no value of it could ever satisfy the clause,
/// which is why `pull_base` is not on a dispatching route.
#[test]
fn no_browser_reachable_route_rewrites_unconfirmed_history_or_opens_a_pr() {
    let mut rewriting: Vec<&str> = Vec::new();
    for spec in INVENTORY {
        let Some(cmd) = dispatched_command(&spec.route) else {
            continue;
        };
        if rewrites_history(cmd) {
            rewriting.push(spec.name);
            assert_eq!(
                confirmation_of(cmd),
                Confirmation::Pending,
                "'{}' would dispatch {cmd:?}, which rewrites history without \
                 landing on SPECS §5.1's confirmation prompt first",
                spec.name
            );
        }
        // No exception clause on this half, and there never is one: FlightDeck
        // does not open pull requests, from any surface (SPECS §5).
        assert!(
            !creates_pull_request(cmd),
            "'{}' would dispatch {cmd:?}, which creates a PR (SPECS §5)",
            spec.name
        );
    }

    // Exactly one row uses the exception, and it is the one §5.1 names. A second
    // arrival here is not a test to update — it is a boundary decision that has
    // to be argued in the spec first.
    assert_eq!(
        rewriting,
        vec![names::REBASE_WORKTREE],
        "SPECS §5.1 sanctions one history-rewriting command from a user surface"
    );

    // The other direction, so the surface is honest both ways: the row §5.2
    // keeps off a dispatching route is still *offered*, carrying the sentence
    // saying why it will be refused rather than being hidden from the palette.
    let pull_base = lookup(names::PULL_BASE).expect("the row is offered, not hidden");
    assert_eq!(pull_base.route, Route::NotSupported(PULL_BASE_REFUSAL));
    assert_eq!(
        confirmation_of(&Command::PullBase),
        Confirmation::None,
        "if pull-base ever grows a confirmation step, revisit the decision \
         rather than the test"
    );
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
        assert_ne!(
            confirmation_of(cmd),
            Confirmation::Given,
            "'{}' carries a pre-confirmed {cmd:?}",
            spec.name
        );
    }
}

/// **The smuggling attempt, made explicit.** `rebase_worktree` is the row the
/// §5.1 exception rests on, and the exception is only worth anything while the
/// confirmation is unforgeable from a frame.
///
/// A `Command` frame is `{ seq, name, args }`. The `args` are the browser's
/// only input, and a forwarding row does not read them: `run_web_command` takes
/// the [`PaletteAction`] out of *this table*. So the payload that reaches
/// `AppState::dispatch` is the table's, whatever the frame said — which is why
/// there is no arm below that could be handed `confirm: true`.
#[test]
fn a_frame_cannot_smuggle_a_confirmed_rebase() {
    let spec = lookup(names::REBASE_WORKTREE).expect("the row is offered");

    // 1. The table carries the unconfirmed value, and nothing else.
    assert_eq!(
        spec.route,
        Route::Palette(PaletteAction::Dispatch(Command::RebaseWorktree {
            confirm: false
        })),
        "the row must carry the value that can only ask (SPECS §5.1)"
    );

    // 2. The row a browser receives carries no `args` of its own to echo back,
    //    so even a frame built from the host's own row starts from nothing.
    assert!(spec.view().run.args.is_none());

    // 3. And the payload is not reachable from the frame: `dispatched_command`
    //    is the whole of what a forwarding row hands onward, and it is the
    //    table's value. A frame carrying `{"confirm": true}` changes nothing
    //    here, because nothing here reads a frame.
    let dispatched = dispatched_command(&spec.route).expect("the row dispatches");
    assert_eq!(confirmation_of(dispatched), Confirmation::Pending);
    assert_eq!(dispatched, &Command::RebaseWorktree { confirm: false });
}

/// The git family is reachable, and each row carries the value its spec section
/// requires. Pinned as a set so a later task cannot quietly hand one of them a
/// confirmed payload — or drop a row back to a refusal — without saying so here.
#[test]
fn the_git_rows_dispatch_the_values_their_spec_sections_require() {
    let expected = [
        (
            names::REBASE_WORKTREE,
            Command::RebaseWorktree { confirm: false },
        ),
        (names::PUSH_BRANCH, Command::PushBranch { confirm: None }),
        (
            names::FINISH_LOCAL_MERGE,
            Command::FinishLocalMerge { confirm: false },
        ),
    ];
    for (name, cmd) in &expected {
        let spec = lookup(name).expect("the row is offered");
        assert_eq!(
            dispatched_command(&spec.route),
            Some(cmd),
            "'{name}' must forward the unconfirmed value"
        );
        assert!(
            spec.refusal().is_none(),
            "'{name}' runs now — it must not also claim it will be refused"
        );
    }

    // The two rows the git family still does not run, each for its own stated
    // reason: §5.2's boundary decision, and an overlay with no browser design
    // (`remote-control-ll5.8`). Neither is a dialog, so neither is ll5.4's.
    assert_eq!(
        lookup(names::PULL_BASE).map(|s| &s.route),
        Some(&Route::NotSupported(PULL_BASE_REFUSAL))
    );
    assert_eq!(
        lookup(names::SHOW_GIT_STATUS).map(|s| &s.route),
        Some(&Route::NotSupported(UNDESIGNED_OVERLAY_REFUSAL))
    );
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
