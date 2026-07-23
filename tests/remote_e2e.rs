//! FlightDeck Remote — Tier A protocol end-to-end suite (issue c3m.7).
//!
//! This is the CI gate for the remote protocol. It stands up the **real** relay
//! binary, the **real** desktop TUI under a PTY (running against a generated
//! fixture repo whose one agent slot is the deterministic `fake-agent.sh` stub),
//! and a **real** Rust phone driver that speaks the full §5 handshake + E2E
//! sealing. It then exercises every remote capability and asserts both the
//! sealed protocol reply AND the real side effect on disk / in live desktop
//! state.
//!
//! The shared harness pieces (`support::relay`, `support::desktop`,
//! `support::phone`) are reused verbatim — this file only wires them together
//! and drives the capabilities. See `tests/e2e/support/*` for the building
//! blocks and their own module tests.

// The Tier A E2E suite stands up the real desktop against a bash-built fixture
// repo (scripts/e2e/make-fixture-project.sh + fake-agent.sh) driven under a PTY.
// GitHub's windows-latest runners have no bash/WSL, so the whole suite — and the
// support module it pulls in — is Unix-only; ubuntu + macos provide the coverage
// (the relay itself is Linux-deployed, exercised further by the Relay workflow).
#![cfg(not(windows))]

#[path = "e2e/support/mod.rs"]
mod support;

use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant};

use flightdeck_remote_protocol::{
    AgentStatus, AgentType, CommandAck, CommandBody, CommandId, CommandOutcome, DesktopToPhone,
    PermissionChoice, ProjectId, PromptId, ShellEventKind, ShellId, StateSnapshot, TranscriptItem,
};

use support::desktop::{make_fixture, DesktopHandle};
use support::phone::PhoneDriver;
use support::relay::RelayHandle;
use tempfile::TempDir;

/// The autopair claim code baked into the harness (desktop offers it,
/// phone claims it — also the E2E HKDF salt).
const CLAIM_TOKEN: &str = "4729";

/// How long we wait for the desktop to boot and advance its pairing overlay to
/// display the claim code. First-run global-config seeding + first relay
/// connect all happen before the code renders, so this is generous.
const PAIRING_TIMEOUT: Duration = Duration::from_secs(45);

/// Budget for a single command's ack to come back sealed over the feed.
const ACK_TIMEOUT: Duration = Duration::from_secs(20);

/// Budget for a real, slower side effect to become observable (a worktree to be
/// created / removed, an agent to launch and transition status, a shell to
/// echo). Real processes + git + PTYs ⇒ generous.
const EFFECT_TIMEOUT: Duration = Duration::from_secs(45);

/// Filesystem / snapshot poll granularity.
const POLL: Duration = Duration::from_millis(200);

// ===========================================================================
// Smoke test (kept from the bootstrap harness): the relay binary alone.
// ===========================================================================

/// The real relay binary builds, boots, answers `/healthz`, and is killed
/// cleanly on drop (no leaked process, no leaked port).
#[test]
fn relay_boots_and_healthz_ok() {
    let relay = RelayHandle::spawn();

    assert!(relay.port() > 0, "relay should be bound to a real port");
    assert_eq!(
        relay.ws_url(),
        format!("ws://127.0.0.1:{}/ws", relay.port())
    );
    assert_eq!(
        relay.http_base(),
        format!("http://127.0.0.1:{}", relay.port())
    );
    assert!(relay.healthz_ok(), "relay should still answer /healthz ok");
    drop(relay);
}

// ===========================================================================
// Harness: pair once, hold everything alive for the whole test.
// ===========================================================================

/// A fully paired harness: real relay + real desktop-in-PTY + paired phone
/// driver, plus the fixture repo path and the temp dirs that must stay alive.
struct Harness {
    /// Held only for its kill-on-drop lifetime; the phone talks to it by URL.
    _relay: RelayHandle,
    /// Held only for its kill-on-drop lifetime (the real desktop under a PTY).
    _desktop: DesktopHandle,
    phone: PhoneDriver,
    fixture: PathBuf,
    /// Owns the fixture repo directory; dropped last.
    _fixture_dir: TempDir,
}

