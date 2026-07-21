use super::*;
use crate::app::state::AppState;
use crate::contracts::{ProjectState as CoreProjectState, PtySize, TabState, STATE_VERSION};
use crate::testing::FakePty;
use crate::tui::render::GitStatusCache;

// --- fixtures ----------------------------------------------------------------

fn entry(id: &str) -> SessionEntry {
    SessionEntry {
        id: SessionId::new(id),
        project: 0,
        tab: 0,
        name: id.to_string(),
        backend: Some(StatusBackend::Claude),
        status: AgentStatus::Idle,
        primary_running: true,
        all_stopped: false,
        bracketed_paste: true,
        pending_prompt: None,
    }
}

fn index_with(sessions: Vec<SessionEntry>) -> SessionIndex {
    SessionIndex {
        sessions,
        projects: vec![ProjectEntry {
            id: ProjectId::new("proj"),
            index: 0,
            base_branch: "main".to_string(),
        }],
    }
}

fn cid(s: &str) -> CommandId {
    CommandId::new(s)
}

// --- ledger idempotency --------------------------------------------------------

#[test]
fn ledger_reemits_duplicate_with_original_outcome() {
    let mut ledger = CommandLedger::new();
    assert!(ledger.duplicate_ack(&cid("c1")).is_none());

    ledger.record(
        cid("c1"),
        CommandOutcome::Applied,
        Some("Restarted primary agent.".to_string()),
    );
    let ack = ledger.duplicate_ack(&cid("c1")).expect("duplicate ack");
    assert_eq!(ack.command_id, cid("c1"));
    assert_eq!(ack.outcome, CommandOutcome::Duplicate);
    let msg = ack.message.unwrap();
    assert!(
        msg.contains("applied"),
        "message should carry the original outcome: {msg}"
    );
    assert!(msg.contains("Restarted primary agent."));

    // Unseen ids still process as new.
    assert!(ledger.duplicate_ack(&cid("c2")).is_none());
}

#[test]
fn ledger_evicts_oldest_beyond_capacity() {
    let mut ledger = CommandLedger::new();
    for i in 0..(LEDGER_CAPACITY + 10) {
        ledger.record(cid(&format!("c{i}")), CommandOutcome::Applied, None);
    }
    // The first ten were evicted; the newest are still remembered.
    assert!(ledger.duplicate_ack(&cid("c0")).is_none());
    assert!(ledger.duplicate_ack(&cid("c9")).is_none());
    assert!(ledger.duplicate_ack(&cid("c10")).is_some());
    assert!(ledger
        .duplicate_ack(&cid(&format!("c{}", LEDGER_CAPACITY + 9)))
        .is_some());
}

// --- reply -------------------------------------------------------------------

#[test]
fn reply_translates_to_bracketed_paste_plus_enter() {
    let index = index_with(vec![entry("t1")]);
    let t = translate(
        &CommandBody::Reply {
            session_id: SessionId::new("t1"),
            text: "fix the tests\nplease".to_string(),
        },
        &index,
    );
    // Byte-exact: bracketed-paste guards, newlines → CR, then the Enter CR.
    assert_eq!(
        t,
        Translation::PtyInput {
            project: 0,
            tab: 0,
            bytes: b"\x1b[200~fix the tests\rplease\x1b[201~\r".to_vec(),
        }
    );
}

#[test]
fn reply_without_bracketed_paste_sends_raw_text_plus_enter() {
    let mut e = entry("t1");
    e.bracketed_paste = false;
    let index = index_with(vec![e]);
    let t = translate(
        &CommandBody::Reply {
            session_id: SessionId::new("t1"),
            text: "run it\n".to_string(),
        },
        &index,
    );
    assert_eq!(
        t,
        Translation::PtyInput {
            project: 0,
            tab: 0,
            bytes: b"run it\r\r".to_vec(),
        }
    );
}

