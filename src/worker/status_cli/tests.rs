use super::*;
use crate::worker::state::{ExitReport, SupervisorPhase, SupervisorState, SUPERVISOR_STATE_SCHEMA};
use crate::worker::status::tests::sample_runtime;
use crate::worker::status::WorkerState;

fn supervisor(phase: SupervisorPhase) -> SupervisorState {
    SupervisorState {
        schema: SUPERVISOR_STATE_SCHEMA,
        supervisor_pid: 4100,
        started_at: 1_700_000_000,
        phase,
        worker_pid: None,
        worker_started_at: None,
        restarts: 5,
        last_exit: Some(ExitReport {
            at: 1_700_000_600,
            code: Some(101),
            signal: None,
            description: "exited with code 101 (panic)".to_string(),
        }),
        gave_up_reason: Some("5 crashes within 10 minutes".to_string()),
        backoff_until: None,
    }
}

fn device(id: &str, label: &str, state: DeviceState) -> DeviceReport {
    DeviceReport {
        id: id.to_string(),
        label: label.to_string(),
        address: Some("100.64.1.9".to_string()),
        paired_at: 1_700_000_000,
        last_seen: Some(1_700_000_000 + 3 * 3_600),
        state,
        revoked: false,
    }
}

fn code(listening: bool) -> PairingCode {
    PairingCode {
        code: "4821".to_string(),
        expires_in_secs: 120,
        address: listening.then(|| "box.tail1234.ts.net:7420".to_string()),
        listening,
        not_listening_reason: (!listening).then(|| "no Tailscale address".to_string()),
    }
}

#[test]
fn status_of_a_worker_that_is_not_running() {
    let report = status::merge(None, None, false, false);
    let text = render_status(&report, false).unwrap();
    assert!(text.starts_with("FlightDeck worker: stopped\n"), "{text}");
    let json: serde_json::Value =
        serde_json::from_str(&render_status(&report, true).unwrap()).unwrap();
    assert_eq!(json["state"], "stopped");
    assert_eq!(json["pid"], serde_json::Value::Null);
    assert_eq!(json["listen"], serde_json::Value::Null);
}

#[test]
fn status_of_a_running_worker() {
    let report = status::merge(
        Some(sample_runtime()),
        Some(supervisor(SupervisorPhase::Running)),
        true,
        true,
    );
    assert_eq!(report.state, WorkerState::Running);
    let text = render_status(&report, false).unwrap();
    assert!(text.contains("running (pid 4242"), "{text}");
    assert!(text.contains("Listening: 100.64.1.2:7420"), "{text}");
    assert!(text.contains("fix-login"), "{text}");
    let pretty = render_status(&report, true).unwrap();
    assert!(pretty.ends_with("}\n"), "newline-terminated");
    let back: WorkerStatusReport = serde_json::from_str(&pretty).unwrap();
    assert_eq!(back, report);
}

#[test]
fn status_of_a_worker_whose_supervisor_gave_up() {
    let report = status::merge(
        None,
        Some(supervisor(SupervisorPhase::GaveUp)),
        false,
        false,
    );
    let text = render_status(&report, false).unwrap();
    assert!(text.starts_with("FlightDeck worker: gave up\n"), "{text}");
    assert!(
        text.contains("Gave up: 5 crashes within 10 minutes"),
        "{text}"
    );
    assert!(
        text.contains("Last exit: exited with code 101 (panic)"),
        "{text}"
    );
    let json: serde_json::Value =
        serde_json::from_str(&render_status(&report, true).unwrap()).unwrap();
    assert_eq!(json["state"], "gave_up");
    assert_eq!(json["gave_up_reason"], "5 crashes within 10 minutes");
    assert_eq!(json["supervisor_pid"], serde_json::Value::Null);
}

#[test]
fn pair_text_names_the_code_the_address_and_where_to_type_them() {
    let text = render_pair(&code(true));
    assert!(text.contains("Pairing code: 4821"), "{text}");
    assert!(text.contains("valid for 120 s, single use"), "{text}");
    assert!(
        text.contains("Address:      box.tail1234.ts.net:7420"),
        "{text}"
    );
    assert!(text.contains("Connect to remote…"), "{text}");
    assert!(!text.contains("Warning"), "{text}");
}

