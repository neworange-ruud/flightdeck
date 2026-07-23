use super::*;
use crate::contracts::{Config, InterpretedStatus, ProjectState as CoreProjectState, TabState};
use crate::contracts::{PtySize, STATE_VERSION};
use crate::testing::FakePty;
use crate::tui::render::GitStatusCache;

use flightdeck_remote_protocol::relay::EncryptedEnvelope;
use flightdeck_remote_protocol::{
    CommandBody, CommandId, DesktopToPhone, PhoneCommand, PromptKind, Role, TranscriptItem,
};

use std::io::Write as _;

/// Seed a Claude session JSONL for a tab whose worktree resolves to
/// `worktree_abs` (`<repo_root>/worktrees/<name>`; `repo_root` is `/repo` in
/// `app_with_tabs`), placed under a temp `home` at the path
/// `newest_session_path` locates. Hand `home` to `set_transcript_home`.
fn seed_claude_session(home: &std::path::Path, worktree_abs: &str, lines: &[&str]) {
    let mangled: String = worktree_abs
        .chars()
        .map(|c| if c == '/' || c == '.' { '-' } else { c })
        .collect();
    let dir = home.join(".claude").join("projects").join(mangled);
    std::fs::create_dir_all(&dir).unwrap();
    let mut f =
        std::fs::File::create(dir.join("11111111-1111-1111-1111-111111111111.jsonl")).unwrap();
    for l in lines {
        writeln!(f, "{l}").unwrap();
    }
}

/// Append one JSONL record to the session seeded by [`seed_claude_session`],
/// simulating the agent writing a new turn after the initial sync.
fn append_claude_line(home: &std::path::Path, worktree_abs: &str, line: &str) {
    let mangled: String = worktree_abs
        .chars()
        .map(|c| if c == '/' || c == '.' { '-' } else { c })
        .collect();
    let path = home
        .join(".claude")
        .join("projects")
        .join(mangled)
        .join("11111111-1111-1111-1111-111111111111.jsonl");
    let mut f = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    writeln!(f, "{line}").unwrap();
}

// --- fixtures --------------------------------------------------------------

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

/// Build an [`AppState`] with the given tabs, each spawned with a (Running)
/// fake primary so `display_status` honours the injected interpreted status.
fn app_with_tabs(tabs: Vec<TabState>) -> AppState {
    let pty = FakePty::new();
    let state = CoreProjectState {
        version: STATE_VERSION,
        project_root_relative: ".".to_string(),
        base_branch: "main".to_string(),
        tabs,
    };
    let mut app = AppState::new(
        Config::default(),
        state,
        "/repo",
        "/repo/.flightdeck/state.json",
    );
    for tab in app.tabs.iter_mut() {
        tab.session
            .spawn_primary(
                &pty,
                "agent",
                &[],
                std::path::Path::new("/repo"),
                PtySize::default(),
            )
            .unwrap();
    }
    app
}

fn set_status(app: &mut AppState, tab: usize, s: InterpretedStatus) {
    app.tabs[tab].interpreted = Some(s);
}

fn view<'a>(name: &'a str, app: &'a AppState, cache: &'a GitStatusCache) -> ProjectView<'a> {
    ProjectView {
        id: ProjectId::new(name),
        name,
        state: app,
        cache,
    }
}

fn paired_bridge() -> RemoteBridge {
    let mut b = RemoteBridge::passthrough(0);
    b.handle_inbound(RemoteInbound::Paired {
        pairing_id: PairingId::new("pair-1"),
        peer_device_id: None,
    });
    b
}

fn collect<'a>(
    b: &mut RemoteBridge,
    views: &[ProjectView<'a>],
    now_ms: u64,
) -> Vec<DesktopToPhone> {
    let mut raw = Vec::new();
    b.tick(views, now_ms, &mut |o| raw.push(o));
    raw.iter().map(decode).collect()
}

fn decode(o: &RemoteOutbound) -> DesktopToPhone {
    match o {
        RemoteOutbound::SendEnvelope { ciphertext, .. } => {
            let bytes = STANDARD.decode(ciphertext).unwrap();
            serde_json::from_slice(&bytes).unwrap()
        }
        other => panic!("expected SendEnvelope, got {other:?}"),
    }
}

// --- pairing gating --------------------------------------------------------

#[test]
fn no_output_without_a_pairing() {
    let mut b = RemoteBridge::passthrough(0);
    let app = app_with_tabs(vec![tab_state("t1", "fix-login", "claude")]);
    let cache = GitStatusCache::new();
    let views = vec![view("proj", &app, &cache)];
    let msgs = collect(&mut b, &views, 1_000);
    assert!(msgs.is_empty());
    assert!(!b.is_paired());
}

