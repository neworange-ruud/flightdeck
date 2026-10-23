//! Which workers have a remote window open in this process.
//!
//! The Workers section's background links raise an OS notification for a
//! worker's status edges, except while that worker's own window is open: the
//! window notifies instead, so nothing is doubled (`specs/WORKER_PRD.md` §4.5).
//! The remote window records itself here when it opens and closes; the
//! background links ask [`is_open`]. Counted, not a set: two windows to one
//! address may briefly coexist while one takes over from the other.
//!
//! Lives in the library (as `flightdeck_desktop::workers::registry`) rather
//! than the binary's `workers` module so it is a real, tested API while the
//! two halves that call it are written separately.

use std::collections::HashMap;

use gpui::{App, Global};

/// Open remote windows per worker address (`host:port`, as saved).
#[derive(Debug, Default)]
pub struct OpenRemoteWindows(HashMap<String, usize>);

impl Global for OpenRemoteWindows {}

/// Host names are case-insensitive, and a saved address may carry stray
/// whitespace from being typed: both spellings name one worker.
fn key(address: &str) -> String {
    address.trim().to_ascii_lowercase()
}

/// A remote window to `address` opened.
pub fn opened(address: &str, cx: &mut App) {
    *cx.default_global::<OpenRemoteWindows>()
        .0
        .entry(key(address))
        .or_default() += 1;
}

/// A remote window to `address` closed. Closing more windows than were
/// opened is not an error: the count stops at zero.
pub fn closed(address: &str, cx: &mut App) {
    if !cx.has_global::<OpenRemoteWindows>() {
        return;
    }
    let windows = &mut cx.global_mut::<OpenRemoteWindows>().0;
    let key = key(address);
    if let Some(count) = windows.get_mut(&key) {
        *count = count.saturating_sub(1);
        if *count == 0 {
            windows.remove(&key);
        }
    }
}

/// Whether at least one remote window to `address` is open.
pub fn is_open(address: &str, cx: &App) -> bool {
    cx.try_global::<OpenRemoteWindows>()
        .is_some_and(|windows| windows.0.get(&key(address)).is_some_and(|n| *n > 0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    #[gpui::test]
    fn nothing_is_open_before_any_window(cx: &mut TestAppContext) {
        cx.update(|cx| {
            assert!(!is_open("box.ts.net:7420", cx));
            // Closing what never opened is harmless.
            closed("box.ts.net:7420", cx);
            assert!(!is_open("box.ts.net:7420", cx));
        });
    }

    #[gpui::test]
    fn open_until_every_window_closed(cx: &mut TestAppContext) {
        cx.update(|cx| {
            opened("box.ts.net:7420", cx);
            opened("box.ts.net:7420", cx);
            assert!(is_open("box.ts.net:7420", cx));
            assert!(!is_open("other.ts.net:7420", cx));
            closed("box.ts.net:7420", cx);
            assert!(is_open("box.ts.net:7420", cx), "one window remains");
            closed("box.ts.net:7420", cx);
            assert!(!is_open("box.ts.net:7420", cx));
            closed("box.ts.net:7420", cx);
            opened("box.ts.net:7420", cx);
            assert!(is_open("box.ts.net:7420", cx), "the count stopped at zero");
        });
    }

    #[gpui::test]
    fn addresses_match_case_and_whitespace_insensitively(cx: &mut TestAppContext) {
        cx.update(|cx| {
            opened(" Box.TS.net:7420", cx);
            assert!(is_open("box.ts.net:7420", cx));
            closed("BOX.ts.net:7420 ", cx);
            assert!(!is_open("box.ts.net:7420", cx));
        });
    }
}