impl Harness {
    /// Stand up relay → fixture → desktop, wait for the pairing code to render,
    /// then pair the phone. Panics with a descriptive message on any failure.
    fn boot() -> Self {
        let relay = RelayHandle::spawn();
        let (fixture, fixture_dir) = make_fixture(relay.port());
        let mut desktop = DesktopHandle::spawn(&fixture);

        // The desktop autopair overlay must advance to *displaying* the code
        // (not merely "Offering"), which proves the pairing offer was accepted
        // by the relay and the desktop is ready to be claimed.
        let saw_code = desktop.wait_for_output(CLAIM_TOKEN, PAIRING_TIMEOUT);
        assert!(
            saw_code,
            "desktop never displayed the pairing code {CLAIM_TOKEN} within {PAIRING_TIMEOUT:?}; \
             still running = {}; PTY output so far:\n{}",
            desktop.is_running(),
            desktop.output_snapshot()
        );

        let phone = PhoneDriver::pair(&relay.ws_url(), CLAIM_TOKEN);

        Harness {
            _relay: relay,
            _desktop: desktop,
            phone,
            fixture,
            _fixture_dir: fixture_dir,
        }
    }

    /// The fixture repo's `.flightdeck/worktrees` root.
    fn worktrees_root(&self) -> PathBuf {
        self.fixture.join(".flightdeck").join("worktrees")
    }

    /// Path to a named session's worktree on disk.
    fn worktree(&self, name: &str) -> PathBuf {
        self.worktrees_root().join(name)
    }

    /// The desktop's sandboxed `$HOME`. Agent session files (which the desktop
    /// tails to reconstruct the phone transcript, remote-control-72k) resolve
    /// under here — `~/.claude/projects/<mangled worktree>/…`.
    fn home(&self) -> &Path {
        self._desktop.home()
    }
}

/// Claude Code's on-disk session directory for a worktree under `home`: the
/// absolute worktree path with every `/`, `\` and `.` folded to `-`, under
/// `~/.claude/projects/`. Mirrors `agents::resume::claude_project_dir` so the
/// test writes a session file exactly where the desktop's reconstruction looks
/// for it.
fn claude_project_dir(home: &Path, worktree: &Path) -> PathBuf {
    let mangled: String = worktree
        .to_string_lossy()
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | '.') {
                '-'
            } else {
                c
            }
        })
        .collect();
    home.join(".claude").join("projects").join(mangled)
}

// ===========================================================================
// Small protocol / filesystem helpers.
// ===========================================================================

/// Await the sealed [`CommandAck`] for `command_id`, discarding any interleaved
/// feed messages. Panics (descriptively) on timeout.
fn await_ack(phone: &mut PhoneDriver, command_id: &CommandId, timeout: Duration) -> CommandAck {
    let msg = phone.recv_until(
        timeout,
        |m| matches!(m, DesktopToPhone::CommandAck(a) if &a.command_id == command_id),
    );
    match msg {
        DesktopToPhone::CommandAck(a) => a,
        other => unreachable!("recv_until returned a non-ack: {other:?}"),
    }
}

/// Ask for a fresh snapshot and return the next [`StateSnapshot`] the desktop
/// pushes. Panics on timeout.
fn request_snapshot(phone: &mut PhoneDriver, timeout: Duration) -> StateSnapshot {
    phone.command(CommandBody::RequestSnapshot { project_id: None });
    let msg = phone.recv_until(timeout, |m| matches!(m, DesktopToPhone::Snapshot(_)));
    match msg {
        DesktopToPhone::Snapshot(s) => s,
        other => unreachable!("recv_until returned a non-snapshot: {other:?}"),
    }
}

/// Find a session by name across all projects in a snapshot.
fn find_session<'a>(
    snap: &'a StateSnapshot,
    name: &str,
) -> Option<&'a flightdeck_remote_protocol::SessionState> {
    snap.projects
        .iter()
        .flat_map(|p| p.sessions.iter())
        .find(|s| s.name == name)
}

