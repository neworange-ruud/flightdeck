//! Wire-format tests for the web protocol (D12).
//!
//! Three jobs, in order of what they protect:
//!
//! 1. **Round-trip every variant.** The SPA is a hand-written TypeScript mirror
//!    of these types (D9), so a shape that does not survive `to_value` →
//!    `from_value` is a shape the mirror cannot be trusted against.
//! 2. **Pin the decisions that are easy to erode.** The version constant, the
//!    byte cursor, `truncated`, `Shutdown`'s self-initiated case, and the fact
//!    that `Resize` cannot name a PTY.
//! 3. **Prove the forward-compatibility policy**, rather than describing it in a
//!    doc comment and hoping.

use super::*;
use crate::agents::status::combine_status;
use crate::contracts::ProcessState;
use serde_json::json;

// ---------------------------------------------------------------------------
// Helpers: one fully-populated value per composite type, so a round-trip test
// exercises every field rather than the defaults.
// ---------------------------------------------------------------------------

fn round_trip<T>(value: &T) -> T
where
    T: Serialize + serde::de::DeserializeOwned,
{
    let json = serde_json::to_value(value).expect("serialize");
    serde_json::from_value(json).expect("deserialize")
}

fn geometry() -> Geometry {
    Geometry {
        cols: 120,
        rows: 34,
    }
}

fn status() -> SessionStatus {
    SessionStatus {
        interpreted: InterpretedStatus::WaitingForInput,
        manual: Some(ManualStatus::Blocked),
        bucket: StatusBucket::Waiting,
        running_time_secs: 42,
    }
}

fn git_bar() -> GitBar {
    GitBar {
        branch: Some("flightdeck/fix-login".into()),
        added: 3,
        modified: 2,
        removed: 1,
        ahead: 0,
        behind: 0,
        drift: 3,
        has_upstream: true,
        files_changed: 6,
        collected: true,
    }
}

fn terminal_view() -> TerminalView {
    TerminalView {
        terminal_id: TerminalId::new("tab_1:primary"),
        session_id: TabId("tab_1".into()),
        role: TerminalRole::Primary,
        title: "agent".into(),
        geometry: geometry(),
        byte_len: 4096,
        replay_from: 512,
        alive: true,
        exit_code: None,
    }
}

fn session_view() -> SessionView {
    SessionView {
        session_id: TabId("tab_1".into()),
        project_id: ProjectId::new("proj_1"),
        name: "fix-login".into(),
        agent: "claude".into(),
        agent_display_name: "Claude Code".into(),
        phase: SessionPhase::Ready,
        status: status(),
        git: git_bar(),
        terminals: vec![terminal_view()],
        lifecycle_reporting: true,
        recovered: true,
        attached_existing_branch: false,
    }
}

fn project_view() -> ProjectView {
    ProjectView {
        project_id: ProjectId::new("proj_1"),
        name: "flightdeck".into(),
        root: "/Users/ruud/Projects/flightdeck".into(),
        base_branch: "main".into(),
        dot: Some(StatusBucket::Waiting),
        sessions: vec![session_view()],
    }
}

fn seat_info() -> SeatInfo {
    SeatInfo {
        viewer_id: Some(ViewerId::new("view_1")),
        label: "192.168.2.20 · Chrome on macOS".into(),
        address: Some("192.168.2.20".into()),
        user_agent_label: Some("Chrome on macOS".into()),
        seat: Seat::Writing,
        holds_input: true,
        since_ms: 1_700_000_000_000,
        is_you: true,
    }
}

fn activity_event() -> ActivityEvent {
    ActivityEvent {
        event_id: EventId::new("ev_1"),
        at_ms: 1_700_000_000_123,
        project_id: ProjectId::new("proj_1"),
        project_name: "flightdeck".into(),
        session_id: TabId("tab_1".into()),
        session_name: "fix-login".into(),
        from: InterpretedStatus::Working,
        to: InterpretedStatus::WaitingForInput,
        manual: None,
        reason: "asked a question".into(),
        tier: ActivityTier::Attention,
        read: false,
    }
}

#[test]
fn activity_reason_round_trips_and_defaults_to_empty_never_a_guess() {
    // The reason is the part of a feed row a user actually reads (artboard
    // 2e), so it has to survive the wire.
    let event = activity_event();
    let wire = serde_json::to_string(&event).unwrap();
    let back: ActivityEvent = serde_json::from_str(&wire).unwrap();
    assert_eq!(back.reason, "asked a question");
    assert_eq!(back, event);

    // A payload with no reason at all decodes to the empty string rather than
    // failing or inventing one. Turn 2 §5.1's "unknown stays unknown" applies
    // to the reason exactly as it does to the statuses: the honest answer to
    // "why did this happen" is sometimes nothing, and it must never be padded
    // out to look informative.
    let mut value: serde_json::Value = serde_json::from_str(&wire).unwrap();
    value.as_object_mut().unwrap().remove("reason");
    let without: ActivityEvent = serde_json::from_value(value).unwrap();
    assert_eq!(without.reason, "");
}

fn dialog_view() -> DialogView {
    DialogView {
        dialog_id: DialogId::new("dlg_1"),
        kind: "new_agent".into(),
        title: "New agent".into(),
        origin: DialogOrigin::Browser {
            viewer_id: Some(ViewerId::new("view_1")),
            label: "192.168.2.20".into(),
        },
        body: None,
    }
}

fn snapshot() -> Snapshot {
    Snapshot {
        protocol_version: PROTOCOL_VERSION,
        host_version: "0.9.1".into(),
        server_time_ms: 1_700_000_000_000,
        viewer_id: ViewerId::new("view_1"),
        seat: Seat::Writing,
        seats: vec![
            SeatInfo {
                viewer_id: None,
                label: "desktop".into(),
                address: None,
                user_agent_label: None,
                seat: Seat::Writing,
                holds_input: false,
                since_ms: 1_699_999_000_000,
                is_you: false,
            },
            seat_info(),
        ],
        last_input_seq: 17,
        projects: vec![project_view()],
        selection: Selection {
            project_id: Some(ProjectId::new("proj_1")),
            session_id: Some(TabId("tab_1".into())),
            terminal_id: Some(TerminalId::new("tab_1:primary")),
            split_view: false,
        },
        geometry: geometry(),
        replay_capacity_bytes: 256 * 1024,
        activity: vec![activity_event()],
        dialog: Some(dialog_view()),
        commands: vec![command_view()],
    }
}

