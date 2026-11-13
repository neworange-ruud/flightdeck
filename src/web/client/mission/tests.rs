//! Mission control built from a mirrored workspace: the same rules as a local
//! window's, the activity a host states plus what this client saw, and the
//! host's prompts offered only where the host takes an answer.

use super::*;
use crate::contracts::{AgentTabPosition, InterpretedStatus};
use crate::view::{CardAction, LastEventKind, SessionKey};
use crate::web::protocol::{
    Delta, Geometry, GitBar, ProjectView, PromptButtonView, Seat, Selection, SessionPhase,
    SessionStatus, Snapshot, StatusBucket, TerminalRole, TerminalView, PROTOCOL_VERSION,
};

const NOW: u64 = 2_000_000_000;
const HOUR: u64 = 3_600;

use InterpretedStatus::*;

fn session(
    project: &str,
    id: &str,
    status: InterpretedStatus,
    output_ago: Option<u64>,
) -> SessionView {
    SessionView {
        session_id: TabId(id.to_string()),
        project_id: project.into(),
        name: id.to_string(),
        agent: "claude".to_string(),
        agent_display_name: "Claude Code".to_string(),
        phase: SessionPhase::Ready,
        status: SessionStatus {
            interpreted: status,
            manual: None,
            bucket: StatusBucket::from_interpreted(status),
            running_time_secs: 0,
        },
        git: GitBar::default(),
        terminals: vec![TerminalView {
            terminal_id: format!("{id}:primary").as_str().into(),
            session_id: TabId(id.to_string()),
            role: TerminalRole::Primary,
            title: "claude".to_string(),
            geometry: Geometry { cols: 80, rows: 24 },
            byte_len: 0,
            replay_from: 0,
            alive: true,
            exit_code: None,
        }],
        lifecycle_reporting: true,
        recovered: false,
        attached_existing_branch: false,
        worktree_path: String::new(),
        activity: output_ago.map(|ago| TabActivity {
            last_output_at: Some(NOW - ago),
            ..TabActivity::default()
        }),
        prompt: None,
        listening_ports: Vec::new(),
    }
}

fn project(root: &str, sessions: Vec<SessionView>) -> ProjectView {
    ProjectView {
        project_id: root.into(),
        name: root.trim_start_matches('/').to_string(),
        root: root.to_string(),
        base_branch: "main".to_string(),
        dot: None,
        sessions,
    }
}

fn snapshot(projects: Vec<ProjectView>) -> Snapshot {
    Snapshot {
        protocol_version: PROTOCOL_VERSION,
        host_version: "1.0.0".to_string(),
        server_time_ms: 0,
        viewer_id: "v".into(),
        seat: Seat::Writing,
        seats: Vec::new(),
        last_input_seq: 0,
        projects,
        selection: Selection::default(),
        geometry: Geometry { cols: 80, rows: 24 },
        replay_capacity_bytes: 0,
        activity: Vec::new(),
        dialog: None,
        commands: crate::web::commands::inventory(),
        help: None,
        about: None,
        update: None,
        sidebar_position: AgentTabPosition::default(),
        host_machine: None,
        host_kind: None,
        worker: None,
    }
}

/// Two projects, the same shape as the local Mission control tests: tiles
/// `b-attn`, `a-wait`, `b-work`; `a-idle` a card; `a-old` quiet.
fn workspace() -> RemoteWorkspace {
    let mut ws = RemoteWorkspace::new();
    ws.apply_snapshot(snapshot(vec![
        project(
            "/alpha",
            vec![
                session("/alpha", "a-wait", WaitingForInput, Some(900)),
                session("/alpha", "a-idle", Idle, Some(HOUR)),
                session("/alpha", "a-old", Idle, Some(40 * 24 * HOUR)),
            ],
        ),
        project(
            "/beta",
            vec![
                session("/beta", "b-work", Working, Some(5)),
                session("/beta", "b-attn", NeedsAttention, Some(30)),
            ],
        ),
    ]));
    ws
}

fn build(ws: &RemoteWorkspace, watch: &ActivityWatch) -> MissionView {
    mission_view(ws, watch, &MissionScope::default(), NOW, &[])
}

fn names(view: &MissionView) -> (Vec<&str>, Vec<&str>) {
    (
        view.tiles.iter().map(|t| t.row.name.as_str()).collect(),
        view.cards.iter().map(|c| c.row.name.as_str()).collect(),
    )
}

