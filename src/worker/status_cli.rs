//! The CLI side of the control socket: `worker status`, `pair` (and
//! `pair --phone`), `devices` and `revoke` (SPECS §33). Each returns the process exit code.
//!
//! Each command asks the running worker over [`control::request`] and prints
//! the answer. `status` also works with nothing running: it merges whatever
//! it can learn (the worker's answer, `supervisor.json`, which locks are
//! held) through [`status::merge`]. The rest need a worker and say how to
//! start one when there is none. The text renderers are pure functions, so
//! every shape is a unit test.

use std::io::Write;
use std::path::Path;
use std::time::Duration;

use crate::contracts::{Clock, FileSystem, RealClock, RealFs};
use crate::worker::control::{
    self, ControlError, ControlRequest, ControlResponse, DeviceReport, DeviceState, PairingCode,
    PhonePairingReport, PhonePairingState, PhoneReport,
};
use crate::worker::paths::WorkerPaths;
use crate::worker::state;
use crate::worker::status::{self, WorkerStatusReport};

/// What every command but `status` says when no worker answers.
pub const NOT_RUNNING: &str = "The worker is not running. Start it with `flightdeck worker start`.";

/// How long a command waits for the worker: a little over the server's own
/// [`control::REPLY_TIMEOUT`], so a slow run loop is reported by the server
/// ("did not answer in time") rather than cut off here.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(6);

/// `flightdeck worker status [--json]`. Works when the worker is stopped.
pub fn run_status(json: bool) -> i32 {
    with_paths(|paths, out, err| status_to(paths, json, out, err))
}

/// `flightdeck worker pair [--json]`: mint a pairing code in the running worker.
pub fn run_pair(json: bool) -> i32 {
    with_paths(|paths, out, err| pair_to(paths, json, out, err))
}

/// `flightdeck worker pair --phone [--json]`: pair the iPhone app, waiting
/// until it has paired, failed, or the code ran out.
pub fn run_pair_phone(json: bool) -> i32 {
    with_paths(|paths, out, err| {
        let mut pause = || std::thread::sleep(PHONE_POLL);
        pair_phone_to(paths, json, PHONE_REQUEST_LIMIT, &mut pause, out, err)
    })
}

/// `flightdeck worker devices [--json]`: the paired Desktops.
pub fn run_devices(json: bool) -> i32 {
    with_paths(|paths, out, err| devices_to(paths, json, RealClock.now_unix_secs(), out, err))
}

/// `flightdeck worker revoke <device>`: revoke one Desktop and close its
/// connections.
pub fn run_revoke(device: &str) -> i32 {
    with_paths(|paths, out, err| revoke_to(paths, device, out, err))
}

/// Run `f` with this account's worker paths and the real output streams.
fn with_paths(f: impl FnOnce(&WorkerPaths, &mut dyn Write, &mut dyn Write) -> i32) -> i32 {
    let mut out = std::io::stdout();
    let mut err = std::io::stderr();
    match WorkerPaths::current() {
        Some(paths) => f(&paths, &mut out, &mut err),
        None => {
            let _ = writeln!(
                err,
                "flightdeck worker: no home directory ($HOME is not set)."
            );
            1
        }
    }
}

/// Whether another process holds `lock` (taking it for a moment to find
/// out). A lock file that does not exist is not held, and is not created.
fn held(fs: &dyn FileSystem, lock: &Path) -> bool {
    fs.exists(lock) && matches!(fs.try_lock_exclusive(lock), Ok(None))
}

/// `worker status` over explicit streams.
fn status_to(paths: &WorkerPaths, json: bool, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let runtime = match control::request(
        &paths.control_socket,
        &ControlRequest::Status,
        REQUEST_TIMEOUT,
    ) {
        Ok(ControlResponse::Status(report)) => Some(*report),
        _ => None,
    };
    let sup = state::read(&paths.supervisor_state);
    // A worker that answered holds run.lock: probing it then would only risk
    // the start-up race (a `worker run` taking its lock at that instant).
    let worker_lock = runtime.is_some() || held(&RealFs, &paths.run_lock);
    let supervisor_lock = held(&RealFs, &paths.supervisor_lock);
    let report = status::merge(runtime, sup, worker_lock, supervisor_lock);
    match render_status(&report, json) {
        Ok(text) => {
            let _ = write!(out, "{text}");
            0
        }
        Err(e) => {
            let _ = writeln!(err, "flightdeck worker: {e}");
            1
        }
    }
}