/// One palette row as the host describes it, exercising every optional field so
/// the round-trip test cannot skip one.
fn command_view() -> CommandView {
    CommandView {
        id: "open_worktree_in_file_manager".into(),
        label: "Open Worktree in File Manager".into(),
        group: "Worktree".into(),
        run: CommandRun {
            name: "open_worktree_in_file_manager".into(),
            args: None,
        },
        host_only: true,
        answers_dialog: true,
        annotation: Some("host only".into()),
        target: None,
        refusal: Some("Run it from the desktop.".into()),
    }
}

/// Every [`Delta`] variant, so the round-trip test cannot silently skip one.
fn all_deltas() -> Vec<Delta> {
    vec![
        Delta::ProjectUpsert(project_view()),
        Delta::ProjectRemoved {
            project_id: ProjectId::new("proj_1"),
        },
        Delta::SessionUpsert(session_view()),
        Delta::SessionRemoved {
            session_id: TabId("tab_1".into()),
        },
        Delta::Status {
            session_id: TabId("tab_1".into()),
            status: status(),
        },
        Delta::Git {
            session_id: TabId("tab_1".into()),
            git: git_bar(),
        },
        Delta::ProjectDot {
            project_id: ProjectId::new("proj_1"),
            dot: None,
        },
        Delta::Selection(Selection::default()),
        Delta::TerminalUpsert(terminal_view()),
        Delta::TerminalClosed {
            terminal_id: TerminalId::new("tab_1:child:2"),
            exit_code: Some(130),
        },
        Delta::Geometry {
            terminal_id: TerminalId::new("tab_1:primary"),
            geometry: geometry(),
        },
        Delta::Activity(activity_event()),
        Delta::DialogOpened(dialog_view()),
        Delta::DialogClosed {
            dialog_id: DialogId::new("dlg_1"),
            outcome: DialogOutcome::Cancelled,
        },
        Delta::Seats {
            you: Seat::Observing,
            seats: vec![seat_info()],
            server_time_ms: 1_700_000_012_000,
            you_were_preempted: false,
        },
    ]
}

/// Every [`ServerMsg`] variant except the `Unrecognized` catch-all, which is
/// deserialize-only by design and covered by the forward-compat tests.
fn all_server_msgs() -> Vec<ServerMsg> {
    let mut msgs = vec![
        ServerMsg::Snapshot(snapshot()),
        ServerMsg::TermBytes(TermBytes {
            terminal_id: TerminalId::new("tab_1:primary"),
            offset: 512,
            data: vec![0x1b, b'[', b'3', b'1', b'm', 0xff, 0x00],
            truncated: true,
        }),
        ServerMsg::Ack(Ack {
            seq: 9,
            outcome: AckOutcome::Ignored,
            detail: Some("read-only viewer".into()),
        }),
        ServerMsg::Error(WireError::seat_held(seat_info())),
        ServerMsg::Shutdown {
            reason: ShutdownReason::HostQuit,
            self_initiated: true,
            detail: Some("Ctrl-q from this browser".into()),
        },
    ];
    msgs.extend(all_deltas().into_iter().map(ServerMsg::Delta));
    msgs
}

/// Every [`ClientMsg`] variant except the catch-all.
fn all_client_msgs() -> Vec<ClientMsg> {
    vec![
        ClientMsg::Attach(Attach {
            protocol_version: PROTOCOL_VERSION,
            seat: SeatRequest::TakeOver,
            cursors: vec![TermCursor {
                terminal_id: TerminalId::new("tab_1:primary"),
                next_offset: 4096,
            }],
            resume_viewer: Some(ViewerId::new("view_0")),
            viewport: Some(Viewport {
                cols: 200,
                rows: 60,
            }),
            client: Some(ClientInfo {
                user_agent: Some("Chrome on macOS".into()),
                label: None,
            }),
        }),
        ClientMsg::Input(Input {
            seq: 18,
            terminal_id: TerminalId::new("tab_1:primary"),
            data: vec![0x0d],
        }),
        ClientMsg::Resize(Resize {
            viewport: Viewport {
                cols: 200,
                rows: 60,
            },
        }),
        ClientMsg::Command(Command {
            seq: 19,
            name: command::SELECT_SESSION.into(),
            args: Some(json!({ "session_id": "tab_2" })),
        }),
    ]
}

// ---------------------------------------------------------------------------
// 1. Round trips
// ---------------------------------------------------------------------------

#[test]
fn every_server_msg_variant_round_trips() {
    for msg in all_server_msgs() {
        assert_eq!(round_trip(&msg), msg, "server frame did not round-trip");
    }
}

#[test]
fn every_client_msg_variant_round_trips() {
    for msg in all_client_msgs() {
        assert_eq!(round_trip(&msg), msg, "client frame did not round-trip");
    }
}

#[test]
fn every_delta_variant_round_trips_and_is_tagged() {
    for delta in all_deltas() {
        let value = serde_json::to_value(&delta).unwrap();
        assert!(
            value.get("change").and_then(|c| c.as_str()).is_some(),
            "every delta must carry a `change` tag: {value}"
        );
        assert_eq!(round_trip(&delta), delta);
    }
}

#[test]
fn server_frames_are_internally_tagged_with_flattened_payloads() {
    // The tag sits beside the payload's own fields, not above a nested object —
    // the same convention as the phone protocol, and what the TS mirror expects.
    let value = serde_json::to_value(ServerMsg::TermBytes(TermBytes::live(
        TerminalId::new("t1"),
        7,
        vec![b'h', b'i'],
    )))
    .unwrap();
    assert_eq!(value["type"], "term_bytes");
    assert_eq!(value["terminal_id"], "t1");
    assert_eq!(value["offset"], 7);
    assert!(
        value.get("0").is_none(),
        "the newtype payload must not be nested: {value}"
    );
}

#[test]
fn ids_are_plain_json_strings() {
    assert_eq!(
        serde_json::to_value(TerminalId::new("tab_1:primary")).unwrap(),
        json!("tab_1:primary")
    );
    assert_eq!(
        serde_json::to_value(TabId("tab_1".into())).unwrap(),
        json!("tab_1")
    );
}