#[test]
fn the_remote_view_follows_the_local_rules() {
    let ws = workspace();
    let view = build(&ws, &ActivityWatch::new());
    let (tiles, cards) = names(&view);
    assert_eq!(tiles, ["b-attn", "a-wait", "b-work"]);
    assert_eq!(cards, ["a-idle"]);
    assert_eq!(view.quiet_count, 1);
    assert_eq!(view.needs_you_count, 2);
    assert_eq!(view.working_count, 1);
    assert_eq!(
        view.tiles[0].key,
        SessionKey {
            project: 1,
            tab_id: "b-attn".to_string()
        }
    );
    assert_eq!(view.tiles[0].tab_index, 1);
    assert_eq!(view.tiles[0].project_name, "beta");

    // The project filter names the host's root.
    let beta = MissionScope {
        project: Some("/beta".to_string()),
        ..MissionScope::default()
    };
    let view = mission_view(&ws, &ActivityWatch::new(), &beta, NOW, &[]);
    assert_eq!(view.project_filter, Some(1));
    assert_eq!(names(&view).0, ["b-attn", "b-work"]);
    assert_eq!(view.needs_you_count, 2, "the badge is not filtered");
}

#[test]
fn a_host_without_activity_dates_only_what_this_client_saw() {
    let mut ws = workspace();
    for p in ws.projects.iter_mut() {
        for s in p.sessions.iter_mut() {
            s.activity = None;
        }
    }
    let mut watch = ActivityWatch::new();
    watch.observe(&ws, NOW - 100);
    let view = build(&ws, &watch);
    assert!(view.cards.is_empty(), "nothing dated: no cards");
    assert_eq!(view.quiet_count, 2);
    assert_eq!(names(&view).0, ["a-wait", "b-attn", "b-work"], "by name");

    // `b-work` finishes: dated when the delta arrives, and it is a card.
    let applied = ws.apply_delta(Delta::Status {
        session_id: TabId("b-work".to_string()),
        status: SessionStatus {
            interpreted: Idle,
            manual: None,
            bucket: StatusBucket::Idle,
            running_time_secs: 0,
        },
    });
    assert!(applied.changed);
    watch.observe(&ws, NOW - 60);
    let view = build(&ws, &watch);
    assert_eq!(names(&view).1, ["b-work"]);
    let card = &view.cards[0];
    assert_eq!(
        card.last_event.map(|e| e.kind),
        Some(LastEventKind::StatusChange)
    );
    assert_eq!(card.last_event.map(|e| e.secs_ago), Some(60));
    assert_eq!(card.row.status_since_secs, Some(60), "aged from the change");
}

#[test]
fn what_this_client_saw_brings_the_hosts_activity_forward() {
    let mut ws = workspace();
    let mut watch = ActivityWatch::new();
    watch.observe(&ws, NOW - 10);
    // `a-old` gets new git work: dated now, so it is no longer quiet.
    let git = GitBar {
        collected: true,
        modified: 2,
        files_changed: 2,
        ..GitBar::default()
    };
    // First sight of its git is a baseline...
    ws.apply_delta(Delta::Git {
        session_id: TabId("a-old".to_string()),
        git: GitBar {
            collected: true,
            ..GitBar::default()
        },
    });
    watch.observe(&ws, NOW - 5);
    assert_eq!(build(&ws, &watch).quiet_count, 1);
    // ... and a change after it is an edge.
    ws.apply_delta(Delta::Git {
        session_id: TabId("a-old".to_string()),
        git,
    });
    watch.observe(&ws, NOW - 2);
    let view = build(&ws, &watch);
    assert_eq!(view.quiet_count, 0);
    let old = view.cards.iter().find(|c| c.row.name == "a-old").unwrap();
    assert_eq!(
        old.last_event.map(|e| e.kind),
        Some(LastEventKind::GitChange)
    );
    assert_eq!(old.action, Some(CardAction::Push), "dirty, never pushed");

    // A new snapshot restates everything: the client's stamps start over, and
    // the host's own word (40 days ago) stands until something moves again.
    let projects = ws.projects.clone();
    ws.apply_snapshot(snapshot(projects));
    watch.observe(&ws, NOW);
    assert_eq!(build(&ws, &watch).quiet_count, 1, "the host's word again");
}

