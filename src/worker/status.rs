//! `flightdeck worker status`: the stable `--json` document and its text
//! rendering (SPECS §33).
//!
//! A status is assembled from up to three sources, any of which may be
//! missing: the running worker's own report over the control socket
//! ([`WorkerRuntimeReport`]), the supervisor's `supervisor.json`
//! ([`SupervisorState`]), and whether each lock file is held. [`merge`] turns
//! them into one [`WorkerStatusReport`]; it is pure, so every combination is a
//! unit test.
//!
//! **The JSON shape is a contract for scripts** (schema [`STATUS_SCHEMA`]):
//! field names and meanings do not change, and additions are optional fields.
//! Every timestamp is unix seconds.

use serde::{Deserialize, Serialize};

use crate::worker::control::{DeviceReport, DeviceState, PhoneReport};
use crate::worker::service::{self, BootReport};
use crate::worker::state::{ExitReport, SupervisorPhase, SupervisorState};

/// The `schema` of the `worker status --json` document.
pub const STATUS_SCHEMA: u32 = 1;

/// The `worker status --json` document.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerStatusReport {
    /// [`STATUS_SCHEMA`].
    pub schema: u32,
    pub state: WorkerState,
    /// The version of the `flightdeck` binary that printed this.
    pub cli_version: String,
    /// The running worker's version (may differ from `cli_version` after an
    /// update until `worker restart`).
    pub version: Option<String>,
    pub pid: Option<u32>,
    pub supervisor_pid: Option<u32>,
    pub uptime_secs: Option<u64>,
    pub listen: Option<ListenReport>,
    pub devices: Vec<DeviceReport>,
    pub attached: Option<AttachedReport>,
    pub background_links: u32,
    pub projects: Vec<ProjectReport>,
    pub unread_messages: u32,
    /// The phone link, while the worker runs with `[worker] phone` on.
    #[serde(default)]
    pub phone: Option<PhoneStatusReport>,
    /// Crash restarts since the supervisor started.
    pub restarts: u32,
    pub last_exit: Option<ExitReport>,
    pub gave_up_reason: Option<String>,
    /// Whether start on boot (`flightdeck worker enable`) is set up; `None`
    /// where the OS has no supported service manager.
    #[serde(default)]
    pub start_on_boot: Option<BootReport>,
}

/// The worker's overall state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerState {
    /// The worker answers on its control socket.
    Running,
    /// A worker or supervisor holds its lock but does not answer yet.
    Starting,
    /// The worker crashed; the supervisor will restart it.
    Backoff,
    Stopped,
    /// The supervisor stopped restarting it (see `gave_up_reason`).
    GaveUp,
}

impl WorkerState {
    /// The word `worker status` prints.
    pub fn as_str(self) -> &'static str {
        match self {
            WorkerState::Running => "running",
            WorkerState::Starting => "starting",
            WorkerState::Backoff => "restarting after a crash",
            WorkerState::Stopped => "stopped",
            WorkerState::GaveUp => "gave up",
        }
    }
}

/// What a running worker reports about itself over the control socket.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerRuntimeReport {
    pub version: String,
    pub pid: u32,
    pub uptime_secs: u64,
    pub listen: ListenReport,
    pub devices: Vec<DeviceReport>,
    pub attached: Option<AttachedReport>,
    pub background_links: u32,
    pub projects: Vec<ProjectReport>,
    pub unread_messages: u32,
    /// The phone link; `None` while `[worker] phone` is off.
    #[serde(default)]
    pub phone: Option<PhoneStatusReport>,
}

/// The worker's phone link (`[worker] phone = true`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhoneStatusReport {
    /// Whether FlightDeck Remote started (see the log when it did not).
    pub running: bool,
    /// The `host:port` new phone pairings dial over Tailscale, while the
    /// direct link is listening.
    pub direct: Option<String>,
    pub phones: Vec<PhoneReport>,
}

/// Where the worker listens, or why it does not.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListenReport {
    pub listening: bool,
    /// Bound `ip:port`s, IPv6 in brackets.
    pub addresses: Vec<String>,
    /// The MagicDNS `name:port` a Desktop should dial, when Tailscale has one.
    pub advertised: Option<String>,
    /// Why it is not listening ("no Tailscale address", "bind failed: …").
    pub reason: Option<String>,
    /// Where the advertised name came from.
    pub origin: Option<String>,
}

