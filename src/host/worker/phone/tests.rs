//! The worker's phone link, below the relay: the opt-in, the pairing report
//! the CLI polls, the phone list and unpairing, against the fakes in
//! `crate::testing`. The relay and the direct link themselves are driven end
//! to end in `tests/worker_e2e.rs`.

use std::net::SocketAddr;
use std::path::Path;

use super::*;
use crate::contracts::{Clock, Config};
use crate::host::testing::{self, worker_options};
use crate::host::worker::WorkerNotifier;
use crate::remote::state::{save_remote_state, Pairing, PairingTransport, RemoteState};
use crate::testing::{FakeClock, FakeCommandRunner, FakeContainerRuntime, FakeFs, FakePty};
use crate::Env;
use flightdeck_remote_protocol::PairingId;

struct Fakes {
    fs: FakeFs,
    pty: FakePty,
    clock: FakeClock,
    container: FakeContainerRuntime,
    command: FakeCommandRunner,
    notifier: WorkerNotifier,
}

impl Fakes {
    fn new() -> Fakes {
        Fakes {
            fs: FakeFs::new().with_dir("/home/me"),
            pty: FakePty::new(),
            clock: FakeClock::default(),
            container: FakeContainerRuntime::new(),
            command: FakeCommandRunner::new(),
            notifier: WorkerNotifier::new(),
        }
    }

    fn env(&self) -> Env<'_> {
        Env {
            fs: &self.fs,
            pty: &self.pty,
            clock: &self.clock,
            container: &self.container,
            command: &self.command,
            terminal: crate::terminal::session::TerminalProfile::TUI,
        }
    }

    /// A worker with `[worker] phone` set to `phone`, nothing started.
    fn worker(&self, phone: bool) -> AppHost<'_> {
        let mut opts = worker_options("/home/me");
        opts.config.phone = phone;
        opts.global_config = Config::default();
        opts.global_config.remote.relay_url = "ws://relay.test/ws".to_string();
        opts.global_config.remote.direct_port = 47811;
        testing::worker_host(self.env(), &self.notifier, Vec::new(), opts)
    }
}

const STATE: &str = "/home/me/.flightdeck/worker/remote.json";

fn ready(id: &str, transport: PairingTransport) -> Pairing {
    let mut p = Pairing::new(id);
    p.established = true;
    p.peer_key_agreement_public_key = Some("ka".to_string());
    p.claim_token = Some("4729".to_string());
    p.transport = transport;
    p
}

/// Write the worker's `remote.json` with these pairings.
fn pairings(fs: &FakeFs, pairings: Vec<Pairing>) {
    let state = RemoteState {
        pairings,
        ..RemoteState::default()
    };
    save_remote_state(fs, Path::new(STATE), &state).unwrap();
}

#[test]
fn the_phone_link_is_off_unless_the_worker_opts_in() {
    let fakes = Fakes::new();
    let mut host = fakes.worker(false);
    assert!(host.remote_wiring.is_none(), "no relay channels to start");
    assert_eq!(host.phone_status(), None);
    assert!(host.phones().is_empty());
    let report = host.begin_phone_pairing();
    assert_eq!(report.state, PhonePairingState::Off);
    assert_eq!(report.message, PHONE_OFF);
    assert!(host.pairing_session.is_none(), "nothing was offered");
}

#[test]
fn opting_in_keeps_the_relay_channels_and_forces_the_direct_link() {
    let fakes = Fakes::new();
    let host = fakes.worker(true);
    assert!(host.remote_wiring.is_some());
    let setup = host.phone_setup().expect("a phone setup").clone();
    assert_eq!(setup.state, Path::new(STATE));
    assert!(setup.remote.enabled, "[worker] phone is the opt-in");
    assert!(
        setup.remote.direct_enabled,
        "a worker's phone link is direct"
    );
    assert_eq!(setup.remote.relay_url, "ws://relay.test/ws");
    assert_eq!(setup.remote.direct_port, 47811);
}

#[test]
fn a_phone_link_that_did_not_start_says_so() {
    let fakes = Fakes::new();
    let mut host = fakes.worker(true);
    // Not started: no relay client, as when the identity was unreadable.
    let report = host.begin_phone_pairing();
    assert_eq!(report.state, PhonePairingState::Off);
    assert_eq!(report.message, PHONE_NOT_STARTED);
    let status = host.phone_status().expect("on");
    assert!(!status.running);
    assert_eq!(status.direct, None);
}

#[test]
fn the_report_follows_the_pairing_session_until_the_code_runs_out() {
    let fakes = Fakes::new();
    let session = |phase_ms: i64| {
        let mut s = PairingSession::begin("ws://relay.test/ws");
        s.on_offered(PairingId::new("pair_1"), "4729".to_string(), phase_ms);
        s
    };
    let now = fakes.clock.now_millis() as i64;

    let waiting = report(&session(now + 90_000), now as u64);
    assert_eq!(waiting.state, PhonePairingState::Waiting);
    assert_eq!(waiting.code.as_deref(), Some("4729"));
    assert!(waiting
        .qr_payload
        .as_deref()
        .is_some_and(|p| p.starts_with("fdr1:")));
    assert_eq!(waiting.seconds_remaining, Some(90));

    let requesting = report(&PairingSession::begin("ws://relay.test/ws"), now as u64);
    assert_eq!(requesting.state, PhonePairingState::Requesting);
    assert_eq!(requesting.code, None);

    // The worker's tick turns a code that ran out into a failure.
    let mut expired = Some(session(now));
    expire_worker_pairing(&mut expired, now as u64);
    let failed = report(expired.as_ref().unwrap(), now as u64);
    assert_eq!(failed.state, PhonePairingState::Failed);
    assert!(failed.message.contains("worker pair --phone"), "{failed:?}");

    // A code still running is left alone.
    let mut live = Some(session(now + 5_000));
    expire_worker_pairing(&mut live, now as u64);
    assert_eq!(
        report(live.as_ref().unwrap(), now as u64).state,
        PhonePairingState::Waiting
    );
}

