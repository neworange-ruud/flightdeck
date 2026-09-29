//! A waiting tile's inline answers (remote-control-bmej.5.6), against a real
//! host tracking prompts: the prompt comes from the host's own detection (the
//! binary fallback, or an OpenCode sidecar on disk), not from a stub.

use super::*;
use flightdeck::view::{PromptAnswer, PromptReply, SessionKey};

/// One project whose `wait` tab is waiting for input and whose `work` tab is
/// working, tracked for prompts and pumped once so the wait's prompt is
/// detected. Tab ids are made unique (`alpha-0`, …): prompt tracking keys
/// sessions by tab id, as the relay does, and real tab ids never repeat.
/// `sidecar` is an OpenCode `agent-prompt.json` for the waiting tab.
fn waiting_host(
    f: &'static Fakes,
    sidecar: Option<&str>,
) -> (flightdeck::host::AppHost<'static>, tempfile::TempDir) {
    use InterpretedStatus::*;
    let mut alpha = project(
        f,
        "alpha",
        &[
            ("wait", Some(WaitingForInput), 10),
            ("work", Some(Working), 20),
        ],
    );
    for (i, tab) in alpha.state.tabs.iter_mut().enumerate() {
        tab.meta.id = format!("alpha-{i}");
    }
    // The tabs run `opencode`: a backend whose answer keys FlightDeck knows
    // (an unregistered agent is a custom one, answered in its own TUI).
    alpha.state.registry.agents.insert(
        "opencode".to_string(),
        flightdeck::contracts::AgentDef {
            key: "opencode".to_string(),
            display_name: "OpenCode".to_string(),
            command: "opencode".to_string(),
            ..Default::default()
        },
    );
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join(".flightdeck");
    std::fs::create_dir_all(&dir).unwrap();
    if let Some(body) = sidecar {
        std::fs::write(dir.join("agent-prompt.json"), body).unwrap();
    }
    // Where the status hook writes the sidecar: the status file's root.
    alpha.state.tabs[0].status_file = Some(dir.join("agent-status"));
    let mut host = testing::host(env(f), &f.notifier, vec![alpha], 0);
    host.track_prompts();
    host.pump();
    (host, root)
}

#[gpui::test]
fn a_waiting_tile_offers_approve_and_deny_and_a_click_answers(app: &mut TestAppContext) {
    let f = fakes();
    let (host, _root) = waiting_host(f, None);
    let (model, cx) = open_host(app, host);
    cx.simulate_keystrokes("alt-m");
    let prompt_id = model.read_with(cx, |m, _| {
        m.host()
            .pending_prompt(0, "alpha-0")
            .expect("the wait's prompt is detected")
            .prompt_id
    });
    assert!(cx.debug_bounds("mission-tile-0-approve").is_some());
    assert!(cx.debug_bounds("mission-tile-0-deny").is_some());
    assert!(
        cx.debug_bounds("mission-tile-0-open-to-answer").is_none(),
        "answerable inline"
    );
    assert!(
        cx.debug_bounds("mission-tile-1-approve").is_none(),
        "a working tile has no answers"
    );
    take_dispatched(&model, cx);

    click(cx, "mission-tile-0-approve");
    assert_eq!(
        take_dispatched(&model, cx),
        [HostEvent::AnswerPrompt(PromptReply {
            key: SessionKey {
                project: 0,
                tab_id: "alpha-0".to_string(),
            },
            prompt_id: prompt_id.clone(),
            answer: PromptAnswer::Approve,
        })],
        "only the answer: the click does not also select the tile"
    );

    assert!(
        cx.debug_bounds("mission-tile-0-approve").is_none(),
        "answered: the row goes, though the agent has not reported back yet"
    );
}

#[gpui::test]
fn deny_answers_the_same_prompt(app: &mut TestAppContext) {
    let f = fakes();
    let (host, _root) = waiting_host(f, None);
    let (model, cx) = open_host(app, host);
    cx.simulate_keystrokes("alt-m");
    let prompt_id = model.read_with(cx, |m, _| {
        m.host().pending_prompt(0, "alpha-0").unwrap().prompt_id
    });
    take_dispatched(&model, cx);
    click(cx, "mission-tile-0-deny");
    assert_eq!(
        take_dispatched(&model, cx),
        [HostEvent::AnswerPrompt(PromptReply {
            key: SessionKey {
                project: 0,
                tab_id: "alpha-0".to_string(),
            },
            prompt_id,
            answer: PromptAnswer::Deny,
        })]
    );
}

#[gpui::test]
fn a_single_select_question_offers_its_options(app: &mut TestAppContext) {
    let f = fakes();
    let (host, _root) = waiting_host(
        f,
        Some(
            r#"{"kind":"question","text":"Which framework?","options":[{"label":"React"},{"label":"Vue"}]}"#,
        ),
    );
    let (model, cx) = open_host(app, host);
    cx.simulate_keystrokes("alt-m");
    assert!(cx.debug_bounds("mission-tile-0-option-0").is_some());
    assert!(cx.debug_bounds("mission-tile-0-option-1").is_some());
    assert!(cx.debug_bounds("mission-tile-0-approve").is_none());
    take_dispatched(&model, cx);
    click(cx, "mission-tile-0-option-1");
    assert!(matches!(
        take_dispatched(&model, cx).as_slice(),
        [HostEvent::AnswerPrompt(PromptReply {
            answer: PromptAnswer::Option(1),
            ..
        })]
    ));
}

#[gpui::test]
fn an_unsupported_prompt_offers_to_open_the_tile(app: &mut TestAppContext) {
    let f = fakes();
    // A checklist: toggles, not one keystroke.
    let (host, _root) = waiting_host(
        f,
        Some(
            r#"{"kind":"question","text":"Which checks?","multiple":true,"options":[{"label":"Tests"},{"label":"Clippy"}]}"#,
        ),
    );
    let (model, cx) = open_host(app, host);
    cx.simulate_keystrokes("alt-m");
    assert!(cx.debug_bounds("mission-tile-0-approve").is_none());
    assert!(cx.debug_bounds("mission-tile-0-option-0").is_none());
    take_dispatched(&model, cx);

    click(cx, "mission-tile-0-open-to-answer");
    let events = take_dispatched(&model, cx);
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, HostEvent::AnswerPrompt(_))),
        "nothing is answered from the tile"
    );
    assert_eq!(selected(&model, cx), sel("alpha", "wait"));
    assert_eq!(
        mode_of(&model, cx),
        InputMode::Terminal,
        "opened full size, as Enter does"
    );
}
