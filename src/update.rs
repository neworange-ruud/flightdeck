//! `flightdeck update`: in-place self-update for installer-based installs
//! (SPECS §29).
//!
//! FlightDeck ships through two channels: the shell installer (curl | sh, from
//! GitHub Releases) and a Homebrew tap. Only the shell installer writes an
//! *install receipt* — the manifest [`axoupdater`] reads to learn where the
//! binary lives and where its releases are hosted. Homebrew, `cargo install`,
//! and hand-copied binaries leave no receipt.
//!
//! That receipt is also our package-manager guard: we self-update **only** when
//! a receipt exists *and* it was written for the running executable. If it is
//! absent (Homebrew/manual), or present but for a binary at a different path (a
//! stale/foreign receipt, or a moved binary), self-replacing would desync the
//! managing package manager (its formula would still believe the old version is
//! installed). Neither case is an error — both are the signal to defer to the
//! package manager and exit cleanly.

use crate::contracts::error::{FlightDeckError, Result};
use axoupdater::Version;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::mpsc::Sender;

/// App name as recorded in the install receipt and GitHub Release artifacts.
const APP_NAME: &str = "flightdeck";
/// GitHub coordinates of the published releases (mirrors `dist-workspace.toml`).
const RELEASE_OWNER: &str = "neworange-ruud";
const RELEASE_REPO: &str = "flightdeck";
/// Minimum gap between background update checks: once a day (SPECS §30).
const CHECK_INTERVAL_SECS: u64 = 24 * 60 * 60;