#[test]
fn reply_rejections() {
    let index = index_with(vec![entry("t1")]);
    // Unknown session.
    assert!(matches!(
        translate(
            &CommandBody::Reply {
                session_id: SessionId::new("nope"),
                text: "hi".to_string(),
            },
            &index,
        ),
        Translation::Reject { reason } if reason.contains("unknown session")
    ));
    // Empty text.
    assert!(matches!(
        translate(
            &CommandBody::Reply {
                session_id: SessionId::new("t1"),
                text: "  \n".to_string(),
            },
            &index,
        ),
        Translation::Reject { .. }
    ));
    // Agent not running.
    let mut stopped = entry("t2");
    stopped.primary_running = false;
    let index = index_with(vec![stopped]);
    assert!(matches!(
        translate(
            &CommandBody::Reply {
                session_id: SessionId::new("t2"),
                text: "hi".to_string(),
            },
            &index,
        ),
        Translation::Reject { reason } if reason.contains("not running")
    ));
}

// --- permission decisions ------------------------------------------------------

#[test]
fn permission_keystroke_map_per_backend() {
    use PermissionChoice::{AllowOnce, Deny};
    use StatusBackend::{Claude, Codex, OpenCode};
    // Claude Code: numbered options — "1" = allow once; Esc rejects. Never
    // Enter (it would take the focused option, which may not be allow-once).
    assert_eq!(permission_keystroke(Claude, AllowOnce), b"1");
    assert_eq!(permission_keystroke(Claude, Deny), b"\x1b");
    // Codex: "y" approves once; Esc declines.
    assert_eq!(permission_keystroke(Codex, AllowOnce), b"y");
    assert_eq!(permission_keystroke(Codex, Deny), b"\x1b");
    // OpenCode: Enter is the fixed accept-once binding; Esc rejects.
    assert_eq!(permission_keystroke(OpenCode, AllowOnce), b"\r");
    assert_eq!(permission_keystroke(OpenCode, Deny), b"\x1b");
    // No mapping may ever be empty (a decision must move the prompt).
    for backend in [Claude, Codex, OpenCode] {
        for choice in [AllowOnce, Deny] {
            assert!(!permission_keystroke(backend, choice).is_empty());
        }
    }
}

fn decision(prompt: &str, choice: PermissionChoice) -> CommandBody {
    CommandBody::PermissionDecision {
        session_id: SessionId::new("t1"),
        prompt_id: PromptId::new(prompt),
        choice: Some(choice),
        option_index: None,
        option_indices: None,
        free_text: None,
    }
}

fn decision_option(prompt: &str, option_index: u32) -> CommandBody {
    CommandBody::PermissionDecision {
        session_id: SessionId::new("t1"),
        prompt_id: PromptId::new(prompt),
        choice: None,
        option_index: Some(option_index),
        option_indices: None,
        free_text: None,
    }
}

fn decision_options(prompt: &str, option_indices: Vec<u32>) -> CommandBody {
    CommandBody::PermissionDecision {
        session_id: SessionId::new("t1"),
        prompt_id: PromptId::new(prompt),
        choice: None,
        option_index: None,
        option_indices: Some(option_indices),
        free_text: None,
    }
}

fn decision_free_text(prompt: &str, text: &str) -> CommandBody {
    CommandBody::PermissionDecision {
        session_id: SessionId::new("t1"),
        prompt_id: PromptId::new(prompt),
        choice: None,
        option_index: None,
        option_indices: None,
        free_text: Some(text.to_string()),
    }
}

/// A NeedsInput session with a pending prompt on the given backend.
fn answerable(prompt: &str, backend: StatusBackend) -> SessionIndex {
    let mut e = entry("t1");
    e.status = AgentStatus::NeedsInput;
    e.pending_prompt = Some(PromptId::new(prompt));
    e.backend = Some(backend);
    index_with(vec![e])
}

#[test]
fn permission_decision_injects_backend_keystroke() {
    let mut e = entry("t1");
    e.status = AgentStatus::NeedsInput;
    e.pending_prompt = Some(PromptId::new("t1:p3"));
    e.backend = Some(StatusBackend::Codex);
    let index = index_with(vec![e]);
    assert_eq!(
        translate(&decision("t1:p3", PermissionChoice::AllowOnce), &index),
        Translation::PtyInput {
            project: 0,
            tab: 0,
            bytes: b"y".to_vec(),
        }
    );
    assert_eq!(
        translate(&decision("t1:p3", PermissionChoice::Deny), &index),
        Translation::PtyInput {
            project: 0,
            tab: 0,
            bytes: b"\x1b".to_vec(),
        }
    );
}