/// The Desktop whose window is attached.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachedReport {
    pub device_id: String,
    pub label: String,
    /// When it attached (unix seconds).
    pub since: u64,
}

/// One open project.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectReport {
    pub name: String,
    /// The repository root, absolute.
    pub root: String,
    pub agents: Vec<AgentReport>,
}

/// One agent session in a project.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentReport {
    pub id: String,
    pub name: String,
    /// The agent key (`claude`, `codex`, …).
    pub agent: String,
    /// The interpreted status label (`working`, `needs_input`, …).
    pub status: String,
    /// Whether its process is alive.
    pub running: bool,
}

/// Combine what is known into one report.
///
/// - A runtime report means the worker answered: `Running`, whatever the
///   supervisor file says.
/// - Without one, a held supervisor lock makes the supervisor's phase the
///   state (`Starting`, `Backoff`, …; `Running` without an answer is still
///   `Starting`), and a held run lock alone (a foreground `worker run` not
///   answering yet) is `Starting`.
/// - With neither lock held nothing runs: `Stopped`, unless the state file
///   records that the supervisor gave up, which stays `GaveUp` so the reason
///   is not lost.
///
/// The supervisor's pid is only reported while its lock is held (a stale
/// file's pid may belong to another process by now); restarts, the last exit
/// and the give-up reason are always carried, as they explain a stop.
pub fn merge(
    runtime: Option<WorkerRuntimeReport>,
    sup: Option<SupervisorState>,
    worker_lock_held: bool,
    supervisor_lock_held: bool,
) -> WorkerStatusReport {
    let sup_phase = sup.as_ref().map(|s| s.phase);
    let state = if runtime.is_some() {
        WorkerState::Running
    } else if supervisor_lock_held {
        match sup_phase {
            Some(SupervisorPhase::Backoff) => WorkerState::Backoff,
            Some(SupervisorPhase::GaveUp) => WorkerState::GaveUp,
            Some(SupervisorPhase::Stopping | SupervisorPhase::Stopped) => WorkerState::Stopped,
            Some(SupervisorPhase::Starting | SupervisorPhase::Running) | None => {
                WorkerState::Starting
            }
        }
    } else if worker_lock_held {
        WorkerState::Starting
    } else if sup_phase == Some(SupervisorPhase::GaveUp) {
        WorkerState::GaveUp
    } else {
        WorkerState::Stopped
    };

    let supervisor_pid = sup
        .as_ref()
        .filter(|_| supervisor_lock_held)
        .map(|s| s.supervisor_pid);
    let (restarts, last_exit, gave_up_reason) = match &sup {
        Some(s) => (s.restarts, s.last_exit.clone(), s.gave_up_reason.clone()),
        None => (0, None, None),
    };
    let mut report = WorkerStatusReport {
        schema: STATUS_SCHEMA,
        state,
        cli_version: crate::VERSION.to_string(),
        version: None,
        pid: None,
        supervisor_pid,
        uptime_secs: None,
        listen: None,
        devices: Vec::new(),
        attached: None,
        background_links: 0,
        projects: Vec::new(),
        unread_messages: 0,
        phone: None,
        restarts,
        last_exit,
        gave_up_reason,
        start_on_boot: None,
    };
    match runtime {
        Some(rt) => {
            report.version = Some(rt.version);
            report.pid = Some(rt.pid);
            report.uptime_secs = Some(rt.uptime_secs);
            report.listen = Some(rt.listen);
            report.devices = rt.devices;
            report.attached = rt.attached;
            report.background_links = rt.background_links;
            report.projects = rt.projects;
            report.unread_messages = rt.unread_messages;
            report.phone = rt.phone;
        }
        None => {
            // A supervised worker that has not answered yet still has a pid.
            if supervisor_lock_held && worker_lock_held {
                report.pid = sup.as_ref().and_then(|s| s.worker_pid);
            }
        }
    }
    report
}

