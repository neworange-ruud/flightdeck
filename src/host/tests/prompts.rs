//! Inline prompt answers (remote-control-bmej.5.6): what
//! [`AppHost::pending_prompt`] / [`AppHost::mission_view`] report for a
//! waiting session, and what [`HostEvent::AnswerPrompt`] types.
//!
//! The prompts come from the same fixtures the remote bridge's tests feed its
//! detection — an OpenCode `agent-prompt.json`, a Claude `agent-question.json`
//! sidecar, or no sidecar at all (the binary fallback) — written where the
//! tab's status hook would write them, with the `waiting` status line on the
//! status file the host polls. Nothing is stubbed between the sidecar and the
//! bytes on the fake PTY.

use super::*;
use crate::remote::bridge::passthrough_seal;
use crate::view::{PromptAnswer, PromptReply, PromptShape, PromptView, SessionKey};
use crate::web::arbiter::{InputArbiter, Writer};
use flightdeck_remote_protocol::relay::EncryptedEnvelope;
use flightdeck_remote_protocol::{
    CommandBody, CommandId, PairingId, PermissionChoice, PhoneCommand, PromptId, Role,
};

/// The OpenCode single-select question from the bridge's sidecar tests.
const OPENCODE_QUESTION: &str = r#"{"kind":"question","text":"Which framework?","options":[
    {"label":"React","description":"Use React"},{"label":"Vue"},
    {"label":"Svelte","description":"Use Svelte"}]}"#;
/// The OpenCode checklist question from the bridge's sidecar tests.
const OPENCODE_CHECKLIST: &str = r#"{"kind":"question","text":"Which checks?","multiple":true,
    "options":[{"label":"Tests"},{"label":"Clippy"}]}"#;
/// The OpenCode permission from the bridge's sidecar tests.
const OPENCODE_PERMISSION: &str = r#"{"kind":"permission","text":"Run rm -rf?","options":[
    {"label":"Allow once"},{"label":"Deny"}]}"#;
/// The Claude AskUserQuestion hook payload from the bridge's sidecar tests.
const CLAUDE_QUESTION: &str = r#"{"tool_name":"AskUserQuestion","tool_input":{"questions":[{"question":"Lunch?","header":"L","options":[{"label":"Pizza","description":"cheesy"},{"label":"Sushi"}]}]}}"#;
/// A Claude AskUserQuestion carrying two questions: a tabbed form.
const CLAUDE_TWO_QUESTIONS: &str = r#"{"tool_name":"AskUserQuestion","tool_input":{"questions":[
    {"question":"Lunch?","header":"L","options":[{"label":"Pizza"},{"label":"Sushi"}]},
    {"question":"Drink?","header":"D","options":[{"label":"Tea"},{"label":"Coffee"}]}]}}"#;