#[test]
fn permission_decision_rejections() {
    // Not currently needs-input.
    let mut e = entry("t1");
    e.pending_prompt = Some(PromptId::new("t1:p1"));
    let index = index_with(vec![e]);
    assert!(matches!(
        translate(&decision("t1:p1", PermissionChoice::AllowOnce), &index),
        Translation::Reject { reason } if reason.contains("no pending")
    ));

    // Stale / superseded prompt id.
    let mut e = entry("t1");
    e.status = AgentStatus::NeedsInput;
    e.pending_prompt = Some(PromptId::new("t1:p2"));
    let index = index_with(vec![e]);
    assert!(matches!(
        translate(&decision("t1:p1", PermissionChoice::AllowOnce), &index),
        Translation::Reject { reason } if reason.contains("no longer pending")
    ));

    // Unknown/custom backend: refuse honestly rather than guess keystrokes.
    let mut e = entry("t1");
    e.status = AgentStatus::NeedsInput;
    e.pending_prompt = Some(PromptId::new("t1:p1"));
    e.backend = None;
    let index = index_with(vec![e]);
    assert!(matches!(
        translate(&decision("t1:p1", PermissionChoice::AllowOnce), &index),
        Translation::Reject { reason } if reason.contains("custom agent")
    ));

    // Unknown session.
    let index = index_with(vec![]);
    assert!(matches!(
        translate(&decision("t1:p1", PermissionChoice::Deny), &index),
        Translation::Reject { reason } if reason.contains("unknown session")
    ));
}

#[test]
fn option_keystroke_uses_the_option_number_for_claude() {
    use StatusBackend::Claude;
    // Claude's AskUserQuestion numbers its options 1..N; pressing the digit
    // selects AND submits that option (verified live). index 0 => "1", etc.
    assert_eq!(option_keystroke(Claude, 0), Some(b"1".to_vec()));
    assert_eq!(option_keystroke(Claude, 1), Some(b"2".to_vec()));
    assert_eq!(option_keystroke(Claude, 2), Some(b"3".to_vec()));
    assert_eq!(option_keystroke(Claude, 8), Some(b"9".to_vec()));
    // Beyond single digits, fall back to CSI arrow navigation.
    assert_eq!(
        option_keystroke(Claude, 9),
        Some([b"\x1b[B".repeat(9), b"\r".to_vec()].concat())
    );
}

#[test]
fn option_keystroke_arrow_nav_for_opencode() {
    use StatusBackend::{Codex, OpenCode};
    // OpenCode uses arrow-nav: index 0 => just Enter; each step adds a DOWN
    // arrow. DOWN is always the CSI form `ESC [ B` (matching the desktop's own
    // key encoding — the desktop is not DECCKM-aware, remote-control-dc9).
    assert_eq!(option_keystroke(OpenCode, 0), Some(b"\r".to_vec()));
    assert_eq!(option_keystroke(OpenCode, 1), Some(b"\x1b[B\r".to_vec()));
    assert_eq!(
        option_keystroke(OpenCode, 2),
        Some(b"\x1b[B\x1b[B\r".to_vec())
    );
    // Codex has no multi-option prompt.
    assert_eq!(option_keystroke(Codex, 0), None);
    assert_eq!(option_keystroke(Codex, 2), None);
}

#[test]
fn permission_decision_option_index_claude_sends_the_option_number() {
    let index = answerable("t1:p3", StatusBackend::Claude);
    for (opt, digit) in [(0u32, b"1"), (1, b"2"), (2, b"3")] {
        assert_eq!(
            translate(&decision_option("t1:p3", opt), &index),
            Translation::PtyInput {
                project: 0,
                tab: 0,
                bytes: digit.to_vec(),
            }
        );
    }
}

#[test]
fn permission_decision_option_index_arrow_nav_for_opencode() {
    let index = answerable("t1:p3", StatusBackend::OpenCode);
    assert_eq!(
        translate(&decision_option("t1:p3", 0), &index),
        Translation::PtyInput {
            project: 0,
            tab: 0,
            bytes: b"\r".to_vec(),
        }
    );
    assert_eq!(
        translate(&decision_option("t1:p3", 2), &index),
        Translation::PtyInput {
            project: 0,
            tab: 0,
            bytes: b"\x1b[B\x1b[B\r".to_vec(),
        }
    );
}