// ---------------------------------------------------------------------------
// 2. Version negotiation (D9, turn 2 §4, remote-control-l7ya)
// ---------------------------------------------------------------------------

#[test]
fn protocol_version_is_two_and_the_whole_supported_range() {
    // v2 is D14 as revised: `Seat` and `SeatRequest` are closed vocabularies and
    // both grew a member the peer must understand, which the module's
    // forward-compatibility policy makes a bump by definition. There is still no
    // range, because server and SPA ship in one binary (D9) and a stale tab is
    // told to reload rather than served a half-spoken protocol.
    assert_eq!(PROTOCOL_VERSION, 2);
    assert_eq!(MIN_SUPPORTED_VERSION, 2);
    assert_eq!(MAX_SUPPORTED_VERSION, 2);
    // That the preferred version sits inside the advertised range is asserted at
    // compile time in `protocol.rs`, not here.
}

#[test]
fn matching_version_is_accepted() {
    assert_eq!(check_version(PROTOCOL_VERSION), Ok(PROTOCOL_VERSION));
}

#[test]
fn mismatched_version_is_representable_and_detectable() {
    // A stale tab left open across `flightdeck update`: the page still speaks
    // v1's seat model, this host speaks v2's. Exactly the case D14's revision
    // creates, and the reason it is a bump rather than an additive field.
    let err = check_version(1).expect_err("v1 must not be accepted by a v2 host");
    assert_eq!(
        err,
        VersionMismatch {
            local: 2,
            peer: 1,
            min_supported: 2,
            max_supported: 2,
        }
    );
    // Newer than our ceiling is equally a mismatch — there is no downgrade path,
    // because server and SPA ship together.
    assert!(check_version(3).is_err());

    // And it is representable on the wire, with the numbers the browser needs.
    let frame = ServerMsg::Error(WireError::version_mismatch(err));
    let value = serde_json::to_value(&frame).unwrap();
    assert_eq!(value["type"], "error");
    assert_eq!(value["code"], "version_mismatch");
    assert_eq!(value["version"]["peer"], 1);
    assert_eq!(value["version"]["max_supported"], 2);
    assert_eq!(round_trip(&frame), frame);
}

#[test]
fn snapshot_carries_the_version_for_the_browser_to_compare() {
    // The browser's own detection path: compare the snapshot against its
    // baked-in constant. A newer host talking to this build is detectable.
    let mut snap = snapshot();
    snap.protocol_version = PROTOCOL_VERSION + 1;
    let value = serde_json::to_value(ServerMsg::Snapshot(snap)).unwrap();
    let parsed: ServerMsg = serde_json::from_value(value).unwrap();
    let ServerMsg::Snapshot(parsed) = parsed else {
        panic!("expected a snapshot");
    };
    assert_ne!(parsed.protocol_version, PROTOCOL_VERSION);
    assert!(check_version(parsed.protocol_version).is_err());
}

// ---------------------------------------------------------------------------
// 3. Byte cursors (D2, Q2, Q3)
// ---------------------------------------------------------------------------

#[test]
fn term_bytes_carries_an_offset_on_every_frame() {
    let frame = TermBytes::live(
        TerminalId::new("t1"),
        1_048_576,
        vec![0xde, 0xad, 0xbe, 0xef],
    );
    let value = serde_json::to_value(&frame).unwrap();
    assert_eq!(value["offset"], 1_048_576);
    assert_eq!(value["truncated"], false);
    // Base64, not a JSON string of the raw bytes: 0xde 0xad 0xbe 0xef is not
    // valid UTF-8 and must survive anyway.
    assert_eq!(value["data"], "3q2+7w==");
    assert_eq!(round_trip(&frame), frame);
    assert_eq!(frame.data, vec![0xde, 0xad, 0xbe, 0xef]);
}

#[test]
fn next_offset_is_the_cursor_the_viewer_sends_back() {
    let frame = TermBytes::live(TerminalId::new("t1"), 100, vec![0; 40]);
    assert_eq!(frame.next_offset(), 140);

    // Round-tripping through the cursor type is exactly the reconnect path.
    let cursor = TermCursor {
        terminal_id: frame.terminal_id.clone(),
        next_offset: frame.next_offset(),
    };
    assert_eq!(round_trip(&cursor), cursor);
    let attach = Attach {
        protocol_version: PROTOCOL_VERSION,
        seat: SeatRequest::Write,
        cursors: vec![cursor],
        resume_viewer: Some(ViewerId::new("view_0")),
        viewport: None,
        client: None,
    };
    let value = serde_json::to_value(&attach).unwrap();
    assert_eq!(value["cursors"][0]["next_offset"], 140);
}

#[test]
fn truncated_marks_a_cursor_that_aged_out_of_the_ring() {
    // The viewer asked to resume at 100; the ring only still holds from 4096,
    // so output was lost and the frame must say so (Q2/Q3).
    let asked_for = 100;
    let ring_starts_at = 4096;
    let frame = TermBytes {
        terminal_id: TerminalId::new("t1"),
        offset: ring_starts_at,
        data: b"replay".to_vec(),
        truncated: ring_starts_at > asked_for,
    };
    assert!(frame.truncated, "a lost-output resume must be flagged");
    assert_eq!(round_trip(&frame), frame);

    // `truncated` defaults to false so a live frame need not carry it, and an
    // older peer's frame without the field still parses.
    let live: TermBytes = serde_json::from_value(json!({
        "terminal_id": "t1",
        "offset": 4102,
        "data": "aGk="
    }))
    .unwrap();
    assert!(!live.truncated);
    assert_eq!(live.data, b"hi");
}

#[test]
fn terminal_view_lets_a_viewer_detect_loss_before_any_bytes_arrive() {
    // replay_from > the viewer's saved cursor means output is already gone; the
    // browser can render "you missed output" from the snapshot alone.
    let view = terminal_view();
    let saved_cursor = 100;
    assert!(view.replay_from > saved_cursor);
    assert!(view.byte_len >= view.replay_from);
    assert_eq!(round_trip(&view), view);
}

// ---------------------------------------------------------------------------
// 4. Geometry (D4)
// ---------------------------------------------------------------------------