// --- snapshot on connect ---------------------------------------------------

#[test]
fn first_tick_sends_full_snapshot() {
    let mut b = paired_bridge();
    let app = app_with_tabs(vec![tab_state("t1", "fix-login", "claude")]);
    let cache = GitStatusCache::new();
    let views = vec![view("proj", &app, &cache)];
    let msgs = collect(&mut b, &views, 1_000);
    let snap = msgs
        .iter()
        .find_map(|m| match m {
            DesktopToPhone::Snapshot(s) => Some(s),
            _ => None,
        })
        .expect("snapshot");
    assert_eq!(snap.projects.len(), 1);
    assert_eq!(snap.projects[0].sessions.len(), 1);
    assert_eq!(snap.projects[0].sessions[0].name, "fix-login");
}

// --- deltas after the baseline ---------------------------------------------

#[test]
fn status_change_sends_delta_not_snapshot() {
    let mut b = paired_bridge();
    let mut app = app_with_tabs(vec![tab_state("t1", "fix-login", "claude")]);
    set_status(&mut app, 0, InterpretedStatus::Idle);
    let cache = GitStatusCache::new();
    // Tick 1: snapshot baseline.
    {
        let views = vec![view("proj", &app, &cache)];
        let _ = collect(&mut b, &views, 1_000);
    }
    // Tick 2: status changes → StatusUpdate, no snapshot.
    set_status(&mut app, 0, InterpretedStatus::Working);
    let views = vec![view("proj", &app, &cache)];
    let msgs = collect(&mut b, &views, 2_000);
    assert!(msgs
        .iter()
        .any(|m| matches!(m, DesktopToPhone::StatusUpdate(_))));
    assert!(!msgs
        .iter()
        .any(|m| matches!(m, DesktopToPhone::Snapshot(_))));
    let update = msgs.iter().find_map(|m| match m {
        DesktopToPhone::StatusUpdate(u) => Some(u),
        _ => None,
    });
    assert_eq!(update.unwrap().updates[0].status, AgentStatus::Working);
}

// --- events ----------------------------------------------------------------

#[test]
fn working_to_idle_emits_finished_event() {
    let mut b = paired_bridge();
    let mut app = app_with_tabs(vec![tab_state("t1", "fix-login", "claude")]);
    set_status(&mut app, 0, InterpretedStatus::Working);
    let cache = GitStatusCache::new();
    {
        let views = vec![view("proj", &app, &cache)];
        let _ = collect(&mut b, &views, 1_000); // arm
    }
    set_status(&mut app, 0, InterpretedStatus::Idle);
    let views = vec![view("proj", &app, &cache)];
    let msgs = collect(&mut b, &views, 2_000);
    let ev = msgs
        .iter()
        .find_map(|m| match m {
            DesktopToPhone::Event(e) => Some(e),
            _ => None,
        })
        .expect("event");
    assert!(matches!(
        ev.kind,
        flightdeck_remote_protocol::EventKind::Finished { .. }
    ));
    assert_eq!(ev.deep_link.session_id.as_str(), "t1");
}

#[test]
fn grace_window_suppresses_events() {
    // grace_until_ms = 10_000: an edge at t=2_000 is tracked but not sent.
    let mut b = RemoteBridge::passthrough(10_000);
    b.handle_inbound(RemoteInbound::Paired {
        pairing_id: PairingId::new("pair-1"),
        peer_device_id: None,
    });
    let mut app = app_with_tabs(vec![tab_state("t1", "fix-login", "claude")]);
    set_status(&mut app, 0, InterpretedStatus::Working);
    let cache = GitStatusCache::new();
    {
        let views = vec![view("proj", &app, &cache)];
        let _ = collect(&mut b, &views, 1_000);
    }
    set_status(&mut app, 0, InterpretedStatus::Idle);
    let views = vec![view("proj", &app, &cache)];
    let msgs = collect(&mut b, &views, 2_000);
    assert!(!msgs.iter().any(|m| matches!(m, DesktopToPhone::Event(_))));
}

// --- needs-input preview flows into the session row ------------------------