#[test]
fn pair_text_warns_when_not_listening() {
    let text = render_pair(&code(false));
    assert!(text.contains("Pairing code: 4821"), "{text}");
    assert!(text.contains("none yet"), "{text}");
    assert!(
        text.contains("Warning: the worker is not listening (no Tailscale address)"),
        "{text}"
    );
}

#[test]
fn devices_render_as_a_table() {
    let now = 1_700_000_000 + 2 * 86_400;
    let mut revoked = device("tok2", "studio", DeviceState::Offline);
    revoked.revoked = true;
    revoked.last_seen = None;
    revoked.address = None;
    let text = render_devices(
        &[device("tok1", "mbp", DeviceState::Attached), revoked],
        now,
    );
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines[0],
        "ID    LABEL   STATE     PAIRED  LAST SEEN  ADDRESS"
    );
    assert_eq!(
        lines[1],
        "tok1  mbp     attached  2d ago  1d ago     100.64.1.9"
    );
    assert_eq!(lines[2], "tok2  studio  revoked   2d ago  never      -");
    assert!(text.contains("flightdeck worker revoke"), "{text}");
}

#[test]
fn no_devices_says_how_to_pair_one() {
    assert_eq!(
        render_devices(&[], 0),
        "No paired Desktops. Pair one with `flightdeck worker pair`.\n"
    );
}

#[test]
fn ago_uses_the_largest_unit() {
    assert_eq!(ago(100, 100), "just now");
    assert_eq!(ago(100, 200), "just now", "a clock that moved back");
    assert_eq!(ago(3_000, 0), "50m ago");
    assert_eq!(ago(7_200, 0), "2h ago");
    assert_eq!(ago(3 * 86_400 + 5, 0), "3d ago");
}

#[test]
fn revoke_text_counts_the_closed_connections() {
    let d = device("tok1", "mbp", DeviceState::Offline);
    assert!(render_revoked(&d, 0).starts_with("Revoked mbp [tok1]. It had no open connections."));
    assert!(render_revoked(&d, 1).contains("Closed its 1 open connection."));
    assert!(render_revoked(&d, 2).contains("Closed its 2 open connections."));
}

/// Against a real socket: a fake run loop answering on a short temp path.
#[cfg(unix)]
mod over_the_socket {
    use super::*;
    use crate::worker::control::{serve, ControlCall, ControlServer};
    use std::sync::mpsc::Receiver;

    struct TempHome {
        _dir: tempfile::TempDir,
        paths: WorkerPaths,
    }

    /// A worker home under `/tmp` (the socket path must stay under 104
    /// bytes on macOS).
    fn home() -> TempHome {
        let dir = tempfile::Builder::new()
            .prefix("fds")
            .tempdir_in("/tmp")
            .unwrap();
        let paths = WorkerPaths::from_home(dir.path());
        paths.ensure_dirs().unwrap();
        TempHome { _dir: dir, paths }
    }

    fn answering(
        paths: &WorkerPaths,
        answer: impl Fn(&ControlRequest) -> ControlResponse + Send + 'static,
    ) -> ControlServer {
        let (server, calls): (ControlServer, Receiver<ControlCall>) =
            serve(&paths.control_socket).unwrap();
        std::thread::spawn(move || {
            while let Ok(call) = calls.recv() {
                let response = answer(&call.request);
                call.reply(response);
            }
        });
        server
    }