#[test]
fn permission_decision_option_index_rejected_for_codex() {
    let index = answerable("t1:p3", StatusBackend::Codex);
    assert!(matches!(
        translate(&decision_option("t1:p3", 1), &index),
        Translation::Reject { reason } if reason.contains("multi-option")
    ));
}

#[test]
fn multi_option_keystroke_toggles_and_confirms_for_claude() {
    use StatusBackend::Claude;
    // Claude multi-select is a tabbed form: from the first option, walk DOWN to
    // each target and press Enter to TOGGLE it, then Tab to the Confirm tab and
    // Enter to submit. [0, 2] → Enter (toggle 0), DOWN DOWN, Enter (toggle 2),
    // Tab, Enter. DOWN is always the CSI form `ESC [ B`.
    assert_eq!(
        multi_option_keystroke(Claude, &[0, 2]),
        Some(b"\r\x1b[B\x1b[B\r\t\r".to_vec())
    );
    // Deduped + sorted first.
    assert_eq!(
        multi_option_keystroke(Claude, &[2, 0, 2]),
        Some(b"\r\x1b[B\x1b[B\r\t\r".to_vec()),
        "indices are deduped and sorted before navigation"
    );
    // A single selection still toggles then confirms (DOWN once to option 1).
    assert_eq!(
        multi_option_keystroke(Claude, &[1]),
        Some(b"\x1b[B\r\t\r".to_vec())
    );
    // Empty selection is not actionable.
    assert_eq!(multi_option_keystroke(Claude, &[]), None);
}

#[test]
fn multi_option_keystroke_toggles_and_confirms_for_opencode() {
    use StatusBackend::{Codex, OpenCode};
    // OpenCode renders the SAME form, so the sequence is identical to Claude's.
    assert_eq!(
        multi_option_keystroke(OpenCode, &[0, 2]),
        Some(b"\r\x1b[B\x1b[B\r\t\r".to_vec())
    );
    assert_eq!(
        multi_option_keystroke(OpenCode, &[1]),
        Some(b"\x1b[B\r\t\r".to_vec())
    );
    // Codex has no multi-option prompt.
    assert_eq!(multi_option_keystroke(Codex, &[0, 1]), None);
}

#[test]
fn permission_decision_option_indices_claude_defers_the_submit_enter() {
    let index = answerable("t1:p3", StatusBackend::Claude);
    // Claude: everything up to and including the Tab to the Confirm tab is
    // written immediately; the trailing submit Enter is deferred so it lands
    // after the tab renders (remote-control-dc9).
    assert_eq!(
        translate(&decision_options("t1:p3", vec![0, 2]), &index),
        Translation::PtyInputThenDeferred {
            project: 0,
            tab: 0,
            session_id: SessionId::new("t1"),
            immediate: b"\r\x1b[B\x1b[B\r\t".to_vec(),
            deferred: b"\r".to_vec(),
            delay_ms: crate::remote::commands::MULTI_SELECT_SUBMIT_DELAY_MS,
        }
    );
}

#[test]
fn permission_decision_option_indices_toggles_and_confirms_for_opencode() {
    let index = answerable("t1:p3", StatusBackend::OpenCode);
    assert_eq!(
        translate(&decision_options("t1:p3", vec![0, 2]), &index),
        Translation::PtyInput {
            project: 0,
            tab: 0,
            bytes: b"\r\x1b[B\x1b[B\r\t\r".to_vec(),
        }
    );
}

#[test]
fn permission_decision_option_indices_rejected_for_codex() {
    let index = answerable("t1:p3", StatusBackend::Codex);
    assert!(matches!(
        translate(&decision_options("t1:p3", vec![0, 1]), &index),
        Translation::Reject { reason } if reason.contains("multi-select")
    ));
}

#[test]
fn permission_decision_empty_option_indices_falls_through() {
    // An empty indices vec is not a valid multi-select answer; with no other
    // field set it is rejected as an empty decision (not silently accepted).
    let index = answerable("t1:p3", StatusBackend::Claude);
    assert!(matches!(
        translate(&decision_options("t1:p3", vec![]), &index),
        Translation::Reject { reason } if reason.contains("empty decision")
    ));
}