#[test]
fn needs_input_populates_pending_question() {
    let home = tempfile::tempdir().unwrap();
    seed_claude_session(
        home.path(),
        "/repo/worktrees/fix-login",
        &[
            r#"{"type":"assistant","uuid":"a1","message":{"content":[{"type":"text","text":"May I run the installer script?"}]}}"#,
        ],
    );
    let mut b = paired_bridge();
    b.set_transcript_home(Some(home.path().to_path_buf()));
    let mut app = app_with_tabs(vec![tab_state("t1", "fix-login", "claude")]);
    set_status(&mut app, 0, InterpretedStatus::Working);
    let cache = GitStatusCache::new();
    // Sync the session file so the agent's last prose becomes the preview.
    {
        let views = vec![view("proj", &app, &cache)];
        let _ = collect(&mut b, &views, 1_000);
    }
    // Transition to needs-input. This is a plain permission prompt (no
    // AskUserQuestion in the JSONL), so the binary fallback — and thus its
    // preview — is DEFERRED by PROMPT_SETTLE_MS while the bridge waits for a
    // possibly-racing question to be ingested (remote-control-qa1). The status
    // still flips immediately.
    set_status(&mut app, 0, InterpretedStatus::WaitingForInput);
    let views = vec![view("proj", &app, &cache)];
    let early = collect(&mut b, &views, 2_000);
    let early_update = early
        .iter()
        .find_map(|m| match m {
            DesktopToPhone::StatusUpdate(u) => Some(u),
            _ => None,
        })
        .expect("status update");
    assert_eq!(early_update.updates[0].status, AgentStatus::NeedsInput);
    assert!(
        early_update.updates[0].pending_question.is_none(),
        "the binary preview is deferred until the settle window elapses"
    );

    // After the settle window (no question arrived), the binary fallback is
    // synthesized and its preview reaches the phone.
    let views = vec![view("proj", &app, &cache)];
    let settled = collect(&mut b, &views, 2_000 + super::PROMPT_SETTLE_MS + 1);
    let d = settled
        .iter()
        .find_map(|m| match m {
            DesktopToPhone::StatusUpdate(u) => u.updates.first(),
            _ => None,
        })
        .expect("status update after settle");
    assert!(d
        .pending_question
        .as_deref()
        .unwrap()
        .contains("installer script"));
}

#[test]
fn ask_user_question_racing_the_status_flip_is_not_shown_as_a_binary_prompt() {
    // Reproduces remote-control-qa1's premature-answer bug: the PreToolUse hook
    // flips status to waiting before the AskUserQuestion tool_use is written to
    // the JSONL. The bridge must NOT emit the binary allow/deny fallback in that
    // window (its "Allow once" keystroke would be consumed by the live question
    // selector as an answer); it must surface the real Question once ingested.
    let home = tempfile::tempdir().unwrap();
    let worktree = "/repo/worktrees/fix-login";
    seed_claude_session(
        home.path(),
        worktree,
        &[
            r#"{"type":"assistant","uuid":"a1","message":{"content":[{"type":"text","text":"Working on it."}]}}"#,
        ],
    );
    let mut b = paired_bridge();
    b.set_transcript_home(Some(home.path().to_path_buf()));
    let mut app = app_with_tabs(vec![tab_state("t1", "fix-login", "claude")]);
    set_status(&mut app, 0, InterpretedStatus::Working);
    let cache = GitStatusCache::new();
    {
        let views = vec![view("proj", &app, &cache)];
        let _ = collect(&mut b, &views, 1_000);
    }

    // Status flips to waiting while the question is still racing → no prompt.
    set_status(&mut app, 0, InterpretedStatus::WaitingForInput);
    {
        let views = vec![view("proj", &app, &cache)];
        let early = collect(&mut b, &views, 2_000);
        assert!(
            !early.iter().any(|m| matches!(m,
                DesktopToPhone::TranscriptAppend(f)
                    if f.items.iter().any(|i| matches!(i, TranscriptItem::PermissionPrompt { .. })))),
            "no prompt may be emitted while the AskUserQuestion is still racing"
        );
    }

    // The AskUserQuestion lands; the next tick surfaces it as a Question — never
    // a binary allow/deny — and still within the settle window.
    append_claude_line(
        home.path(),
        worktree,
        r#"{"type":"assistant","uuid":"aq1","message":{"content":[{"type":"tool_use","name":"AskUserQuestion","input":{"questions":[{"question":"Pizza or sushi?","header":"Lunch","multiSelect":false,"options":[{"label":"Pizza"},{"label":"Sushi"}]}]}}]}}"#,
    );
    let views = vec![view("proj", &app, &cache)];
    let msgs = collect(&mut b, &views, 2_100);
    let (kind, free_text) = msgs
        .iter()
        .filter_map(|m| match m {
            DesktopToPhone::TranscriptAppend(f) => Some(f),
            _ => None,
        })
        .flat_map(|f| f.items.iter())
        .find_map(|i| match i {
            TranscriptItem::PermissionPrompt {
                kind,
                allow_free_text,
                ..
            } => Some((*kind, *allow_free_text)),
            _ => None,
        })
        .expect("the question should now be surfaced");
    assert_eq!(
        kind,
        PromptKind::Question,
        "surfaced as a Question, not binary"
    );
    assert!(free_text, "AskUserQuestion allows a free-text answer");
}