#[test]
fn phones_are_the_established_pairings_in_the_workers_own_state() {
    let fakes = Fakes::new();
    let host = fakes.worker(true);
    let mut half = Pairing::new("pair_half");
    half.claim_token = Some("1".to_string());
    pairings(
        &fakes.fs,
        vec![
            ready("pair_direct", PairingTransport::Direct),
            ready("pair_relay", PairingTransport::Relay),
            half,
        ],
    );
    assert_eq!(
        host.phones(),
        vec![
            PhoneReport {
                id: "pair_direct".to_string(),
                transport: "direct".to_string(),
                connected: false,
            },
            PhoneReport {
                id: "pair_relay".to_string(),
                transport: "relay".to_string(),
                connected: false,
            },
        ],
        "a pairing the phone never finished is not a phone"
    );
    // The TUI's own file is never read.
    pairings_at(
        &fakes.fs,
        "/home/me/.flightdeck/remote.json",
        vec![ready("pair_tui", PairingTransport::Direct)],
    );
    assert!(host.phones().iter().all(|p| p.id != "pair_tui"));
}

fn pairings_at(fs: &FakeFs, path: &str, pairings: Vec<Pairing>) {
    let state = RemoteState {
        pairings,
        ..RemoteState::default()
    };
    save_remote_state(fs, Path::new(path), &state).unwrap();
}

#[test]
fn revoking_a_phone_unpairs_it_through_the_relay_client() {
    let fakes = Fakes::new();
    let mut host = fakes.worker(true);
    let (_in_tx, out_rx) = host.remote_wiring.take().expect("the relay channels");
    pairings(
        &fakes.fs,
        vec![
            ready("pair_abc", PairingTransport::Direct),
            ready("pair_abd", PairingTransport::Direct),
        ],
    );

    assert!(host.revoke_phone("nope").is_none(), "no phone matches");
    match host.revoke_phone("pair_ab") {
        Some(Err(message)) => assert!(message.contains("pair_abc, pair_abd"), "{message}"),
        other => panic!("an ambiguous prefix: {other:?}"),
    }
    assert!(out_rx.try_recv().is_err(), "nothing unpaired yet");

    let revoked = host
        .revoke_phone("pair_abd")
        .expect("a phone")
        .expect("unpaired");
    assert_eq!(revoked.id, "pair_abd");
    match out_rx.try_recv() {
        Ok(RemoteOutbound::Unpair { pairing_id }) => assert_eq!(pairing_id.as_str(), "pair_abd"),
        other => panic!("expected an unpair, got {other:?}"),
    }
}

#[test]
fn the_direct_link_listens_where_the_worker_does() {
    let remote = RemoteConfig {
        direct_port: 47811,
        direct_host: "box.ts.net".to_string(),
        ..RemoteConfig::default()
    };
    let fixed: SocketAddr = "127.0.0.1:7420".parse().unwrap();
    match direct_bind(&ListenPlan::Fixed(vec![fixed]), &remote) {
        BindPlan::Fixed(addrs) => assert_eq!(addrs, vec!["127.0.0.1:0".parse().unwrap()]),
        _ => panic!("the tests' loopback, on a free port"),
    }
    let tailscale = ListenPlan::Tailscale {
        port: 7420,
        source: Arc::new(crate::remote::direct::net::RealAddressSource),
        probe: Arc::new(crate::remote::direct::net::CliTailscaleProbe),
        poll: std::time::Duration::from_secs(5),
    };
    match direct_bind(&tailscale, &remote) {
        BindPlan::Tailscale {
            port,
            host_override,
            ..
        } => assert_eq!((port, host_override.as_str()), (47811, "box.ts.net")),
        _ => panic!("Tailscale only, on [remote] direct_port"),
    }
    let configured = ListenPlan::Configured {
        bind: "0.0.0.0".to_string(),
        port: 7420,
    };
    assert!(matches!(direct_bind(&configured, &remote), BindPlan::None));
}

#[test]
fn a_long_host_name_is_shortened_but_keeps_the_worker_mark() {
    assert_eq!(mark_worker("buildbox"), "buildbox (worker)");
    let long = "sat12-dp155-e321dded-3d78-45e8-bce3-b1e861108fdd-9259480AC71B.local";
    let marked = mark_worker(long);
    assert!(marked.ends_with(" (worker)"), "{marked}");
    assert_eq!(marked.chars().count(), MAX_MACHINE_NAME_CHARS);
    assert!(marked.starts_with("sat12-dp155-"), "{marked}");
}