/// Where the status hook writes the sidecars: `<status_root>/.flightdeck/`.
#[derive(Clone, Copy)]
enum Sidecar {
    None,
    OpenCode(&'static str),
    Claude(&'static str),
}

/// A one-project host whose one tab runs `agent` (`claude`, `codex`,
/// `opencode`) and is waiting on the prompt `sidecar` describes, surfaced by
/// the host's own pumps. `remote` builds the phone's path instead of the
/// desktop's: a relay-less bridge fed commands as envelopes, and no local
/// tracker.
struct Waiting<'f> {
    host: AppHost<'f>,
    pty: FakePtyHandle,
    tab_id: String,
    /// Holds the sidecars; the tab's status root.
    _root: TempDir,
}

fn agent_def(fakes: &Fakes, agent: &str) -> AgentDef {
    let path = fakes.dir.path().join(agent);
    std::fs::write(&path, "#!/bin/sh\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
    }
    AgentDef {
        key: agent.to_string(),
        display_name: agent.to_string(),
        command: path.to_str().unwrap().to_string(),
        args: vec![],
        status_patterns: StatusPatterns::default(),
    }
}

fn waiting<'f>(fakes: &'f Fakes, agent: &str, sidecar: Sidecar, remote: bool) -> Waiting<'f> {
    let def = agent_def(fakes, agent);
    let mut config = Config::default();
    config.ui.default_agent = def.key.clone();
    config.worktrees.root = ".flightdeck/worktrees".to_string();
    config.agents.insert(def.key.clone(), def);
    let pty = fakes.pty.queue_session();
    let mut state = AppState::new(
        config,
        default_state("main"),
        "/repo",
        "/repo/.flightdeck/state.json",
    );
    state
        .dispatch(
            Command::NewAgentTab {
                name: "Task".to_string(),
                agent_key: None,
            },
            &fakes.services(),
        )
        .expect("the tab is created");
    let root = TempDir::new().unwrap();
    let status = root.path().join(".flightdeck").join("agent-status");
    state.tabs[0].status_file = Some(status.clone());
    let tab_id = state.tabs[0].meta.id.clone();

    let mut host = fakes.host(state);
    if remote {
        host.remote_bridge = Some(RemoteBridge::passthrough(0));
    } else {
        host.track_prompts();
    }

    // The hook writes the sidecar before it flips the status to `waiting`.
    let dir = root.path().join(".flightdeck");
    std::fs::create_dir_all(&dir).unwrap();
    match sidecar {
        Sidecar::None => {}
        Sidecar::OpenCode(body) => std::fs::write(dir.join("agent-prompt.json"), body).unwrap(),
        Sidecar::Claude(body) => std::fs::write(dir.join("agent-question.json"), body).unwrap(),
    }
    fakes.clock.set_millis(1_000);
    fakes.fs.write(&status, "working\nwaiting\n").unwrap();
    host.pump();
    // Past the Claude settle window, so a Claude wait with no question has
    // surfaced its binary fallback too.
    fakes.clock.set_millis(2_000);
    host.pump();
    Waiting {
        host,
        pty,
        tab_id,
        _root: root,
    }
}

impl Waiting<'_> {
    fn prompt(&self) -> Option<PromptView> {
        self.host.pending_prompt(0, &self.tab_id)
    }

    fn answer(&mut self, answer: PromptAnswer) {
        let prompt_id = self.prompt().expect("a prompt is pending").prompt_id;
        self.host
            .handle(HostEvent::AnswerPrompt(PromptReply {
                key: SessionKey {
                    project: 0,
                    tab_id: self.tab_id.clone(),
                },
                prompt_id,
                answer,
            }))
            .unwrap();
    }

    /// What the phone sends for the same tap, spelled out field by field (not
    /// through [`PromptAnswer::decision`], which is what is under test).
    fn phone_answers(&mut self, answer: PromptAnswer) {
        let prompt_id = self.prompt().expect("a prompt is pending").prompt_id;
        let (choice, option_index) = match answer {
            PromptAnswer::Approve => (Some(PermissionChoice::AllowOnce), None),
            PromptAnswer::Deny => (Some(PermissionChoice::Deny), None),
            PromptAnswer::Option(i) => (None, Some(i)),
        };
        let cmd = PhoneCommand {
            command_id: CommandId::new("c1"),
            issued_at_ms: 0,
            body: CommandBody::PermissionDecision {
                session_id: SessionId::new(self.tab_id.clone()),
                prompt_id: PromptId::new(prompt_id),
                choice,
                option_index,
                option_indices: None,
                free_text: None,
                answers: None,
            },
        };
        let plain = serde_json::to_vec(&cmd).unwrap();
        let (nonce, ciphertext) = passthrough_seal()(&plain, 1, 0).unwrap();
        self.host
            .remote_bridge
            .as_mut()
            .unwrap()
            .handle_inbound(RemoteInbound::Envelope(EncryptedEnvelope {
                pairing_id: PairingId::new("pair-1"),
                seq: 1,
                sender: Role::Phone,
                sent_at_ms: 0,
                nonce,
                ciphertext,
            }));
    }
}

fn labels(view: &PromptView) -> Vec<&str> {
    view.buttons.iter().map(|b| b.label.as_str()).collect()
}