#[test]
fn reads_claude_question_sidecar_into_a_structured_prompt() {
    // The PreToolUse hook pipes its stdin (the AskUserQuestion payload) to
    // `.flightdeck/agent-question.json`; the bridge parses `tool_input` from it
    // into a Question prompt on the needs-input edge (remote-control-qa1).
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".flightdeck")).unwrap();
    std::fs::write(
        dir.path().join(".flightdeck/agent-question.json"),
        r#"{"tool_name":"AskUserQuestion","tool_input":{"questions":[{"question":"Lunch?","header":"L","options":[{"label":"Pizza","description":"cheesy"},{"label":"Sushi"}]}]}}"#,
    )
    .unwrap();

    let sp = super::read_claude_question_sidecar(dir.path()).expect("parsed sidecar");
    assert_eq!(sp.kind, PromptKind::Question);
    assert_eq!(sp.command, "Lunch?");
    assert_eq!(sp.options.len(), 2);
    assert_eq!(sp.options[0].label, "Pizza");
    assert!(
        sp.allow_free_text,
        "AskUserQuestion allows a free-text answer"
    );

    // A missing/blank sidecar yields no prompt (→ binary fallback for a real
    // permission).
    let empty = tempfile::tempdir().unwrap();
    assert!(super::read_claude_question_sidecar(empty.path()).is_none());
}

// --- transcript reconstruction from the session file -----------------------

#[test]
fn session_file_flushes_as_transcript_append() {
    let home = tempfile::tempdir().unwrap();
    seed_claude_session(
        home.path(),
        "/repo/worktrees/fix-login",
        &[
            r#"{"type":"assistant","uuid":"a1","message":{"content":[{"type":"text","text":"Hello from the agent."}]}}"#,
        ],
    );
    let mut b = paired_bridge();
    b.set_transcript_home(Some(home.path().to_path_buf()));
    let app = app_with_tabs(vec![tab_state("t1", "fix-login", "claude")]);
    let cache = GitStatusCache::new();
    let views = vec![view("proj", &app, &cache)];
    let msgs = collect(&mut b, &views, 1_000);
    let feed = msgs
        .iter()
        .find_map(|m| match m {
            DesktopToPhone::TranscriptAppend(f) => Some(f),
            _ => None,
        })
        .expect("transcript append");
    assert_eq!(feed.session_id.as_str(), "t1");
    assert!(feed
        .items
        .iter()
        .any(|i| matches!(i, TranscriptItem::AgentMessage { text, .. } if text == "Hello from the agent.")));
}

// --- inbound request handling ----------------------------------------------

fn envelope(cmd: &PhoneCommand) -> EncryptedEnvelope {
    let plain = serde_json::to_vec(cmd).unwrap();
    let (nonce, ciphertext) = passthrough_seal()(&plain, 1, 0).unwrap();
    EncryptedEnvelope {
        pairing_id: PairingId::new("pair-1"),
        seq: 1,
        sender: Role::Phone,
        sent_at_ms: 0,
        nonce,
        ciphertext,
    }
}

#[test]
fn request_snapshot_command_forces_snapshot() {
    let mut b = paired_bridge();
    let app = app_with_tabs(vec![tab_state("t1", "fix-login", "claude")]);
    let cache = GitStatusCache::new();
    // Consume the connect snapshot first.
    {
        let views = vec![view("proj", &app, &cache)];
        let _ = collect(&mut b, &views, 1_000);
    }
    // Phone asks for a fresh snapshot.
    let cmd = PhoneCommand {
        command_id: CommandId::new("c1"),
        issued_at_ms: 0,
        body: CommandBody::RequestSnapshot { project_id: None },
    };
    b.handle_inbound(RemoteInbound::Envelope(envelope(&cmd)));
    let views = vec![view("proj", &app, &cache)];
    let msgs = collect(&mut b, &views, 2_000);
    assert!(msgs
        .iter()
        .any(|m| matches!(m, DesktopToPhone::Snapshot(_))));
}