#[test]
fn snapshot_carries_the_hosts_authoritative_geometry() {
    let value = serde_json::to_value(ServerMsg::Snapshot(snapshot())).unwrap();
    assert_eq!(value["geometry"]["cols"], 120);
    assert_eq!(value["geometry"]["rows"], 34);
    // And per terminal, so a non-selected terminal letterboxes correctly too.
    assert_eq!(
        value["projects"][0]["sessions"][0]["terminals"][0]["geometry"]["rows"],
        34
    );
}

#[test]
fn resize_is_viewport_only_and_cannot_name_a_pty() {
    // The D4 invariant, enforced by the type's shape rather than by a rule the
    // server has to remember: a Resize frame has nowhere to put a target, so it
    // cannot express "resize that PTY".
    let frame = ClientMsg::Resize(Resize {
        viewport: Viewport {
            cols: 200,
            rows: 60,
        },
    });
    let value = serde_json::to_value(&frame).unwrap();
    let object = value.as_object().expect("a JSON object");
    let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec!["type", "viewport"],
        "Resize must carry nothing but its viewport: {value}"
    );
    for forbidden in ["terminal_id", "session_id", "project_id", "cols", "rows"] {
        assert!(
            !object.contains_key(forbidden),
            "Resize must not carry `{forbidden}` — the desktop owns PTY geometry (D4)"
        );
    }
    assert_eq!(value["viewport"]["cols"], 200);
    assert_eq!(round_trip(&frame), frame);
}

#[test]
fn geometry_converts_from_the_desktops_pty_size() {
    // The host's grid comes from PtySize; the conversion keeps the two honest.
    let geo: Geometry = PtySize {
        rows: 34,
        cols: 120,
    }
    .into();
    assert_eq!(geo, geometry());
}

// ---------------------------------------------------------------------------
// 5. Seats (D14)
// ---------------------------------------------------------------------------

#[test]
fn the_three_attach_intents_are_distinct_on_the_wire() {
    let spellings: Vec<String> = [
        SeatRequest::Write,
        SeatRequest::TakeOver,
        SeatRequest::Observe,
    ]
    .iter()
    .map(|s| {
        serde_json::to_value(s)
            .unwrap()
            .as_str()
            .unwrap()
            .to_owned()
    })
    .collect();
    assert_eq!(spellings, vec!["write", "take_over", "observe"]);
}

#[test]
fn a_refused_keystroke_names_the_writer_that_is_typing() {
    // A refusal that says only "that did not work" is indistinguishable from a
    // broken host, and 2f exists precisely so that neither person has to wonder
    // why the keys stopped working. So the refusal carries the holder, whole.
    let err = WireError::seat_held(seat_info());
    assert_eq!(err.code, ErrorCode::SeatHeld);
    assert!(err.message.contains("192.168.2.20"));
    assert!(
        err.message.contains("typing"),
        "the sentence names the act, not the failure: {}",
        err.message
    );
    let holder = err.incumbent.clone().expect("the holder must be described");
    assert_eq!(holder.seat, Seat::Writing);
    assert!(holder.holds_input, "and must be the one holding the turn");
    assert!(holder.since_ms > 0, "how long it has been connected");
    assert_eq!(round_trip(&err), err);
}

#[test]
fn a_seat_carries_its_three_facts_in_three_fields_never_one_string_to_split() {
    // Artboard 2f's arriving-viewer panel lists address / browser / connected as
    // three rows. The compact chip (2c) wants one line. Both are served, and the
    // browser is never asked to take the line back apart — a user-agent string is
    // attacker-supplied and may contain the ` · ` separator, so a split is a
    // parse the attacker gets to steer.
    let value = serde_json::to_value(seat_info()).unwrap();
    assert_eq!(value["address"], "192.168.2.20");
    assert_eq!(value["user_agent_label"], "Chrome on macOS");
    assert_eq!(value["label"], "192.168.2.20 · Chrome on macOS");
    assert!(value["since_ms"].as_i64().unwrap() > 0);
    assert_eq!(round_trip(&seat_info()), seat_info());

    // A separator inside the browser's own claim is exactly the case a
    // browser-side split gets wrong, and exactly the case the wire gets right.
    let hostile = SeatInfo {
        user_agent_label: Some("Chrome · 10.0.0.1 on macOS".into()),
        ..seat_info()
    };
    let back = round_trip(&hostile);
    assert_eq!(
        back.address.as_deref(),
        Some("192.168.2.20"),
        "the host-observed address is never displaced by anything the client said"
    );
    assert_eq!(back.user_agent_label, hostile.user_agent_label);
}

#[test]
fn a_seat_without_the_split_fields_decodes_to_unknown_not_to_a_guess() {
    // Both fields are additive `#[serde(default)]`, in the idiom the surrounding
    // types already use, so a payload from before the split still decodes — and
    // it decodes to "we do not know" rather than to an invented address. 2f then
    // drops the row instead of printing a placeholder.
    let mut value = serde_json::to_value(seat_info()).unwrap();
    let object = value.as_object_mut().unwrap();
    object.remove("address");
    object.remove("user_agent_label");
    let without: SeatInfo = serde_json::from_value(value).unwrap();
    assert_eq!(without.address, None);
    assert_eq!(without.user_agent_label, None);
    assert_eq!(
        without.label, "192.168.2.20 · Chrome on macOS",
        "the one-line chip label still works, which is why it stays"
    );
}

#[test]
fn the_desktop_row_has_no_address_because_it_arrived_over_no_socket() {
    let desktop = SeatInfo {
        viewer_id: None,
        label: "desktop".into(),
        address: None,
        user_agent_label: None,
        seat: Seat::Writing,
        holds_input: false,
        since_ms: 1_699_999_000_000,
        is_you: false,
    };
    let value = serde_json::to_value(&desktop).unwrap();
    assert!(value["address"].is_null(), "never a fabricated `localhost`");
    assert!(value["user_agent_label"].is_null());
    assert_eq!(round_trip(&desktop), desktop);
}

