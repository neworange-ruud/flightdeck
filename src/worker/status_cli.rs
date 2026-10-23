//! The CLI side of the control socket: `worker status`, `pair`, `devices`
//! and `revoke` (SPECS §33). Each returns the process exit code.

// stub: filled by G

/// `flightdeck worker status [--json]`. Works when the worker is stopped.
pub fn run_status(_json: bool) -> i32 {
    // stub: filled by G
    1
}

/// `flightdeck worker pair [--json]`: mint a pairing code in the running worker.
pub fn run_pair(_json: bool) -> i32 {
    // stub: filled by G
    1
}

/// `flightdeck worker devices [--json]`: the paired Desktops.
pub fn run_devices(_json: bool) -> i32 {
    // stub: filled by G
    1
}

/// `flightdeck worker revoke <device>`: revoke one Desktop and close its
/// connections.
pub fn run_revoke(_device: &str) -> i32 {
    // stub: filled by G
    1
}