#[test]
fn permission_decision_free_text_encodes_reply() {
    // Free text is delivered like any reply: the text as a paste (bracketed
    // when the agent enabled DECSET 2004) followed by the Enter CR.
    let index = answerable("t1:p3", StatusBackend::Claude);
    assert_eq!(
        translate(&decision_free_text("t1:p3", "my own answer"), &index),
        Translation::PtyInput {
            project: 0,
            tab: 0,
            bytes: b"\x1b[200~my own answer\x1b[201~\r".to_vec(),
        }
    );
}

#[test]
fn permission_decision_free_text_wins_over_option_index() {
    // Precedence: a non-empty free_text beats a set option_index.
    let index = answerable("t1:p3", StatusBackend::Claude);
    let body = CommandBody::PermissionDecision {
        session_id: SessionId::new("t1"),
        prompt_id: PromptId::new("t1:p3"),
        choice: None,
        option_index: Some(2),
        option_indices: None,
        free_text: Some("typed instead".to_string()),
    };
    assert_eq!(
        translate(&body, &index),
        Translation::PtyInput {
            project: 0,
            tab: 0,
            bytes: b"\x1b[200~typed instead\x1b[201~\r".to_vec(),
        }
    );
}

#[test]
fn permission_decision_empty_decision_rejected() {
    let index = answerable("t1:p3", StatusBackend::Claude);
    // All fields None.
    let empty = CommandBody::PermissionDecision {
        session_id: SessionId::new("t1"),
        prompt_id: PromptId::new("t1:p3"),
        choice: None,
        option_index: None,
        option_indices: None,
        free_text: None,
    };
    assert!(matches!(
        translate(&empty, &index),
        Translation::Reject { reason } if reason.contains("empty decision")
    ));
    // Empty free_text is treated as no decision too.
    assert!(matches!(
        translate(&decision_free_text("t1:p3", ""), &index),
        Translation::Reject { reason } if reason.contains("empty decision")
    ));
}

// --- lifecycle -----------------------------------------------------------------

#[test]
fn restart_translates_to_restart_agent_command() {
    let mut e = entry("t1");
    e.project = 0;
    e.tab = 2;
    let index = index_with(vec![e]);
    assert_eq!(
        translate(
            &CommandBody::RestartAgent {
                session_id: SessionId::new("t1"),
            },
            &index,
        ),
        Translation::Dispatch {
            project: 0,
            tab: 2,
            command: Command::RestartAgent,
        }
    );
}

#[test]
fn close_running_session_sends_ctrl_c_primary() {
    let index = index_with(vec![entry("t1")]);
    assert_eq!(
        translate(
            &CommandBody::CloseSession {
                session_id: SessionId::new("t1"),
            },
            &index,
        ),
        Translation::Dispatch {
            project: 0,
            tab: 0,
            command: Command::CloseAgentTab {
                action: Some(CloseAction::CtrlCPrimary),
            },
        }
    );
}

#[test]
fn close_stopped_session_closes_if_all_stopped() {
    let mut e = entry("t1");
    e.primary_running = false;
    e.all_stopped = true;
    let index = index_with(vec![e]);
    assert_eq!(
        translate(
            &CommandBody::CloseSession {
                session_id: SessionId::new("t1"),
            },
            &index,
        ),
        Translation::Dispatch {
            project: 0,
            tab: 0,
            command: Command::CloseAgentTab {
                action: Some(CloseAction::IfAllStopped),
            },
        }
    );
}

#[test]
fn lifecycle_unknown_session_rejected() {
    let index = index_with(vec![]);
    for body in [
        CommandBody::RestartAgent {
            session_id: SessionId::new("ghost"),
        },
        CommandBody::CloseSession {
            session_id: SessionId::new("ghost"),
        },
        CommandBody::ClearManualStatus {
            session_id: SessionId::new("ghost"),
        },
    ] {
        assert!(matches!(
            translate(&body, &index),
            Translation::Reject { reason } if reason.contains("unknown session")
        ));
    }
}

// --- new agent -------------------------------------------------------------------

