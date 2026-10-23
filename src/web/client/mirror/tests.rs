//! [`RemoteWorkspace`] against recorded frames, asserted as a rendered outline
//! so a change in what the mirror holds reads as a diff of the picture.

use super::*;
use crate::web::protocol::{
    ActivityTier, DialogOrigin, GitBar, SessionPhase, SessionStatus, StatusBucket,
};

fn terminal(session: &str, id: &str, role: TerminalRole) -> TerminalView {
    TerminalView {
        terminal_id: id.into(),
        session_id: TabId(session.to_string()),
        role,
        title: match role {
            TerminalRole::Primary => "agent".to_string(),
            TerminalRole::Agent => "agent 2".to_string(),
            TerminalRole::Shell => "shell 1".to_string(),
        },
        geometry: Geometry {
            cols: 120,
            rows: 34,
        },
        byte_len: 0,
        replay_from: 0,
        alive: true,
        exit_code: None,
    }
}

fn session(project: &str, id: &str, name: &str, shells: &[&str]) -> SessionView {
    let mut terminals = vec![terminal(
        id,
        &format!("{id}:primary"),
        TerminalRole::Primary,
    )];
    for shell in shells {
        terminals.push(terminal(id, shell, TerminalRole::Shell));
    }
    SessionView {
        session_id: TabId(id.to_string()),
        project_id: project.into(),
        name: name.to_string(),
        agent: "claude".to_string(),
        agent_display_name: "Claude".to_string(),
        phase: SessionPhase::Ready,
        status: SessionStatus {
            interpreted: crate::contracts::InterpretedStatus::Working,
            manual: None,
            bucket: StatusBucket::InProgress,
            running_time_secs: 60,
        },
        git: GitBar::default(),
        terminals,
        lifecycle_reporting: true,
        recovered: false,
        attached_existing_branch: false,
        worktree_path: String::new(),
    }
}

fn snapshot() -> Snapshot {
    Snapshot {
        protocol_version: crate::web::protocol::PROTOCOL_VERSION,
        host_version: "1.0.0".to_string(),
        server_time_ms: 1_000,
        viewer_id: "viewer-1".into(),
        seat: Seat::Writing,
        seats: Vec::new(),
        last_input_seq: 0,
        projects: vec![
            ProjectView {
                project_id: "/repo/app".into(),
                name: "app".to_string(),
                root: "/repo/app".to_string(),
                base_branch: "main".to_string(),
                dot: Some(StatusBucket::InProgress),
                sessions: vec![
                    session("/repo/app", "s1", "login", &["s1:child:1"]),
                    session("/repo/app", "s2", "search", &[]),
                ],
            },
            ProjectView {
                project_id: "/repo/site".into(),
                name: "site".to_string(),
                root: "/repo/site".to_string(),
                base_branch: "main".to_string(),
                dot: None,
                sessions: vec![session("/repo/site", "s3", "footer", &[])],
            },
        ],
        selection: Selection {
            project_id: Some("/repo/app".into()),
            session_id: Some(TabId("s2".to_string())),
            terminal_id: Some("s2:primary".into()),
            split_view: false,
        },
        geometry: Geometry {
            cols: 120,
            rows: 34,
        },
        replay_capacity_bytes: 262_144,
        activity: Vec::new(),
        dialog: None,
        commands: Vec::new(),
        help: None,
        about: None,
        update: None,
        sidebar_position: AgentTabPosition::default(),
        host_machine: None,
        host_kind: None,
        worker: None,
    }
}

