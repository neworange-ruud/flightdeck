use super::*;
use crate::contracts::{Config, InterpretedStatus, ProjectState as CoreProjectState, TabState};
use crate::contracts::{PtySize, STATE_VERSION};
use crate::testing::FakePty;
use crate::tui::render::GitStatusCache;

use flightdeck_remote_protocol::relay::EncryptedEnvelope;
use flightdeck_remote_protocol::{
    CommandBody, CommandId, DesktopToPhone, PhoneCommand, Role, TranscriptItem,
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
    // Transition to needs-input.
    set_status(&mut app, 0, InterpretedStatus::WaitingForInput);
    let views = vec![view("proj", &app, &cache)];
    let msgs = collect(&mut b, &views, 2_000);
    let update = msgs
        .iter()
        .find_map(|m| match m {
            DesktopToPhone::StatusUpdate(u) => Some(u),
            _ => None,
        })
        .expect("status update");
    let d = &update.updates[0];
    assert_eq!(d.status, AgentStatus::NeedsInput);
    assert!(d
        .pending_question
        .as_deref()
        .unwrap()
        .contains("installer script"));
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