#[test]
fn request_transcript_command_returns_feed() {
    let home = tempfile::tempdir().unwrap();
    seed_claude_session(
        home.path(),
        "/repo/worktrees/fix-login",
        &[
            r#"{"type":"assistant","uuid":"a1","message":{"content":[{"type":"text","text":"some prior output"}]}}"#,
        ],
    );
    let mut b = paired_bridge();
    b.set_transcript_home(Some(home.path().to_path_buf()));
    let app = app_with_tabs(vec![tab_state("t1", "fix-login", "claude")]);
    let cache = GitStatusCache::new();
    {
        let views = vec![view("proj", &app, &cache)];
        let _ = collect(&mut b, &views, 1_000);
    }
    let cmd = PhoneCommand {
        command_id: CommandId::new("c2"),
        issued_at_ms: 0,
        body: CommandBody::RequestTranscript {
            session_id: SessionId::new("t1"),
            from_index: None,
        },
    };
    b.handle_inbound(RemoteInbound::Envelope(envelope(&cmd)));
    let views = vec![view("proj", &app, &cache)];
    let msgs = collect(&mut b, &views, 2_000);
    let feed = msgs
        .iter()
        .find_map(|m| match m {
            DesktopToPhone::Transcript(f) => Some(f),
            _ => None,
        })
        .expect("transcript feed");
    assert!(feed.replace);
    assert_eq!(feed.session_id.as_str(), "t1");
}

#[test]
fn unknown_command_is_queued_for_command_bridge() {
    let mut b = paired_bridge();
    let cmd = PhoneCommand {
        command_id: CommandId::new("c3"),
        issued_at_ms: 0,
        body: CommandBody::Reply {
            session_id: SessionId::new("t1"),
            text: "keep going".to_string(),
        },
    };
    b.handle_inbound(RemoteInbound::Envelope(envelope(&cmd)));
    let queued = b.take_pending_commands();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].command_id.as_str(), "c3");
    // Drained: a second take is empty.
    assert!(b.take_pending_commands().is_empty());
}

// --- serialization round-trip ----------------------------------------------

#[test]
fn seal_open_round_trip_preserves_message() {
    let seal = passthrough_seal();
    let open = passthrough_open();
    let msg = DesktopToPhone::Rollup(flightdeck_remote_protocol::RollupUpdate { projects: vec![] });
    let bytes = serde_json::to_vec(&msg).unwrap();
    let (nonce, ciphertext) = seal(&bytes, 1, 0).unwrap();
    let plain = open(1, Role::Desktop, 0, &nonce, &ciphertext).unwrap();
    let round: DesktopToPhone = serde_json::from_slice(&plain).unwrap();
    assert_eq!(round, msg);
}

// --- outbound seq continuity across channel re-derivation (bbf) -------------

/// Collect the raw outbound envelopes a tick produces (seq intact).
fn collect_raw<'a>(
    b: &mut RemoteBridge,
    views: &[ProjectView<'a>],
    now_ms: u64,
) -> Vec<RemoteOutbound> {
    let mut raw = Vec::new();
    b.tick(views, now_ms, &mut |o| raw.push(o));
    raw
}

fn seq_of(o: &RemoteOutbound) -> u64 {
    match o {
        RemoteOutbound::SendEnvelope { seq, .. } => *seq,
        other => panic!("expected SendEnvelope, got {other:?}"),
    }
}

/// Re-deriving the E2E channel for the SAME, already-active pairing (a repeat
/// `pairing_claimed`, or the startup go-live) must NOT rewind the outbound seq:
/// the phone only reset its receive cursor on a genuine first claim, so a rewind
/// would make it drop every "duplicate" seq and stall the feed (remote-control-bbf).
#[test]
fn install_channel_floors_outbound_seq() {
    let app = app_with_tabs(vec![tab_state("t1", "fix-login", "claude")]);
    let cache = GitStatusCache::new();
    let views = vec![view("proj", &app, &cache)];

    let mut b = paired_bridge();
    let first = collect_raw(&mut b, &views, 1_000);
    let high = first.iter().map(seq_of).max().expect("first tick emits");
    assert!(high >= 1);

    // Re-derive the channel for the same pairing, passing a stale resume-from of
    // 0 (as the runtime `pairing_claimed` path does). The floor must hold.
    b.install_channel(passthrough_seal(), passthrough_open(), 0);
    // Re-confirming the same pairing asks for a fresh snapshot without rewinding.
    b.handle_inbound(RemoteInbound::Paired {
        pairing_id: PairingId::new("pair-1"),
        peer_device_id: None,
    });
    let second = collect_raw(&mut b, &views, 2_000);
    let next = second.iter().map(seq_of).min().expect("second tick emits");
    assert_eq!(
        next,
        high + 1,
        "outbound seq must keep ascending across a same-pairing re-derivation, not reset"
    );
}