/// `worker status` for a person: a few lines, the important ones first.
pub fn render_text(r: &WorkerStatusReport) -> String {
    let mut out = String::new();
    let mut line = |s: String| {
        out.push_str(&s);
        out.push('\n');
    };

    let mut head = format!("FlightDeck worker: {}", r.state.as_str());
    let mut facts = Vec::new();
    if let Some(pid) = r.pid {
        facts.push(format!("pid {pid}"));
    }
    if let Some(up) = r.uptime_secs {
        facts.push(format!("up {}", format_duration(up)));
    }
    if !facts.is_empty() {
        head.push_str(&format!(" ({})", facts.join(", ")));
    }
    line(head);

    match &r.version {
        Some(v) if *v != r.cli_version => line(format!(
            "Version: {v} (this command is {}; run `flightdeck worker restart` to switch)",
            r.cli_version
        )),
        Some(v) => line(format!("Version: {v}")),
        None => {}
    }

    if let Some(listen) = &r.listen {
        if listen.listening {
            let mut l = format!("Listening: {}", listen.addresses.join(", "));
            if let Some(adv) = &listen.advertised {
                l.push_str(&format!(" (pair with {adv})"));
            }
            line(l);
        } else {
            let why = listen.reason.as_deref().unwrap_or("unknown reason");
            line(format!("Not listening: {why}"));
        }
    }

    if r.state == WorkerState::Running {
        match &r.attached {
            Some(a) => line(format!("Attached: {}", a.label)),
            None => line("Attached: none".to_string()),
        }
        line(format!("Background links: {}", r.background_links));
        if r.devices.is_empty() {
            line("Paired devices: none (run `flightdeck worker pair`)".to_string());
        } else {
            line(format!("Paired devices: {}", r.devices.len()));
            for d in &r.devices {
                let state = match d.state {
                    DeviceState::Attached => "attached",
                    DeviceState::Background => "background",
                    DeviceState::Offline => "offline",
                };
                let revoked = if d.revoked { ", revoked" } else { "" };
                line(format!("  {} [{}] {state}{revoked}", d.label, d.id));
            }
        }
        if r.projects.is_empty() {
            line("Projects: none".to_string());
        } else {
            line(format!("Projects: {}", r.projects.len()));
            for p in &r.projects {
                line(format!("  {} ({})", p.name, p.root));
                for a in &p.agents {
                    let run = if a.running { "" } else { ", exited" };
                    line(format!("    {} [{}] {}{run}", a.name, a.agent, a.status));
                }
            }
        }
        if let Some(phone) = &r.phone {
            let link = match (phone.running, &phone.direct) {
                (false, _) => "not running (see `flightdeck worker logs`)".to_string(),
                (true, Some(direct)) => format!("direct link on {direct}"),
                (true, None) => "direct link not listening yet (no Tailscale address)".to_string(),
            };
            line(format!("Phone: {link}"));
            if phone.phones.is_empty() {
                line("Paired phones: none (run `flightdeck worker pair --phone`)".to_string());
            } else {
                line(format!("Paired phones: {}", phone.phones.len()));
                for p in &phone.phones {
                    let state = if p.connected { "connected" } else { "offline" };
                    line(format!("  {} [{}] {state}", p.id, p.transport));
                }
            }
        }
        if r.unread_messages > 0 {
            line(format!("Unread messages: {}", r.unread_messages));
        }
    }

    if r.restarts > 0 {
        line(format!("Restarts: {}", r.restarts));
    }
    if let Some(exit) = &r.last_exit {
        line(format!("Last exit: {}", exit.description));
    }
    if let Some(reason) = &r.gave_up_reason {
        line(format!("Gave up: {reason}"));
    }
    if let Some(boot) = &r.start_on_boot {
        for l in service::render_status(boot, None) {
            line(l);
        }
    }
    out
}