/// The mirror as a picture: every project, session and terminal, with `>` on
/// this client's selection and `@` on the host's.
fn outline(ws: &RemoteWorkspace) -> String {
    let mut out = String::new();
    let selected_session = ws.selected_session().map(|s| s.session_id.clone());
    let selected_terminal = ws.selected_terminal().map(|t| t.terminal_id.clone());
    for project in &ws.projects {
        let mark = if ws.selected_project().map(|p| &p.project_id) == Some(&project.project_id) {
            ">"
        } else {
            " "
        };
        out.push_str(&format!("{mark} {}\n", project.name));
        for session in &project.sessions {
            let me = if selected_session.as_ref() == Some(&session.session_id) {
                ">"
            } else {
                " "
            };
            let host = if ws.host_is_viewing(&session.session_id) {
                "@"
            } else {
                " "
            };
            out.push_str(&format!(
                "  {me}{host} {} [{:?}]\n",
                session.name, session.status.bucket
            ));
            for terminal in &session.terminals {
                let me = if selected_terminal.as_ref() == Some(&terminal.terminal_id) {
                    ">"
                } else {
                    " "
                };
                let alive = if terminal.alive { "" } else { " (exited)" };
                out.push_str(&format!("     {me} {}{alive}\n", terminal.title));
            }
        }
    }
    out
}

#[test]
fn the_first_snapshot_starts_on_the_hosts_selection() {
    let mut ws = RemoteWorkspace::new();
    ws.apply_snapshot(snapshot());
    assert_eq!(
        outline(&ws),
        "\
> app
     login [InProgress]
       agent
       shell 1
  >@ search [InProgress]
     > agent
  site
     footer [InProgress]
       agent
"
    );
}

#[test]
fn browsing_never_moves_the_host_marker_and_the_host_moving_never_moves_us() {
    let mut ws = RemoteWorkspace::new();
    ws.apply_snapshot(snapshot());

    assert!(ws.select_terminal(&"s1:child:1".into()));
    ws.apply_delta(Delta::Selection(Selection {
        project_id: Some("/repo/site".into()),
        session_id: Some(TabId("s3".to_string())),
        terminal_id: Some("s3:primary".into()),
        split_view: false,
    }));

    assert_eq!(
        outline(&ws),
        "\
> app
  >  login [InProgress]
       agent
     > shell 1
     search [InProgress]
       agent
  site
   @ footer [InProgress]
       agent
"
    );
}

#[test]
fn each_session_remembers_its_own_terminal() {
    let mut ws = RemoteWorkspace::new();
    ws.apply_snapshot(snapshot());
    ws.select_terminal(&"s1:child:1".into());
    ws.select_session(&TabId("s2".to_string()));
    ws.select_session(&TabId("s1".to_string()));
    assert_eq!(
        ws.selected_terminal().map(|t| t.terminal_id.as_str()),
        Some("s1:child:1")
    );
}

#[test]
fn each_project_remembers_its_own_session() {
    let mut ws = RemoteWorkspace::new();
    ws.apply_snapshot(snapshot());
    ws.select_session(&TabId("s2".to_string()));
    ws.select_project(&"/repo/site".into());
    assert_eq!(
        ws.selected_session().map(|s| s.session_id.0.as_str()),
        Some("s3")
    );
    ws.select_project(&"/repo/app".into());
    assert_eq!(
        ws.selected_session().map(|s| s.session_id.0.as_str()),
        Some("s2")
    );
}

#[test]
fn a_project_whose_remembered_session_closed_lands_on_its_first() {
    let mut ws = RemoteWorkspace::new();
    ws.apply_snapshot(snapshot());
    ws.select_session(&TabId("s2".to_string()));
    ws.select_project(&"/repo/site".into());
    ws.apply_delta(Delta::SessionRemoved {
        session_id: TabId("s2".to_string()),
    });
    ws.select_project(&"/repo/app".into());
    assert_eq!(
        ws.selected_session().map(|s| s.session_id.0.as_str()),
        Some("s1")
    );
}

