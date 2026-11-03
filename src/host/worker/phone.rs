//! The worker's phone link (SPECS §33, remote-control-qlyd.28): FlightDeck
//! Remote in a worker, switched on with `[worker] phone = true`.
//!
//! The phone bridge is the TUI's own ([`crate::remote::RemoteBridge`], fed by
//! [`AppHost::pump`]). What a worker sets up differently lives here:
//!
//! * **Its own relay identity and pairings** in `~/.flightdeck/worker/
//!   remote.json` ([`crate::worker::paths::WorkerPaths::phone_state`]), so the
//!   phone sees the worker as a machine of its own and a TUI's pairings on the
//!   same account are never served by the worker.
//! * **The direct link, always**, on the worker's Tailscale address(es) only
//!   (W8) — or the tests' loopback — whatever `[remote] direct_enabled` says.
//!   The relay still carries pairing, push registration and pushes.
//! * **A "(worker)" machine name**, so the phone tells it apart from a TUI.
//! * **Pairing over the control socket** ([`AppHost::begin_phone_pairing`])
//!   instead of an overlay, since a worker draws nothing.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use crate::contracts::RemoteConfig;
use crate::host::AppHost;
use crate::remote::direct::{BindPlan, DirectOptions};
use crate::remote::pairing::{PairingPhase, PairingSession};
use crate::remote::state::load_remote_state;
use crate::remote::{PeerLiveness, RemoteOutbound};
use crate::web::server::ListenPlan;
use crate::worker::control::{PhonePairingReport, PhonePairingState, PhoneReport};
use crate::worker::status::PhoneStatusReport;
use crate::{RemoteFlavor, RemoteStart};

/// What `[worker] phone = true` hands the worker to start its phone link.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PhoneSetup {
    /// The worker's `remote.json`.
    pub(super) state: PathBuf,
    /// `[remote]`, with the relay client and the direct link forced on.
    pub(super) remote: RemoteConfig,
}

impl PhoneSetup {
    /// The setup for a worker whose global `[remote]` is `remote`, keeping
    /// its state in `state`. `enabled` and `direct_enabled` are forced on:
    /// `[worker] phone` is the opt-in, and a worker's phone link is direct.
    pub(crate) fn new(remote: &RemoteConfig, state: PathBuf) -> Self {
        PhoneSetup {
            state,
            remote: RemoteConfig {
                enabled: true,
                direct_enabled: true,
                ..remote.clone()
            },
        }
    }
}

/// Where the direct link listens for a worker whose web server listens on
/// `listen`: the same Tailscale addresses on `[remote] direct_port`, or —
/// for the tests' fixed addresses — the same IPs on a free port.
pub(crate) fn direct_bind(listen: &ListenPlan, remote: &RemoteConfig) -> BindPlan {
    match listen {
        ListenPlan::Tailscale { source, probe, .. } => BindPlan::Tailscale {
            port: remote.direct_port,
            host_override: remote.direct_host.clone(),
            source: Arc::clone(source),
            probe: Arc::clone(probe),
        },
        ListenPlan::Fixed(addrs) => {
            BindPlan::Fixed(addrs.iter().map(|a| SocketAddr::new(a.ip(), 0)).collect())
        }
        // Never a worker's plan; listen nowhere rather than somewhere wider.
        ListenPlan::Configured { .. } => BindPlan::None,
    }
}

/// The name a worker announces to the phone: this machine's, marked as a
/// worker. `FLIGHTDECK_MACHINE_NAME` still wins, verbatim.
pub(crate) fn worker_machine_name() -> Option<String> {
    if std::env::var_os("FLIGHTDECK_MACHINE_NAME").is_some() {
        return None;
    }
    crate::remote::client::machine_name().map(|name| {
        let marked = format!("{name} (worker)");
        marked.chars().take(64).collect()
    })
}

/// Turn a pairing code that ran out into a failure the CLI can report:
/// a worker has no overlay whose countdown would show it.
pub(crate) fn expire_worker_pairing(session: &mut Option<PairingSession>, now_ms: u64) {
    if let Some(ps) = session.as_mut() {
        if matches!(ps.phase(), PairingPhase::Displaying { .. })
            && ps.seconds_remaining(now_ms as i64) == Some(0)
        {
            ps.fail(
                "The pairing code expired. Run `flightdeck worker pair --phone` for a new one.",
            );
        }
    }
}