#[test]
fn losing_the_input_lock_is_a_seat_delta_not_a_shutdown() {
    // Nobody is disconnected and nobody is demoted by a takeover under D14 as
    // revised: the interrupted writer keeps its seat and its socket, and learns
    // that the *turn* moved from `holds_input`. That is what lets it either wait
    // for the holder to go quiet or watch read-only instead of fighting.
    let delta = Delta::Seats {
        you: Seat::Writing,
        seats: vec![SeatInfo {
            viewer_id: Some(ViewerId::new("view_2")),
            label: "192.168.2.31 · Safari on iPadOS".into(),
            address: Some("192.168.2.31".into()),
            user_agent_label: Some("Safari on iPadOS".into()),
            seat: Seat::Writing,
            holds_input: true,
            since_ms: 1_700_000_100_000,
            is_you: false,
        }],
        server_time_ms: 1_700_000_112_000,
        // The ordinary case: the turn moved, but not because anybody confirmed
        // an override. See the dedicated test below.
        you_were_preempted: false,
    };
    let value = serde_json::to_value(&delta).unwrap();
    assert_eq!(value["change"], "seats");
    assert_eq!(
        value["you"], "writing",
        "the recipient still holds a writer's seat — only the turn moved"
    );
    assert_eq!(value["seats"][0]["holds_input"], true);
    assert_eq!(round_trip(&delta), delta);
}

#[test]
fn a_role_and_a_turn_are_two_fields_because_they_are_two_facts() {
    // Protocol v1 merged them into one `controlling` flag, and that is exactly
    // what D14's revision had to undo: the merged flag cannot express "three
    // writers, one of them mid-burst". Several rows say `writing`; at most one
    // says `holds_input`.
    let seats = [
        SeatInfo {
            viewer_id: None,
            label: "desktop".into(),
            address: None,
            user_agent_label: None,
            seat: Seat::Writing,
            holds_input: false,
            since_ms: 1_699_999_000_000,
            is_you: false,
        },
        SeatInfo {
            seat: Seat::Writing,
            holds_input: true,
            ..seat_info()
        },
        SeatInfo {
            viewer_id: Some(ViewerId::new("view_9")),
            label: "192.168.2.31".into(),
            address: Some("192.168.2.31".into()),
            user_agent_label: None,
            seat: Seat::Observing,
            holds_input: false,
            since_ms: 1_700_000_100_000,
            is_you: false,
        },
    ];
    assert_eq!(
        seats.iter().filter(|s| s.seat == Seat::Writing).count(),
        2,
        "more than one writer is the normal case now"
    );
    assert_eq!(
        seats.iter().filter(|s| s.holds_input).count(),
        1,
        "and exactly one of them has the turn"
    );

    // Additive and defaulted, in the idiom the rest of these types use. `false`
    // from an older host is the honest reading: not "the lock is free" but
    // "this row is not the one holding it", which is true of every row there.
    let mut value = serde_json::to_value(&seats[1]).unwrap();
    value.as_object_mut().unwrap().remove("holds_input");
    let without: SeatInfo = serde_json::from_value(value).unwrap();
    assert!(!without.holds_input);
}

#[test]
fn a_seat_delta_carries_the_clock_its_rows_are_dated_against() {
    // A `since_ms` with no reference clock is a number the browser cannot
    // honestly use: dating it against `Date.now()` measures a host instant with
    // a local clock that may be wrong. `Snapshot` has always paired the two, and
    // 2f's `connected` row was silently dropped on the delta path for want of
    // the same pairing.
    let delta = Delta::Seats {
        you: Seat::Observing,
        seats: vec![seat_info()],
        server_time_ms: 1_700_000_012_000,
        you_were_preempted: false,
    };
    let value = serde_json::to_value(&delta).unwrap();
    assert_eq!(value["server_time_ms"], 1_700_000_012_000i64);
    assert_eq!(
        value["seats"][0]["since_ms"], 1_700_000_000_000i64,
        "12 seconds, measured entirely on the host's clock"
    );
    assert_eq!(round_trip(&delta), delta);

    // Additive and defaulted, in the idiom the rest of these types use: a host
    // from before this field still decodes, and decodes to `0` — which the
    // browser reads as "no clock was sent" and renders the row without its
    // `connected` line, never with a fabricated or negative duration.
    let mut older = serde_json::to_value(&delta).unwrap();
    older.as_object_mut().unwrap().remove("server_time_ms");
    let without: Delta = serde_json::from_value(older).unwrap();
    let Delta::Seats {
        server_time_ms,
        seats,
        ..
    } = &without
    else {
        panic!("still a seat delta");
    };
    assert_eq!(
        *server_time_ms, 0,
        "absent is not a time, and never a guess"
    );
    assert_eq!(seats.len(), 1, "the rows themselves survive intact");
}

#[test]
fn a_seat_delta_says_whether_the_recipient_was_preempted_on_purpose() {
    // The lock moves on every ordinary hand-off, so "it left me" cannot be what
    // opens 2f's evicted panel — that would be a modal every time the other
    // person starts a sentence. The distinguishing fact is intent, which exists
    // only here, at the moment of the act, so the frame carries it.
    let delta = Delta::Seats {
        you: Seat::Writing,
        seats: vec![seat_info()],
        server_time_ms: 1_700_000_012_000,
        you_were_preempted: true,
    };
    let value = serde_json::to_value(&delta).unwrap();
    assert_eq!(value["you_were_preempted"], true);
    assert_eq!(round_trip(&delta), delta);

    // Additive and defaulted, which is precisely why this field is *not* a
    // `PROTOCOL_VERSION` bump: it is neither a required field nor a closed
    // vocabulary growing a member. `false` is the honest reading for a host that
    // never sends it — "this host reports no preemptions" — which leaves the
    // browser exactly where it was before the field existed rather than
    // guessing from the rows.
    let mut older = serde_json::to_value(&delta).unwrap();
    older.as_object_mut().unwrap().remove("you_were_preempted");
    let without: Delta = serde_json::from_value(older).unwrap();
    let Delta::Seats {
        you_were_preempted,
        seats,
        ..
    } = &without
    else {
        panic!("still a seat delta");
    };
    assert!(
        !you_were_preempted,
        "absent is 'nobody said this was deliberate', never 'it was'"
    );
    assert_eq!(seats.len(), 1, "the rows themselves survive intact");
}

#[test]
fn read_only_input_is_acked_not_dropped() {
    // §5.1: a keystroke must never disappear silently. An observer's input gets
    // an explicit `ignored`.
    let ack = Ack {
        seq: 3,
        outcome: AckOutcome::Ignored,
        detail: Some("this tab is watching read-only".into()),
    };
    let value = serde_json::to_value(&ack).unwrap();
    assert_eq!(value["outcome"], "ignored");
    assert_eq!(round_trip(&ack), ack);
}

