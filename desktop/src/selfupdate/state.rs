//! What the update banner shows, and what changes it: the background check,
//! "Check for Updates…" and "Update now".
//!
//! The status is a GPUI global, so the banner (`overlays::update`) reads it
//! from the `App` it is already given and the shell needs no new plumbing.
//! Changing it refreshes every window, and bumps a [`generation`]: a window
//! that dismissed the banner hides it only for the generation it dismissed,
//! so a new finding (or [`reshow`], the status bar's update pill) brings it
//! back.

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use flightdeck::contracts::real::RealFs;
use gpui::{App, Global};

use super::apply::UpdateFs;
use super::apply::{Installer, Outcome};
use super::check::{cache_path, fetch_latest, newer_version, RECHECK_TICK_SECS};
use super::kind::{detect, Arch, DirectKind, InstallKind, Os, Probe};
use super::release::Version;
use super::source::{
    msi_handoff, spawn_handoff, GithubReleases, ReleaseSource, SystemUnzip, Unzip, UpdateError,
};

/// The desktop app's own version.
fn current_version() -> Option<Version> {
    Version::parse(env!("CARGO_PKG_VERSION"))
}

/// Where the update flow stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateStatus {
    /// "Check for Updates…" is asking GitHub.
    Checking,
    /// "Check for Updates…" found nothing newer than `version`, the running
    /// one. The background check never reports this: it stays silent.
    UpToDate { version: String },
    /// "Check for Updates…" could not reach the release list. Also never
    /// reported by the background check.
    CheckFailed { message: String },
    /// A newer release exists. `kind` decides what the banner offers.
    Available { version: String, kind: InstallKind },
    /// "Update now" is downloading and installing.
    Installing { version: String },
    /// The new version is installed (or staged); a restart runs it.
    Installed { version: String, outcome: Outcome },
    /// The install failed; the installed copy is untouched.
    Failed { version: String, message: String },
}

impl UpdateStatus {
    /// An update is underway or done, so nothing a check finds may replace
    /// the status: the user would lose the "Restart" they are about to press.
    fn is_committed(&self) -> bool {
        matches!(
            self,
            UpdateStatus::Installing { .. } | UpdateStatus::Installed { .. }
        )
    }

    /// The version a newer release is known at, if any.
    pub fn newer_version(&self) -> Option<&str> {
        match self {
            UpdateStatus::Available { version, .. }
            | UpdateStatus::Installing { version }
            | UpdateStatus::Installed { version, .. }
            | UpdateStatus::Failed { version, .. } => Some(version),
            UpdateStatus::Checking
            | UpdateStatus::UpToDate { .. }
            | UpdateStatus::CheckFailed { .. } => None,
        }
    }
}

struct UpdateModel {
    status: UpdateStatus,
    generation: u64,
}

impl Global for UpdateModel {}

/// The current status, if the check has found anything.
pub fn status(cx: &App) -> Option<UpdateStatus> {
    cx.try_global::<UpdateModel>().map(|m| m.status.clone())
}

/// Bumped by every status change and by [`reshow`]. A window hides the
/// banner while the generation it dismissed is current. 0: nothing yet.
pub fn generation(cx: &App) -> u64 {
    cx.try_global::<UpdateModel>().map_or(0, |m| m.generation)
}

/// Replace the status and redraw.
pub fn set_status(cx: &mut App, status: UpdateStatus) {
    let generation = generation(cx) + 1;
    cx.set_global(UpdateModel { status, generation });
    cx.refresh_windows();
}

