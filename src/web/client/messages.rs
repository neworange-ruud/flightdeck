//! What a worker says to the front-end attached to it, in words
//! (`specs/WORKER_PRD.md` §4.5): its host messages, and the advice when its
//! FlightDeck is not the attached front-end's.
//!
//! A worker has no status line of its own, so what the TUI would put there
//! (a refused merge, a failed spawn or clone, an update) is sent to the
//! attached window as a host message. A message that arrives while the window
//! is open is a status-bar notice ([`super::session::RemoteSession`]), and an
//! error is also a message overlay. Messages the worker kept while nothing
//! was attached are replayed with the snapshot and shown together, as one
//! overlay: [`away_text`].
//!
//! Moved here from FlightDeck Desktop (`remote-control-ki05`) so the terminal
//! UI's `flightdeck connect` says the same things. Only the update advice
//! differs between the two, because each updates itself differently:
//! [`Advice`].

use crate::web::protocol::{HostMessage, MessageLevel};

/// One message as a line: its level when it is louder than information,
/// then the worker's words verbatim.
pub fn line(message: &HostMessage) -> String {
    match message.level {
        MessageLevel::Info => message.text.clone(),
        MessageLevel::Warning => format!("Warning: {}", message.text),
        MessageLevel::Error => format!("Error: {}", message.text),
    }
}

/// The overlay for messages replayed on attach: a heading with their count,
/// then one line each, oldest first.
pub fn away_text(messages: &[HostMessage]) -> String {
    let heading = match messages.len() {
        1 => "1 message while you were away".to_string(),
        n => format!("{n} messages while you were away"),
    };
    std::iter::once(heading)
        .chain(messages.iter().map(line))
        .collect::<Vec<_>>()
        .join("\n")
}

/// How to bring a worker to the front-end's version: from its shell, update,
/// then restart the worker so the new binary serves.
pub const WORKER_UPDATE_ADVICE: &str =
    "Update it from the worker shell: `flightdeck update`, then `flightdeck worker restart`.";

/// What a front-end tells its user to update when its version and a worker's
/// differ: itself (when the worker is newer), or either side (when which is
/// older is not known). Updating the worker is the same advice for every
/// front-end, [`WORKER_UPDATE_ADVICE`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Advice {
    /// When the worker is the newer side: update this front-end.
    pub update_self: &'static str,
    /// When it is not known which side is older.
    pub update_either: &'static str,
}

/// `major.minor.patch` of a version, ignoring a pre-release or build suffix;
/// `None` when it does not parse.
fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let core = s.trim().trim_start_matches('v').split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let version = (
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    );
    parts.next().is_none().then_some(version)
}

/// What to update, given the worker's version (`host`) and the front-end's
/// (`app`): the front-end when the worker is newer, else the worker. A
/// version that does not parse keeps the worker advice.
fn which(host: &str, app: &str, advice: &Advice) -> &'static str {
    match (parse_version(host), parse_version(app)) {
        (Some(host), Some(app)) if host > app => advice.update_self,
        _ => WORKER_UPDATE_ADVICE,
    }
}

/// The version-skew banner, when the worker's FlightDeck (`host`) is not the
/// core the front-end was built with (`app`). `None` when they match, or the
/// worker has not said. The advice names the older side.
pub fn skew_warning(host: &str, app: &str, advice: &Advice) -> Option<String> {
    if host.is_empty() || host == app {
        return None;
    }
    Some(format!(
        "This worker runs FlightDeck {host}; this app was built with {app}. {}",
        which(host, app, advice)
    ))
}

/// What a worker link that ended on a protocol mismatch says: the link's own
/// reason, then the same advice as the banner (with the versions, when the
/// worker said its version before the mismatch; without them, which side is
/// older is not known).
pub fn mismatch_advice(reason: &str, host: &str, app: &str, advice: &Advice) -> String {
    let text = skew_warning(host, app, advice).unwrap_or_else(|| advice.update_either.to_string());
    if reason.is_empty() {
        text
    } else {
        format!("{reason} {text}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(id: u64, level: MessageLevel, text: &str) -> HostMessage {
        HostMessage {
            id,
            at_ms: id as i64,
            level,
            text: text.to_string(),
        }
    }

    const ADVICE: Advice = Advice {
        update_self: "Update me.",
        update_either: "Update whichever is older.",
    };

    #[test]
    fn the_away_overlay_counts_and_lists_oldest_first() {
        let text = away_text(&[
            message(1, MessageLevel::Info, "FlightDeck 2.0 is available."),
            message(2, MessageLevel::Error, "Clone failed: repository not found"),
        ]);
        assert_eq!(
            text,
            "2 messages while you were away\n\
             FlightDeck 2.0 is available.\n\
             Error: Clone failed: repository not found"
        );
        assert!(away_text(&[message(1, MessageLevel::Warning, "x")])
            .starts_with("1 message while you were away\nWarning: x"));
    }

    #[test]
    fn the_newer_side_decides_whose_advice_is_given() {
        assert_eq!(skew_warning("1.4.0", "1.4.0", &ADVICE), None);
        assert!(skew_warning("1.5.0", "1.4.0", &ADVICE)
            .unwrap()
            .ends_with("Update me."));
        assert!(skew_warning("1.3.0", "1.4.0", &ADVICE)
            .unwrap()
            .ends_with(WORKER_UPDATE_ADVICE));
        assert_eq!(
            mismatch_advice("v9.", "", "1.4.0", &ADVICE),
            "v9. Update whichever is older."
        );
    }
}