#[test]
fn new_agent_routes_to_main_loop_with_registry_key() {
    let index = index_with(vec![]);
    let t = translate(
        &CommandBody::NewAgent {
            project_id: ProjectId::new("proj"),
            agent_type: AgentType::ClaudeCode,
            name: "fix-login".to_string(),
            base_branch: "main".to_string(),
            first_task: "make the login test pass".to_string(),
        },
        &index,
    );
    assert_eq!(
        t,
        Translation::NeedsMainLoop(MainLoopAction::NewAgent {
            project: 0,
            name: "fix-login".to_string(),
            agent_key: "claude".to_string(),
            first_task: "make the login test pass".to_string(),
        })
    );

    // The other two built-ins map to their registry keys.
    for (ty, key) in [
        (AgentType::Codex, "codex"),
        (AgentType::Opencode, "opencode"),
    ] {
        let t = translate(
            &CommandBody::NewAgent {
                project_id: ProjectId::new("proj"),
                agent_type: ty,
                name: "n".to_string(),
                base_branch: "main".to_string(),
                first_task: String::new(),
            },
            &index,
        );
        match t {
            Translation::NeedsMainLoop(MainLoopAction::NewAgent { agent_key, .. }) => {
                assert_eq!(agent_key, key)
            }
            other => panic!("expected NeedsMainLoop, got {other:?}"),
        }
    }
}

#[test]
fn new_agent_rejects_unknown_project_and_base_mismatch() {
    let index = index_with(vec![]);
    assert!(matches!(
        translate(
            &CommandBody::NewAgent {
                project_id: ProjectId::new("ghost"),
                agent_type: AgentType::Codex,
                name: "n".to_string(),
                base_branch: "main".to_string(),
                first_task: String::new(),
            },
            &index,
        ),
        Translation::Reject { reason } if reason.contains("unknown project")
    ));
    assert!(matches!(
        translate(
            &CommandBody::NewAgent {
                project_id: ProjectId::new("proj"),
                agent_type: AgentType::Codex,
                name: "n".to_string(),
                base_branch: "develop".to_string(),
                first_task: String::new(),
            },
            &index,
        ),
        Translation::Reject { reason } if reason.contains("base branch must be 'main'")
    ));
}

// --- manual status ---------------------------------------------------------------

#[test]
fn manual_status_set_and_clear_translate_to_dispatch() {
    let index = index_with(vec![entry("t1")]);
    assert_eq!(
        translate(
            &CommandBody::SetManualStatus {
                session_id: SessionId::new("t1"),
                label: "blocked".to_string(),
            },
            &index,
        ),
        Translation::Dispatch {
            project: 0,
            tab: 0,
            command: Command::SetManualStatus(Some(ManualStatus::Blocked)),
        }
    );
    assert_eq!(
        translate(
            &CommandBody::ClearManualStatus {
                session_id: SessionId::new("t1"),
            },
            &index,
        ),
        Translation::Dispatch {
            project: 0,
            tab: 0,
            command: Command::SetManualStatus(None),
        }
    );
    // An unknown label is rejected with the valid options listed.
    assert!(matches!(
        translate(
            &CommandBody::SetManualStatus {
                session_id: SessionId::new("t1"),
                label: "on fire".to_string(),
            },
            &index,
        ),
        Translation::Reject { reason } if reason.contains("in progress")
    ));
}

// --- git actions -----------------------------------------------------------------

#[test]
fn git_actions_dispatch_the_guarded_commands() {
    let mut e = entry("fix-login");
    e.tab = 3;
    let index = index_with(vec![e]);
    // Pull base → the global PullBase command (routed through the session).
    assert_eq!(
        translate(
            &CommandBody::GitPullBase {
                session_id: SessionId::new("fix-login"),
            },
            &index,
        ),
        Translation::Dispatch {
            project: 0,
            tab: 3,
            command: Command::PullBase,
        }
    );
    // Merge back → the *confirmed* FinishLocalMerge (phone confirmed, PRD §8).
    assert_eq!(
        translate(
            &CommandBody::GitMergeBack {
                session_id: SessionId::new("fix-login"),
            },
            &index,
        ),
        Translation::Dispatch {
            project: 0,
            tab: 3,
            command: Command::FinishLocalMerge { confirm: true },
        }
    );
    // Abandon (matching confirm name) → the confirmed AbandonWorktree.
    assert_eq!(
        translate(
            &CommandBody::GitAbandonWorktree {
                session_id: SessionId::new("fix-login"),
                confirm_name: "fix-login".to_string(),
            },
            &index,
        ),
        Translation::Dispatch {
            project: 0,
            tab: 3,
            command: Command::AbandonWorktree { confirm: true },
        }
    );
}