/// What `worker pair --phone` says while the phone link is off.
pub const PHONE_OFF: &str = "Phone pairing is off on this worker. Set `phone = true` under \
     [worker] in ~/.flightdeck/config.toml, then run `flightdeck worker restart`.";

/// What it says when FlightDeck Remote was switched on but did not start.
pub const PHONE_NOT_STARTED: &str = "FlightDeck Remote did not start on this worker; \
     see `flightdeck worker logs`.";

/// The report for `session`, `now_ms` dating its countdown.
fn report(session: &PairingSession, now_ms: u64) -> PhonePairingReport {
    let mut out = PhonePairingReport {
        state: PhonePairingState::Requesting,
        code: None,
        qr_payload: None,
        seconds_remaining: None,
        message: String::new(),
    };
    match session.phase() {
        PairingPhase::Idle | PairingPhase::Offering => {
            out.message = session
                .stall_reason()
                .map(str::to_string)
                .unwrap_or_else(|| "Requesting a pairing code from the relay…".to_string());
        }
        PairingPhase::Displaying {
            code, qr_payload, ..
        } => {
            out.state = PhonePairingState::Waiting;
            out.code = Some(code.clone());
            out.qr_payload = Some(qr_payload.clone());
            out.seconds_remaining = session.seconds_remaining(now_ms as i64);
            out.message =
                "Scan the QR or type the code in the FlightDeck app — waiting…".to_string();
        }
        PairingPhase::Established { .. } => {
            out.state = PhonePairingState::Paired;
            out.message = "Phone paired. End-to-end encrypted.".to_string();
        }
        PairingPhase::Failed { message } => {
            out.state = PhonePairingState::Failed;
            out.message = message.clone();
        }
    }
    out
}

fn off(message: &str) -> PhonePairingReport {
    PhonePairingReport {
        state: PhonePairingState::Off,
        code: None,
        qr_payload: None,
        seconds_remaining: None,
        message: message.to_string(),
    }
}