// ---------------------------------------------------------------------------
// 6. Shutdown (Q5)
// ---------------------------------------------------------------------------

#[test]
fn shutdown_distinguishes_a_deliberate_quit_from_a_network_failure() {
    // A network failure produces no frame at all — that is the point. Any
    // Shutdown frame therefore means "deliberate", and the reason says which.
    for reason in [
        ShutdownReason::HostQuit,
        ShutdownReason::ServerStopped,
        ShutdownReason::TokenRevoked,
        ShutdownReason::Restarting,
        ShutdownReason::Unknown,
    ] {
        let frame = ServerMsg::Shutdown {
            reason,
            self_initiated: false,
            detail: None,
        };
        assert_eq!(round_trip(&frame), frame, "{reason:?} did not round-trip");
    }

    let value = serde_json::to_value(ServerMsg::Shutdown {
        reason: ShutdownReason::ServerStopped,
        self_initiated: false,
        detail: Some("Stop Web Interface".into()),
    })
    .unwrap();
    assert_eq!(value["type"], "shutdown");
    assert_eq!(value["reason"], "server_stopped");
}

#[test]
fn shutdown_tells_i_asked_for_this_from_the_host_went_away() {
    let mine = ServerMsg::Shutdown {
        reason: ShutdownReason::HostQuit,
        self_initiated: true,
        detail: Some("Ctrl-q from this browser".into()),
    };
    let theirs = ServerMsg::Shutdown {
        reason: ShutdownReason::HostQuit,
        self_initiated: false,
        detail: None,
    };
    assert_ne!(mine, theirs, "the two screens must be distinguishable");
    assert_eq!(serde_json::to_value(&mine).unwrap()["self_initiated"], true);
    assert_eq!(round_trip(&mine), mine);

    // The field defaults, so a frame from an older host reads as "not mine" —
    // which is the safe direction: a failure screen, never a false "you did
    // this".
    let parsed: ServerMsg =
        serde_json::from_value(json!({ "type": "shutdown", "reason": "host_quit" })).unwrap();
    assert_eq!(parsed, theirs);
}

#[test]
fn only_a_restart_asks_the_browser_to_keep_retrying() {
    assert!(ShutdownReason::Restarting.should_retry());
    for reason in [
        ShutdownReason::HostQuit,
        ShutdownReason::ServerStopped,
        ShutdownReason::TokenRevoked,
        // An unrecognised final word means stop, not spin.
        ShutdownReason::Unknown,
    ] {
        assert!(!reason.should_retry(), "{reason:?} must not retry");
    }
}

// ---------------------------------------------------------------------------
// 7. Status vocabulary — the D12 anti-drift guard
// ---------------------------------------------------------------------------

#[test]
fn every_interpreted_status_survives_the_wire() {
    // The web protocol reuses `InterpretedStatus` through its label rather than
    // forking a parallel enum. That only holds if every variant round-trips, so
    // this list is the guard: a new status added to the domain type must be
    // added to `as_str`/`from_str_lossy` or this fails.
    let all = [
        InterpretedStatus::Starting,
        InterpretedStatus::Running,
        InterpretedStatus::Working,
        InterpretedStatus::Idle,
        InterpretedStatus::WaitingForInput,
        InterpretedStatus::NeedsAttention,
        InterpretedStatus::Completed,
        InterpretedStatus::Failed,
        InterpretedStatus::Stopped,
        InterpretedStatus::SessionLost,
        InterpretedStatus::Recovered,
        InterpretedStatus::Unknown,
    ];
    for interpreted in all {
        let mut s = status();
        s.interpreted = interpreted;
        s.bucket = StatusBucket::from_interpreted(interpreted);
        let value = serde_json::to_value(&s).unwrap();
        assert_eq!(value["interpreted"], interpreted.as_str());
        assert_eq!(round_trip(&s), s, "{interpreted:?} did not round-trip");
    }
}

#[test]
fn every_manual_status_survives_the_wire_and_none_means_cleared() {
    for manual in [
        ManualStatus::InProgress,
        ManualStatus::Waiting,
        ManualStatus::Blocked,
        ManualStatus::Done,
    ] {
        let mut s = status();
        s.manual = Some(manual);
        assert_eq!(serde_json::to_value(&s).unwrap()["manual"], manual.as_str());
        assert_eq!(round_trip(&s), s);
    }
    let mut cleared = status();
    cleared.manual = None;
    assert!(serde_json::to_value(&cleared).unwrap()["manual"].is_null());
    assert_eq!(round_trip(&cleared), cleared);

    // An override label we do not know reads as "no override" rather than as a
    // guess, because manual takes colour priority in the design.
    let parsed: SessionStatus = serde_json::from_value(json!({
        "interpreted": "idle",
        "manual": "vibing",
        "bucket": "idle",
        "running_time_secs": 0
    }))
    .unwrap();
    assert_eq!(parsed.manual, None);
}

#[test]
fn unknown_stays_unknown_and_is_never_folded_into_idle() {
    // turn 2 §5.1: an agent with no lifecycle hooks must not be reported idle.
    assert_eq!(
        StatusBucket::from_interpreted(InterpretedStatus::Unknown),
        StatusBucket::Unknown
    );
    assert_ne!(StatusBucket::Unknown, StatusBucket::Idle);

    // And `unknown -> unknown` is a legal feed row.
    let mut event = activity_event();
    event.from = InterpretedStatus::Unknown;
    event.to = InterpretedStatus::Unknown;
    event.tier = ActivityTier::Quiet;
    let value = serde_json::to_value(&event).unwrap();
    assert_eq!(value["from"], "unknown");
    assert_eq!(value["to"], "unknown");
    assert_eq!(round_trip(&event), event);
}