#[test]
fn deltas_update_status_git_terminals_and_sessions() {
    let mut ws = RemoteWorkspace::new();
    ws.apply_snapshot(snapshot());

    ws.apply_delta(Delta::Status {
        session_id: TabId("s1".to_string()),
        status: SessionStatus {
            interpreted: crate::contracts::InterpretedStatus::WaitingForInput,
            manual: None,
            bucket: StatusBucket::Waiting,
            running_time_secs: 61,
        },
    });
    ws.apply_delta(Delta::Git {
        session_id: TabId("s1".to_string()),
        git: GitBar {
            branch: Some("flightdeck/login".to_string()),
            added: 3,
            ..GitBar::default()
        },
    });
    ws.apply_delta(Delta::TerminalUpsert(terminal(
        "s2",
        "s2:child:4",
        TerminalRole::Shell,
    )));
    ws.apply_delta(Delta::SessionUpsert(session(
        "/repo/site",
        "s4",
        "nav",
        &[],
    )));
    let applied = ws.apply_delta(Delta::TerminalClosed {
        terminal_id: "s1:child:1".into(),
        exit_code: Some(0),
    });
    assert!(
        applied.resync,
        "closed and removed read alike: ask for a snapshot"
    );
    ws.apply_delta(Delta::SessionRemoved {
        session_id: TabId("s3".to_string()),
    });

    assert_eq!(ws.session(&TabId("s1".to_string())).unwrap().git.added, 3);
    assert_eq!(
        outline(&ws),
        "\
> app
     login [Waiting]
       agent
       shell 1 (exited)
  >@ search [InProgress]
     > agent
       shell 1
  site
     nav [InProgress]
       agent
"
    );
}

#[test]
fn a_closed_selection_falls_back_to_a_neighbour() {
    let mut ws = RemoteWorkspace::new();
    ws.apply_snapshot(snapshot());
    ws.select_terminal(&"s1:child:1".into());

    // The shell this client was looking at goes away in a fresh snapshot.
    let mut next = snapshot();
    next.projects[0].sessions[0].terminals.truncate(1);
    ws.apply_snapshot(next);
    assert_eq!(
        ws.selected_terminal().map(|t| t.terminal_id.as_str()),
        Some("s1:primary"),
        "the session's primary, not the host's selection"
    );

    // And the whole project goes away.
    ws.apply_delta(Delta::ProjectRemoved {
        project_id: "/repo/app".into(),
    });
    assert_eq!(ws.selected_project().map(|p| p.name.as_str()), Some("site"));
    assert_eq!(
        ws.selected_session().map(|s| s.name.as_str()),
        Some("footer")
    );
}

#[test]
fn a_later_snapshot_keeps_this_clients_selection() {
    let mut ws = RemoteWorkspace::new();
    ws.apply_snapshot(snapshot());
    ws.select_session(&TabId("s3".to_string()));
    ws.apply_snapshot(snapshot());
    assert_eq!(
        ws.selected_session().map(|s| s.name.as_str()),
        Some("footer")
    );
}

#[test]
fn dialogs_seats_and_activity_are_mirrored() {
    let mut ws = RemoteWorkspace::new();
    ws.apply_snapshot(snapshot());

    ws.apply_delta(Delta::DialogOpened(DialogView {
        dialog_id: "dialog-1".into(),
        kind: "rename_tab".to_string(),
        title: "Rename".to_string(),
        origin: DialogOrigin::Desktop,
        body: None,
    }));
    assert_eq!(
        ws.dialog.as_ref().map(|d| d.kind.as_str()),
        Some("rename_tab")
    );
    // A close for some other dialog leaves this one up.
    ws.apply_delta(Delta::DialogClosed {
        dialog_id: "dialog-0".into(),
        outcome: crate::web::protocol::DialogOutcome::Superseded,
    });
    assert!(ws.dialog.is_some());
    ws.apply_delta(Delta::DialogClosed {
        dialog_id: "dialog-1".into(),
        outcome: crate::web::protocol::DialogOutcome::Confirmed,
    });
    assert!(ws.dialog.is_none());

    ws.apply_delta(Delta::Seats {
        you: Seat::Observing,
        seats: vec![SeatInfo {
            viewer_id: None,
            label: "desktop".to_string(),
            address: None,
            user_agent_label: None,
            seat: Seat::Writing,
            holds_input: true,
            since_ms: 0,
            is_you: false,
        }],
        server_time_ms: 5_000,
        you_were_preempted: true,
    });
    assert_eq!(ws.seat, Some(Seat::Observing));
    assert_eq!(ws.input_holder().map(|s| s.label.as_str()), Some("desktop"));
    assert!(ws.take_preempted());
    assert!(!ws.take_preempted(), "reported once");

    let event = ActivityEvent {
        event_id: "e1".into(),
        at_ms: 1,
        project_id: "/repo/app".into(),
        project_name: "app".to_string(),
        session_id: TabId("s1".to_string()),
        session_name: "login".to_string(),
        from: crate::contracts::InterpretedStatus::Working,
        to: crate::contracts::InterpretedStatus::Idle,
        manual: None,
        reason: String::new(),
        tier: ActivityTier::Finished,
        read: false,
    };
    ws.apply_delta(Delta::Activity(event.clone()));
    ws.apply_delta(Delta::Activity(event));
    assert_eq!(ws.activity.len(), 1, "an event id is recorded once");
}