impl AppHost<'_> {
    /// The worker's phone setup, when `[worker] phone` is on.
    fn phone_setup(&self) -> Option<&PhoneSetup> {
        self.worker.as_ref().and_then(|w| w.phone.as_ref())
    }

    /// Bring up FlightDeck Remote for a worker with `[worker] phone` on: its
    /// own state file, the direct link on the worker's addresses, the
    /// "(worker)" name, then the TUI's bridge over it. Logs what happened;
    /// a failure leaves the worker running without its phone link.
    pub(super) fn start_phone(&mut self, now0: u64) {
        let Some(setup) = self.phone_setup().cloned() else {
            return;
        };
        let Some((inbound_tx, outbound_rx)) = self.remote_wiring.take() else {
            return;
        };
        let bind = match self.worker.as_ref() {
            Some(w) => direct_bind(&w.listen, &setup.remote),
            None => return,
        };
        let flavor = RemoteFlavor {
            direct: DirectOptions {
                bind,
                ..DirectOptions::for_config(&setup.remote, &setup.state)
            },
            machine_name: worker_machine_name(),
        };
        let started = crate::start_remote_at(
            &self.env,
            &setup.state,
            setup.remote.clone(),
            flavor,
            inbound_tx,
            outbound_rx,
        );
        let line = match started {
            RemoteStart::Started(remote) => {
                self.remote_setup = Some(*remote);
                format!(
                    "phone link started (relay {}, direct link on port {})",
                    setup.remote.relay_url, setup.remote.direct_port
                )
            }
            RemoteStart::HeldElsewhere => format!(
                "phone link not started: another process holds {}",
                setup.state.with_file_name("remote.lock").display()
            ),
            RemoteStart::Off => format!(
                "phone link not started: could not read or create {}",
                setup.state.display()
            ),
        };
        if let Some(w) = self.worker.as_mut() {
            w.log(line);
        }
        self.install_remote_bridge(now0);
    }

    /// The phone link for `worker status`; `None` while `[worker] phone` is
    /// off.
    pub fn phone_status(&self) -> Option<PhoneStatusReport> {
        self.phone_setup()?;
        let direct = self
            .remote_setup
            .as_ref()
            .and_then(|s| s.direct.as_ref())
            .and_then(|d| d.endpoint())
            .map(|e| format!("{}:{}", e.host, e.port));
        Some(PhoneStatusReport {
            running: self.remote_setup.is_some(),
            direct,
            phones: self.phones(),
        })
    }

    /// Start pairing a phone, or report the attempt already running. A
    /// finished attempt (paired, failed, expired) is replaced by a new one.
    pub fn begin_phone_pairing(&mut self) -> PhonePairingReport {
        if self.phone_setup().is_none() {
            return off(PHONE_OFF);
        }
        let Some(relay_url) = self.remote_setup.as_ref().map(|s| s.relay_url.clone()) else {
            return off(PHONE_NOT_STARTED);
        };
        let running = self.pairing_session.as_ref().is_some_and(|ps| {
            matches!(
                ps.phase(),
                PairingPhase::Idle | PairingPhase::Offering | PairingPhase::Displaying { .. }
            )
        });
        if !running {
            let session = match self.autopair_hint.as_deref() {
                Some(hint) => PairingSession::begin_with_hint(relay_url, hint),
                None => PairingSession::begin(relay_url),
            };
            let _ = self.remote_out_tx.send(RemoteOutbound::RequestPairing {
                claim_token_hint: Some(session.hint().to_string()),
            });
            self.pairing_session = Some(session);
            if let Some(w) = self.worker.as_mut() {
                w.log("started a phone pairing");
            }
        }
        self.phone_pairing_report()
    }

    /// How the phone pairing attempt is going.
    pub fn phone_pairing_report(&self) -> PhonePairingReport {
        if self.phone_setup().is_none() {
            return off(PHONE_OFF);
        }
        if self.remote_setup.is_none() {
            return off(PHONE_NOT_STARTED);
        }
        match self.pairing_session.as_ref() {
            Some(ps) => report(ps, self.env.clock.now_millis()),
            None => PhonePairingReport {
                state: PhonePairingState::Failed,
                code: None,
                qr_payload: None,
                seconds_remaining: None,
                message: "No phone pairing is in progress.".to_string(),
            },
        }
    }

    /// The paired phones, from the worker's `remote.json`. Empty while the
    /// phone link is off.
    pub fn phones(&self) -> Vec<PhoneReport> {
        let Some(setup) = self.phone_setup() else {
            return Vec::new();
        };
        let Ok(state) = load_remote_state(self.env.fs, &setup.state) else {
            return Vec::new();
        };
        let live = self.remote_bridge.as_ref().and_then(|b| {
            (b.peer_liveness() != PeerLiveness::NoPeer)
                .then(|| b.active_pairing().map(|p| p.as_str().to_string()))
                .flatten()
        });
        state
            .pairings
            .iter()
            .filter(|p| p.is_e2e_ready())
            .map(|p| PhoneReport {
                id: p.pairing_id.clone(),
                transport: if p.is_direct() { "direct" } else { "relay" }.to_string(),
                connected: live.as_deref() == Some(p.pairing_id.as_str()),
            })
            .collect()
    }

    /// Unpair the phone `selector` names (its pairing id or a unique
    /// prefix). `None` when no phone matches; `Err` when several do.
    pub fn revoke_phone(
        &mut self,
        selector: &str,
    ) -> Option<std::result::Result<PhoneReport, String>> {
        let phones = self.phones();
        let chosen = match phones.iter().find(|p| p.id == selector) {
            Some(exact) => exact.clone(),
            None => {
                let by_prefix: Vec<&PhoneReport> = phones
                    .iter()
                    .filter(|p| p.id.starts_with(selector))
                    .collect();
                match by_prefix.as_slice() {
                    [] => return None,
                    [one] => (*one).clone(),
                    many => {
                        let ids: Vec<&str> = many.iter().map(|p| p.id.as_str()).collect();
                        return Some(Err(format!(
                            "'{selector}' matches several phones: {}. Name one by its id.",
                            ids.join(", ")
                        )));
                    }
                }
            }
        };
        let pairing_id = flightdeck_remote_protocol::PairingId::new(chosen.id.clone());
        let _ = self.remote_out_tx.send(RemoteOutbound::Unpair {
            pairing_id: pairing_id.clone(),
        });
        if let Some(b) = self.remote_bridge.as_mut() {
            if b.active_pairing() == Some(&pairing_id) {
                b.reset_to_passthrough();
            }
        }
        if self.pairing_session.as_ref().and_then(|ps| ps.pairing_id()) == Some(&pairing_id) {
            self.pairing_session = None;
        }
        if let Some(w) = self.worker.as_mut() {
            w.log(format!("unpaired phone {}", chosen.id));
        }
        Some(Ok(PhoneReport {
            connected: false,
            ..chosen
        }))
    }
}

#[cfg(test)]
mod tests;