#[test]
fn status_buckets_match_the_designs_sidebar_labels() {
    use InterpretedStatus as I;
    for (interpreted, bucket) in [
        (I::Starting, StatusBucket::InProgress),
        (I::Running, StatusBucket::InProgress),
        (I::Working, StatusBucket::InProgress),
        (I::Idle, StatusBucket::Idle),
        (I::Completed, StatusBucket::Idle),
        (I::Stopped, StatusBucket::Idle),
        (I::Recovered, StatusBucket::Idle),
        (I::WaitingForInput, StatusBucket::Waiting),
        (I::NeedsAttention, StatusBucket::Waiting),
        (I::Failed, StatusBucket::Error),
        (I::SessionLost, StatusBucket::Error),
        (I::Unknown, StatusBucket::Unknown),
    ] {
        assert_eq!(
            StatusBucket::from_interpreted(interpreted),
            bucket,
            "{interpreted:?}"
        );
    }
}

#[test]
fn attention_beats_busy_in_the_project_dot() {
    // The brief's precedence rule, and the activity chip's three tiers.
    assert_eq!(
        StatusBucket::rollup([StatusBucket::Idle, StatusBucket::InProgress]),
        Some(StatusBucket::InProgress)
    );
    assert_eq!(
        StatusBucket::rollup([
            StatusBucket::InProgress,
            StatusBucket::Waiting,
            StatusBucket::Idle
        ]),
        Some(StatusBucket::Waiting)
    );
    assert_eq!(
        StatusBucket::rollup([StatusBucket::Error, StatusBucket::InProgress]),
        Some(StatusBucket::Error)
    );
    // Unknown never outranks a status we could actually determine.
    assert_eq!(
        StatusBucket::rollup([StatusBucket::Unknown, StatusBucket::Idle]),
        Some(StatusBucket::Idle)
    );
    assert_eq!(StatusBucket::rollup([]), None);

    assert_eq!(
        ActivityTier::for_bucket(StatusBucket::Waiting),
        ActivityTier::Attention
    );
    assert_eq!(
        ActivityTier::for_bucket(StatusBucket::Error),
        ActivityTier::Attention
    );
    assert_eq!(
        ActivityTier::for_bucket(StatusBucket::Idle),
        ActivityTier::Finished
    );
    assert_eq!(
        ActivityTier::for_bucket(StatusBucket::Unknown),
        ActivityTier::Quiet
    );
}

#[test]
fn session_status_is_built_from_the_desktops_own_display_status() {
    // The seam that stops the two surfaces disagreeing: the wire status is
    // *derived* from `DisplayStatus`, never assembled by hand.
    let display = combine_status(
        ProcessState::Running,
        Some(InterpretedStatus::Working),
        Some(ManualStatus::InProgress),
    );
    let wire = SessionStatus::from_display(display, 12);
    assert_eq!(wire.interpreted, InterpretedStatus::Working);
    assert_eq!(wire.manual, Some(ManualStatus::InProgress));
    assert_eq!(wire.bucket, StatusBucket::InProgress);
    assert_eq!(wire.running_time_secs, 12);
    assert_eq!(round_trip(&wire), wire);
}

#[test]
fn terminal_role_mirrors_the_desktops_terminal_kind() {
    for (kind, role) in [
        (TerminalKind::Primary, TerminalRole::Primary),
        (TerminalKind::Agent, TerminalRole::Agent),
        (TerminalKind::Child, TerminalRole::Shell),
    ] {
        assert_eq!(TerminalRole::from(kind), role);
    }
}

#[test]
fn the_git_bar_does_not_drift_from_the_phone_protocols_indicators() {
    // D12's other accepted cost. Every key the phone protocol emits for the same
    // facts must appear here with the same value, so a rename on either side
    // fails a test instead of quietly producing two vocabularies.
    use flightdeck_remote_protocol::GitIndicators;

    let phone = GitIndicators {
        branch: Some("flightdeck/fix-login".into()),
        added: 3,
        modified: 2,
        removed: 1,
        ahead: 0,
        behind: 0,
        drift: 3,
        has_upstream: true,
    };
    let web = git_bar();
    let phone_json = serde_json::to_value(&phone).unwrap();
    let web_json = serde_json::to_value(&web).unwrap();
    for (key, value) in phone_json.as_object().unwrap() {
        assert_eq!(
            web_json.get(key),
            Some(value),
            "`{key}` differs between the phone and web git shapes"
        );
    }
    // The two web-only facts the phone row has no use for.
    assert_eq!(web_json["files_changed"], 6);
    assert_eq!(web_json["collected"], true);
    assert_eq!(phone.is_clean(), web.is_clean());
}

#[test]
fn not_collected_is_not_clean() {
    // `git: ?` and `clean` mean opposite things; the wire must be able to say so.
    let uncollected = GitBar::default();
    assert!(!uncollected.collected);
    assert!(
        uncollected.is_clean(),
        "an empty count set looks clean, which is exactly why `collected` exists"
    );
    assert_eq!(round_trip(&uncollected), uncollected);
}

// ---------------------------------------------------------------------------
// 8. Input queue (turn 2 §5.1)
// ---------------------------------------------------------------------------

#[test]
fn input_carries_a_seq_the_ack_answers() {
    let input = Input {
        seq: 7,
        terminal_id: TerminalId::new("t1"),
        data: b"ls\r".to_vec(),
    };
    let value = serde_json::to_value(ClientMsg::Input(input.clone())).unwrap();
    assert_eq!(value["type"], "input");
    assert_eq!(value["seq"], 7);
    assert_eq!(value["data"], "bHMN");

    let ack = Ack {
        seq: input.seq,
        outcome: AckOutcome::Applied,
        detail: None,
    };
    assert_eq!(round_trip(&ack).seq, 7);
}

#[test]
fn snapshot_tells_a_resuming_viewer_what_already_landed() {
    // Without this the browser must choose between losing keystrokes and
    // doubling them. §5.1 permits neither.
    let snap = snapshot();
    assert_eq!(snap.last_input_seq, 17);
    let queued = [16u64, 17, 18, 19];
    let replay: Vec<u64> = queued
        .iter()
        .copied()
        .filter(|seq| *seq > snap.last_input_seq)
        .collect();
    assert_eq!(replay, vec![18, 19]);
}

// ---------------------------------------------------------------------------
// 9. Dialogs (D13) and the M2 command door
// ---------------------------------------------------------------------------

