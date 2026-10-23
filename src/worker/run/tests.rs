use super::*;
use crate::testing::FakeFs;
use crate::worker::control::DeviceState;
use crate::worker::status::ListenReport;

fn home() -> &'static Path {
    Path::new("/home/u")
}

fn config_path() -> std::path::PathBuf {
    home().join(".flightdeck").join("config.toml")
}

#[test]
fn a_missing_global_config_is_written_with_defaults_and_loaded() {
    let fs = FakeFs::new();
    let config = load_global_config(&fs, home()).unwrap();
    assert!(
        fs.exists(&config_path()),
        "the documented default is written"
    );
    assert_eq!(config.worker.port, 7420);
    assert_eq!(config.worker.default_cols, 200);
    assert!(!config.agents.is_empty());
}

#[test]
fn an_invalid_worker_table_is_a_config_error() {
    let fs = FakeFs::new();
    load_global_config(&fs, home()).unwrap();
    let mut table: toml::Table = fs.read_to_string(&config_path()).unwrap().parse().unwrap();
    let mut worker = toml::Table::new();
    worker.insert("port".to_string(), toml::Value::Integer(0));
    table.insert("worker".to_string(), toml::Value::Table(worker));
    let broken = toml::to_string(&table).unwrap();
    fs.write(&config_path(), &broken).unwrap();
    match load_global_config(&fs, home()) {
        Err(ConfigProblem::Invalid(message)) => {
            assert!(message.contains("worker.port"), "{message}");
            assert!(message.contains("config.toml"), "{message}");
        }
        other => panic!("expected an invalid config, got {other:?}"),
    }
}

#[test]
fn an_unparsable_global_config_is_a_config_error_not_a_silent_default() {
    let fs = FakeFs::new().with_file(config_path(), "[ui\nnot toml");
    assert!(matches!(
        load_global_config(&fs, home()),
        Err(ConfigProblem::Invalid(_))
    ));
}

#[test]
fn the_release_plan_listens_on_tailscale_at_the_configured_port() {
    match tailscale_plan(7421) {
        ListenPlan::Tailscale { port, poll, .. } => {
            assert_eq!(port, 7421);
            assert_eq!(poll, std::time::Duration::from_secs(5));
        }
        other => panic!("expected a Tailscale plan, got {other:?}"),
    }
}

#[cfg(debug_assertions)]
#[test]
fn the_test_listen_seam_parses_a_comma_list() {
    assert_eq!(
        parse_test_listen("127.0.0.1:0, [::1]:7420").unwrap(),
        vec![
            "127.0.0.1:0".parse().unwrap(),
            "[::1]:7420".parse().unwrap()
        ]
    );
    assert!(parse_test_listen("").is_err());
    assert!(parse_test_listen(" , ").is_err());
    let err = parse_test_listen("127.0.0.1:0,localhost:80").unwrap_err();
    assert!(err.contains("'localhost:80'"), "{err}");
}

#[test]
fn the_restart_marker_is_read_once() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("restart.marker");
    assert_eq!(take_restart_marker(&marker), None);
    std::fs::write(&marker, "1700000000\n").unwrap();
    assert_eq!(take_restart_marker(&marker).as_deref(), Some("1700000000"));
    assert!(!marker.exists(), "deleted after reading");
    assert_eq!(take_restart_marker(&marker), None);
}

#[test]
fn restart_messages() {
    assert_eq!(
        restart_message("1700000000"),
        "started by `flightdeck worker restart` (requested at unix 1700000000)"
    );
    assert_eq!(
        restart_message(""),
        "started by `flightdeck worker restart`"
    );
}

/// A worker API that records what the control socket asked of it.
#[derive(Default)]
struct FakeTarget {
    pairs: u32,
    revoked: Vec<String>,
    shutdown_intent: Option<bool>,
}

fn device(id: &str, label: &str) -> DeviceReport {
    DeviceReport {
        id: id.to_string(),
        label: label.to_string(),
        address: None,
        paired_at: 1,
        last_seen: None,
        state: DeviceState::Offline,
        revoked: false,
    }
}