/// `3d 4h`, `2h 5m`, `7m 3s`, `42s`: the two largest units.
fn format_duration(secs: u64) -> String {
    let (d, h, m, s) = (secs / 86_400, secs / 3_600 % 24, secs / 60 % 60, secs % 60);
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else if m > 0 {
        format!("{m}m {s}s")
    } else {
        format!("{s}s")
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::worker::state::SUPERVISOR_STATE_SCHEMA;
    use serde_json::json;

    pub(crate) fn sample_runtime() -> WorkerRuntimeReport {
        WorkerRuntimeReport {
            version: "1.27.0".to_string(),
            pid: 4242,
            uptime_secs: 7_380,
            listen: ListenReport {
                listening: true,
                addresses: vec!["100.64.1.2:7420".to_string()],
                advertised: Some("box.tail1234.ts.net:7420".to_string()),
                reason: None,
                origin: Some("magic_dns".to_string()),
            },
            devices: vec![DeviceReport {
                id: "tok1".to_string(),
                label: "mbp".to_string(),
                address: Some("100.64.1.9".to_string()),
                paired_at: 1_700_000_000,
                last_seen: Some(1_700_000_500),
                state: DeviceState::Attached,
                revoked: false,
            }],
            attached: Some(AttachedReport {
                device_id: "tok1".to_string(),
                label: "mbp".to_string(),
                since: 1_700_000_500,
            }),
            background_links: 1,
            projects: vec![ProjectReport {
                name: "app".to_string(),
                root: "/home/u/flightdeck/app".to_string(),
                agents: vec![AgentReport {
                    id: "t1".to_string(),
                    name: "fix-login".to_string(),
                    agent: "claude".to_string(),
                    status: "working".to_string(),
                    running: true,
                }],
            }],
            unread_messages: 0,
            phone: None,
        }
    }

    fn sup(phase: SupervisorPhase) -> SupervisorState {
        SupervisorState {
            schema: SUPERVISOR_STATE_SCHEMA,
            supervisor_pid: 4100,
            started_at: 1_700_000_000,
            phase,
            worker_pid: Some(4242),
            worker_started_at: Some(1_700_000_001),
            restarts: 0,
            last_exit: None,
            gave_up_reason: None,
            backoff_until: None,
        }
    }

    fn crashed() -> ExitReport {
        ExitReport {
            at: 1_700_000_600,
            code: Some(101),
            signal: None,
            description: "exited with code 101 (panic)".to_string(),
        }
    }

    #[test]
    fn a_runtime_answer_is_running() {
        let r = merge(
            Some(sample_runtime()),
            Some(sup(SupervisorPhase::Running)),
            true,
            true,
        );
        assert_eq!(r.state, WorkerState::Running);
        assert_eq!(r.pid, Some(4242));
        assert_eq!(r.supervisor_pid, Some(4100));
        assert_eq!(r.version.as_deref(), Some("1.27.0"));
        assert_eq!(r.uptime_secs, Some(7_380));
        assert_eq!(r.projects.len(), 1);
        assert_eq!(r.cli_version, crate::VERSION);
    }

    #[test]
    fn a_foreground_run_without_a_supervisor_is_running() {
        let r = merge(Some(sample_runtime()), None, true, false);
        assert_eq!(r.state, WorkerState::Running);
        assert_eq!(r.supervisor_pid, None);
        assert_eq!(r.restarts, 0);
    }

    #[test]
    fn a_supervisor_that_gave_up_keeps_its_reason_after_exiting() {
        let mut s = sup(SupervisorPhase::GaveUp);
        s.restarts = 5;
        s.worker_pid = None;
        s.last_exit = Some(crashed());
        s.gave_up_reason = Some("5 crashes within 10 minutes".to_string());
        let r = merge(None, Some(s), false, false);
        assert_eq!(r.state, WorkerState::GaveUp);
        assert_eq!(
            r.gave_up_reason.as_deref(),
            Some("5 crashes within 10 minutes")
        );
        assert_eq!(r.last_exit, Some(crashed()));
        assert_eq!(r.restarts, 5);
        assert_eq!(r.supervisor_pid, None, "its lock is free: the pid is stale");
        assert_eq!(r.pid, None);
    }

    #[test]
    fn nothing_known_and_no_locks_is_stopped() {
        let r = merge(None, None, false, false);
        assert_eq!(r.state, WorkerState::Stopped);
        assert_eq!(r.pid, None);
        assert_eq!(r.listen, None);
        assert_eq!(r.last_exit, None);
    }

    #[test]
    fn a_stale_running_state_without_locks_is_stopped() {
        // The supervisor was killed -9: its file still says running.
        let r = merge(None, Some(sup(SupervisorPhase::Running)), false, false);
        assert_eq!(r.state, WorkerState::Stopped);
        assert_eq!(r.supervisor_pid, None);
    }

    #[test]
    fn a_held_lock_without_an_answer_follows_the_supervisor() {
        let starting = merge(None, Some(sup(SupervisorPhase::Running)), true, true);
        assert_eq!(starting.state, WorkerState::Starting);
        assert_eq!(starting.pid, Some(4242));
        assert_eq!(starting.supervisor_pid, Some(4100));

        let mut b = sup(SupervisorPhase::Backoff);
        b.worker_pid = None;
        b.restarts = 2;
        b.last_exit = Some(crashed());
        let backoff = merge(None, Some(b), false, true);
        assert_eq!(backoff.state, WorkerState::Backoff);
        assert_eq!(backoff.restarts, 2);
        assert_eq!(backoff.pid, None);

        let foreground = merge(None, None, true, false);
        assert_eq!(foreground.state, WorkerState::Starting);
    }

    /// The documented `worker status --json` shape (schema 1). Scripts depend
    /// on these names: a change here is a breaking change.
    #[test]
    fn status_json_shape_is_pinned() {
        let mut s = sup(SupervisorPhase::Running);
        s.restarts = 1;
        s.last_exit = Some(crashed());
        let r = merge(Some(sample_runtime()), Some(s), true, true);
        let expected = json!({
            "schema": 1,
            "state": "running",
            "cli_version": crate::VERSION,
            "version": "1.27.0",
            "pid": 4242,
            "supervisor_pid": 4100,
            "uptime_secs": 7380,
            "listen": {
                "listening": true,
                "addresses": ["100.64.1.2:7420"],
                "advertised": "box.tail1234.ts.net:7420",
                "reason": null,
                "origin": "magic_dns"
            },
            "devices": [{
                "id": "tok1",
                "label": "mbp",
                "address": "100.64.1.9",
                "paired_at": 1_700_000_000u64,
                "last_seen": 1_700_000_500u64,
                "state": "attached",
                "revoked": false
            }],
            "attached": {"device_id": "tok1", "label": "mbp", "since": 1_700_000_500u64},
            "background_links": 1,
            "projects": [{
                "name": "app",
                "root": "/home/u/flightdeck/app",
                "agents": [{
                    "id": "t1",
                    "name": "fix-login",
                    "agent": "claude",
                    "status": "working",
                    "running": true
                }]
            }],
            "unread_messages": 0,
            "phone": null,
            "restarts": 1,
            "last_exit": {
                "at": 1_700_000_600u64,
                "code": 101,
                "signal": null,
                "description": "exited with code 101 (panic)"
            },
            "gave_up_reason": null,
            "start_on_boot": null
        });
        assert_eq!(serde_json::to_value(&r).unwrap(), expected);
        // And it reads back, as does the shape from before `phone` and
        // `start_on_boot` existed.
        let back: WorkerStatusReport = serde_json::from_value(expected.clone()).unwrap();
        assert_eq!(back, r);
        let mut before_phone = expected;
        before_phone.as_object_mut().unwrap().remove("phone");
        before_phone
            .as_object_mut()
            .unwrap()
            .remove("start_on_boot");
        let back: WorkerStatusReport = serde_json::from_value(before_phone).unwrap();
        assert_eq!(back, r);
    }

    fn boot() -> BootReport {
        BootReport {
            manager: service::Manager::Systemd,
            unit: "/home/u/.config/systemd/user/flightdeck-worker.service".to_string(),
            installed: true,
            enabled: true,
            loaded: true,
            active: true,
            linger: Some(true),
            other_binary: false,
            error: None,
        }
    }

    /// `start_on_boot` is an addition to schema 1: its names are pinned too.
    #[test]
    fn start_on_boot_json_shape_is_pinned() {
        let mut r = merge(None, None, false, false);
        r.start_on_boot = Some(boot());
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(
            v["start_on_boot"],
            json!({
                "manager": "systemd",
                "unit": "/home/u/.config/systemd/user/flightdeck-worker.service",
                "installed": true,
                "enabled": true,
                "loaded": true,
                "active": true,
                "linger": true,
                "other_binary": false,
                "error": null
            })
        );
        let back: WorkerStatusReport = serde_json::from_value(v).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn status_text_ends_with_start_on_boot() {
        let mut r = merge(None, None, false, false);
        r.start_on_boot = Some(boot());
        let text = render_text(&r);
        assert!(
            text.ends_with("Start on boot: on (systemd user unit, lingering on)\n"),
            "{text}"
        );
        r.start_on_boot = None;
        assert!(!render_text(&r).contains("Start on boot"));
    }

    #[test]
    fn status_text_shows_the_phone_link_when_it_is_on() {
        let mut rt = sample_runtime();
        let off = render_text(&merge(Some(rt.clone()), None, true, false));
        assert!(!off.contains("Phone"), "{off}");

        rt.phone = Some(PhoneStatusReport {
            running: true,
            direct: Some("box.tail1234.ts.net:47800".to_string()),
            phones: vec![PhoneReport {
                id: "pair_1".to_string(),
                transport: "direct".to_string(),
                connected: true,
            }],
        });
        let on = render_text(&merge(Some(rt.clone()), None, true, false));
        assert!(
            on.contains("Phone: direct link on box.tail1234.ts.net:47800"),
            "{on}"
        );
        assert!(on.contains("Paired phones: 1"), "{on}");
        assert!(on.contains("  pair_1 [direct] connected"), "{on}");

        rt.phone = Some(PhoneStatusReport {
            running: false,
            direct: None,
            phones: Vec::new(),
        });
        let broken = render_text(&merge(Some(rt), None, true, false));
        assert!(broken.contains("Phone: not running"), "{broken}");
        assert!(broken.contains("worker pair --phone"), "{broken}");
    }

    #[test]
    fn stopped_json_carries_nulls_not_missing_fields() {
        let r = merge(None, None, false, false);
        let v = serde_json::to_value(&r).unwrap();
        for key in [
            "version",
            "pid",
            "supervisor_pid",
            "uptime_secs",
            "listen",
            "attached",
        ] {
            assert!(v[key].is_null(), "{key} should be null: {v}");
        }
        assert_eq!(v["state"], "stopped");
        assert_eq!(v["devices"], json!([]));
    }

    #[test]
    fn render_text_running() {
        let r = merge(Some(sample_runtime()), None, true, false);
        let text = render_text(&r);
        assert!(
            text.starts_with("FlightDeck worker: running (pid 4242, up 2h 3m)\n"),
            "{text}"
        );
        assert!(
            text.contains("Listening: 100.64.1.2:7420 (pair with box.tail1234.ts.net:7420)"),
            "{text}"
        );
        assert!(text.contains("Attached: mbp"), "{text}");
        assert!(text.contains("  mbp [tok1] attached"), "{text}");
        assert!(text.contains("  app (/home/u/flightdeck/app)"), "{text}");
        assert!(text.contains("    fix-login [claude] working"), "{text}");
    }

    #[test]
    fn render_text_flags_a_version_skew_and_not_listening() {
        let mut rt = sample_runtime();
        rt.version = "0.0.1".to_string();
        rt.listen = ListenReport {
            listening: false,
            addresses: vec![],
            advertised: None,
            reason: Some("no Tailscale address".to_string()),
            origin: None,
        };
        let text = render_text(&merge(Some(rt), None, true, false));
        assert!(text.contains("worker restart"), "{text}");
        assert!(
            text.contains("Not listening: no Tailscale address"),
            "{text}"
        );
    }

    #[test]
    fn render_text_gave_up() {
        let mut s = sup(SupervisorPhase::GaveUp);
        s.restarts = 5;
        s.last_exit = Some(crashed());
        s.gave_up_reason = Some("5 crashes within 10 minutes".to_string());
        let text = render_text(&merge(None, Some(s), false, false));
        assert!(text.starts_with("FlightDeck worker: gave up\n"), "{text}");
        assert!(
            text.contains("Last exit: exited with code 101 (panic)"),
            "{text}"
        );
        assert!(
            text.contains("Gave up: 5 crashes within 10 minutes"),
            "{text}"
        );
        assert!(!text.contains("Projects"), "{text}");
    }

    #[test]
    fn durations() {
        assert_eq!(format_duration(42), "42s");
        assert_eq!(format_duration(423), "7m 3s");
        assert_eq!(format_duration(7_380), "2h 3m");
        assert_eq!(format_duration(273_600), "3d 4h");
    }
}