/// The status as `worker status` prints it: text, or the schema-1 JSON
/// document (pretty-printed, newline-terminated).
fn render_status(report: &WorkerStatusReport, json: bool) -> serde_json::Result<String> {
    if json {
        serde_json::to_string_pretty(report).map(|s| s + "\n")
    } else {
        Ok(status::render_text(report))
    }
}

/// Ask the running worker; every way of not getting a useful answer is
/// printed to `err` and becomes `Err(exit code)`.
fn ask(
    paths: &WorkerPaths,
    request: &ControlRequest,
    err: &mut dyn Write,
) -> Result<ControlResponse, i32> {
    let message = match control::request(&paths.control_socket, request, REQUEST_TIMEOUT) {
        Ok(ControlResponse::Error { message }) => message,
        Ok(response) => return Ok(response),
        Err(ControlError::NotRunning) => {
            if held(&RealFs, &paths.supervisor_lock) {
                "The worker is not answering yet: its supervisor is starting or restarting it. \
                 See `flightdeck worker status`."
                    .to_string()
            } else {
                NOT_RUNNING.to_string()
            }
        }
        Err(ControlError::Timeout) => "The worker did not answer in time. \
             See `flightdeck worker status` and `flightdeck worker logs`."
            .to_string(),
        Err(e) => format!("flightdeck worker: {e}"),
    };
    let _ = writeln!(err, "{message}");
    Err(1)
}

/// An answer of the wrong kind (a worker of another version, say).
fn unexpected(response: &ControlResponse, err: &mut dyn Write) -> i32 {
    let _ = writeln!(
        err,
        "flightdeck worker: unexpected answer from the worker: {response:?}"
    );
    1
}

/// Print `value` as pretty JSON.
fn print_json(value: &impl serde::Serialize, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    match serde_json::to_string_pretty(value) {
        Ok(text) => {
            let _ = writeln!(out, "{text}");
            0
        }
        Err(e) => {
            let _ = writeln!(err, "flightdeck worker: {e}");
            1
        }
    }
}

/// `worker pair` over explicit streams.
fn pair_to(paths: &WorkerPaths, json: bool, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let code = match ask(paths, &ControlRequest::Pair, err) {
        Ok(ControlResponse::Paired(code)) => code,
        Ok(other) => return unexpected(&other, err),
        Err(code) => return code,
    };
    if json {
        return print_json(&code, out, err);
    }
    let _ = write!(out, "{}", render_pair(&code));
    0
}

/// `worker pair` for a person (PRD §4.1): the code, how long it lasts, the
/// address to type and where, and a warning when no Desktop can reach the
/// worker yet.
fn render_pair(code: &PairingCode) -> String {
    let mut out = format!(
        "Pairing code: {}   (valid for {} s, single use)\n",
        code.code, code.expires_in_secs
    );
    match &code.address {
        Some(address) => out.push_str(&format!("Address:      {address}\n")),
        None => out.push_str("Address:      none yet (the worker is not listening)\n"),
    }
    out.push_str(
        "\nIn FlightDeck Desktop, choose \"Connect to remote…\", enter the address, \
         then the code.\n",
    );
    if !code.listening {
        let why = code
            .not_listening_reason
            .as_deref()
            .unwrap_or("unknown reason");
        out.push_str(&format!(
            "\nWarning: the worker is not listening ({why}), so no Desktop can reach it yet.\n\
             It starts listening as soon as Tailscale gives this machine an address; \
             mint a new code if this one expires first.\n"
        ));
    }
    out
}

/// How often `pair --phone` asks the worker how the pairing is going.
const PHONE_POLL: Duration = Duration::from_millis(250);
/// How long `pair --phone` waits for the relay to hand out a code before it
/// gives up (the worker keeps asking; a later `pair --phone` picks it up).
const PHONE_REQUEST_LIMIT: Duration = Duration::from_secs(60);