/// The (only) project id in the fixture snapshot.
fn only_project_id(snap: &StateSnapshot) -> ProjectId {
    assert_eq!(
        snap.projects.len(),
        1,
        "fixture is expected to have exactly one project; snapshot: {snap:?}"
    );
    snap.projects[0].project_id.clone()
}

/// Poll fresh snapshots until `pred(session)` holds for the named session, or
/// `timeout` elapses. Returns the matching session. Panics with the last
/// snapshot on timeout.
fn wait_for_session(
    phone: &mut PhoneDriver,
    name: &str,
    timeout: Duration,
    pred: impl Fn(&flightdeck_remote_protocol::SessionState) -> bool,
    what: &str,
) -> flightdeck_remote_protocol::SessionState {
    let deadline = Instant::now() + timeout;
    loop {
        let snap = request_snapshot(phone, ACK_TIMEOUT);
        if let Some(s) = find_session(&snap, name) {
            if pred(s) {
                return s.clone();
            }
        }
        if Instant::now() >= deadline {
            panic!(
                "session {name:?} did not satisfy [{what}] within {timeout:?}; \
                 last snapshot: {snap:?}"
            );
        }
        sleep(POLL);
    }
}

/// Poll until the named session is *absent* from a fresh snapshot.
fn wait_for_session_gone(phone: &mut PhoneDriver, name: &str, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        let snap = request_snapshot(phone, ACK_TIMEOUT);
        if find_session(&snap, name).is_none() {
            return;
        }
        if Instant::now() >= deadline {
            panic!("session {name:?} was still present after {timeout:?}; snapshot: {snap:?}");
        }
        sleep(POLL);
    }
}

/// Poll a path until it exists (or vanishes, if `want_present == false`).
fn wait_for_path(path: &Path, want_present: bool, timeout: Duration, what: &str) {
    let deadline = Instant::now() + timeout;
    loop {
        if path.exists() == want_present {
            return;
        }
        if Instant::now() >= deadline {
            panic!(
                "path {} did not become {} within {timeout:?} [{what}]",
                path.display(),
                if want_present { "present" } else { "absent" }
            );
        }
        sleep(POLL);
    }
}

/// Poll a file until it contains `needle`. Returns the file contents. Missing
/// file is treated as "not yet".
fn wait_for_file_contains(path: &Path, needle: &str, timeout: Duration) -> String {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(contents) = std::fs::read_to_string(path) {
            if contents.contains(needle) {
                return contents;
            }
        }
        if Instant::now() >= deadline {
            let existing = std::fs::read_to_string(path).unwrap_or_else(|_| "<missing>".into());
            panic!(
                "file {} never contained {needle:?} within {timeout:?}; contents:\n{existing}",
                path.display()
            );
        }
        sleep(POLL);
    }
}

// ===========================================================================
// The one comprehensive capability flow.
// ===========================================================================