/// Switching to a genuinely DIFFERENT pairing (a new peer with a fresh receive
/// cursor at 0) restarts the outbound stream from seq 1.
#[test]
fn switching_pairing_restarts_outbound_seq() {
    let app = app_with_tabs(vec![tab_state("t1", "fix-login", "claude")]);
    let cache = GitStatusCache::new();
    let views = vec![view("proj", &app, &cache)];

    let mut b = paired_bridge();
    let first = collect_raw(&mut b, &views, 1_000);
    assert!(first.iter().map(seq_of).max().unwrap() >= 1);

    b.handle_inbound(RemoteInbound::Paired {
        pairing_id: PairingId::new("pair-2"),
        peer_device_id: None,
    });
    let second = collect_raw(&mut b, &views, 2_000);
    assert_eq!(
        seq_of(&second[0]),
        1,
        "a new pairing's first envelope must be seq 1"
    );
}

/// A `SeqResync` (the relay rejected our outbound seq after losing its watermark)
/// restarts the active pairing's outbound stream from seq 1 with a fresh full
/// snapshot, so a restarted relay accepts it and the phone re-syncs.
#[test]
fn seq_resync_restarts_stream_from_seq_1_with_snapshot() {
    let app = app_with_tabs(vec![tab_state("t1", "fix-login", "claude")]);
    let cache = GitStatusCache::new();
    let views = vec![view("proj", &app, &cache)];

    let mut b = paired_bridge();
    let first = collect_raw(&mut b, &views, 1_000);
    assert!(first.iter().map(seq_of).max().unwrap() >= 1);

    b.handle_inbound(RemoteInbound::SeqResync {
        pairing_id: PairingId::new("pair-1"),
    });
    let after = collect_raw(&mut b, &views, 2_000);
    assert_eq!(
        seq_of(&after[0]),
        1,
        "resynced stream restarts gaplessly from seq 1"
    );
    assert!(
        matches!(decode(&after[0]), DesktopToPhone::Snapshot(_)),
        "the resynced stream must lead with a fresh full snapshot"
    );
}

/// A `SeqResync` for a *different* pairing than the active one is ignored (no
/// spurious rewind of the live stream).
#[test]
fn seq_resync_for_other_pairing_is_ignored() {
    let app = app_with_tabs(vec![tab_state("t1", "fix-login", "claude")]);
    let cache = GitStatusCache::new();
    let views = vec![view("proj", &app, &cache)];

    let mut b = paired_bridge();
    let high = collect_raw(&mut b, &views, 1_000)
        .iter()
        .map(seq_of)
        .max()
        .unwrap();

    b.handle_inbound(RemoteInbound::SeqResync {
        pairing_id: PairingId::new("other-pairing"),
    });
    // Trigger another send; seq must keep ascending (no reset).
    b.handle_inbound(RemoteInbound::Paired {
        pairing_id: PairingId::new("pair-1"),
        peer_device_id: None,
    });
    let next = collect_raw(&mut b, &views, 2_000)
        .iter()
        .map(seq_of)
        .min()
        .unwrap();
    assert_eq!(
        next,
        high + 1,
        "an unrelated resync must not rewind the stream"
    );
}

// --- OpenCode prompt sidecar (remote-control-tdv) --------------------------

/// Write `<worktree>/.flightdeck/agent-prompt.json` with `body`, returning the
/// worktree root so the reader can be pointed at it.
fn write_sidecar(dir: &std::path::Path, body: &str) {
    let fd = dir.join(".flightdeck");
    std::fs::create_dir_all(&fd).unwrap();
    std::fs::write(fd.join("agent-prompt.json"), body).unwrap();
}

/// The most recently pushed item of a builder, via its full load.
fn last_item(builder: &TranscriptBuilder) -> TranscriptItem {
    builder.load(None).items.into_iter().next_back().unwrap()
}