// -- a worker host (v7) -----------------------------------------------------

fn shell(byte_len: u64, alive: bool) -> WorkerShellView {
    WorkerShellView {
        terminal_id: crate::web::protocol::WORKER_SHELL_TERMINAL_ID.into(),
        title: "zsh".to_string(),
        geometry: Geometry {
            cols: 120,
            rows: 34,
        },
        byte_len,
        replay_from: 0,
        alive,
        exit_code: None,
    }
}

fn message(id: u64, text: &str) -> HostMessage {
    HostMessage {
        id,
        at_ms: 1_000 + id as i64,
        level: crate::web::protocol::MessageLevel::Info,
        text: text.to_string(),
    }
}

fn worker_snapshot(messages: Vec<HostMessage>) -> Snapshot {
    Snapshot {
        host_kind: Some(HostKind::Worker),
        worker: Some(WorkerSnapshot {
            home: "/home/u".to_string(),
            shell: Some(shell(10, true)),
            health: None,
            status: WorkerStatusView {
                projects_root: "/home/u/projects".to_string(),
                ..WorkerStatusView::default()
            },
            connections: ConnectionsView::default(),
            host_messages: messages,
        }),
        ..snapshot()
    }
}

#[test]
fn a_v6_snapshot_is_not_a_worker_and_worker_deltas_ask_for_a_snapshot() {
    let mut ws = RemoteWorkspace::new();
    ws.apply_snapshot(Snapshot {
        protocol_version: 6,
        ..snapshot()
    });
    assert_eq!(ws.host_kind, None);
    assert!(!ws.is_worker());
    assert!(ws.worker.is_none());
    assert!(ws.worker_shell().is_none());
    assert!(ws.take_notifications().is_empty());
    assert_eq!(
        ws.streamed_terminals().len(),
        ws.terminals().count(),
        "no shell"
    );
    let applied = ws.apply_delta(Delta::WorkerShell(shell(1, true)));
    assert!(applied.resync && !applied.changed);

    ws.apply_snapshot(Snapshot {
        host_kind: Some(HostKind::Tui),
        ..snapshot()
    });
    assert!(!ws.is_worker());
}

