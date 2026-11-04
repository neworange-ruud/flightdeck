//! What a worker says to its window in words (`specs/WORKER_PRD.md` §4.5):
//! its host messages, and the advice when its FlightDeck is not this app's.
//!
//! A worker has no status line of its own, so what the TUI would put there
//! (a refused merge, a failed spawn or clone, an update) is sent to the
//! attached window as a host message. A message that arrives while the window
//! is open is a status-bar notice ([`super::RemoteModel`]), and an error is
//! also a message overlay. Messages the worker kept while no window was
//! attached are replayed with the snapshot and shown together, as one
//! overlay: [`away_text`].

use flightdeck::web::protocol::{HostMessage, MessageLevel};

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

/// How to bring a worker to this app's version: from its shell, update, then
/// restart the worker so the new binary serves.
pub const UPDATE_ADVICE: &str =
    "Update it from the worker shell: `flightdeck update`, then `flightdeck worker restart`.";

/// How to bring this app to the worker's version, when the worker is the
/// newer one: updating the worker would change nothing.
pub const APP_UPDATE_ADVICE: &str =
    "Update FlightDeck Desktop; it offers a new release when one is out.";

/// The advice when it is not known which side is older (a link that ended on
/// a protocol mismatch before the worker said its version).
pub const EITHER_UPDATE_ADVICE: &str = "Update whichever is older: FlightDeck Desktop, or the \
     worker from its shell with `flightdeck update`, then `flightdeck worker restart`.";

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

/// What to update, given the worker's version (`host`) and this app's
/// (`app`): the app when the worker is newer, else the worker. A version
/// that does not parse keeps the worker advice, as before.
fn advice(host: &str, app: &str) -> &'static str {
    match (parse_version(host), parse_version(app)) {
        (Some(host), Some(app)) if host > app => APP_UPDATE_ADVICE,
        _ => UPDATE_ADVICE,
    }
}

/// The version-skew banner, when the worker's FlightDeck (`host`) is not the
/// core this app was built with (`app`). `None` when they match, or the
/// worker has not said. The advice names the older side.
pub fn skew_warning(host: &str, app: &str) -> Option<String> {
    if host.is_empty() || host == app {
        return None;
    }
    Some(format!(
        "This worker runs FlightDeck {host}; this app was built with {app}. {}",
        advice(host, app)
    ))
}

/// What a worker link that ended on a protocol mismatch says: the link's own
/// reason, then the same advice as the banner (with the versions, when the
/// worker said its version before the mismatch; without them, which side is
/// older is not known).
pub fn mismatch_advice(reason: &str, host: &str, app: &str) -> String {
    let advice = skew_warning(host, app).unwrap_or_else(|| EITHER_UPDATE_ADVICE.to_string());
    if reason.is_empty() {
        advice
    } else {
        format!("{reason} {advice}")
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
    fn skew_is_only_warned_about_when_the_versions_differ() {
        assert_eq!(skew_warning("1.4.0", "1.4.0"), None);
        assert_eq!(skew_warning("", "1.4.0"), None, "not said yet");
        let warning = skew_warning("1.3.2", "1.4.0").unwrap();
        assert_eq!(
            warning,
            "This worker runs FlightDeck 1.3.2; this app was built with 1.4.0. Update it from \
             the worker shell: `flightdeck update`, then `flightdeck worker restart`."
        );
    }

    #[test]
    fn a_newer_worker_is_told_to_update_the_app_not_the_worker() {
        // Desktop 0.11.1 was cut before the core's 1.32.0 bump: the worker on
        // 1.32.0 is the newer side, and updating it changes nothing.
        let warning = skew_warning("1.32.0", "1.31.0").unwrap();
        assert_eq!(
            warning,
            format!(
                "This worker runs FlightDeck 1.32.0; this app was built with 1.31.0. \
                 {APP_UPDATE_ADVICE}"
            )
        );
        assert!(!warning.contains("flightdeck update"));
        // Compared as numbers, not text: 1.10.0 is newer than 1.9.0.
        assert!(skew_warning("1.10.0", "1.9.0")
            .unwrap()
            .ends_with(APP_UPDATE_ADVICE));
        assert!(skew_warning("1.9.0", "1.10.0")
            .unwrap()
            .ends_with(UPDATE_ADVICE));
    }

    #[test]
    fn an_unreadable_version_keeps_the_worker_advice_and_a_suffix_is_ignored() {
        assert!(skew_warning("nightly", "1.4.0")
            .unwrap()
            .ends_with(UPDATE_ADVICE));
        assert!(skew_warning("1.5.0-beta.1", "1.4.0")
            .unwrap()
            .ends_with(APP_UPDATE_ADVICE));
    }

    #[test]
    fn a_mismatch_ends_with_the_same_advice() {
        let text = mismatch_advice("The host speaks web protocol v8.", "", "1.4.0");
        assert_eq!(
            text,
            format!("The host speaks web protocol v8. {EITHER_UPDATE_ADVICE}")
        );
        assert!(mismatch_advice("", "1.3.0", "1.4.0").starts_with("This worker runs"));
        assert!(mismatch_advice("v9.", "1.5.0", "1.4.0").ends_with(APP_UPDATE_ADVICE));
    }
}
