//! The Desktop's words for a worker whose FlightDeck is not this app's
//! (`specs/WORKER_PRD.md` §4.5): the version-skew banner and the advice after
//! a protocol mismatch. The wording itself, and the host messages, are the
//! core's ([`flightdeck::web::client::messages`]); only who to update is the
//! Desktop's: [`ADVICE`].

use flightdeck::web::client::messages::{self as core, Advice};

/// How to bring this app to the worker's version, when the worker is the
/// newer one: updating the worker would change nothing.
pub const APP_UPDATE_ADVICE: &str =
    "Update FlightDeck Desktop; it offers a new release when one is out.";

/// The advice when it is not known which side is older (a link that ended on
/// a protocol mismatch before the worker said its version).
pub const EITHER_UPDATE_ADVICE: &str = "Update whichever is older: FlightDeck Desktop, or the \
     worker from its shell with `flightdeck update`, then `flightdeck worker restart`.";

/// The Desktop's side of the update advice.
pub const ADVICE: Advice = Advice {
    update_self: APP_UPDATE_ADVICE,
    update_either: EITHER_UPDATE_ADVICE,
};

/// The version-skew banner, when the worker's FlightDeck (`host`) is not the
/// core this app was built with (`app`). `None` when they match, or the
/// worker has not said. The advice names the older side.
pub fn skew_warning(host: &str, app: &str) -> Option<String> {
    core::skew_warning(host, app, &ADVICE)
}

/// What a worker link that ended on a protocol mismatch says: the link's own
/// reason, then the same advice as the banner.
pub fn mismatch_advice(reason: &str, host: &str, app: &str) -> String {
    core::mismatch_advice(reason, host, app, &ADVICE)
}

#[cfg(test)]
mod tests {
    use super::*;

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
            .ends_with(core::WORKER_UPDATE_ADVICE));
    }

    #[test]
    fn an_unreadable_version_keeps_the_worker_advice_and_a_suffix_is_ignored() {
        assert!(skew_warning("nightly", "1.4.0")
            .unwrap()
            .ends_with(core::WORKER_UPDATE_ADVICE));
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