#[test]
fn git_abandon_rejects_on_confirm_name_mismatch() {
    let index = index_with(vec![entry("fix-login")]);
    assert!(matches!(
        translate(
            &CommandBody::GitAbandonWorktree {
                session_id: SessionId::new("fix-login"),
                confirm_name: "wrong-name".to_string(),
            },
            &index,
        ),
        Translation::Reject { reason }
            if reason.contains("does not match") && reason.contains("fix-login")
    ));
}

#[test]
fn git_actions_reject_unknown_session() {
    let index = index_with(vec![]);
    for body in [
        CommandBody::GitPullBase {
            session_id: SessionId::new("ghost"),
        },
        CommandBody::GitMergeBack {
            session_id: SessionId::new("ghost"),
        },
        CommandBody::GitAbandonWorktree {
            session_id: SessionId::new("ghost"),
            confirm_name: "ghost".to_string(),
        },
    ] {
        assert!(matches!(
            translate(&body, &index),
            Translation::Reject { reason } if reason.contains("unknown session")
        ));
    }
}

// --- shell -----------------------------------------------------------------------

#[test]
fn shell_commands_resolve_to_shell_translations() {
    use flightdeck_remote_protocol::ShellId;
    let mut e = entry("t1");
    e.tab = 2;
    let index = index_with(vec![e]);
    assert_eq!(
        translate(
            &CommandBody::ShellOpen {
                session_id: SessionId::new("t1"),
                shell_id: ShellId::new("s1"),
                cols: 100,
                rows: 40,
            },
            &index,
        ),
        Translation::Shell {
            project: 0,
            tab: 2,
            session_id: SessionId::new("t1"),
            action: ShellAction::Open {
                shell_id: ShellId::new("s1"),
                cols: 100,
                rows: 40,
            },
        }
    );
    // Input carries the raw bytes of the UTF-8 string verbatim.
    assert_eq!(
        translate(
            &CommandBody::ShellInput {
                session_id: SessionId::new("t1"),
                shell_id: ShellId::new("s1"),
                data: "ls -la\n".to_string(),
            },
            &index,
        ),
        Translation::Shell {
            project: 0,
            tab: 2,
            session_id: SessionId::new("t1"),
            action: ShellAction::Input {
                shell_id: ShellId::new("s1"),
                bytes: b"ls -la\n".to_vec(),
            },
        }
    );
    assert_eq!(
        translate(
            &CommandBody::ShellInterrupt {
                session_id: SessionId::new("t1"),
                shell_id: ShellId::new("s1"),
            },
            &index,
        ),
        Translation::Shell {
            project: 0,
            tab: 2,
            session_id: SessionId::new("t1"),
            action: ShellAction::Interrupt {
                shell_id: ShellId::new("s1"),
            },
        }
    );
    assert_eq!(
        translate(
            &CommandBody::ShellClose {
                session_id: SessionId::new("t1"),
                shell_id: ShellId::new("s1"),
            },
            &index,
        ),
        Translation::Shell {
            project: 0,
            tab: 2,
            session_id: SessionId::new("t1"),
            action: ShellAction::Close {
                shell_id: ShellId::new("s1"),
            },
        }
    );
}

#[test]
fn shell_commands_reject_unknown_session() {
    use flightdeck_remote_protocol::ShellId;
    let index = index_with(vec![]);
    assert!(matches!(
        translate(
            &CommandBody::ShellOpen {
                session_id: SessionId::new("ghost"),
                shell_id: ShellId::new("s1"),
                cols: 80,
                rows: 24,
            },
            &index,
        ),
        Translation::Reject { reason } if reason.contains("unknown session")
    ));
}

// --- still-unimplemented seams ----------------------------------------------------

#[test]
fn mark_read_rejects_honestly() {
    let index = index_with(vec![entry("t1")]);
    assert!(matches!(
        translate(
            &CommandBody::MarkRead {
                event_ids: vec![flightdeck_remote_protocol::EventId::new("ev:1")],
            },
            &index,
        ),
        Translation::Reject { reason } if reason.contains("not implemented")
    ));
}