/// Pair once, then drive every remote capability in a stateful, sensible order,
/// asserting the sealed protocol reply AND the real side effect for each.
#[test]
fn remote_capabilities_end_to_end() {
    let mut h = Harness::boot();

    // -------------------------------------------------------------------
    // snapshot: the initial snapshot arrives right after pairing.
    // -------------------------------------------------------------------
    let initial = {
        let msg = h
            .phone
            .recv_until(ACK_TIMEOUT, |m| matches!(m, DesktopToPhone::Snapshot(_)));
        match msg {
            DesktopToPhone::Snapshot(s) => s,
            other => unreachable!("expected initial snapshot, got {other:?}"),
        }
    };
    let project_id = only_project_id(&initial);
    // Fresh fixture: the project has no sessions yet.
    assert!(
        initial.projects[0].sessions.is_empty(),
        "fresh fixture should start with no sessions; got {:?}",
        initial.projects[0].sessions
    );

    // -------------------------------------------------------------------
    // request_snapshot: yields a fresh snapshot with the project present.
    // -------------------------------------------------------------------
    let refreshed = request_snapshot(&mut h.phone, ACK_TIMEOUT);
    assert_eq!(
        only_project_id(&refreshed),
        project_id,
        "request_snapshot should return the same project"
    );

    // -------------------------------------------------------------------
    // new_agent (session A): a worktree appears on disk, the fake agent runs
    // (status → working), and the session shows up in the snapshot.
    // -------------------------------------------------------------------
    const SESSION_A: &str = "remote-alpha";
    let new_a = h.phone.command(CommandBody::NewAgent {
        project_id: project_id.clone(),
        agent_type: AgentType::ClaudeCode,
        name: SESSION_A.to_string(),
        base_branch: "main".to_string(),
        first_task: String::new(),
    });
    let ack = await_ack(&mut h.phone, &new_a, ACK_TIMEOUT);
    assert_eq!(
        ack.outcome,
        CommandOutcome::Accepted,
        "new_agent should be accepted (worktree creation is async); message: {:?}",
        ack.message
    );

    // Worktree directory appears under .flightdeck/worktrees/<name>.
    wait_for_path(
        &h.worktree(SESSION_A),
        true,
        EFFECT_TIMEOUT,
        "session A worktree created",
    );
    // The fake agent launches in the worktree and writes its status file.
    let status_a = h
        .worktree(SESSION_A)
        .join(".flightdeck")
        .join("agent-status");
    wait_for_file_contains(&status_a, "working", EFFECT_TIMEOUT);
    // And the session shows up in the snapshot.
    let sess_a = wait_for_session(
        &mut h.phone,
        SESSION_A,
        EFFECT_TIMEOUT,
        |s| matches!(s.status, AgentStatus::Working | AgentStatus::Idle),
        "session A present and working/idle",
    );
    let session_a_id = sess_a.session_id.clone();

    // -------------------------------------------------------------------
    // reply: the text lands in the worktree's agent-replies.log and the fake
    // agent transitions working → idle after consuming it.
    // -------------------------------------------------------------------
    const REPLY_TEXT: &str = "hello-from-phone-e2e";
    let reply_cmd = h.phone.command(CommandBody::Reply {
        session_id: session_a_id.clone(),
        text: REPLY_TEXT.to_string(),
    });
    let ack = await_ack(&mut h.phone, &reply_cmd, ACK_TIMEOUT);
    assert_eq!(
        ack.outcome,
        CommandOutcome::Applied,
        "reply should be applied; message: {:?}",
        ack.message
    );
    let replies_log = h
        .worktree(SESSION_A)
        .join(".flightdeck")
        .join("agent-replies.log");
    let log = wait_for_file_contains(&replies_log, REPLY_TEXT, EFFECT_TIMEOUT);
    assert!(
        log.contains(REPLY_TEXT),
        "agent-replies.log should contain the reply text"
    );
    // The agent emits `idle` after handling the reply line.
    wait_for_file_contains(&status_a, "idle", EFFECT_TIMEOUT);
    wait_for_session(
        &mut h.phone,
        SESSION_A,
        EFFECT_TIMEOUT,
        |s| matches!(s.status, AgentStatus::Idle),
        "session A idle after reply",
    );

    // -------------------------------------------------------------------
    // permission_decision: with no pending permission prompt (the agent is
    // idle, not asking), the desktop honestly rejects the decision rather
    // than typing a keystroke into the wrong place.
    // -------------------------------------------------------------------
    let perm_cmd = h.phone.command(CommandBody::PermissionDecision {
        session_id: session_a_id.clone(),
        prompt_id: PromptId::new("no-such-prompt"),
        choice: Some(PermissionChoice::AllowOnce),
        option_index: None,
        option_indices: None,
        free_text: None,
        answers: None,
    });
    let ack = await_ack(&mut h.phone, &perm_cmd, ACK_TIMEOUT);
    assert_eq!(
        ack.outcome,
        CommandOutcome::Rejected,
        "permission_decision with no pending prompt should be rejected; message: {:?}",
        ack.message
    );
    assert!(
        ack.message
            .as_deref()
            .is_some_and(|m| m.contains("pending permission prompt")),
        "reject reason should mention the missing pending prompt; got {:?}",
        ack.message
    );

    // -------------------------------------------------------------------
    // set_manual_status: the session flips to the cyan manual override.
    // -------------------------------------------------------------------
    let set_manual = h.phone.command(CommandBody::SetManualStatus {
        session_id: session_a_id.clone(),
        label: "blocked".to_string(),
    });
    let ack = await_ack(&mut h.phone, &set_manual, ACK_TIMEOUT);
    assert_eq!(
        ack.outcome,
        CommandOutcome::Applied,
        "set_manual_status should apply; message: {:?}",
        ack.message
    );
    let manual = wait_for_session(
        &mut h.phone,
        SESSION_A,
        EFFECT_TIMEOUT,
        |s| matches!(s.status, AgentStatus::Manual { .. }),
        "session A shows a manual override",
    );
    match &manual.status {
        AgentStatus::Manual { label } => assert!(
            !label.is_empty(),
            "manual override should carry a non-empty label"
        ),
        other => panic!("expected manual status, got {other:?}"),
    }

    // -------------------------------------------------------------------
    // clear_manual_status: the override is dropped; the session returns to
    // its real (idle) status.
    // -------------------------------------------------------------------
    let clear_manual = h.phone.command(CommandBody::ClearManualStatus {
        session_id: session_a_id.clone(),
    });
    let ack = await_ack(&mut h.phone, &clear_manual, ACK_TIMEOUT);
    assert_eq!(
        ack.outcome,
        CommandOutcome::Applied,
        "clear_manual_status should apply; message: {:?}",
        ack.message
    );
    wait_for_session(
        &mut h.phone,
        SESSION_A,
        EFFECT_TIMEOUT,
        |s| !matches!(s.status, AgentStatus::Manual { .. }),
        "session A manual override cleared",
    );

    // -------------------------------------------------------------------
    // transcript reconstruction (remote-control-72k): the desktop tails the
    // agent's own session file (not the raw PTY, which for a cursor-addressed
    // full-screen agent carries no newline-terminated prose) and pushes the
    // reconstructed items to the phone as TranscriptAppend. The fake agent is a
    // deterministic stub that does not write a real session log, so we drop a
    // Claude-format session file exactly where the reconstruction looks for it
    // (~/.claude/projects/<mangled worktree>/<uuid>.jsonl under the desktop's
    // sandboxed HOME) and assert its agent prose reaches the phone.
    // -------------------------------------------------------------------
    const AGENT_MARKER: &str = "agent-reply-marker-e2e-7714";
    let session_dir = claude_project_dir(h.home(), &h.worktree(SESSION_A));
    std::fs::create_dir_all(&session_dir).expect("create Claude session dir under desktop HOME");
    let session_jsonl = session_dir.join("11111111-1111-1111-1111-111111111111.jsonl");
    let user_rec = r#"{"type":"user","uuid":"e2e-u1","message":{"content":"Give me an overview"}}"#;
    let agent_rec = format!(
        r#"{{"type":"assistant","uuid":"e2e-a1","message":{{"content":[{{"type":"text","text":"{AGENT_MARKER} — here is the overview."}}]}}}}"#
    );
    std::fs::write(&session_jsonl, format!("{user_rec}\n{agent_rec}\n"))
        .expect("write Claude session JSONL");

    // The desktop reconstructs each tick and proactively flushes new items as
    // TranscriptAppend — assert the agent's prose (identified by the marker)
    // reaches the phone as an AgentMessage for session A.
    let appended = h.phone.recv_until(EFFECT_TIMEOUT, |m| {
        matches!(m, DesktopToPhone::TranscriptAppend(f)
            if f.session_id == session_a_id
            && f.items.iter().any(|it| matches!(it,
                TranscriptItem::AgentMessage { text, .. } if text.contains(AGENT_MARKER))))
    });
    match appended {
        DesktopToPhone::TranscriptAppend(feed) => {
            assert_eq!(feed.session_id, session_a_id, "append for session A");
            assert!(
                !feed.replace,
                "an incremental TranscriptAppend has replace = false"
            );
            assert!(
                feed.items.iter().any(|it| matches!(it,
                    TranscriptItem::AgentMessage { text, .. } if text.contains(AGENT_MARKER))),
                "the append carries the agent's reconstructed prose; feed: {feed:?}"
            );
        }
        other => unreachable!("recv_until returned a non-append: {other:?}"),
    }

    // A subsequent request_transcript returns a full (replace = true) load that
    // now includes the same agent prose.
    h.phone.command(CommandBody::RequestTranscript {
        session_id: session_a_id.clone(),
        from_index: None,
    });
    let transcript = h.phone.recv_until(
        EFFECT_TIMEOUT,
        |m| matches!(m, DesktopToPhone::Transcript(t) if t.session_id == session_a_id),
    );
    match transcript {
        DesktopToPhone::Transcript(feed) => {
            assert_eq!(feed.session_id, session_a_id, "transcript for session A");
            assert!(feed.replace, "a full transcript load sets replace = true");
            assert!(
                feed.items.iter().any(|it| matches!(it,
                    TranscriptItem::AgentMessage { text, .. } if text.contains(AGENT_MARKER))),
                "the full load includes the reconstructed agent prose; feed: {feed:?}"
            );
        }
        other => unreachable!("recv_until returned a non-transcript: {other:?}"),
    }

    // -------------------------------------------------------------------
    // git status detail: alongside a full snapshot the desktop pushes each
    // session's git detail. Assert we get a GitStatus for session A with a
    // real branch — the git-detail plane works.
    // -------------------------------------------------------------------
    h.phone
        .command(CommandBody::RequestSnapshot { project_id: None });
    let git_status = h.phone.recv_until(
        EFFECT_TIMEOUT,
        |m| matches!(m, DesktopToPhone::GitStatus(d) if d.session_id == session_a_id),
    );
    match git_status {
        DesktopToPhone::GitStatus(detail) => {
            assert_eq!(detail.session_id, session_a_id);
            assert!(
                detail.branch.is_some(),
                "session A worktree should have a checked-out branch; detail: {detail:?}"
            );
        }
        other => unreachable!("recv_until returned a non-git-status: {other:?}"),
    }

    // -------------------------------------------------------------------
    // shell: open a shell, run `echo`, see the marker in ShellOutput, then
    // interrupt and close it. Assert the lifecycle frames + the output.
    // -------------------------------------------------------------------
    const SHELL_MARKER: &str = "e2e-shell-marker-9271";
    let shell_id = ShellId::new("shell-e2e-1");
    let shell_open = h.phone.command(CommandBody::ShellOpen {
        session_id: session_a_id.clone(),
        shell_id: shell_id.clone(),
        cols: 100,
        rows: 30,
    });
    let ack = await_ack(&mut h.phone, &shell_open, ACK_TIMEOUT);
    assert_eq!(
        ack.outcome,
        CommandOutcome::Applied,
        "shell_open should apply; message: {:?}",
        ack.message
    );
    // The Opened lifecycle event carries the geometry we asked for.
    let opened = h.phone.recv_until(EFFECT_TIMEOUT, |m| {
        matches!(m, DesktopToPhone::ShellEvent(e)
            if e.shell_id == shell_id && matches!(e.kind, ShellEventKind::Opened { .. }))
    });
    match opened {
        DesktopToPhone::ShellEvent(e) => match e.kind {
            ShellEventKind::Opened { cols, rows } => {
                assert_eq!((cols, rows), (100, 30), "shell opened with our geometry");
            }
            other => unreachable!("expected Opened, got {other:?}"),
        },
        other => unreachable!("expected a ShellEvent, got {other:?}"),
    }
    // Run a command; its echo/output carries the marker.
    let shell_input = h.phone.command(CommandBody::ShellInput {
        session_id: session_a_id.clone(),
        shell_id: shell_id.clone(),
        data: format!("echo {SHELL_MARKER}\r"),
    });
    let ack = await_ack(&mut h.phone, &shell_input, ACK_TIMEOUT);
    assert_eq!(
        ack.outcome,
        CommandOutcome::Applied,
        "shell_input should apply; message: {:?}",
        ack.message
    );
    let output = h.phone.recv_until(EFFECT_TIMEOUT, |m| {
        matches!(m, DesktopToPhone::ShellOutput(o)
            if o.shell_id == shell_id && o.data.contains(SHELL_MARKER))
    });
    match output {
        DesktopToPhone::ShellOutput(o) => {
            assert!(o.data.contains(SHELL_MARKER), "shell output carries marker");
            assert!(o.seq >= 1, "shell output seq is monotonic from 1");
        }
        other => unreachable!("expected ShellOutput, got {other:?}"),
    }
    // Interrupt (Ctrl-C) the foreground line.
    let shell_int = h.phone.command(CommandBody::ShellInterrupt {
        session_id: session_a_id.clone(),
        shell_id: shell_id.clone(),
    });
    let ack = await_ack(&mut h.phone, &shell_int, ACK_TIMEOUT);
    assert!(
        matches!(ack.outcome, CommandOutcome::Applied),
        "shell_interrupt should apply; message: {:?}",
        ack.message
    );
    // Close the shell; expect a Closed (or Exited) lifecycle event.
    let shell_close = h.phone.command(CommandBody::ShellClose {
        session_id: session_a_id.clone(),
        shell_id: shell_id.clone(),
    });
    let ack = await_ack(&mut h.phone, &shell_close, ACK_TIMEOUT);
    assert_eq!(
        ack.outcome,
        CommandOutcome::Applied,
        "shell_close should apply; message: {:?}",
        ack.message
    );
    let closed = h.phone.recv_until(EFFECT_TIMEOUT, |m| {
        matches!(m, DesktopToPhone::ShellEvent(e)
            if e.shell_id == shell_id
            && matches!(e.kind, ShellEventKind::Closed | ShellEventKind::Exited { .. }))
    });
    assert!(
        matches!(closed, DesktopToPhone::ShellEvent(_)),
        "expected a Closed/Exited shell event, got {closed:?}"
    );

    // -------------------------------------------------------------------
    // restart_agent: the primary agent is restarted in place (fresh process,
    // same worktree).
    // -------------------------------------------------------------------
    let restart = h.phone.command(CommandBody::RestartAgent {
        session_id: session_a_id.clone(),
    });
    let ack = await_ack(&mut h.phone, &restart, ACK_TIMEOUT);
    assert_eq!(
        ack.outcome,
        CommandOutcome::Applied,
        "restart_agent should apply; message: {:?}",
        ack.message
    );

    // -------------------------------------------------------------------
    // git_pull_base: the fixture base folder is dirty only with untracked files
    // (.gitignore / config.toml / state.json), which do not block a rebase, so
    // nothing is stashed. The fixture has no configured upstream, so the
    // underlying `git pull --rebase` cannot proceed and is honestly refused
    // (rather than raising a hard error — no rebase is left mid-flight to abort).
    // -------------------------------------------------------------------
    let pull = h.phone.command(CommandBody::GitPullBase {
        session_id: session_a_id.clone(),
    });
    let ack = await_ack(&mut h.phone, &pull, ACK_TIMEOUT);
    assert_eq!(
        ack.outcome,
        CommandOutcome::Rejected,
        "git_pull_base should be rejected when the base has no upstream to pull; message: {:?}",
        ack.message
    );
    assert!(
        ack.message.as_deref().is_some_and(|m| !m.trim().is_empty()),
        "pull_base reject should carry git's reason; got {:?}",
        ack.message
    );

    // -------------------------------------------------------------------
    // git_merge_back: with a dirty base, local merge is disabled — rejected.
    // -------------------------------------------------------------------
    let merge = h.phone.command(CommandBody::GitMergeBack {
        session_id: session_a_id.clone(),
    });
    let ack = await_ack(&mut h.phone, &merge, ACK_TIMEOUT);
    assert_eq!(
        ack.outcome,
        CommandOutcome::Rejected,
        "git_merge_back should be rejected on a dirty base; message: {:?}",
        ack.message
    );

    // -------------------------------------------------------------------
    // git_abandon_worktree: the destructive type-to-confirm path. A wrong
    // confirmation name is rejected; the exact name force-removes the
    // worktree and drops the session.
    // -------------------------------------------------------------------
    let bad_abandon = h.phone.command(CommandBody::GitAbandonWorktree {
        session_id: session_a_id.clone(),
        confirm_name: "wrong-name".to_string(),
    });
    let ack = await_ack(&mut h.phone, &bad_abandon, ACK_TIMEOUT);
    assert_eq!(
        ack.outcome,
        CommandOutcome::Rejected,
        "abandon with the wrong confirm name must be rejected; message: {:?}",
        ack.message
    );
    // The worktree must still be present after the rejected abandon.
    assert!(
        h.worktree(SESSION_A).exists(),
        "worktree must survive a rejected abandon"
    );

    let abandon = h.phone.command(CommandBody::GitAbandonWorktree {
        session_id: session_a_id.clone(),
        confirm_name: SESSION_A.to_string(),
    });
    let ack = await_ack(&mut h.phone, &abandon, ACK_TIMEOUT);
    assert_eq!(
        ack.outcome,
        CommandOutcome::Applied,
        "abandon with the exact confirm name should apply; message: {:?}",
        ack.message
    );
    // Real side effect: the worktree directory is gone and the session drops
    // out of the snapshot.
    wait_for_path(
        &h.worktree(SESSION_A),
        false,
        EFFECT_TIMEOUT,
        "session A worktree removed by abandon",
    );
    wait_for_session_gone(&mut h.phone, SESSION_A, EFFECT_TIMEOUT);

    // -------------------------------------------------------------------
    // close_session (session B): a clean close removes the session's tab
    // (no worktree teardown — that's abandon's job). A running agent gets a
    // Ctrl-C first; once it has stopped, a follow-up close removes the tab.
    // We retry close until the session is gone, which exercises both the
    // Ctrl-C and the if-all-stopped branches.
    // -------------------------------------------------------------------
    const SESSION_B: &str = "remote-beta";
    let new_b = h.phone.command(CommandBody::NewAgent {
        project_id: project_id.clone(),
        agent_type: AgentType::ClaudeCode,
        name: SESSION_B.to_string(),
        base_branch: "main".to_string(),
        first_task: String::new(),
    });
    let ack = await_ack(&mut h.phone, &new_b, ACK_TIMEOUT);
    assert_eq!(
        ack.outcome,
        CommandOutcome::Accepted,
        "new_agent B should be accepted; message: {:?}",
        ack.message
    );
    wait_for_path(
        &h.worktree(SESSION_B),
        true,
        EFFECT_TIMEOUT,
        "session B worktree created",
    );
    let sess_b = wait_for_session(
        &mut h.phone,
        SESSION_B,
        EFFECT_TIMEOUT,
        |s| matches!(s.status, AgentStatus::Working | AgentStatus::Idle),
        "session B present",
    );
    let session_b_id = sess_b.session_id.clone();

    // Drive close_session until the tab is gone.
    let deadline = Instant::now() + EFFECT_TIMEOUT;
    loop {
        let close = h.phone.command(CommandBody::CloseSession {
            session_id: session_b_id.clone(),
        });
        let ack = await_ack(&mut h.phone, &close, ACK_TIMEOUT);
        assert_eq!(
            ack.outcome,
            CommandOutcome::Applied,
            "close_session should apply (Ctrl-C or removal); message: {:?}",
            ack.message
        );

        let snap = request_snapshot(&mut h.phone, ACK_TIMEOUT);
        if find_session(&snap, SESSION_B).is_none() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "session B was never closed within {EFFECT_TIMEOUT:?}; last close ack: {ack:?}"
        );
        sleep(POLL);
    }

    // Clean teardown (explicit for clarity; Drop does this anyway).
    drop(h);
}