/// `worker pair --phone` over explicit streams. `pause` runs between polls;
/// `request_limit` bounds the wait for a code.
fn pair_phone_to(
    paths: &WorkerPaths,
    json: bool,
    request_limit: Duration,
    pause: &mut dyn FnMut(),
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    let ask_phone = |request: &ControlRequest, err: &mut dyn Write| match ask(paths, request, err) {
        Ok(ControlResponse::PhonePairing(report)) => Ok(report),
        Ok(other) => Err(unexpected(&other, err)),
        Err(code) => Err(code),
    };
    let mut report = match ask_phone(&ControlRequest::PairPhone, err) {
        Ok(report) => report,
        Err(code) => return code,
    };
    let started = std::time::Instant::now();
    // What was last printed: the countdown alone changing prints nothing.
    let mut shown: Option<(PhonePairingState, Option<String>, String)> = None;
    loop {
        let key = (report.state, report.code.clone(), report.message.clone());
        if shown.as_ref() != Some(&key) {
            if json {
                match serde_json::to_string(&report) {
                    Ok(line) => {
                        let _ = writeln!(out, "{line}");
                    }
                    Err(e) => {
                        let _ = writeln!(err, "flightdeck worker: {e}");
                        return 1;
                    }
                }
            } else {
                match report.state {
                    PhonePairingState::Failed | PhonePairingState::Off => {
                        let _ = writeln!(err, "{}", report.message);
                    }
                    _ => {
                        let _ = write!(out, "{}", render_phone_pairing(&report));
                    }
                }
            }
            let _ = out.flush();
            shown = Some(key);
        }
        match report.state {
            PhonePairingState::Paired => return 0,
            PhonePairingState::Failed | PhonePairingState::Off => return 1,
            PhonePairingState::Requesting if started.elapsed() >= request_limit => {
                let _ = writeln!(
                    err,
                    "The relay did not hand out a pairing code within {} s ({}). \
                     See `flightdeck worker logs`, then try again.",
                    request_limit.as_secs(),
                    report.message
                );
                return 1;
            }
            _ => {}
        }
        pause();
        report = match ask_phone(&ControlRequest::PhonePairingStatus, err) {
            Ok(report) => report,
            Err(code) => return code,
        };
    }
}

/// One step of `worker pair --phone` for a person: the wait for a code, the
/// QR and code to scan or type, or the success.
fn render_phone_pairing(report: &PhonePairingReport) -> String {
    match report.state {
        PhonePairingState::Waiting => {
            let mut out = String::from(
                "Open the FlightDeck app on your iPhone, choose Pair, and scan this QR code \
                 (or type the code).\n\n",
            );
            if let Some(art) = report
                .qr_payload
                .as_deref()
                .and_then(crate::remote::pairing::qr_art)
            {
                for row in &art.rows {
                    out.push_str(row);
                    out.push('\n');
                }
                out.push('\n');
            }
            out.push_str(&format!(
                "Code: {}",
                report.code.as_deref().unwrap_or("(none)")
            ));
            if let Some(secs) = report.seconds_remaining {
                out.push_str(&format!("   (valid for {secs} s, single use)"));
            }
            out.push_str(
                "\n\nWaiting for the phone… Ctrl-C stops waiting; the code stays valid until it \
                 runs out.\n",
            );
            out
        }
        PhonePairingState::Paired => format!(
            "{}\nIt is listed by `flightdeck worker devices`; unpair it with \
             `flightdeck worker revoke <id>`.\n",
            report.message
        ),
        _ => format!("{}\n", report.message),
    }
}

/// `worker devices` over explicit streams; `now` (unix seconds) dates the
/// "ago" columns.
fn devices_to(
    paths: &WorkerPaths,
    json: bool,
    now: u64,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    let (devices, phones) = match ask(paths, &ControlRequest::Devices, err) {
        Ok(ControlResponse::Devices { devices, phones }) => (devices, phones),
        Ok(other) => return unexpected(&other, err),
        Err(code) => return code,
    };
    // The JSON stays the Desktops array it has always been.
    if json {
        return print_json(&devices, out, err);
    }
    let mut text = render_devices(&devices, now);
    if !phones.is_empty() {
        text.push('\n');
        text.push_str(&render_phones(&phones));
    }
    let _ = write!(out, "{text}");
    0
}