#[test]
fn every_agents_prompt_fixtures_become_the_expected_inline_view() {
    use PromptShape::*;
    let cases: [(&str, Sidecar, PromptShape, &[&str]); 8] = [
        ("claude", Sidecar::None, Binary, &["Approve", "Deny"]),
        (
            "claude",
            Sidecar::Claude(CLAUDE_QUESTION),
            SingleSelect,
            &["Pizza", "Sushi"],
        ),
        (
            "claude",
            Sidecar::Claude(CLAUDE_TWO_QUESTIONS),
            Unsupported,
            &[],
        ),
        ("codex", Sidecar::None, Binary, &["Approve", "Deny"]),
        ("opencode", Sidecar::None, Binary, &["Approve", "Deny"]),
        (
            "opencode",
            Sidecar::OpenCode(OPENCODE_PERMISSION),
            Binary,
            &["Approve", "Deny"],
        ),
        (
            "opencode",
            Sidecar::OpenCode(OPENCODE_QUESTION),
            SingleSelect,
            &["React", "Vue", "Svelte"],
        ),
        (
            "opencode",
            Sidecar::OpenCode(OPENCODE_CHECKLIST),
            Unsupported,
            &[],
        ),
    ];
    for (agent, sidecar, shape, expected) in cases {
        let fakes = Fakes::new();
        let w = waiting(&fakes, agent, sidecar, false);
        let view = w.prompt().unwrap_or_else(|| panic!("{agent}: a prompt"));
        assert_eq!(view.shape, shape, "{agent}");
        assert_eq!(labels(&view), expected, "{agent}");
        // Mission control's tile carries the same view.
        let tile = w.host.mission_view().tiles.into_iter().next().unwrap();
        assert_eq!(tile.prompt, Some(view), "{agent}: the tile's prompt");
    }
}

#[test]
fn a_claude_wait_shows_nothing_until_it_settles_on_binary() {
    let fakes = Fakes::new();
    let mut w = waiting(&fakes, "claude", Sidecar::None, false);
    let status = w.host.active_state().tabs[0].status_file.clone().unwrap();
    // Leave the wait, then start a new one: inside the settle window the
    // previous (answered) prompt must not be offered again.
    fakes
        .fs
        .write(&status, "working\nwaiting\nworking\n")
        .unwrap();
    fakes.clock.set_millis(3_000);
    w.host.pump();
    assert_eq!(w.prompt(), None, "not waiting");
    fakes
        .fs
        .write(&status, "working\nwaiting\nworking\nwaiting\n")
        .unwrap();
    fakes.clock.set_millis(4_000);
    w.host.pump();
    assert_eq!(
        w.prompt(),
        None,
        "a Claude wait inside the settle window offers nothing yet"
    );
    fakes.clock.set_millis(4_800);
    w.host.pump();
    assert_eq!(w.prompt().map(|v| v.shape), Some(PromptShape::Binary));
}

#[test]
fn nothing_is_offered_without_tracking_or_without_a_wait() {
    let fakes = Fakes::new();
    let (state, _pty) = fakes.state_with_a_tab();
    let tab_id = state.tabs[0].meta.id.clone();
    let mut host = fakes.host(state);
    host.pump();
    assert_eq!(host.pending_prompt(0, &tab_id), None, "not tracked");
    host.track_prompts();
    host.pump();
    assert_eq!(host.pending_prompt(0, &tab_id), None, "not waiting");
    assert_eq!(
        host.mission_view()
            .tiles
            .first()
            .and_then(|t| t.prompt.clone()),
        None
    );
}

/// Every tick's cumulative PTY input for `ticks` 50 ms steps from 2 s on.
fn timeline(w: &mut Waiting, fakes: &Fakes, ticks: u64) -> Vec<Vec<u8>> {
    (0..ticks)
        .map(|i| {
            fakes.clock.set_millis(2_000 + i * 50);
            w.host.tick();
            w.pty.input()
        })
        .collect()
}