/// `flightdeck update`: check GitHub Releases and replace the running binary in
/// place when a newer version exists. Does not launch the TUI and does not
/// require being inside a Git repository — updates work from anywhere.
///
/// Installer-based installs self-update; everything else (Homebrew, `cargo
/// install`, manual copies) is deferred to the package manager (see
/// [`not_self_updatable_guidance`]).
pub fn run() -> Result<()> {
    let mut updater = axoupdater::AxoUpdater::new_for(APP_NAME);

    // The receipt is the package-manager guard, but only when it actually
    // belongs to THIS executable. `load_receipt` searching the user's config
    // dirs can turn up a receipt for a *different* install (e.g. a stale one
    // from a since-removed installer copy) even when the running binary came
    // from Homebrew — `check_receipt_is_for_this_executable` compares install
    // paths to catch that. If we self-updated on a foreign receipt, axoupdater's
    // own eligibility check makes `run_sync` a no-op, which would otherwise be
    // reported as the misleading "already up to date". So we gate on both and
    // defer when either fails. (The eligibility check requires a loaded
    // receipt, hence the short-circuit.)
    let receipt_loaded = updater.load_receipt().is_ok();
    let receipt_matches_executable = receipt_loaded
        && updater
            .check_receipt_is_for_this_executable()
            .unwrap_or(false);
    if must_defer(receipt_loaded, receipt_matches_executable) {
        println!("{}", not_self_updatable_guidance());
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

/// Whether `flightdeck update` must defer to the package manager instead of
/// self-replacing the binary: either there is no install receipt, or the receipt
/// is for a different executable than the one running (a stale/foreign receipt,
/// or a moved binary). Self-updating in either case would desync the managing
/// package manager, so we defer. Pure, so the decision is unit-testable.
fn must_defer(receipt_loaded: bool, receipt_matches_executable: bool) -> bool {
    !receipt_loaded || !receipt_matches_executable
}

/// Guidance printed when this install can't be self-updated — no receipt, or a
/// receipt that isn't for this binary. Kept pure (no I/O) so the deferral path
/// is unit-testable: it must steer Homebrew users to refresh the tap before
/// upgrading and never imply a self-update happened.
fn not_self_updatable_guidance() -> String {
    format!(
        "FlightDeck: this install can't self-update via `{APP_NAME} update` (no install \
         receipt for this binary).\n\
         \n\
         If you installed via Homebrew, update with:\n\
         \x20\x20brew update && brew upgrade {APP_NAME}\n\
         \n\
         Otherwise re-run the installer to get the latest release:\n\
         \x20\x20curl --proto '=https' --tlsv1.2 -LsSf \
         https://github.com/neworange-ruud/flightdeck/releases/latest/download/flightdeck-installer.sh | sh"
    )
}

// ---------------------------------------------------------------------------
// Update notice (SPECS §30)
// ---------------------------------------------------------------------------
//
// When `[update] check = true`, FlightDeck makes a once-a-day background check
// against GitHub Releases on startup and surfaces a status-bar hint when a newer
// release than the running binary exists. It is strictly a *notice*: it never
// downloads or replaces anything (that stays the explicit `flightdeck update`),
// never blocks startup (it runs on a background thread), and swallows every
// error (offline, rate-limited, unparsable cache) so it can't disrupt the app.
//
// The check uses `query_new_version`, which only needs the release source — it
// does NOT rely on an install receipt, so the notice works for Homebrew installs
// too (the common case), even though those defer to `brew update && brew upgrade`
// to actually update.

/// On-disk record of the last check, so a restart within the interval reuses the
/// result instead of hitting the network again. Stored per-user (not per-repo).
#[derive(Debug, Default, Serialize, Deserialize)]
struct CheckCache {
    /// Wall-clock seconds of the last completed check.
    last_check_unix: u64,
    /// Latest version string seen at that check (may equal the running version).
    latest_version: String,
}

/// The GitHub release source the check queries.
fn release_source() -> axoupdater::ReleaseSource {
    axoupdater::ReleaseSource {
        release_type: axoupdater::ReleaseSourceType::GitHub,
        owner: RELEASE_OWNER.to_string(),
        name: RELEASE_REPO.to_string(),
        app_name: APP_NAME.to_string(),
    }
}

/// This binary's compiled version.
fn current_version() -> Option<Version> {
    Version::parse(env!("CARGO_PKG_VERSION")).ok()
}

/// Per-user cache file (global, not repo-scoped). macOS-first; honors
/// `FLIGHTDECK_UPDATE_CACHE` (tests) and `XDG_CACHE_HOME` when set. Falls back
/// to `USERPROFILE` on native Windows shells (cmd.exe/PowerShell), which don't
/// set `HOME`, so the once-a-day cache still has a stable location there.
fn cache_path() -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var("FLIGHTDECK_UPDATE_CACHE") {
        return Some(PathBuf::from(explicit));
    }
    if let Ok(xdg) = std::env::var("XDG_CACHE_HOME") {
        return Some(
            PathBuf::from(xdg)
                .join("flightdeck")
                .join("update-check.json"),
        );
    }
    if let Some(home) = std::env::var_os("HOME") {
        return Some(
            PathBuf::from(home).join("Library/Application Support/flightdeck/update-check.json"),
        );
    }
    #[cfg(windows)]
    {
        if let Some(profile) = std::env::var_os("USERPROFILE") {
            return Some(
                PathBuf::from(profile).join("AppData\\Local\\flightdeck\\update-check.json"),
            );
        }
    }
    None
}

/// Pure: is a fresh check due, given the last check time and now?
fn is_due(now_unix: u64, last_check_unix: u64) -> bool {
    now_unix.saturating_sub(last_check_unix) >= CHECK_INTERVAL_SECS
}

/// Pure: given the cached check (if any), now, and the running version, decide
/// whether a fresh network check is due and what notice the cache alone already
/// justifies (a newer version seen on the last check that we still haven't
/// caught up to). Keeping this pure makes the once-a-day logic unit-testable
/// without touching the disk or network.
fn evaluate(
    cache: Option<&CheckCache>,
    now_unix: u64,
    current: &Version,
) -> (bool, Option<String>) {
    let due = cache.is_none_or(|c| is_due(now_unix, c.last_check_unix));
    let cached_notice = cache
        .and_then(|c| Version::parse(&c.latest_version).ok())
        .filter(|latest| latest > current)
        .map(|latest| latest.to_string());
    (due, cached_notice)
}

/// Query GitHub Releases for the latest version. Works regardless of install
/// method (no receipt required). Returns `None` on any error so the check is
/// always best-effort. Blocking — call it on a background thread.
fn fetch_latest_version() -> Option<Version> {
    let mut updater = axoupdater::AxoUpdater::new_for(APP_NAME);
    updater.set_release_source(release_source());
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()?;
    match rt.block_on(updater.query_new_version()) {
        Ok(Some(latest)) => Some(latest.clone()),
        _ => None,
    }
}

fn read_cache() -> Option<CheckCache> {
    let raw = std::fs::read_to_string(cache_path()?).ok()?;
    serde_json::from_str(&raw).ok()
}

fn write_cache(cache: &CheckCache) {
    let Some(path) = cache_path() else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(cache) {
        let _ = std::fs::write(&path, json);
    }
}

/// Start the update check (SPECS §30). Returns an immediate notice (the
/// latest version string) when a *cached* result already shows a newer release,
/// so a restart surfaces yesterday's finding instantly. When a fresh check is
/// due (a day has elapsed, or no cache exists) it additionally spawns a
/// background thread that queries GitHub, refreshes the cache, and sends any
/// newer version over `tx`. A no-op returning `None` when `enabled` is false.
///
/// `now_unix` is the current wall-clock time ([`crate::contracts::Clock::now_unix_secs`]).
pub fn start_check(enabled: bool, now_unix: u64, tx: Sender<String>) -> Option<String> {
    if !enabled {
        return None;
    }
    let current = current_version()?;
    let (due, cached_notice) = evaluate(read_cache().as_ref(), now_unix, &current);

    if due {
        std::thread::spawn(move || {
            let Some(latest) = fetch_latest_version() else {
                return;
            };
            write_cache(&CheckCache {
                last_check_unix: now_unix,
                latest_version: latest.to_string(),
            });
            if latest > current {
                let _ = tx.send(latest.to_string());
            }
        });
    }
    cached_notice
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_self_updatable_guidance_defers_to_homebrew() {
        let msg = not_self_updatable_guidance();
        // Must point Homebrew users at the package manager, not self-update.
        assert!(
            msg.contains("brew update && brew upgrade flightdeck"),
            "guidance must steer Homebrew users to refresh the tap before `brew upgrade`: {msg}"
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

    #[test]
    fn must_defer_unless_receipt_is_for_this_executable() {
        // The only case that self-updates: a receipt that exists AND matches the
        // running binary.
        assert!(!must_defer(true, true), "eligible install must self-update");
        // No receipt at all (Homebrew, manual copy).
        assert!(must_defer(false, false), "no receipt must defer");
        // Receipt present but for a different binary (stale/foreign receipt, or a
        // moved binary). This is the case that previously produced a misleading
        // "already up to date" — it must defer instead.
        assert!(
            must_defer(true, false),
            "a receipt for another executable must defer, not self-update"
        );
    }

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn check_is_due_only_after_a_full_day() {
        let last = 1_000_000;
        assert!(!is_due(last, last), "same instant is not due");
        assert!(
            !is_due(last + CHECK_INTERVAL_SECS - 1, last),
            "just under a day is not due"
        );
        assert!(
            is_due(last + CHECK_INTERVAL_SECS, last),
            "exactly a day is due"
        );
    }

    #[test]
    fn evaluate_with_no_cache_is_due_and_silent() {
        let (due, notice) = evaluate(None, 5_000_000, &v("1.0.2"));
        assert!(due, "missing cache must trigger a fresh check");
        assert_eq!(notice, None, "no cached version means no notice yet");
    }

    #[test]
    fn evaluate_surfaces_cached_newer_version_without_rechecking() {
        let cache = CheckCache {
            last_check_unix: 5_000_000,
            latest_version: "1.0.3".to_string(),
        };
        // 1 hour later: not due, but the cache already knows 1.0.3 > 1.0.2.
        let (due, notice) = evaluate(Some(&cache), 5_000_000 + 3600, &v("1.0.2"));
        assert!(!due, "within the interval, no fresh check");
        assert_eq!(notice.as_deref(), Some("1.0.3"));
    }

    #[test]
    fn evaluate_is_silent_when_cache_matches_current_version() {
        let cache = CheckCache {
            last_check_unix: 5_000_000,
            latest_version: "1.0.3".to_string(),
        };
        // Already on the latest the cache knows about → no notice.
        let (_due, notice) = evaluate(Some(&cache), 5_000_000 + 3600, &v("1.0.3"));
        assert_eq!(notice, None);
    }

    #[test]
    fn evaluate_rechecks_after_a_day_even_with_a_cache() {
        let cache = CheckCache {
            last_check_unix: 5_000_000,
            latest_version: "1.0.2".to_string(),
        };
        let (due, _notice) = evaluate(Some(&cache), 5_000_000 + CHECK_INTERVAL_SECS, &v("1.0.2"));
        assert!(due, "a day later the check is due again");
    }

    #[test]
    fn start_check_disabled_is_a_noop() {
        let (tx, _rx) = std::sync::mpsc::channel();
        assert_eq!(start_check(false, 5_000_000, tx), None);
    }
}