#[test]
fn a_remote_card_never_offers_pull_base() {
    let mut ws = workspace();
    for p in ws.projects.iter_mut() {
        for s in p.sessions.iter_mut() {
            s.git = GitBar {
                collected: true,
                has_upstream: true,
                upstream: Some("origin/x".to_string()),
                drift: 12,
                ..GitBar::default()
            };
        }
    }
    let view = build(&ws, &ActivityWatch::new());
    let idle = &view.cards[0];
    assert_eq!(idle.detail, "Idle · clean · base +12");
    // Locally that is Pull base; a remote surface may not pull the base, and
    // a clean, pushed session is done with.
    assert_eq!(idle.action, Some(CardAction::Finish));
}

fn binary(id: &str) -> PendingPrompt {
    PendingPrompt {
        prompt_id: id.to_string(),
        shape: PromptShapeView::Binary,
        text: "Run `cargo test`?".to_string(),
        buttons: vec![
            PromptButtonView {
                label: "Approve".to_string(),
                answer: PromptAnswerView::Approve,
            },
            PromptButtonView {
                label: "Deny".to_string(),
                answer: PromptAnswerView::Deny,
            },
        ],
    }
}

fn with_prompt(prompt: PendingPrompt) -> RemoteWorkspace {
    let mut ws = workspace();
    ws.projects[0].sessions[0].prompt = Some(Box::new(prompt));
    ws
}

#[test]
fn a_waiting_tile_carries_the_hosts_prompt() {
    let ws = with_prompt(binary("p1"));
    let view = build(&ws, &ActivityWatch::new());
    let (_, wait) = view
        .tile(&SessionKey {
            project: 0,
            tab_id: "a-wait".to_string(),
        })
        .unwrap();
    let prompt = wait.prompt.as_ref().expect("the host's prompt");
    assert_eq!(prompt.shape, PromptShape::Binary);
    assert_eq!(prompt.prompt_id, "p1");
    assert_eq!(
        prompt.buttons.iter().map(|b| b.answer).collect::<Vec<_>>(),
        [PromptAnswer::Approve, PromptAnswer::Deny]
    );
    assert!(view
        .tiles
        .iter()
        .filter(|t| t.row.name != "a-wait")
        .all(|t| t.prompt.is_none()));

    // Answered from here: gone until the host says otherwise.
    let view = mission_view(
        &ws,
        &ActivityWatch::new(),
        &MissionScope::default(),
        NOW,
        &["p1".to_string()],
    );
    assert!(view.tiles.iter().all(|t| t.prompt.is_none()));
}

#[test]
fn a_host_that_takes_no_answer_gets_open_to_answer() {
    let mut ws = with_prompt(binary("p1"));
    ws.commands.retain(|c| c.run.name != names::ANSWER_PROMPT);
    assert!(!answers_prompts(&ws));
    let view = build(&ws, &ActivityWatch::new());
    let prompt = view.tiles[1].prompt.as_ref().unwrap();
    assert_eq!(view.tiles[1].row.name, "a-wait");
    assert_eq!(prompt.shape, PromptShape::Unsupported);
    assert!(prompt.buttons.is_empty());
}

#[test]
fn an_answer_this_build_does_not_know_is_not_half_offered() {
    let mut prompt = binary("p1");
    prompt.buttons[1].answer = PromptAnswerView::Unrecognized;
    let view = prompt_view(&prompt, true);
    assert_eq!(view.shape, PromptShape::Unsupported);
    assert!(view.buttons.is_empty());
    // An unknown shape reads as Unsupported on the wire already.
    let parsed: PendingPrompt = serde_json::from_value(serde_json::json!({
        "prompt_id": "p2",
        "shape": "checklist_of_the_future",
        "buttons": [{ "label": "x", "answer": { "kind": "telepathy" } }],
    }))
    .unwrap();
    assert_eq!(parsed.shape, PromptShapeView::Unsupported);
    assert_eq!(parsed.buttons[0].answer, PromptAnswerView::Unrecognized);
    assert_eq!(prompt_view(&parsed, true).shape, PromptShape::Unsupported);
}

#[test]
fn an_answer_names_the_session_the_prompt_and_the_choice() {
    let reply = PromptReply {
        key: SessionKey {
            project: 0,
            tab_id: "a-wait".to_string(),
        },
        prompt_id: "p1".to_string(),
        answer: PromptAnswer::Option(2),
    };
    let args = answer_args(&reply);
    assert_eq!(
        args,
        serde_json::json!({
            "session_id": "a-wait",
            "prompt_id": "p1",
            "answer": { "kind": "option", "index": 2 },
        })
    );
    let back: AnswerPromptArgs = serde_json::from_value(args).unwrap();
    assert_eq!(back.answer, PromptAnswerView::Option { index: 2 });
}