#[test]
fn a_dialog_carries_the_origin_label_that_makes_it_acceptable() {
    let value = serde_json::to_value(Delta::DialogOpened(dialog_view())).unwrap();
    assert_eq!(value["change"], "dialog_opened");
    assert_eq!(value["origin"]["origin"], "browser");
    assert_eq!(value["origin"]["label"], "192.168.2.20");

    let desktop = DialogView {
        origin: DialogOrigin::Desktop,
        ..dialog_view()
    };
    let value = serde_json::to_value(&desktop).unwrap();
    assert_eq!(value["origin"]["origin"], "desktop");
    assert_eq!(round_trip(&desktop), desktop);
}

#[test]
fn command_names_are_open_so_m2_needs_no_version_bump() {
    // An M2 command an M1 host has never heard of must still *parse*; the host
    // answers `not_supported` rather than dropping the socket.
    let frame: ClientMsg = serde_json::from_value(json!({
        "type": "command",
        "seq": 4,
        "name": "git_abandon_worktree",
        "args": { "session_id": "tab_1", "confirm_name": "fix-login" }
    }))
    .unwrap();
    let ClientMsg::Command(cmd) = &frame else {
        panic!("expected a command, got {frame:?}");
    };
    assert_eq!(cmd.name, "git_abandon_worktree");
    assert_eq!(cmd.args.as_ref().unwrap()["confirm_name"], "fix-login");
    assert_eq!(round_trip(&frame), frame);

    let refusal = WireError::new(ErrorCode::NotSupported, "not in M1");
    assert_eq!(
        serde_json::to_value(&refusal).unwrap()["code"],
        "not_supported"
    );

    // M1's own names are shared constants, not string literals sprayed around.
    assert_eq!(command::SELECT_SESSION, "select_session");
    assert_eq!(command::REQUEST_SNAPSHOT, "request_snapshot");
}

// ---------------------------------------------------------------------------
// 10. Forward compatibility (the documented policy)
// ---------------------------------------------------------------------------

#[test]
fn an_unknown_server_frame_type_becomes_unrecognized() {
    let parsed: ServerMsg = serde_json::from_value(json!({
        "type": "hologram",
        "whatever": [1, 2, 3]
    }))
    .expect("an unknown frame must parse, not fail");
    assert_eq!(parsed, ServerMsg::Unrecognized);
}

#[test]
fn an_unknown_client_frame_type_becomes_unrecognized() {
    let parsed: ClientMsg =
        serde_json::from_value(json!({ "type": "telepathy", "seq": 1 })).unwrap();
    assert_eq!(parsed, ClientMsg::Unrecognized);
}

#[test]
fn an_unknown_delta_kind_becomes_unrecognized() {
    let parsed: ServerMsg = serde_json::from_value(json!({
        "type": "delta",
        "change": "vibes_changed",
        "vibes": "immaculate"
    }))
    .unwrap();
    assert_eq!(parsed, ServerMsg::Delta(Delta::Unrecognized));
}

#[test]
fn unknown_extra_fields_are_ignored_everywhere() {
    // No type in this module uses `deny_unknown_fields`, so a newer peer may add
    // fields freely. Checked on a frame from each direction.
    let server: ServerMsg = serde_json::from_value(json!({
        "type": "term_bytes",
        "terminal_id": "t1",
        "offset": 9,
        "data": "aGk=",
        "compression": "zstd",
        "nested": { "future": true }
    }))
    .unwrap();
    assert_eq!(
        server,
        ServerMsg::TermBytes(TermBytes::live(TerminalId::new("t1"), 9, b"hi".to_vec()))
    );

    let client: ClientMsg = serde_json::from_value(json!({
        "type": "attach",
        "protocol_version": 1,
        "seat": "observe",
        "shiny_new_field": 1
    }))
    .unwrap();
    let ClientMsg::Attach(attach) = client else {
        panic!("expected an attach");
    };
    assert_eq!(attach.seat, SeatRequest::Observe);
    // Everything added after v1 defaults, so a minimal frame is still valid.
    assert!(attach.cursors.is_empty());
    assert_eq!(attach.resume_viewer, None);
    assert_eq!(attach.viewport, None);
}

#[test]
fn an_unknown_error_code_degrades_but_keeps_its_message() {
    // The message is the half the user reads, so an unknown code must not cost
    // it — which is why `ErrorCode` goes through a lossy string rather than
    // being a tagged enum with a payload-free catch-all.
    let parsed: WireError = serde_json::from_value(json!({
        "code": "quota_exceeded",
        "message": "the host says something we do not understand"
    }))
    .unwrap();
    assert_eq!(parsed.code, ErrorCode::Unknown);
    assert_eq!(
        parsed.message,
        "the host says something we do not understand"
    );
    assert_eq!(
        serde_json::to_value(&parsed).unwrap()["code"],
        "unknown",
        "we re-emit the degraded code honestly rather than echoing a code we did not understand"
    );
}

#[test]
fn an_unknown_shutdown_reason_still_stops_the_retry_loop() {
    let parsed: ServerMsg = serde_json::from_value(json!({
        "type": "shutdown",
        "reason": "abducted",
        "detail": "who knows"
    }))
    .unwrap();
    let ServerMsg::Shutdown { reason, .. } = parsed else {
        panic!("expected a shutdown");
    };
    assert_eq!(reason, ShutdownReason::Unknown);
    assert!(!reason.should_retry());
}

#[test]
fn every_error_code_round_trips_through_its_wire_spelling() {
    for code in [
        ErrorCode::VersionMismatch,
        ErrorCode::Unauthorized,
        ErrorCode::RateLimited,
        ErrorCode::SeatHeld,
        ErrorCode::ReadOnly,
        ErrorCode::UnknownTarget,
        ErrorCode::NotSupported,
        ErrorCode::Internal,
        ErrorCode::Unknown,
    ] {
        assert_eq!(ErrorCode::from_str_lossy(code.as_str()), code);
        let err = WireError::new(code, "detail");
        assert_eq!(round_trip(&err), err);
    }
}

#[test]
fn every_shutdown_reason_round_trips_through_its_wire_spelling() {
    for reason in [
        ShutdownReason::HostQuit,
        ShutdownReason::ServerStopped,
        ShutdownReason::TokenRevoked,
        ShutdownReason::Restarting,
        ShutdownReason::Unknown,
    ] {
        assert_eq!(ShutdownReason::from_str_lossy(reason.as_str()), reason);
    }
}