    fn run(f: impl FnOnce(&mut Vec<u8>, &mut Vec<u8>) -> i32) -> (i32, String, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = f(&mut out, &mut err);
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    #[test]
    fn status_with_nothing_running_is_stopped_and_succeeds() {
        let h = home();
        let (code, out, _) = run(|o, e| status_to(&h.paths, false, o, e));
        assert_eq!(code, 0);
        assert!(out.starts_with("FlightDeck worker: stopped"), "{out}");
        assert!(!h.paths.run_lock.exists(), "probing created no lock file");
    }

    #[test]
    fn status_keeps_a_give_up_reason_after_the_supervisor_exits() {
        let h = home();
        state::write_atomic(
            &h.paths.supervisor_state,
            &supervisor(SupervisorPhase::GaveUp),
        )
        .unwrap();
        let (code, out, _) = run(|o, e| status_to(&h.paths, true, o, e));
        assert_eq!(code, 0);
        let json: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(json["state"], "gave_up");
        assert_eq!(json["restarts"], 5);
    }

    #[test]
    fn status_of_a_running_worker_carries_its_report() {
        let h = home();
        let _server = answering(&h.paths, |_| {
            ControlResponse::Status(Box::new(sample_runtime()))
        });
        let (code, out, _) = run(|o, e| status_to(&h.paths, true, o, e));
        assert_eq!(code, 0);
        let json: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(json["state"], "running");
        assert_eq!(json["pid"], 4242);
        assert_eq!(json["devices"][0]["id"], "tok1");
    }

    #[test]
    fn commands_that_need_a_worker_say_how_to_start_one() {
        let h = home();
        let (code, out, err) = run(|o, e| pair_to(&h.paths, false, o, e));
        assert_eq!((code, out.as_str()), (1, ""));
        assert_eq!(err.trim_end(), NOT_RUNNING);
        let (code, _, err) = run(|o, e| devices_to(&h.paths, true, 0, o, e));
        assert_eq!((code, err.trim_end()), (1, NOT_RUNNING));
        let (code, _, err) = run(|o, e| revoke_to(&h.paths, "mbp", o, e));
        assert_eq!((code, err.trim_end()), (1, NOT_RUNNING));
    }

    #[test]
    fn pair_prints_the_code_or_its_json() {
        let h = home();
        let _server = answering(&h.paths, |_| ControlResponse::Paired(code(true)));
        let (status, out, _) = run(|o, e| pair_to(&h.paths, false, o, e));
        assert_eq!(status, 0);
        assert_eq!(out, render_pair(&code(true)));
        let (status, out, _) = run(|o, e| pair_to(&h.paths, true, o, e));
        assert_eq!(status, 0);
        let back: PairingCode = serde_json::from_str(&out).unwrap();
        assert_eq!(back, code(true));
    }

    #[test]
    fn devices_print_as_json() {
        let h = home();
        let _server = answering(&h.paths, |_| ControlResponse::Devices {
            devices: vec![device("tok1", "mbp", DeviceState::Background)],
        });
        let (status, out, _) = run(|o, e| devices_to(&h.paths, true, 0, o, e));
        assert_eq!(status, 0);
        let back: Vec<DeviceReport> = serde_json::from_str(&out).unwrap();
        assert_eq!(back, vec![device("tok1", "mbp", DeviceState::Background)]);
    }

    #[test]
    fn revoke_prints_the_device_or_the_refusal() {
        let h = home();
        let _server = answering(&h.paths, |req| match req {
            ControlRequest::Revoke { device: d } if d == "mbp" => ControlResponse::Revoked {
                device: device("tok1", "mbp", DeviceState::Offline),
                closed_connections: 2,
            },
            _ => ControlResponse::Error {
                message: "'m' matches more than one Desktop: mbp, mini".to_string(),
            },
        });
        let (status, out, _) = run(|o, e| revoke_to(&h.paths, "mbp", o, e));
        assert_eq!(status, 0);
        assert!(out.starts_with("Revoked mbp [tok1]. Closed its 2 open connections."));
        let (status, out, err) = run(|o, e| revoke_to(&h.paths, "m", o, e));
        assert_eq!((status, out.as_str()), (1, ""));
        assert_eq!(
            err.trim_end(),
            "'m' matches more than one Desktop: mbp, mini"
        );
    }

    #[test]
    fn an_answer_of_the_wrong_kind_is_an_error() {
        let h = home();
        let _server = answering(&h.paths, |_| ControlResponse::Ok);
        let (status, _, err) = run(|o, e| pair_to(&h.paths, false, o, e));
        assert_eq!(status, 1);
        assert!(err.contains("unexpected answer"), "{err}");
    }
}