#[test]
fn a_worker_snapshot_and_each_worker_delta_are_mirrored() {
    let mut ws = RemoteWorkspace::new();
    ws.apply_snapshot(worker_snapshot(Vec::new()));
    assert!(ws.is_worker());
    assert_eq!(ws.worker.as_ref().unwrap().home, "/home/u");
    assert_eq!(ws.worker_shell().map(|s| s.byte_len), Some(10));
    let streamed = ws.streamed_terminals();
    let shell_view = streamed.last().unwrap();
    assert_eq!(
        shell_view.terminal_id.as_str(),
        crate::web::protocol::WORKER_SHELL_TERMINAL_ID
    );
    assert_eq!(shell_view.byte_len, 10);

    let health = Health {
        sampled_at_ms: 5,
        uptime_secs: 60,
        version: "1.2.0".to_string(),
        load_avg_1m_centi: Some(42),
        cpu_count: 8,
        mem_total_bytes: Some(16),
        mem_available_bytes: None,
        disk_free_home_bytes: Some(18),
        disk_free_projects_root_bytes: Some(18),
        agents_total: 2,
        agents_running: 1,
    };
    assert!(ws.apply_delta(Delta::Health(health.clone())).changed);
    assert_eq!(ws.worker.as_ref().unwrap().health, Some(health));

    assert!(ws.apply_delta(Delta::WorkerShell(shell(99, false))).changed);
    assert_eq!(
        ws.worker_shell().map(|s| (s.byte_len, s.alive)),
        Some((99, false))
    );

    let status = WorkerStatusView {
        listening: vec!["100.64.1.2:7420".to_string()],
        advertised: Some("box.tail1234.ts.net:7420".to_string()),
        not_listening_reason: None,
        unread_messages: 3,
        projects_root: "/home/u/src".to_string(),
    };
    assert!(ws.apply_delta(Delta::WorkerStatus(status.clone())).changed);
    assert_eq!(ws.worker.as_ref().unwrap().status, status);

    let connections = ConnectionsView {
        window: Some(crate::web::protocol::WindowInfo {
            label: "studio".to_string(),
            address: Some("100.64.1.9".to_string()),
            since_ms: 7,
            same_device: true,
        }),
        background_links: 2,
    };
    assert!(
        ws.apply_delta(Delta::Connections(connections.clone()))
            .changed
    );
    assert_eq!(ws.worker.as_ref().unwrap().connections, connections);

    let note = NotificationView {
        at_ms: 9,
        title: "login".to_string(),
        body: "Claude finished".to_string(),
        sound: crate::web::protocol::NotificationSoundView::Completion,
    };
    assert!(ws.apply_delta(Delta::Notification(note.clone())).changed);
    assert_eq!(ws.take_notifications(), vec![note]);
    assert!(ws.take_notifications().is_empty(), "taken once");

    assert!(
        ws.apply_delta(Delta::HostMessage(message(1, "hello")))
            .changed
    );
    assert!(
        !ws.apply_delta(Delta::HostMessage(message(1, "hello")))
            .changed,
        "a repeat is recorded once"
    );
    assert_eq!(ws.take_new_messages(), vec![message(1, "hello")]);
    assert!(ws.take_new_messages().is_empty());
}

#[test]
fn unread_messages_replay_once_and_the_history_is_capped() {
    let mut ws = RemoteWorkspace::new();
    ws.apply_snapshot(worker_snapshot(vec![message(1, "a"), message(2, "b")]));
    assert_eq!(ws.take_new_messages().len(), 2, "the replay is new");
    // A reconnect replays what is still unread; nothing is new twice.
    ws.apply_snapshot(worker_snapshot(vec![message(2, "b"), message(3, "c")]));
    assert_eq!(ws.take_new_messages(), vec![message(3, "c")]);
    assert_eq!(ws.worker.as_ref().unwrap().messages.len(), 3);

    for id in 10..(10 + MESSAGE_CAP as u64) {
        ws.apply_delta(Delta::HostMessage(message(id, "x")));
    }
    let messages = &ws.worker.as_ref().unwrap().messages;
    assert_eq!(messages.len(), MESSAGE_CAP);
    assert_eq!(messages.front().map(|m| m.id), Some(10), "oldest dropped");
}

#[test]
fn needs_you_counts_waiting_and_failed_sessions_in_every_project() {
    let mut snap = snapshot();
    snap.projects[0].sessions[0].status.bucket = StatusBucket::Waiting;
    snap.projects[1].sessions[0].status.bucket = StatusBucket::Error;
    let mut ws = RemoteWorkspace::new();
    ws.apply_snapshot(snap);
    assert_eq!(ws.needs_you_count(), 2);
    ws.apply_delta(Delta::Status {
        session_id: TabId("s1".to_string()),
        status: SessionStatus {
            interpreted: crate::contracts::InterpretedStatus::Working,
            manual: None,
            bucket: StatusBucket::InProgress,
            running_time_secs: 1,
        },
    });
    assert_eq!(ws.needs_you_count(), 1);
}