/// Bring the banner back in every window that dismissed it, as it is.
pub fn reshow(cx: &mut App) {
    if let Some(status) = status(cx) {
        set_status(cx, status);
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The effective `[update] check` setting, and whether this session has
/// already asked the release source past the cache.
#[derive(Default)]
struct CheckSettings {
    enabled: bool,
    forced: bool,
}

impl Global for CheckSettings {}

/// Who asked for a check, which decides what its answer may say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckKind {
    /// The launch check and the timer: cached, and silent unless something
    /// newer is found.
    Background,
    /// Past the cache, still silent: a worker runs a newer FlightDeck.
    Forced,
    /// "Check for Updates…": past the cache, and every answer is shown.
    Manual,
}

/// What a finished check makes of `current`: the status to show, or `None`
/// to leave it. Pure, so every path is tested without GPUI or a network.
pub fn after_check(
    current: Option<&UpdateStatus>,
    found: Result<Option<Version>, UpdateError>,
    asked: CheckKind,
    kind: &InstallKind,
    running: Version,
) -> Option<UpdateStatus> {
    if current.is_some_and(UpdateStatus::is_committed) {
        return None;
    }
    let manual = asked == CheckKind::Manual;
    match found {
        Ok(Some(version)) => {
            let version = version.to_string();
            // The same finding again: keep it, so a dismissed banner stays
            // dismissed. A newer one (or an asked-for check) replaces it.
            let same = matches!(
                current,
                Some(UpdateStatus::Available { version: shown, .. }) if *shown == version
            );
            (!same || manual).then(|| UpdateStatus::Available {
                version,
                kind: kind.clone(),
            })
        }
        Ok(None) => manual.then(|| UpdateStatus::UpToDate {
            version: running.to_string(),
        }),
        Err(e) => manual.then(|| UpdateStatus::CheckFailed {
            message: e.to_string(),
        }),
    }
}

/// Start the background check: once now, then every [`RECHECK_TICK_SECS`]
/// for as long as the app runs (the cache keeps the network to a few
/// requests a day). `enabled` is the effective `[update] check` setting;
/// when off nothing is contacted until the user asks.
pub fn start_check(cx: &mut App, enabled: bool) {
    cx.set_global(CheckSettings {
        enabled,
        forced: false,
    });
    if !enabled {
        return;
    }
    run_check(cx, CheckKind::Background);
    cx.spawn(async move |cx| loop {
        cx.background_executor()
            .timer(Duration::from_secs(RECHECK_TICK_SECS))
            .await;
        cx.update(|cx| run_check(cx, CheckKind::Background));
    })
    .detach();
}

/// Check for a newer release now, past the cache: a worker running a newer
/// FlightDeck than this app means a release may have come out since the last
/// check, and the version-skew banner should then offer it. At most once a
/// session, never while an update is already known or underway, and never
/// when `[update] check` is off.
pub fn check_now(cx: &mut App) {
    if !wants_check(cx) {
        return;
    }
    cx.global_mut::<CheckSettings>().forced = true;
    run_check(cx, CheckKind::Forced);
}

/// Whether [`check_now`] would ask: no newer release is known yet, checks
/// are on, and this session has not forced one. Read-only, so a render can
/// test it.
pub fn wants_check(cx: &App) -> bool {
    status(cx).is_none_or(|s| s.newer_version().is_none() && s != UpdateStatus::Checking)
        && cx
            .try_global::<CheckSettings>()
            .is_some_and(|s| s.enabled && !s.forced)
}

/// "Check for Updates…": ask now and show the answer, whatever it is, even
/// with `[update] check` off (the user asked). With an update already
/// underway or installed, it brings that banner back instead.
pub fn check_for_updates(cx: &mut App) {
    match status(cx) {
        Some(status) if status.is_committed() => reshow(cx),
        Some(UpdateStatus::Checking) => {}
        _ => {
            set_status(cx, UpdateStatus::Checking);
            run_check(cx, CheckKind::Manual);
        }
    }
}

/// The check itself, on a background thread.
fn run_check(cx: &mut App, asked: CheckKind) {
    let fail = |cx: &mut App, message: &str| {
        if asked == CheckKind::Manual {
            set_status(
                cx,
                UpdateStatus::CheckFailed {
                    message: message.to_string(),
                },
            );
        }
    };
    let (Some(current), Some(probe)) = (current_version(), Probe::from_env()) else {
        return fail(cx, "this build cannot tell which version it is");
    };
    let Some(cache_file) = cache_path(probe.os, &|name| std::env::var_os(name).map(PathBuf::from))
    else {
        return fail(cx, "no folder to keep the update check in");
    };
    // An architecture no release is built for cannot self-update; the notice
    // still says a newer version exists.
    let kind = match (detect(&RealFs, &probe), Arch::current()) {
        (InstallKind::Direct(_), None) => InstallKind::Unsupported,
        (kind, _) => kind,
    };
    cx.spawn(async move |cx| {
        let found = cx
            .background_executor()
            .spawn(async move {
                let now = unix_now();
                let source = GithubReleases::flightdeck();
                match asked {
                    CheckKind::Background => {
                        Ok(newer_version(&RealFs, &source, &cache_file, now, current))
                    }
                    CheckKind::Forced | CheckKind::Manual => {
                        fetch_latest(&RealFs, &source, &cache_file, now, current)
                    }
                }
            })
            .await;
        cx.update(|cx| {
            let next = after_check(status(cx).as_ref(), found, asked, &kind, current);
            if let Some(next) = next {
                set_status(cx, next);
            }
        });
    })
    .detach();
}

/// "Update now": download, verify and install off the UI thread, then show
/// the result. Does nothing unless an update is available for a direct install.
pub fn begin_install(cx: &mut App) {
    let Some(UpdateStatus::Available {
        version,
        kind: InstallKind::Direct(direct),
    }) = status(cx)
    else {
        return;
    };
    let (Some(current), Some(arch)) = (current_version(), Arch::current()) else {
        return;
    };
    set_status(
        cx,
        UpdateStatus::Installing {
            version: version.clone(),
        },
    );
    cx.spawn(async move |cx| {
        let result = cx
            .background_executor()
            .spawn(async move {
                update_to_latest(
                    &GithubReleases::flightdeck(),
                    &RealFs,
                    &SystemUnzip { os: Os::current() },
                    arch,
                    current,
                    &direct,
                )
            })
            .await;
        cx.update(|cx| {
            set_status(
                cx,
                match result {
                    Ok((installed, outcome)) => UpdateStatus::Installed {
                        version: installed.to_string(),
                        outcome,
                    },
                    Err(e) => UpdateStatus::Failed {
                        version,
                        message: e.to_string(),
                    },
                },
            )
        });
    })
    .detach();
}

/// The banner's "Restart". Usually GPUI's own restart; for an MSI install the
/// downloaded installer must run while the app is gone, so a small helper
/// waits for this process to exit, runs `msiexec` and starts the app again.
pub fn restart(cx: &mut App) {
    let Some(UpdateStatus::Installed {
        version,
        outcome: Outcome::RunInstaller { msi },
    }) = status(cx)
    else {
        cx.restart();
        return;
    };
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            set_status(
                cx,
                UpdateStatus::Failed {
                    version,
                    message: format!("could not find the running app ({e})"),
                },
            );
            return;
        }
    };
    let system_root = std::env::var_os("SystemRoot").map(PathBuf::from);
    let handoff = msi_handoff(std::process::id(), &msi, &exe, system_root.as_deref());
    // Off the UI thread: on Windows starting a process can pump the message
    // loop, which must not happen inside a GPUI update.
    cx.spawn(async move |cx| {
        let spawned = cx
            .background_executor()
            .spawn(async move { spawn_handoff(&handoff) })
            .await;
        cx.update(|cx| match spawned {
            Ok(()) => cx.quit(),
            Err(e) => set_status(
                cx,
                UpdateStatus::Failed {
                    version,
                    message: e.to_string(),
                },
            ),
        });
    })
    .detach();
}