#[test]
fn sidecar_question_surfaces_structured_question_prompt() {
    let wt = tempfile::tempdir().unwrap();
    write_sidecar(
        wt.path(),
        r#"{"kind":"question","text":"Which framework?","options":[
            {"label":"React","description":"Use React"},
            {"label":"Vue"},
            {"label":"Svelte","description":"Use Svelte"}]}"#,
    );

    let sp = read_prompt_sidecar(wt.path()).expect("question sidecar parses");
    assert_eq!(sp.kind, PromptKind::Question);
    assert!(sp.allow_free_text, "questions allow a free-text answer");
    assert!(
        !sp.multi_select,
        "no `multiple` field defaults to single-select"
    );
    assert_eq!(sp.command, "Which framework?");
    assert_eq!(sp.options.len(), 3);
    assert_eq!(sp.options[0].index, 0);
    assert_eq!(sp.options[0].label, "React");
    assert_eq!(sp.options[0].description.as_deref(), Some("Use React"));
    assert_eq!(sp.options[1].description, None);
    assert!(
        sp.options.iter().all(|o| o.choice.is_none()),
        "question options carry no binary choice"
    );

    // Feeding it to a builder makes the needs-input edge emit a Question prompt.
    let mut builder = TranscriptBuilder::new(SessionId::new("s1"));
    builder.set_structured_prompt(sp);
    builder.on_needs_input(1_000);
    match last_item(&builder) {
        TranscriptItem::PermissionPrompt {
            kind,
            command,
            options,
            allow_free_text,
            ..
        } => {
            assert_eq!(kind, PromptKind::Question);
            assert_eq!(command, "Which framework?");
            assert_eq!(options.len(), 3);
            assert!(allow_free_text);
        }
        other => panic!("expected a PermissionPrompt, got {other:?}"),
    }
}

#[test]
fn sidecar_multi_select_question_sets_the_flag() {
    let wt = tempfile::tempdir().unwrap();
    write_sidecar(
        wt.path(),
        r#"{"kind":"question","text":"Which checks?","multiple":true,"options":[
            {"label":"Tests"},{"label":"Clippy"}]}"#,
    );

    let sp = read_prompt_sidecar(wt.path()).expect("question sidecar parses");
    assert_eq!(sp.kind, PromptKind::Question);
    assert!(sp.multi_select, "`multiple`:true is a checklist question");
    assert_eq!(sp.options.len(), 2);
}

#[test]
fn sidecar_permission_is_never_multi_select() {
    // A permission sidecar with a stray `multiple` flag stays single-choice:
    // permissions are always a binary decision.
    let wt = tempfile::tempdir().unwrap();
    write_sidecar(
        wt.path(),
        r#"{"kind":"permission","text":"Run tests?","multiple":true,"options":[
            {"label":"Allow"},{"label":"Deny"}]}"#,
    );

    let sp = read_prompt_sidecar(wt.path()).expect("permission sidecar parses");
    assert_eq!(sp.kind, PromptKind::Permission);
    assert!(!sp.multi_select, "permissions are never multi-select");
}

#[test]
fn missing_sidecar_yields_binary_fallback() {
    let wt = tempfile::tempdir().unwrap();
    assert!(
        read_prompt_sidecar(wt.path()).is_none(),
        "absent sidecar -> binary fallback"
    );

    // A builder with no structured prompt emits the binary allow/deny prompt.
    let mut builder = TranscriptBuilder::new(SessionId::new("s2"));
    builder.on_needs_input(1_000);
    match last_item(&builder) {
        TranscriptItem::PermissionPrompt {
            kind,
            options,
            allow_free_text,
            ..
        } => {
            assert_eq!(kind, PromptKind::Permission);
            assert_eq!(options.len(), 2, "binary allow/deny");
            assert_eq!(options[0].choice, Some(PermissionChoice::AllowOnce));
            assert_eq!(options[1].choice, Some(PermissionChoice::Deny));
            assert!(!allow_free_text);
        }
        other => panic!("expected a PermissionPrompt, got {other:?}"),
    }
}

#[test]
fn sidecar_permission_maps_options_to_binary_choices() {
    let wt = tempfile::tempdir().unwrap();
    write_sidecar(
        wt.path(),
        r#"{"kind":"permission","text":"Run rm -rf?","options":[
            {"label":"Allow once"},{"label":"Deny"}]}"#,
    );
    let sp = read_prompt_sidecar(wt.path()).expect("permission sidecar parses");
    assert_eq!(sp.kind, PromptKind::Permission);
    assert!(!sp.allow_free_text);
    assert_eq!(sp.options[0].choice, Some(PermissionChoice::AllowOnce));
    assert_eq!(sp.options[1].choice, Some(PermissionChoice::Deny));
}

