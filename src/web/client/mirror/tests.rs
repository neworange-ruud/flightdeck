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