/// Fetch the newest release and, when it is newer than `current`, install it
/// over `direct`. The whole of "Update now" apart from the real network and
/// tools, which are passed in.
pub fn update_to_latest(
    source: &dyn ReleaseSource,
    fs: &dyn UpdateFs,
    unzip: &dyn Unzip,
    arch: Arch,
    current: Version,
    direct: &DirectKind,
) -> Result<(Version, Outcome), UpdateError> {
    let release = source
        .latest()?
        .ok_or_else(|| UpdateError::Release("no desktop release is published".into()))?;
    if release.version <= current {
        return Err(UpdateError::Release(format!(
            "v{} is already the newest release",
            release.version
        )));
    }
    let outcome = Installer {
        fs,
        source,
        unzip,
        arch,
    }
    .install(&release, direct)?;
    Ok((release.version, outcome))
}

/// Windows, before any window exists: apply an update staged by the previous
/// run and start the new executable in its place. Other OSes replace their
/// installation while running, so there is nothing to finish.
pub fn finish_staged_update_at_launch() {
    if Os::current() != Os::Windows {
        return;
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    match super::apply::finish_staged(&RealFs, &exe) {
        Ok(true) => {
            let relaunched = std::process::Command::new(&exe)
                .args(std::env::args_os().skip(1))
                .spawn()
                .is_ok();
            if relaunched {
                std::process::exit(0);
            }
        }
        Ok(false) => {}
        Err(e) => eprintln!("flightdeck-desktop: could not apply the staged update: {e}"),
    }
}

#[cfg(test)]
mod tests;