#[test]
fn unclear_permission_and_empty_options_fall_back_to_binary() {
    let wt = tempfile::tempdir().unwrap();

    // Empty options -> None regardless of kind.
    write_sidecar(wt.path(), r#"{"kind":"question","text":"?","options":[]}"#);
    assert!(read_prompt_sidecar(wt.path()).is_none());

    // A permission option whose label is neither allow-ish nor deny-ish -> None.
    write_sidecar(
        wt.path(),
        r#"{"kind":"permission","text":"?","options":[{"label":"Maybe"},{"label":"Deny"}]}"#,
    );
    assert!(
        read_prompt_sidecar(wt.path()).is_none(),
        "an unclassifiable permission option must fall back to binary"
    );

    // Malformed JSON -> None.
    write_sidecar(wt.path(), "{not json");
    assert!(read_prompt_sidecar(wt.path()).is_none());
}

// --- link-state gating: pause seal/queue during a relay outage (0ef.10) -----

/// While the relay link is down the bridge must PAUSE all seal/queue work — it
/// otherwise seals StatusUpdate/Rollup/etc. into the outbound channel every tick
/// during an outage (the client is not draining it while reconnecting), growing
/// it without bound and flooding the backlog on reconnect (remote-control-0ef.10).
/// Reconnect-replay is preserved: a reconnect re-arms a fresh snapshot via
/// `Paired`, and the outbound seq is not corrupted (it never advances while paused).
#[test]
fn disconnected_link_pauses_seal_and_queue() {
    use crate::remote::RemoteLinkState;

    let app = app_with_tabs(vec![tab_state("t1", "fix-login", "claude")]);
    let cache = GitStatusCache::new();
    let views = vec![view("proj", &app, &cache)];

    let mut b = paired_bridge();
    // Baseline: paired + link up (default) → the first tick seals+queues.
    let first = collect_raw(&mut b, &views, 1_000);
    let high = first
        .iter()
        .map(seq_of)
        .max()
        .expect("connected link sends");
    assert!(high >= 1);

    // Relay outage: the client reports the link Disconnected → seal/queue pauses.
    b.handle_inbound(RemoteInbound::Link(RemoteLinkState::Disconnected));
    assert!(
        collect_raw(&mut b, &views, 2_000).is_empty(),
        "no seal/queue while the link is down (0ef.10)"
    );
    // Still paused mid-reconnect (Connecting is not yet authenticated).
    b.handle_inbound(RemoteInbound::Link(RemoteLinkState::Connecting));
    assert!(
        collect_raw(&mut b, &views, 3_000).is_empty(),
        "still paused while reconnecting"
    );
    // And on the terminal Incompatible state.
    b.handle_inbound(RemoteInbound::Link(RemoteLinkState::Incompatible {
        our_version: 3,
        relay_min: 4,
        relay_max: 4,
    }));
    assert!(
        collect_raw(&mut b, &views, 4_000).is_empty(),
        "paused on the terminal version-incompatible state"
    );

    // Reconnect: the real path re-emits Link(Connected) + Paired, which re-arms a
    // fresh snapshot. The stream resumes WITHOUT a stale backlog and the seq keeps
    // ascending from where it left off (not corrupted by the paused ticks).
    b.handle_inbound(RemoteInbound::Link(RemoteLinkState::Connected {
        latency_ms: 5,
    }));
    b.handle_inbound(RemoteInbound::Paired {
        pairing_id: PairingId::new("pair-1"),
        peer_device_id: None,
    });
    let after = collect_raw(&mut b, &views, 5_000);
    assert_eq!(
        after.iter().map(seq_of).min().expect("resumes sending"),
        high + 1,
        "outbound seq keeps ascending across the outage — no gap, no rewind"
    );
    assert!(
        after
            .iter()
            .any(|o| matches!(decode(o), DesktopToPhone::Snapshot(_))),
        "reconnect leads with a fresh snapshot, not a paused-tick backlog"
    );
}

#[test]
fn deferred_pty_writes_fire_only_once_due() {
    // Claude's multi-select submit Enter is queued with a future due time and
    // must not flush early, then flush exactly once when the deadline passes
    // (remote-control-dc9).
    let mut b = RemoteBridge::passthrough(0);
    let sid = SessionId::new("s1");
    b.enqueue_deferred_pty(sid.clone(), 1_000, b"\r".to_vec());

    // Before the deadline: nothing is due.
    assert!(b.take_due_deferred_pty(999).is_empty());
    // At/after the deadline: the write is returned, once.
    let due = b.take_due_deferred_pty(1_000);
    assert_eq!(due, vec![(sid, b"\r".to_vec())]);
    // Already drained — a later poll yields nothing.
    assert!(b.take_due_deferred_pty(2_000).is_empty());
}