impl ControlTarget for FakeTarget {
    fn report(&self) -> WorkerRuntimeReport {
        WorkerRuntimeReport {
            version: "9.9.9".to_string(),
            pid: 1,
            uptime_secs: 5,
            listen: ListenReport {
                listening: false,
                addresses: Vec::new(),
                advertised: None,
                reason: Some("no Tailscale address".to_string()),
                origin: None,
            },
            devices: Vec::new(),
            attached: None,
            background_links: 0,
            projects: Vec::new(),
            unread_messages: 0,
        }
    }
    fn pair(&mut self) -> PairingCode {
        self.pairs += 1;
        PairingCode {
            code: "4821".to_string(),
            expires_in_secs: 120,
            address: Some("box.ts.net:7420".to_string()),
            listening: true,
            not_listening_reason: None,
        }
    }
    fn paired_devices(&self) -> Vec<DeviceReport> {
        vec![device("tok1", "mbp")]
    }
    fn revoke(&mut self, selector: &str) -> Result<(DeviceReport, u32), String> {
        if selector == "mbp" {
            self.revoked.push(selector.to_string());
            Ok((device("tok1", "mbp"), 2))
        } else {
            Err(format!("no paired Desktop matches '{selector}'"))
        }
    }
    fn prepare_shutdown(&mut self, restarting: bool) {
        self.shutdown_intent = Some(restarting);
    }
}

#[test]
fn ping_answers_with_this_process() {
    let mut t = FakeTarget::default();
    let (response, line) = answer(&mut t, &ControlRequest::Ping);
    assert_eq!(
        response,
        ControlResponse::Pong {
            pid: std::process::id(),
            version: crate::VERSION.to_string()
        }
    );
    assert_eq!(line, None);
}

#[test]
fn status_and_devices_come_from_the_host() {
    let mut t = FakeTarget::default();
    match answer(&mut t, &ControlRequest::Status) {
        (ControlResponse::Status(report), None) => assert_eq!(report.version, "9.9.9"),
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(
        answer(&mut t, &ControlRequest::Devices),
        (
            ControlResponse::Devices {
                devices: vec![device("tok1", "mbp")]
            },
            None
        )
    );
}

#[test]
fn pair_mints_a_code_and_never_logs_it() {
    let mut t = FakeTarget::default();
    let (response, line) = answer(&mut t, &ControlRequest::Pair);
    assert_eq!(t.pairs, 1);
    match response {
        ControlResponse::Paired(code) => assert_eq!(code.code, "4821"),
        other => panic!("unexpected {other:?}"),
    }
    let line = line.expect("a pairing is logged");
    assert!(
        !line.contains("4821"),
        "the code leaked into the log: {line}"
    );
}

#[test]
fn revoke_reports_the_device_and_closed_connections() {
    let mut t = FakeTarget::default();
    let (response, line) = answer(
        &mut t,
        &ControlRequest::Revoke {
            device: "mbp".to_string(),
        },
    );
    assert_eq!(
        response,
        ControlResponse::Revoked {
            device: device("tok1", "mbp"),
            closed_connections: 2
        }
    );
    assert_eq!(
        line.as_deref(),
        Some("revoked mbp [tok1]: closed 2 connection(s)")
    );
    assert_eq!(t.revoked, vec!["mbp".to_string()]);
}

#[test]
fn an_unknown_or_ambiguous_revoke_is_an_error_answer() {
    let mut t = FakeTarget::default();
    let (response, line) = answer(
        &mut t,
        &ControlRequest::Revoke {
            device: "nope".to_string(),
        },
    );
    assert_eq!(
        response,
        ControlResponse::Error {
            message: "no paired Desktop matches 'nope'".to_string()
        }
    );
    assert_eq!(line, None);
    assert!(t.revoked.is_empty());
}

#[test]
fn prepare_shutdown_records_the_intent() {
    let mut t = FakeTarget::default();
    let (response, _) = answer(
        &mut t,
        &ControlRequest::PrepareShutdown { restarting: true },
    );
    assert_eq!(response, ControlResponse::Ok);
    assert_eq!(t.shutdown_intent, Some(true));
}