#[test]
fn an_inline_answer_types_exactly_what_the_phone_types_on_the_same_ticks() {
    let cases: [(&str, Sidecar, PromptAnswer, &[u8]); 7] = [
        ("claude", Sidecar::None, PromptAnswer::Approve, b"1"),
        ("claude", Sidecar::None, PromptAnswer::Deny, b"\x1b"),
        (
            "claude",
            Sidecar::Claude(CLAUDE_QUESTION),
            PromptAnswer::Option(1),
            b"2",
        ),
        ("codex", Sidecar::None, PromptAnswer::Approve, b"y"),
        ("codex", Sidecar::None, PromptAnswer::Deny, b"\x1b"),
        ("opencode", Sidecar::None, PromptAnswer::Approve, b"\r"),
        (
            "opencode",
            Sidecar::OpenCode(OPENCODE_QUESTION),
            PromptAnswer::Option(2),
            b"\x1b[B\x1b[B\r",
        ),
    ];
    for (agent, sidecar, answer, bytes) in cases {
        let desktop_fakes = Fakes::new();
        let mut desktop = waiting(&desktop_fakes, agent, sidecar, false);
        desktop.answer(answer);
        let desktop_bytes = timeline(&mut desktop, &desktop_fakes, 20);

        let phone_fakes = Fakes::new();
        let mut phone = waiting(&phone_fakes, agent, sidecar, true);
        phone.phone_answers(answer);
        let phone_bytes = timeline(&mut phone, &phone_fakes, 20);

        assert_eq!(
            desktop_bytes, phone_bytes,
            "{agent} {answer:?}: byte for byte, tick for tick"
        );
        assert_eq!(
            desktop_bytes.last().unwrap().as_slice(),
            bytes,
            "{agent} {answer:?}"
        );
    }
}

#[test]
fn an_answer_is_refused_while_another_writer_holds_the_input_lock() {
    let fakes = Fakes::new();
    let mut w = waiting(&fakes, "claude", Sidecar::None, false);
    let lock = InputArbiter::shared();
    let browser = Writer::Viewer(crate::web::protocol::ViewerId::new("viewer-1"));
    lock.lock()
        .unwrap()
        .claim(&browser, "Chrome on macOS", 2_000);
    w.host.ui.input_lock = Some(lock.clone());

    w.answer(PromptAnswer::Approve);
    assert!(
        w.pty.input().is_empty(),
        "refused: nothing typed, nothing queued"
    );
    assert_eq!(
        w.host.ui.input_holder.as_deref(),
        Some("Chrome on macOS"),
        "and the holder is named"
    );

    // Once the browser has gone quiet, the same click goes through.
    fakes
        .clock
        .set_millis(2_000 + crate::web::arbiter::INPUT_LOCK_IDLE_MS as u64);
    w.answer(PromptAnswer::Approve);
    assert_eq!(w.pty.input(), b"1".to_vec());
}

#[test]
fn a_stale_or_unoffered_answer_is_refused_with_a_message() {
    let fakes = Fakes::new();
    let mut w = waiting(
        &fakes,
        "opencode",
        Sidecar::OpenCode(OPENCODE_CHECKLIST),
        false,
    );
    // A checklist offers no inline answers; an option index sent anyway must
    // not type a number key into it.
    w.answer(PromptAnswer::Option(0));
    assert!(w.pty.input().is_empty(), "nothing typed into the checklist");
    assert!(matches!(
        &w.host.ui.overlay,
        crate::tui::render::UiOverlay::Dialog(d) if d.title.contains("Open the session")
    ));

    let fakes = Fakes::new();
    let mut w = waiting(&fakes, "codex", Sidecar::None, false);
    w.host
        .handle(HostEvent::AnswerPrompt(PromptReply {
            key: SessionKey {
                project: 0,
                tab_id: w.tab_id.clone(),
            },
            prompt_id: "an-old-prompt".to_string(),
            answer: PromptAnswer::Approve,
        }))
        .unwrap();
    assert!(w.pty.input().is_empty(), "a superseded prompt gets no keys");
}

#[test]
fn an_answered_prompt_is_not_offered_again_and_a_repeat_click_types_nothing() {
    let fakes = Fakes::new();
    let mut w = waiting(&fakes, "codex", Sidecar::None, false);
    let prompt_id = w.prompt().unwrap().prompt_id;
    w.answer(PromptAnswer::Approve);
    assert_eq!(w.pty.input(), b"y".to_vec());
    assert_eq!(w.prompt(), None, "still waiting, but answered from here");
    assert_eq!(w.host.mission_view().tiles[0].prompt, None);

    w.host
        .handle(HostEvent::AnswerPrompt(PromptReply {
            key: SessionKey {
                project: 0,
                tab_id: w.tab_id.clone(),
            },
            prompt_id,
            answer: PromptAnswer::Approve,
        }))
        .unwrap();
    assert_eq!(w.pty.input(), b"y".to_vec(), "the second click is dropped");
}
