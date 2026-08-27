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
    // A phone peer is attached: the relay announces peer presence on attach, and
    // the desktop only seals+sends per-tick deltas while a phone is present
    // (remote-control-uqa). These tests model the phone-connected path.
    b.handle_inbound(RemoteInbound::Presence {
        pairing_id: PairingId::new("pair-1"),
        peer: Role::Phone,
        state: flightdeck_remote_protocol::PresenceState::Connected,
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

#[test]
fn no_deltas_sealed_or_sent_while_no_phone_peer_attached() {
    // remote-control-uqa: paired + link up, but no phone attached. The bridge
    // must NOT seal+send per-tick snapshot/status/rollup deltas (the 2026-07-22
    // incident had it sending a status_update ~once a second for hours into an
    // empty relay queue). A phone attaching then gets a fresh full snapshot.
    let mut b = RemoteBridge::passthrough(0);
    b.handle_inbound(RemoteInbound::Paired {
        pairing_id: PairingId::new("pair-1"),
        peer_device_id: None,
    });
    // No Presence event yet → peer_present stays false.
    let app = app_with_tabs(vec![tab_state("t1", "fix-login", "claude")]);
    let cache = GitStatusCache::new();
    let views = vec![view("proj", &app, &cache)];
    assert!(b.is_paired());

    assert!(
        collect(&mut b, &views, 1_000).is_empty(),
        "no phone attached → nothing sealed/sent"
    );
    assert!(
        collect(&mut b, &views, 2_000).is_empty(),
        "still nothing on the next tick (this was the once-a-second spam)"
    );

    // Phone attaches → the next tick leads with a fresh snapshot.
    b.handle_inbound(RemoteInbound::Presence {
        pairing_id: PairingId::new("pair-1"),
        peer: Role::Phone,
        state: flightdeck_remote_protocol::PresenceState::Connected,
    });
    let msgs = collect(&mut b, &views, 3_000);
    assert!(
        msgs.iter()
            .any(|m| matches!(m, DesktopToPhone::Snapshot(_))),
        "a newly-attached phone gets a fresh snapshot, got {msgs:?}"
    );
}

#[test]
fn a_repeat_phone_connected_presence_still_rearms_the_snapshot() {
    // remote-control-e9l: when a phone's socket dies half-open (iOS suspending
    // the app), the relay keeps the stale leg until its idle timeout. The phone
    // reconnecting SUPERSEDES that leg, and the relay deliberately sends no
    // `Disconnected` for a superseded leg — so the desktop sees `Connected`
    // again with `peer_present` already true, i.e. no false→true edge. An
    // edge-triggered re-arm left that phone with no fresh snapshot, and
    // `status_update` deltas can never add or remove a session, so it sat on a
    // stale session list indefinitely.
    let mut b = paired_bridge();
    let cache = GitStatusCache::new();

    // Baseline: the phone has seen this world.
    let app = app_with_tabs(vec![tab_state("t1", "connection-issues", "claude")]);
    {
        let views = vec![view("proj", &app, &cache)];
        let msgs = collect(&mut b, &views, 1_000);
        assert!(msgs
            .iter()
            .any(|m| matches!(m, DesktopToPhone::Snapshot(_))));
    }

    // The session set changes while the phone is away, and the desktop's
    // baseline advances past it (the forced snapshot goes to a leg that is
    // already dead, and the relay can shed it from the queue).
    let app = app_with_tabs(vec![tab_state("t2", "connection-issues-v2", "claude")]);
    {
        let views = vec![view("proj", &app, &cache)];
        let _ = collect(&mut b, &views, 2_000);
    }

    // The phone reattaches by superseding: `Connected` with no intervening
    // `Disconnected`. It must still be handed a full snapshot.
    b.handle_inbound(RemoteInbound::Presence {
        pairing_id: PairingId::new("pair-1"),
        peer: Role::Phone,
        state: flightdeck_remote_protocol::PresenceState::Connected,
    });
    let views = vec![view("proj", &app, &cache)];
    let msgs = collect(&mut b, &views, 3_000);
    let snap = msgs
        .iter()
        .find_map(|m| match m {
            DesktopToPhone::Snapshot(s) => Some(s),
            _ => None,
        })
        .expect("a resupersede-attached phone must get a fresh snapshot");
    assert_eq!(snap.projects[0].sessions.len(), 1);
    assert_eq!(
        snap.projects[0].sessions[0].name, "connection-issues-v2",
        "the snapshot must carry the CURRENT session set, not the stale one"
    );
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

// --- unpaired transcript-sync throttling (remote-control-0ef.13) -----------

#[test]
fn unpaired_transcript_sync_is_throttled_and_forced_on_repair() {
    let home = tempfile::tempdir().unwrap();
    let worktree = "/repo/worktrees/fix-login";
    seed_claude_session(
        home.path(),
        worktree,
        &[
            r#"{"type":"assistant","uuid":"a1","message":{"content":[{"type":"text","text":"first"}]}}"#,
        ],
    );

    // NOT paired: `tick()` never sends, but `sync_transcript` still runs on its
    // own throttled cadence via the caller-supplied (injected-clock) `now_ms`.
    let mut b = RemoteBridge::passthrough(0);
    b.set_transcript_home(Some(home.path().to_path_buf()));
    assert!(!b.is_paired());
    let app = app_with_tabs(vec![tab_state("t1", "fix-login", "claude")]);
    let cache = GitStatusCache::new();
    let sid = SessionId::new("t1");

    let contains = |b: &RemoteBridge, needle: &str| -> bool {
        b.transcripts.get(&sid).is_some_and(|builder| {
            builder.load(None).items.iter().any(
                |i| matches!(i, TranscriptItem::AgentMessage { text, .. } if text.contains(needle)),
            )
        })
    };

    // Tick 1 (t=0ms): no prior unpaired sync recorded, so the first tick always
    // syncs even though unpaired.
    {
        let views = vec![view("proj", &app, &cache)];
        b.tick(&views, 0, &mut |_| {});
    }
    assert!(
        contains(&b, "first"),
        "first tick must sync unconditionally"
    );

    // The agent writes a new turn, but we advance the injected clock by only
    // 500ms — well inside `UNPAIRED_TRANSCRIPT_SYNC_INTERVAL_MS` (3_000ms).
    append_claude_line(
        home.path(),
        worktree,
        r#"{"type":"assistant","uuid":"a2","message":{"content":[{"type":"text","text":"second"}]}}"#,
    );
    {
        let views = vec![view("proj", &app, &cache)];
        b.tick(&views, 500, &mut |_| {});
    }
    assert!(
        !contains(&b, "second"),
        "throttled: unpaired sync must not run again before the interval elapses"
    );

    // Advance the injected clock to exactly the throttle boundary (3_000ms
    // since the last unpaired sync at t=0) — now due.
    {
        let views = vec![view("proj", &app, &cache)];
        b.tick(&views, 3_000, &mut |_| {});
    }
    assert!(
        contains(&b, "second"),
        "past the throttle interval, the unpaired sync must run again"
    );

    // The agent writes yet another turn, and the phone pairs almost
    // immediately after the last unpaired sync (t=3_000 -> t=3_050, only 50ms
    // later — nowhere near due per the throttle). Pairing must force a sync
    // on this very tick regardless, so a late-pairing phone gets full history.
    append_claude_line(
        home.path(),
        worktree,
        r#"{"type":"assistant","uuid":"a3","message":{"content":[{"type":"text","text":"third"}]}}"#,
    );
    b.handle_inbound(RemoteInbound::Paired {
        pairing_id: PairingId::new("pair-1"),
        peer_device_id: None,
    });
    assert!(b.is_paired());
    {
        let views = vec![view("proj", &app, &cache)];
        b.tick(&views, 3_050, &mut |_| {});
    }
    assert!(
        contains(&b, "third"),
        "(re)pairing must force an immediate sync, bypassing the unpaired throttle"
    );
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

/// A `SeqResync` (the peer's inbound cursor is stale) re-sends a fresh full
/// snapshot on the active pairing — WITHOUT renumbering the outbound stream.
///
/// The rewind this used to do (remote-control-bbf) is what livelocked the stream
/// against a relay that persists its watermark: the restart at seq 1 was
/// rejected, which drove another resync, which rewound again
/// (remote-control-arg). The relay now absorbs a rewind itself, so `out_seq`
/// must keep ascending.
#[test]
fn seq_resync_resnapshots_without_renumbering_the_stream() {
    let app = app_with_tabs(vec![tab_state("t1", "fix-login", "claude")]);
    let cache = GitStatusCache::new();
    let views = vec![view("proj", &app, &cache)];

    let mut b = paired_bridge();
    let first = collect_raw(&mut b, &views, 1_000);
    let high = first.iter().map(seq_of).max().unwrap();
    assert!(high >= 1);

    b.handle_inbound(RemoteInbound::SeqResync {
        pairing_id: PairingId::new("pair-1"),
    });
    let after = collect_raw(&mut b, &views, 2_000);
    assert_eq!(
        seq_of(&after[0]),
        high + 1,
        "the resynced stream continues gaplessly; it must NOT restart at seq 1"
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

/// The mirror image of `SeqResync`: the relay rejected our OUTBOUND envelopes
/// because our seq ran ahead of its watermark, and named the seq it will accept
/// next. Here `out_seq` MUST move — it is the thing that is wrong.
///
/// Before this existed the two faults shared one bare advisory and only the
/// inbound half was implemented, so a runaway sender was never corrected: the
/// relay kept demanding `high_water + 1`, the desktop kept counting past it, and
/// the pairing wedged until the user re-paired (remote-control-zv3).
#[test]
fn seq_realign_renumbers_the_outbound_stream_to_the_relays_expected_seq() {
    let app = app_with_tabs(vec![tab_state("t1", "fix-login", "claude")]);
    let cache = GitStatusCache::new();
    let views = vec![view("proj", &app, &cache)];

    let mut b = paired_bridge();
    let high = collect_raw(&mut b, &views, 1_000)
        .iter()
        .map(seq_of)
        .max()
        .unwrap();
    assert!(high >= 1);

    // The relay is far behind us and will only accept 98 next.
    b.handle_inbound(RemoteInbound::SeqRealign {
        pairing_id: PairingId::new("pair-1"),
        next_seq: 98,
    });

    let after = collect_raw(&mut b, &views, 2_000);
    assert_eq!(
        seq_of(&after[0]),
        98,
        "the next envelope must be exactly the seq the relay asked for"
    );
    assert!(
        matches!(decode(&after[0]), DesktopToPhone::Snapshot(_)),
        "realigning must lead with a fresh full snapshot: the peer missed \
         everything sent while we were ahead"
    );
}

/// A realign advisory for a pairing we are no longer feeding must not rewind the
/// live stream — a late frame for a replaced pairing would otherwise reintroduce
/// exactly the divergence this fixes.
#[test]
fn seq_realign_for_other_pairing_is_ignored() {
    let app = app_with_tabs(vec![tab_state("t1", "fix-login", "claude")]);
    let cache = GitStatusCache::new();
    let views = vec![view("proj", &app, &cache)];

    let mut b = paired_bridge();
    let high = collect_raw(&mut b, &views, 1_000)
        .iter()
        .map(seq_of)
        .max()
        .unwrap();

    b.handle_inbound(RemoteInbound::SeqRealign {
        pairing_id: PairingId::new("other-pairing"),
        next_seq: 5,
    });
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
        "an unrelated realign must not renumber the live stream"
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

// --- ack-based peer liveness (remote-control-5qu) --------------------------
//
// GROUND TRUTH these tests encode, from the user's own `~/.flightdeck/remote.json`
// (2026-08-05, confirmed still degrading 2026-08-21):
//
//     pair_0009_73faf508bbca
//       last_sent_seq        23943 -> 32985     (+9042 in 17 days)
//       last_acked_by_peer       0 ->     0     <-- never advanced
//       last_received_seq        0 ->     0     <-- never advanced
//
// The desktop→relay socket was ESTABLISHED the whole time and the link indicator
// (driven by the relay `pong`) showed healthy, because liveness was measured on
// the desktop↔RELAY hop while the failure was on the relay↔PHONE hop. `peer_present`
// latched `true` from a single presence frame and nothing could clear it — the
// relay deliberately sends no `Disconnected` for a superseded leg — so the
// per-tick feed sealed and shipped ~33,000 envelopes into the void.

/// A [`RemoteState`] carrying one pairing, so a test can assert in the same
/// counters the bug report used.
fn recording_state(pairing_id: &str) -> crate::remote::RemoteState {
    let mut st = crate::remote::RemoteState::default();
    st.pairings
        .push(crate::remote::Pairing::new(pairing_id.to_string()));
    st
}

/// Tick the bridge and mirror every sealed envelope into `st` exactly as
/// `client::handle_outbound` does (monotonic `last_sent_seq` bump on a successful
/// send, nothing else). Returns how many envelopes this tick put on the wire.
///
/// This is what lets the assertions read `last_sent_seq` / `last_acked_by_peer`:
/// those cursors live in the relay client's persisted state, and the desktop's
/// side of the reported failure is exactly "one climbing while the other never
/// moves".
fn tick_recording(
    b: &mut RemoteBridge,
    views: &[ProjectView<'_>],
    now_ms: u64,
    st: &mut crate::remote::RemoteState,
) -> usize {
    let mut sent = 0usize;
    b.tick(views, now_ms, &mut |o| {
        if let RemoteOutbound::SendEnvelope {
            pairing_id, seq, ..
        } = &o
        {
            sent += 1;
            if let Some(p) = st.pairing_mut(pairing_id.as_str()) {
                if *seq > p.last_sent_seq {
                    p.last_sent_seq = *seq;
                }
            }
        }
    });
    sent
}

/// A paired bridge whose phone is attached AND whose relay has proved it forwards
/// peer acks — the relay echoes each activated pairing's stored ack cursor right
/// after `auth_ok`, which for a phone that has confirmed nothing is `cursor: 0`.
/// That echo arms the ack deadline without crediting the phone with anything.
fn paired_bridge_with_ack_capable_relay() -> RemoteBridge {
    let mut b = paired_bridge();
    b.handle_inbound(RemoteInbound::PeerAck {
        pairing_id: PairingId::new("pair-1"),
        cursor: 0,
    });
    b
}

/// Deliver the phone's cumulative ack for everything sealed so far, as the relay
/// now forwards it.
fn ack_through(b: &mut RemoteBridge, cursor: u64) {
    b.handle_inbound(RemoteInbound::PeerAck {
        pairing_id: PairingId::new("pair-1"),
        cursor,
    });
}

#[test]
fn a_phone_that_never_acks_stops_being_fed_and_reports_a_dark_peer() {
    // THE REPORTED FAILURE. Presence says `Connected`, the desktop seals a
    // status_update every tick (a Working session's running-time changes every
    // second — the real 1/second spam), and no ack ever comes back. The desktop
    // must stop feeding within a bounded time and must report the peer as not
    // live, WITHOUT needing a presence frame from the relay.
    let mut b = paired_bridge_with_ack_capable_relay();
    let mut st = recording_state("pair-1");
    let mut app = app_with_tabs(vec![tab_state("t1", "fix-login", "claude")]);
    set_status(&mut app, 0, InterpretedStatus::Working);
    let cache = GitStatusCache::new();

    // While the peer is presumed live the feed runs and `last_sent_seq` climbs,
    // exactly as it did on the user's machine.
    let mut now = 1_000u64;
    for _ in 0..10 {
        let views = vec![view("proj", &app, &cache)];
        assert!(
            tick_recording(&mut b, &views, now, &mut st) > 0,
            "the per-tick feed should still be running at {now}ms"
        );
        now += 1_000;
    }
    let climbed_to = st.pairing("pair-1").unwrap().last_sent_seq;
    assert!(
        climbed_to >= 10,
        "last_sent_seq should have climbed, got {climbed_to}"
    );
    assert_eq!(
        st.pairing("pair-1").unwrap().last_acked_by_peer,
        0,
        "no ack ever arrives in this scenario — that is the whole point"
    );
    assert_eq!(
        b.peer_liveness(),
        PeerLiveness::Live,
        "inside the ack window the phone is still given the benefit of the doubt"
    );

    // Run past the ack deadline. No presence frame, no Disconnected, nothing from
    // the relay — just silence from the phone.
    now = 1_000 + PEER_ACK_TIMEOUT_MS + 1_000;
    let views = vec![view("proj", &app, &cache)];
    tick_recording(&mut b, &views, now, &mut st);
    assert_eq!(
        b.peer_liveness(),
        PeerLiveness::Dark,
        "a phone that never acks must stop counting as present"
    );

    // And the feed is off: the cursor stops climbing no matter how long this runs.
    let frozen = st.pairing("pair-1").unwrap().last_sent_seq;
    for _ in 0..30 {
        now += 1_000;
        set_status(&mut app, 0, InterpretedStatus::Working);
        let views = vec![view("proj", &app, &cache)];
        assert_eq!(
            tick_recording(&mut b, &views, now, &mut st),
            0,
            "nothing may be sealed into the void once the peer is dark"
        );
    }
    assert_eq!(
        st.pairing("pair-1").unwrap().last_sent_seq,
        frozen,
        "last_sent_seq must stop climbing — it reached 33,000 in the report"
    );
    assert_eq!(st.pairing("pair-1").unwrap().last_acked_by_peer, 0);
}

#[test]
fn a_quiet_phone_with_nothing_to_ack_is_never_declared_dark() {
    // The other half of the contract: "no news" is only evidence when something
    // is owed. A connected phone that has acked everything (or has been sent
    // nothing at all) must stay present for as long as the desktop is idle —
    // hours of it — or this guard would be worse than the bug.
    let mut b = paired_bridge_with_ack_capable_relay();
    let mut st = recording_state("pair-1");
    // An Idle session: the first tick sends a snapshot, then there is no delta.
    let app = app_with_tabs(vec![tab_state("t1", "fix-login", "claude")]);
    let cache = GitStatusCache::new();

    let views = vec![view("proj", &app, &cache)];
    let n = tick_recording(&mut b, &views, 1_000, &mut st);
    assert!(n > 0, "the first tick sends the snapshot");
    // The phone acks that snapshot and then everything goes quiet.
    ack_through(&mut b, st.pairing("pair-1").unwrap().last_sent_seq);

    let mut now = 2_000u64;
    for _ in 0..12 {
        // Twelve minutes of an idle desktop — twelve times the ack window.
        now += PEER_ACK_TIMEOUT_MS;
        let views = vec![view("proj", &app, &cache)];
        tick_recording(&mut b, &views, now, &mut st);
        assert_eq!(
            b.peer_liveness(),
            PeerLiveness::Live,
            "a phone with nothing outstanding must never be declared dark (at {now}ms)"
        );
    }
}

#[test]
fn a_phone_that_acks_then_stops_goes_dark_after_the_window() {
    // The degradation the user actually described: it works for a while, then
    // stops. Acks flow, then dry up while the feed keeps producing.
    let mut b = paired_bridge_with_ack_capable_relay();
    let mut st = recording_state("pair-1");
    let mut app = app_with_tabs(vec![tab_state("t1", "fix-login", "claude")]);
    set_status(&mut app, 0, InterpretedStatus::Working);
    let cache = GitStatusCache::new();

    // A healthy phone: every tick is acked. It never goes dark, however long.
    let mut now = 1_000u64;
    for _ in 0..90 {
        let views = vec![view("proj", &app, &cache)];
        tick_recording(&mut b, &views, now, &mut st);
        ack_through(&mut b, st.pairing("pair-1").unwrap().last_sent_seq);
        assert_eq!(b.peer_liveness(), PeerLiveness::Live);
        now += 1_000;
    }
    let healthy_sent = st.pairing("pair-1").unwrap().last_sent_seq;
    assert!(healthy_sent >= 90, "a healthy phone keeps being fed");

    // The acks stop (the phone suspends / its socket goes half-open). One ack
    // window later the desktop must notice on its own.
    let stopped_at = now;
    while now < stopped_at + PEER_ACK_TIMEOUT_MS {
        let views = vec![view("proj", &app, &cache)];
        tick_recording(&mut b, &views, now, &mut st);
        assert_eq!(
            b.peer_liveness(),
            PeerLiveness::Live,
            "inside the window the phone is not condemned yet"
        );
        now += 5_000;
    }
    let views = vec![view("proj", &app, &cache)];
    tick_recording(&mut b, &views, now + 1_000, &mut st);
    assert_eq!(b.peer_liveness(), PeerLiveness::Dark);
}

#[test]
fn a_phone_declared_dark_recovers_and_gets_a_fresh_snapshot_on_reattach() {
    // Recovery must not need a re-pair. When the phone comes back the relay
    // announces its presence on attach; that both revives the feed and re-arms a
    // full snapshot, because `status_update` deltas can only mutate sessions the
    // phone already knows (remote-control-e9l).
    let mut b = paired_bridge_with_ack_capable_relay();
    let mut st = recording_state("pair-1");
    let mut app = app_with_tabs(vec![tab_state("t1", "fix-login", "claude")]);
    set_status(&mut app, 0, InterpretedStatus::Working);
    let cache = GitStatusCache::new();

    {
        let views = vec![view("proj", &app, &cache)];
        tick_recording(&mut b, &views, 1_000, &mut st);
    }
    let mut now = 1_000 + PEER_ACK_TIMEOUT_MS + 1_000;
    {
        let views = vec![view("proj", &app, &cache)];
        tick_recording(&mut b, &views, now, &mut st);
    }
    assert_eq!(b.peer_liveness(), PeerLiveness::Dark);

    // The session set changes while the phone is dark, so a delta could never
    // repair its view.
    app = app_with_tabs(vec![tab_state("t2", "fix-login-v2", "claude")]);
    set_status(&mut app, 0, InterpretedStatus::Working);

    // The phone reattaches (superseding leg → `Connected`, no `Disconnected`).
    b.handle_inbound(RemoteInbound::Presence {
        pairing_id: PairingId::new("pair-1"),
        peer: Role::Phone,
        state: flightdeck_remote_protocol::PresenceState::Connected,
    });
    assert_eq!(
        b.peer_liveness(),
        PeerLiveness::Live,
        "a reattaching phone gets a fresh chance"
    );
    now += 1_000;
    let views = vec![view("proj", &app, &cache)];
    let mut sent = Vec::new();
    b.tick(&views, now, &mut |o| sent.push(o));
    let msgs: Vec<DesktopToPhone> = sent.iter().map(decode).collect();
    let snap = msgs
        .iter()
        .find_map(|m| match m {
            DesktopToPhone::Snapshot(s) => Some(s),
            _ => None,
        })
        .expect("a recovered phone must be resynchronised with a full snapshot");
    assert_eq!(snap.projects[0].sessions[0].name, "fix-login-v2");
}

#[test]
fn a_relay_that_never_forwards_peer_acks_leaves_the_guard_disarmed() {
    // COMPATIBILITY, and the reason the guard is armed by evidence rather than by
    // a build flag: the relay used to consume the phone's `ack` purely to trim its
    // own queue and never forwarded it, so `last_acked_by_peer` was structurally
    // pinned at 0 for every desktop in the field. Enforcing an ack deadline
    // against such a relay would darken EVERY phone within a minute. Until the
    // relay proves it forwards acks (by sending one — the post-`auth_ok` cursor
    // echo does it even when the cursor is 0), the bridge must behave exactly as
    // it did before.
    let mut b = paired_bridge(); // no PeerAck ever
    let mut st = recording_state("pair-1");
    let mut app = app_with_tabs(vec![tab_state("t1", "fix-login", "claude")]);
    set_status(&mut app, 0, InterpretedStatus::Working);
    let cache = GitStatusCache::new();

    let mut now = 1_000u64;
    for _ in 0..20 {
        now += PEER_ACK_TIMEOUT_MS;
        let views = vec![view("proj", &app, &cache)];
        assert!(
            tick_recording(&mut b, &views, now, &mut st) > 0,
            "an un-upgraded relay must not change desktop behaviour"
        );
        assert_eq!(b.peer_liveness(), PeerLiveness::Live);
    }
}

#[test]
fn an_unacked_backlog_past_the_bound_darkens_the_peer_before_the_timeout() {
    // The cheap guard the issue asks for regardless: never let the outbound
    // stream run thousands of envelopes ahead of the peer's ack cursor. This
    // catches a phone that reattaches often enough to keep resetting the ack
    // window but never actually receives anything — the flapping-iOS shape.
    let mut b = paired_bridge_with_ack_capable_relay();
    let mut st = recording_state("pair-1");
    let mut app = app_with_tabs(vec![tab_state("t1", "fix-login", "claude")]);
    set_status(&mut app, 0, InterpretedStatus::Working);
    let cache = GitStatusCache::new();

    let mut now = 1_000u64;
    let mut dark_at = None;
    for _ in 0..(MAX_UNACKED_ENVELOPES + 50) {
        let views = vec![view("proj", &app, &cache)];
        tick_recording(&mut b, &views, now, &mut st);
        if b.peer_liveness() == PeerLiveness::Dark {
            dark_at = Some(now);
            break;
        }
        // The phone keeps reattaching, so the CLOCK never condemns it: every
        // `Connected` restarts the ack window (and re-arms the snapshot, which is
        // remote-control-e9l's fix and must survive).
        b.handle_inbound(RemoteInbound::Presence {
            pairing_id: PairingId::new("pair-1"),
            peer: Role::Phone,
            state: flightdeck_remote_protocol::PresenceState::Connected,
        });
        now += 10; // far inside the ack window
    }
    let dark_at = dark_at.expect("the backlog bound must condemn a phone that never receives");
    assert!(
        dark_at - 1_000 < PEER_ACK_TIMEOUT_MS,
        "this must trip on the backlog bound, not on the clock (at {dark_at}ms)"
    );
    let sent = st.pairing("pair-1").unwrap().last_sent_seq;
    assert!(
        sent <= MAX_UNACKED_ENVELOPES + 10,
        "the stream must be stopped near the bound, not thousands past it (got {sent})"
    );
}

#[test]
fn a_relay_queue_overflow_advisory_darkens_the_peer_with_no_acks_at_all() {
    // `rate_limited`/queue-overflow is the relay saying it is SHEDDING our
    // un-acked envelopes because the peer has not drained ~1000 of them. Every
    // already-deployed relay sends it, and the desktop used to swallow it as a
    // non-fatal advisory and keep sealing. It is proof of a peer that is not
    // consuming, so it applies even with the ack guard disarmed.
    let mut b = paired_bridge(); // deliberately NOT ack-armed
    let mut st = recording_state("pair-1");
    let mut app = app_with_tabs(vec![tab_state("t1", "fix-login", "claude")]);
    set_status(&mut app, 0, InterpretedStatus::Working);
    let cache = GitStatusCache::new();
    {
        let views = vec![view("proj", &app, &cache)];
        assert!(tick_recording(&mut b, &views, 1_000, &mut st) > 0);
    }

    b.handle_inbound(RemoteInbound::PeerBacklog {
        pairing_id: PairingId::new("pair-1"),
    });
    assert_eq!(b.peer_liveness(), PeerLiveness::Dark);
    let views = vec![view("proj", &app, &cache)];
    assert_eq!(
        tick_recording(&mut b, &views, 2_000, &mut st),
        0,
        "stop feeding a queue the relay is already shedding"
    );

    // An advisory for a pairing we are not feeding must not touch the live one.
    let mut b2 = paired_bridge();
    b2.handle_inbound(RemoteInbound::PeerBacklog {
        pairing_id: PairingId::new("pair-other"),
    });
    assert_eq!(b2.peer_liveness(), PeerLiveness::Live);
}

#[test]
fn the_relays_own_rejections_are_not_blamed_on_the_phone_but_only_so_often() {
    // "No acks" has two causes and they are NOT the same failure: the phone is
    // gone (remote-control-5qu), or the relay is rejecting our envelopes so the
    // phone never sees them (remote-control-zv3, whose realign path is live in
    // this same code). A realign is proof of the second, so it rebases the ack
    // tracking onto the renumbered stream and grants a fresh window — otherwise
    // the recovery snapshot zv3 arms would be blocked by this very guard and a
    // recoverable stream would wedge again.
    //
    // Bounded, though: a stream that only ever realigns and never collects an ack
    // is indistinguishable from a dead peer, so after MAX_REALIGN_CREDITS the
    // deadline is allowed to stand.
    let mut b = paired_bridge_with_ack_capable_relay();
    let mut st = recording_state("pair-1");
    let mut app = app_with_tabs(vec![tab_state("t1", "fix-login", "claude")]);
    set_status(&mut app, 0, InterpretedStatus::Working);
    let cache = GitStatusCache::new();

    let mut now = 1_000u64;
    for credit in 0..MAX_REALIGN_CREDITS {
        // Run out the ack window with the relay eating everything.
        for _ in 0..3 {
            let views = vec![view("proj", &app, &cache)];
            tick_recording(&mut b, &views, now, &mut st);
            now += PEER_ACK_TIMEOUT_MS / 2;
        }
        b.handle_inbound(RemoteInbound::SeqRealign {
            pairing_id: PairingId::new("pair-1"),
            next_seq: 98 + credit as u64,
        });
        assert_eq!(
            b.peer_liveness(),
            PeerLiveness::Live,
            "a relay-side rejection is not the phone's fault (credit {credit})"
        );
        // The realigned stream is fed again, which is what lets zv3 recover.
        let views = vec![view("proj", &app, &cache)];
        assert!(
            tick_recording(&mut b, &views, now, &mut st) > 0,
            "a realigned stream must be allowed to send its recovery snapshot"
        );
    }

    // Credits exhausted: the same silence now condemns the peer.
    for _ in 0..3 {
        now += PEER_ACK_TIMEOUT_MS;
        let views = vec![view("proj", &app, &cache)];
        tick_recording(&mut b, &views, now, &mut st);
    }
    b.handle_inbound(RemoteInbound::SeqRealign {
        pairing_id: PairingId::new("pair-1"),
        next_seq: 500,
    });
    let views = vec![view("proj", &app, &cache)];
    tick_recording(&mut b, &views, now + PEER_ACK_TIMEOUT_MS, &mut st);
    assert_eq!(
        b.peer_liveness(),
        PeerLiveness::Dark,
        "an endlessly-realigning stream with no acks must still end up dark"
    );

    // A genuine ack restores the credits (a healthy stream realigns at most once).
    ack_through(&mut b, st.pairing("pair-1").unwrap().last_sent_seq);
    assert_eq!(b.peer_liveness(), PeerLiveness::Live);
}

#[test]
fn an_inbound_envelope_is_evidence_the_phone_is_there() {
    // A phone that is receiving but whose acks are lost (or whose relay does not
    // forward them promptly) still talks to us: any inbound envelope revives the
    // feed, without a presence frame. This is also what keeps the ungated
    // event/transcript traffic working as a free liveness probe.
    let mut b = paired_bridge_with_ack_capable_relay();
    let mut st = recording_state("pair-1");
    let mut app = app_with_tabs(vec![tab_state("t1", "fix-login", "claude")]);
    set_status(&mut app, 0, InterpretedStatus::Working);
    let cache = GitStatusCache::new();
    {
        let views = vec![view("proj", &app, &cache)];
        tick_recording(&mut b, &views, 1_000, &mut st);
        tick_recording(&mut b, &views, 1_000 + PEER_ACK_TIMEOUT_MS + 1, &mut st);
    }
    assert_eq!(b.peer_liveness(), PeerLiveness::Dark);

    let cmd = PhoneCommand {
        command_id: CommandId::new("c1"),
        issued_at_ms: 1,
        body: CommandBody::RequestSnapshot { project_id: None },
    };
    b.handle_inbound(RemoteInbound::Envelope(EncryptedEnvelope {
        pairing_id: PairingId::new("pair-1"),
        seq: 1,
        sender: Role::Phone,
        sent_at_ms: 1,
        nonce: String::new(),
        ciphertext: STANDARD.encode(serde_json::to_vec(&cmd).unwrap()),
    }));
    assert_eq!(
        b.peer_liveness(),
        PeerLiveness::Live,
        "the phone just spoke to us; it is plainly there"
    );
}

#[test]
fn a_restart_does_not_blame_the_phone_for_the_previous_sessions_backlog() {
    // At startup `install_channel` floors `out_seq` to the persisted
    // `last_sent_seq` (remote-control-bbf). Those envelopes were sent by a
    // previous process and may well have been acked before the restart, so they
    // must not be counted as outstanding — otherwise a healthy phone is declared
    // dark one ack-window after every launch, with nothing sent in between.
    let mut b = RemoteBridge::passthrough(0);
    b.handle_inbound(RemoteInbound::Paired {
        pairing_id: PairingId::new("pair-1"),
        peer_device_id: None,
    });
    b.install_channel(passthrough_seal(), passthrough_open(), 23_943);
    b.handle_inbound(RemoteInbound::Presence {
        pairing_id: PairingId::new("pair-1"),
        peer: Role::Phone,
        state: flightdeck_remote_protocol::PresenceState::Connected,
    });
    // The relay's post-auth echo: the phone has confirmed nothing it can prove.
    b.handle_inbound(RemoteInbound::PeerAck {
        pairing_id: PairingId::new("pair-1"),
        cursor: 0,
    });

    let mut st = recording_state("pair-1");
    let app = app_with_tabs(vec![tab_state("t1", "fix-login", "claude")]);
    let cache = GitStatusCache::new();
    let views = vec![view("proj", &app, &cache)];
    // The phone receives and acks the reconnect snapshot, then the desktop goes
    // idle: nothing more is sent, so nothing is owed. The inherited 23,943 must
    // not condemn the phone.
    let mut top = 0;
    for now in [1_000u64, 2_000] {
        b.tick(&views, now, &mut |o| {
            if let RemoteOutbound::SendEnvelope { seq, .. } = &o {
                top = top.max(*seq);
            }
        });
    }
    assert!(
        top > 23_943,
        "the outbound stream continues above the resumed mark"
    );
    ack_through(&mut b, top);
    for i in 1..6 {
        let now = 2_000 + i * PEER_ACK_TIMEOUT_MS;
        tick_recording(&mut b, &views, now, &mut st);
        assert_eq!(
            b.peer_liveness(),
            PeerLiveness::Live,
            "a resumed high-water mark is not an un-acked backlog (at {now}ms)"
        );
    }
}
