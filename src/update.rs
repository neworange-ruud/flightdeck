//! `flightdeck update`: in-place self-update for installer-based installs
//! (SPECS §29).
//!
//! FlightDeck ships through two channels: the shell installer (curl | sh, from
//! GitHub Releases) and a Homebrew tap. Only the shell installer writes an
//! *install receipt* — the manifest [`axoupdater`] reads to learn where the
//! binary lives and where its releases are hosted. Homebrew, `cargo install`,
//! and hand-copied binaries leave no receipt.
//!
//! That receipt is also our package-manager guard: if it is absent we must
//! **not** self-replace the binary, because doing so would desync the managing
//! package manager (Homebrew's formula would still believe the old version is
//! installed). So a missing receipt is not an error — it is the signal to defer
//! to the package manager and exit cleanly.

use crate::contracts::error::{FlightDeckError, Result};

/// App name as recorded in the install receipt and GitHub Release artifacts.
const APP_NAME: &str = "flightdeck";

/// `flightdeck update`: check GitHub Releases and replace the running binary in
/// place when a newer version exists. Does not launch the TUI and does not
/// require being inside a Git repository — updates work from anywhere.
///
/// Installer-based installs self-update; everything else (Homebrew, `cargo
/// install`, manual copies) is detected via the absent receipt and deferred to
/// the package manager (see [`no_receipt_guidance`]).
pub fn run() -> Result<()> {
    let mut updater = axoupdater::AxoUpdater::new_for(APP_NAME);

    // The receipt doubles as the package-manager guard: no receipt => not an
    // installer install => defer rather than clobber a managed binary.
    if updater.load_receipt().is_err() {
        println!("{}", no_receipt_guidance());
        return Ok(());
    }

    println!("FlightDeck: checking for updates…");
    match updater.run_sync() {
        Ok(Some(result)) => {
            println!(
                "FlightDeck: updated {} → {}. Restart FlightDeck to use the new version.",
                env!("CARGO_PKG_VERSION"),
                result.new_version,
            );
        }
        Ok(None) => {
            println!(
                "FlightDeck: already up to date (v{}).",
                env!("CARGO_PKG_VERSION")
            );
        }
        Err(e) => {
            return Err(FlightDeckError::Other(format!("update failed: {e}")));
        }
    }
    Ok(())
}

/// Guidance printed when no install receipt is found. Kept pure (no I/O) so the
/// deferral path is unit-testable: it must steer Homebrew users to `brew
/// upgrade` and never imply a self-update happened.
fn no_receipt_guidance() -> String {
    format!(
        "FlightDeck: no install receipt found, so `{APP_NAME} update` can't self-update this \
         install.\n\
         \n\
         If you installed via Homebrew, update with:\n\
         \x20\x20brew upgrade {APP_NAME}\n\
         \n\
         Otherwise re-run the installer to get the latest release:\n\
         \x20\x20curl --proto '=https' --tlsv1.2 -LsSf \
         https://github.com/neworange-ruud/flightdeck/releases/latest/download/flightdeck-installer.sh | sh"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_receipt_guidance_defers_to_homebrew() {
        let msg = no_receipt_guidance();
        // Must point Homebrew users at the package manager, not self-update.
        assert!(
            msg.contains("brew upgrade flightdeck"),
            "guidance must steer Homebrew users to `brew upgrade`: {msg}"
        );
        // Must offer the installer fallback for non-managed installs.
        assert!(
            msg.contains("flightdeck-installer.sh"),
            "guidance must offer the installer fallback: {msg}"
        );
        // Must not claim an update was performed.
        assert!(
            !msg.to_lowercase().contains("updated"),
            "guidance must not imply a self-update happened: {msg}"
        );
    }
}
