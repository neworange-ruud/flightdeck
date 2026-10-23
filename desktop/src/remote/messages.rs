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

/// The version-skew banner, when the worker's FlightDeck (`host`) is not the
/// core this app was built with (`app`). `None` when they match, or the
/// worker has not said.
pub fn skew_warning(host: &str, app: &str) -> Option<String> {
    if host.is_empty() || host == app {
        return None;
    }
    Some(format!(
        "This worker runs FlightDeck {host}; this app was built with {app}. {UPDATE_ADVICE}"
    ))
}

/// What a worker link that ended on a protocol mismatch says: the link's own
/// reason, then the same advice as the banner (with the versions, when the
/// worker said its version before the mismatch).
pub fn mismatch_advice(reason: &str, host: &str, app: &str) -> String {
    let advice = skew_warning(host, app).unwrap_or_else(|| UPDATE_ADVICE.to_string());
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
    fn a_mismatch_ends_with_the_same_advice() {
        let text = mismatch_advice("The host speaks web protocol v8.", "", "1.4.0");
        assert_eq!(
            text,
            format!("The host speaks web protocol v8. {UPDATE_ADVICE}")
        );
        assert!(mismatch_advice("", "1.3.0", "1.4.0").starts_with("This worker runs"));
    }
}