/// The paired phones for a person, as a table.
fn render_phones(phones: &[PhoneReport]) -> String {
    let header = ["PHONE", "LINK", "STATE"].map(String::from);
    let rows: Vec<[String; 3]> = phones
        .iter()
        .map(|p| {
            [
                p.id.clone(),
                p.transport.clone(),
                if p.connected { "connected" } else { "offline" }.to_string(),
            ]
        })
        .collect();
    let mut widths = [0usize; 3];
    for row in std::iter::once(&header).chain(rows.iter()) {
        for (w, cell) in widths.iter_mut().zip(row) {
            *w = (*w).max(cell.chars().count());
        }
    }
    let mut out = String::new();
    for row in std::iter::once(&header).chain(rows.iter()) {
        let cells: Vec<String> = row
            .iter()
            .zip(widths)
            .map(|(cell, w)| format!("{cell:<w$}"))
            .collect();
        out.push_str(cells.join("  ").trim_end());
        out.push('\n');
    }
    out.push_str("\nUnpair one with `flightdeck worker revoke <phone id>`.\n");
    out
}

/// `worker devices` for a person: a table, or how to pair one.
fn render_devices(devices: &[DeviceReport], now: u64) -> String {
    if devices.is_empty() {
        return "No paired Desktops. Pair one with `flightdeck worker pair`.\n".to_string();
    }
    let header = ["ID", "LABEL", "STATE", "PAIRED", "LAST SEEN", "ADDRESS"].map(String::from);
    let rows: Vec<[String; 6]> = devices
        .iter()
        .map(|d| {
            let state = if d.revoked {
                "revoked"
            } else {
                match d.state {
                    DeviceState::Attached => "attached",
                    DeviceState::Background => "background",
                    DeviceState::Offline => "offline",
                }
            };
            [
                d.id.clone(),
                d.label.clone(),
                state.to_string(),
                ago(now, d.paired_at),
                d.last_seen
                    .map(|at| ago(now, at))
                    .unwrap_or_else(|| "never".to_string()),
                d.address.clone().unwrap_or_else(|| "-".to_string()),
            ]
        })
        .collect();
    let mut widths = [0usize; 6];
    for row in std::iter::once(&header).chain(rows.iter()) {
        for (w, cell) in widths.iter_mut().zip(row) {
            *w = (*w).max(cell.chars().count());
        }
    }
    let mut out = String::new();
    for row in std::iter::once(&header).chain(rows.iter()) {
        let cells: Vec<String> = row
            .iter()
            .zip(widths)
            .map(|(cell, w)| format!("{cell:<w$}"))
            .collect();
        out.push_str(cells.join("  ").trim_end());
        out.push('\n');
    }
    out.push_str("\nRevoke one with `flightdeck worker revoke <id or label>`.\n");
    out
}

/// How long before `now` the unix second `at` was: `just now`, `5m ago`,
/// `3h ago`, `2d ago`. A time in the future (a clock that moved) is `just now`.
fn ago(now: u64, at: u64) -> String {
    let secs = now.saturating_sub(at);
    if secs < 60 {
        "just now".to_string()
    } else if secs < 3_600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3_600)
    } else {
        format!("{}d ago", secs / 86_400)
    }
}

/// `worker revoke` over explicit streams.
fn revoke_to(paths: &WorkerPaths, device: &str, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let request = ControlRequest::Revoke {
        device: device.to_string(),
    };
    match ask(paths, &request, err) {
        Ok(ControlResponse::Revoked {
            device,
            closed_connections,
        }) => {
            let _ = write!(out, "{}", render_revoked(&device, closed_connections));
            0
        }
        Ok(ControlResponse::PhoneRevoked { phone }) => {
            let _ = writeln!(
                out,
                "Unpaired phone {}. It must pair again (`flightdeck worker pair --phone`) \
                 to reconnect.",
                phone.id
            );
            0
        }
        Ok(other) => unexpected(&other, err),
        Err(code) => code,
    }
}

/// `worker revoke` for a person.
fn render_revoked(device: &DeviceReport, closed: u32) -> String {
    let connections = match closed {
        0 => "It had no open connections.".to_string(),
        1 => "Closed its 1 open connection.".to_string(),
        n => format!("Closed its {n} open connections."),
    };
    format!(
        "Revoked {} [{}]. {connections}\nIt must pair again (`flightdeck worker pair`) to reconnect.\n",
        device.label, device.id
    )
}

#[cfg(test)]
mod tests;
