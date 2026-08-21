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
    /// Kept alive for the whole test (kill-on-drop). The phone talks to it by
    /// URL; the relay-restart test also drives it directly to simulate a
    /// redeploy, so this is not an `_`-prefixed lifetime-only field.
    relay: RelayHandle,
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
        Self::boot_with(RelayHandle::spawn())
    }

    /// Like [`Self::boot`] but on a relay running the **persistent** SQLite store
    /// — the hosted relay's configuration, and the prerequisite for restarting it
    /// mid-pairing without losing the pairing itself.
    fn boot_persistent() -> Self {
        Self::boot_with(RelayHandle::spawn_persistent())
    }

    /// Shared boot path against an already-running `relay`.
    fn boot_with(relay: RelayHandle) -> Self {
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
            relay,
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

// ===========================================================================
// Relay restart / redeploy — desktop->phone delivery must resume
// (remote-control-bbf).
// ===========================================================================

/// Budget for the desktop to notice a dead relay, reconnect, and resume feeding
/// the phone. The client reconnects with exponential backoff + jitter (1s..60s);
/// a session that stayed up for `MIN_STABLE_SESSION` resets the backoff to its
/// ~1s base, which [`settle_for_stable_session`] guarantees. Generous anyway, so
/// an unlucky backoff can't flake the suite.
const RESUME_TIMEOUT: Duration = Duration::from_secs(90);

/// The desktop client only resets its reconnect backoff after a session that
/// stayed authenticated for at least its `MIN_STABLE_SESSION` (10s, see
/// `src/remote/client.rs`). Sleeping past that before killing the relay keeps the
/// post-restart reconnect at the ~1s base delay instead of a multi-second
/// backoff, which keeps these tests fast and their timeouts meaningful.
const STABLE_SESSION_SETTLE: Duration = Duration::from_secs(12);

/// Hold the pairing open long enough that the desktop's current relay session
/// counts as "stable" (see [`STABLE_SESSION_SETTLE`]), keeping the feed busy
/// meanwhile so both directions stay warm.
fn settle_for_stable_session(phone: &mut PhoneDriver) {
    let deadline = Instant::now() + STABLE_SESSION_SETTLE;
    while Instant::now() < deadline {
        let _ = request_snapshot(phone, ACK_TIMEOUT);
        sleep(POLL);
    }
}

/// A relay restart with the **persistent** store: pairings, claim state and the
/// per-stream seq high-water marks all survive, so the desktop simply reconnects
/// and the phone keeps receiving. This is the deployed configuration
/// (remote-control-vp2), and the regression guard for the reported symptom —
/// "phone prompt executes on the desktop but nothing ever comes back to the
/// phone" after the hosted relay was rescheduled (remote-control-bbf).
#[test]
fn relay_restart_resumes_desktop_to_phone_delivery() {
    let mut h = Harness::boot_persistent();

    // Warm both directions and drive all three seq cursors above zero.
    let snap = request_snapshot(&mut h.phone, ACK_TIMEOUT);
    let _project = only_project_id(&snap);
    settle_for_stable_session(&mut h.phone);

    let cursor_before = h.phone.last_received_seq();
    assert!(
        cursor_before > 0,
        "phone should hold a non-zero receive cursor before the restart, \
         otherwise the test cannot detect a seq divergence"
    );

    // --- The redeploy: same port, same database, brand-new process. ---
    h.relay.restart();
    // The phone's socket died with the old process; a real phone reconnects and
    // resumes from its persisted cursor.
    h.phone.reconnect();

    // The load-bearing assertion: a phone command reaches the desktop AND the
    // desktop's reply reaches the phone. Both legs must survive the restart —
    // the reported bug had inbound working while the feed stalled forever.
    let snap_after = request_snapshot(&mut h.phone, RESUME_TIMEOUT);
    assert_eq!(
        snap_after.projects.len(),
        1,
        "post-restart snapshot should still describe the fixture project: {snap_after:?}"
    );

    // Delivery must keep flowing, not just produce one lucky replayed frame: a
    // second round trip has to complete on the post-restart connection too.
    let snap_again = request_snapshot(&mut h.phone, RESUME_TIMEOUT);
    assert_eq!(
        snap_again.projects.len(),
        1,
        "a second post-restart snapshot request should also be answered: {snap_again:?}"
    );

    // With nothing lost on the relay side, the stream continues monotonically:
    // the cursor advanced and no stream reset was needed.
    assert!(
        h.phone.last_received_seq() > cursor_before,
        "phone receive cursor should have advanced past {cursor_before} after the restart, \
         got {}",
        h.phone.last_received_seq()
    );
    assert_eq!(
        h.phone.stream_resets(),
        0,
        "a persistent-store restart preserves the relay's seq watermark, so the desktop \
         should never have had to restart its outbound stream"
    );

    drop(h);
}

/// The original failure mechanism, reproduced end-to-end: the relay comes back up
/// still knowing the pairing but having **lost the desktop->phone stream's seq
/// watermark**. The desktop's next envelope is then non-monotonic from the
/// relay's point of view, which used to be a fatal `bad_frame` reconnect loop
/// with the feed stalled forever (remote-control-bbf).
///
/// Recovery is now silent. The relay treats its watermark as a *cache* of the
/// sender's cursor rather than an independent authority, so a stream it has no
/// record of adopts whatever seq the next envelope carries
/// (remote-control-arg) — the desktop and phone cursors already agree with each
/// other, and the relay simply falls in behind them.
///
/// That deliberately replaces the older recovery, where the relay answered
/// `seq_violation`, the desktop zeroed its outbound cursor and restarted from
/// seq 1, and the phone accepted the seq-1 envelope as a stream reset. That
/// dance worked only because the relay's watermark was volatile; once the store
/// persisted it, the desktop's restart at seq 1 was rejected by a watermark that
/// was genuinely ahead, which drove another rewind — the livelock in
/// remote-control-arg. So this test now asserts the feed recovers *without* a
/// renumbering, and `stream_resets() == 0` is the regression guard.
#[test]
fn relay_losing_desktop_stream_seq_state_recovers_by_adopting_the_cursor() {
    let mut h = Harness::boot_persistent();

    let snap = request_snapshot(&mut h.phone, ACK_TIMEOUT);
    let _project = only_project_id(&snap);
    settle_for_stable_session(&mut h.phone);

    let cursor_before = h.phone.last_received_seq();
    assert!(
        cursor_before > 1,
        "the phone must be well past seq 1 before the restart, or a relay that wrongly \
         demanded a restart at seq 1 would look identical to one that adopted the \
         cursor; cursor = {cursor_before}"
    );

    // --- The redeploy that loses *part* of its state. ---
    // Stop the relay first: the store opens its database with the no-locking
    // `unix-none` VFS, so editing it while the relay runs is unsafe.
    h.relay.stop();
    let wiped = wipe_desktop_stream_seq_state(h.relay.db_path());
    assert!(
        wiped > 0,
        "expected to wipe at least one desktop->phone queue stream row; if this is 0 the \
         schema or the sender tag changed and this test is no longer reproducing the bug \
         (see remote/relay/src/store/sqlite.rs)"
    );
    h.relay.restart();
    h.phone.reconnect();

    // Despite the divergence, the feed must come back — the relay adopts the
    // desktop's cursor for the stream it no longer knows.
    let snap_after = request_snapshot(&mut h.phone, RESUME_TIMEOUT);
    assert_eq!(
        snap_after.projects.len(),
        1,
        "post-restart snapshot should still describe the fixture project: {snap_after:?}"
    );
    assert_eq!(
        h.phone.stream_resets(),
        0,
        "the relay must adopt the desktop's existing cursor, not make it renumber the \
         stream from seq 1 — that renumbering is what livelocked against a relay whose \
         watermark was genuinely ahead (remote-control-arg)"
    );
    assert!(
        h.phone.last_received_seq() > cursor_before,
        "the adopted stream must continue above the phone's pre-restart cursor \
         (was {cursor_before}, now {})",
        h.phone.last_received_seq()
    );

    // And it must keep flowing, not stall after the first post-restart frame.
    let epoch_cursor = h.phone.last_received_seq();
    let snap_again = request_snapshot(&mut h.phone, RESUME_TIMEOUT);
    assert_eq!(
        snap_again.projects.len(),
        1,
        "the feed must keep delivering after the restart: {snap_again:?}"
    );
    assert!(
        h.phone.last_received_seq() > epoch_cursor,
        "the adopted stream must advance monotonically \
         (cursor was {epoch_cursor}, now {})",
        h.phone.last_received_seq()
    );

    drop(h);
}

/// Delete the relay's persisted **desktop->phone** stream state (its seq
/// high-water mark, ack cursor, and any buffered envelopes) from a stopped
/// relay's SQLite store, leaving the pairing itself and the phone->desktop
/// stream intact. Returns the number of `queue_streams` rows removed.
///
/// This reproduces exactly the asymmetry reported in remote-control-bbf — the
/// desktop's outbound cursor diverges from the relay's while inbound keeps
/// working. `sender = 0` is the desktop direction; see `sender_tag` in
/// `remote/relay/src/store/sqlite.rs` (the caller asserts the delete matched
/// rows, so a change to that mapping fails the test loudly).
fn wipe_desktop_stream_seq_state(db: &Path) -> usize {
    const DESKTOP_SENDER_TAG: i64 = 0;
    let conn = rusqlite::Connection::open(db)
        .unwrap_or_else(|e| panic!("open the relay's sqlite store at {}: {e}", db.display()));
    let streams = conn
        .execute(
            "DELETE FROM queue_streams WHERE sender = ?1",
            [DESKTOP_SENDER_TAG],
        )
        .expect("delete desktop queue_streams rows");
    conn.execute(
        "DELETE FROM queue_envelopes WHERE sender = ?1",
        [DESKTOP_SENDER_TAG],
    )
    .expect("delete desktop queue_envelopes rows");
    streams
}

/// Rewind the relay's persisted **desktop->phone** `high_water` to `to`, leaving
/// the stream row in place. Returns the number of rows updated.
///
/// This is the production wedge from remote-control-zv3, and it is deliberately
/// NOT the same as [`wipe_desktop_stream_seq_state`]: wiping the row leaves
/// `high_water == 0`, which the relay treats as "a stream I have never seen" and
/// adopts. A row that survives with a *low but non-zero* watermark gets no
/// adoption — the relay demands `high_water + 1` forever while the desktop keeps
/// counting up, which is exactly how a live pairing died with the relay expecting
/// 98 and the desktop sending 38,315.
fn rewind_desktop_stream_high_water(db: &Path, to: i64) -> usize {
    const DESKTOP_SENDER_TAG: i64 = 0;
    let conn = rusqlite::Connection::open(db)
        .unwrap_or_else(|e| panic!("open the relay's sqlite store at {}: {e}", db.display()));
    let rows = conn
        .execute(
            "UPDATE queue_streams SET high_water = ?1, ack_cursor = ?1 WHERE sender = ?2",
            [to, DESKTOP_SENDER_TAG],
        )
        .expect("rewind desktop queue_streams high_water");
    // Buffered envelopes above the new watermark would be replayed out of band.
    conn.execute(
        "DELETE FROM queue_envelopes WHERE sender = ?1",
        [DESKTOP_SENDER_TAG],
    )
    .expect("delete desktop queue_envelopes rows");
    rows
}

/// The production outage end to end (remote-control-zv3): the relay's watermark
/// for the desktop->phone stream sits far BELOW the desktop's outbound cursor,
/// and the row survives, so the adoption path never fires. Every envelope is
/// rejected with `seq_violation`, and before the fix neither side was allowed to
/// close the gap — the desktop kept counting up (to ~38k in the field) and the
/// phone received nothing until the user re-paired.
///
/// The desktop must now realign to the seq the relay names and delivery must
/// resume, without a re-pair and without tearing the link down.
#[test]
fn a_desktop_ahead_of_the_relays_watermark_realigns_instead_of_wedging() {
    let mut h = Harness::boot_persistent();

    let snap = request_snapshot(&mut h.phone, ACK_TIMEOUT);
    let _project = only_project_id(&snap);
    settle_for_stable_session(&mut h.phone);

    let cursor_before = h.phone.last_received_seq();
    assert!(
        cursor_before > 1,
        "the desktop must be well past seq 1 before we rewind the relay, or there is no          gap to recover from; cursor = {cursor_before}"
    );

    // Rewind the relay behind the desktop, keeping the stream row so the relay
    // will NOT adopt. Stop it first: the store uses the no-locking `unix-none`
    // VFS, so editing the file under a running relay is unsafe.
    h.relay.stop();
    let rewound = rewind_desktop_stream_high_water(h.relay.db_path(), 1);
    assert!(
        rewound > 0,
        "expected to rewind at least one desktop->phone queue stream row; if this is 0 the          schema or the sender tag changed and this test is no longer reproducing the bug          (see remote/relay/src/store/sqlite.rs)"
    );
    h.relay.restart();
    h.phone.reconnect();

    // The feed must come back on its own. Before the fix this hung forever: the
    // relay answered every envelope with a bare `seq_violation`, which the
    // desktop read as "my INBOUND cursor is stale" — a no-op for a sender that
    // is ahead — so it never renumbered and nothing was ever delivered again.
    let snap_after = request_snapshot(&mut h.phone, RESUME_TIMEOUT);
    assert_eq!(
        snap_after.projects.len(),
        1,
        "the realigned feed must still describe the fixture project: {snap_after:?}"
    );

    // And it keeps flowing, rather than delivering one frame and stalling.
    let epoch_cursor = h.phone.last_received_seq();
    let snap_again = request_snapshot(&mut h.phone, RESUME_TIMEOUT);
    assert_eq!(
        snap_again.projects.len(),
        1,
        "the feed must keep delivering after realigning: {snap_again:?}"
    );
    assert!(
        h.phone.last_received_seq() > epoch_cursor,
        "the realigned stream must advance monotonically \
         (cursor was {epoch_cursor}, now {})",
        h.phone.last_received_seq()
    );

    drop(h);
}