// --- first-task gating -------------------------------------------------------------

#[test]
fn first_task_decision_gating() {
    use FirstTaskDecision::{Expire, Send, Wait};
    // Not running yet: wait.
    assert_eq!(first_task_decision(false, false, 0), Wait);
    // Running, bracketed paste on: send bracketed immediately.
    assert_eq!(first_task_decision(true, true, 0), Send { bracketed: true });
    // Running, no bracketed paste yet: wait inside the window …
    assert_eq!(
        first_task_decision(true, false, FIRST_TASK_BRACKETED_WAIT_MS - 1),
        Wait
    );
    // … then fall back to raw text.
    assert_eq!(
        first_task_decision(true, false, FIRST_TASK_BRACKETED_WAIT_MS),
        Send { bracketed: false }
    );
    // Expired: drop, even if it would otherwise send.
    assert_eq!(
        first_task_decision(true, true, FIRST_TASK_EXPIRY_MS),
        Expire
    );
    assert_eq!(
        first_task_decision(false, false, FIRST_TASK_EXPIRY_MS),
        Expire
    );
}

// --- build_index against a real AppState --------------------------------------------

fn tab_state(id: &str, name: &str, agent: &str) -> TabState {
    TabState {
        id: id.to_string(),
        name: name.to_string(),
        slug: name.to_string(),
        agent: agent.to_string(),
        branch: format!("{name}-branch"),
        worktree_path_relative: format!("worktrees/{name}"),
        base_branch: "main".to_string(),
        base_commit_sha: "abc123".to_string(),
        created_at: "2026-01-01T00:00:00Z".to_string(),
        attached_existing_branch: false,
        recovered: false,
        last_known_status: "unknown".to_string(),
        manual_status: None,
        containerized: false,
        container_image: None,
        runs_on_base: false,
        resume_args: Vec::new(),
    }
}

#[test]
fn build_index_reflects_live_workspace() {
    // Default config so the registry recognises the built-in backends.
    let config = crate::config::schema::default_config("proj", "main");
    let state = CoreProjectState {
        version: STATE_VERSION,
        project_root_relative: ".".to_string(),
        base_branch: "main".to_string(),
        tabs: vec![
            tab_state("t1", "fix-login", "claude"),
            tab_state("t2", "add-tests", "custom-agent"),
        ],
    };
    let mut app = AppState::new(config, state, "/repo", "/repo/.flightdeck/state.json");
    // Spawn a running fake primary for t1 only; t2 stays un-spawned.
    let pty = FakePty::new();
    let _h = pty.queue_session();
    app.tabs[0]
        .session
        .spawn_primary(
            &pty,
            "agent",
            &[],
            std::path::Path::new("/repo"),
            PtySize::default(),
        )
        .unwrap();

    let cache = GitStatusCache::new();
    let views = vec![ProjectView {
        id: ProjectId::new("proj"),
        name: "proj",
        state: &app,
        cache: &cache,
    }];
    let index = build_index(&views, 1_000, &|sid| {
        (sid == "t1").then(|| PromptId::new("t1:p1"))
    });

    assert_eq!(index.projects.len(), 1);
    assert_eq!(index.projects[0].base_branch, "main");
    assert_eq!(index.sessions.len(), 2);

    let s1 = index.session(&SessionId::new("t1")).unwrap();
    assert_eq!((s1.project, s1.tab), (0, 0));
    assert_eq!(s1.backend, Some(StatusBackend::Claude));
    assert!(s1.primary_running);
    assert!(!s1.all_stopped);
    assert!(
        !s1.bracketed_paste,
        "fresh terminal has not enabled DECSET 2004"
    );
    assert_eq!(s1.pending_prompt, Some(PromptId::new("t1:p1")));
    assert_eq!(s1.name, "fix-login");

    let s2 = index.session(&SessionId::new("t2")).unwrap();
    assert_eq!((s2.project, s2.tab), (0, 1));
    assert_eq!(s2.backend, None, "unknown agents must fail closed");
    assert!(!s2.primary_running);
    assert!(s2.all_stopped);
    assert_eq!(s2.pending_prompt, None);
}
