//! What the update banner shows, and the two things that change it: the
//! background check and "Update now".
//!
//! The status is a GPUI global, so the banner (`overlays::update`) reads it
//! from the `App` it is already given and the shell needs no new plumbing.
//! Changing it refreshes every window.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use flightdeck::contracts::real::RealFs;
use gpui::{App, Global};

use super::apply::UpdateFs;
use super::apply::{Installer, Outcome};
use super::check::{cache_path, newer_version};
use super::kind::{detect, Arch, DirectKind, InstallKind, Os, Probe};
use super::release::Version;
use super::source::{GithubReleases, ReleaseSource, SystemUnzip, Unzip, UpdateError};

/// The desktop app's own version.
fn current_version() -> Option<Version> {
    Version::parse(env!("CARGO_PKG_VERSION"))
}

/// Where the update flow stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateStatus {
    /// A newer release exists. `kind` decides what the banner offers.
    Available { version: String, kind: InstallKind },
    /// "Update now" is downloading and installing.
    Installing { version: String },
    /// The new version is installed (or staged); a restart runs it.
    Installed { version: String, outcome: Outcome },
    /// The install failed; the installed copy is untouched.
    Failed { version: String, message: String },
}

struct UpdateModel(UpdateStatus);

impl Global for UpdateModel {}

/// The current status, if the check has found anything.
pub fn status(cx: &App) -> Option<UpdateStatus> {
    cx.try_global::<UpdateModel>().map(|m| m.0.clone())
}

/// Replace the status and redraw.
pub fn set_status(cx: &mut App, status: UpdateStatus) {
    cx.set_global(UpdateModel(status));
    cx.refresh_windows();
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Start the once-a-day check on a background thread. `enabled` is the
/// effective `[update] check` setting; when off nothing is contacted.
pub fn start_check(cx: &mut App, enabled: bool) {
    if !enabled {
        return;
    }
    let (Some(current), Some(probe)) = (current_version(), Probe::from_env()) else {
        return;
    };
    let Some(cache_file) = cache_path(probe.os, &|name| std::env::var_os(name).map(PathBuf::from))
    else {
        return;
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
                newer_version(
                    &RealFs,
                    &GithubReleases::flightdeck(),
                    &cache_file,
                    unix_now(),
                    current,
                )
            })
            .await;
        if let Some(version) = found {
            cx.update(|cx| {
                set_status(
                    cx,
                    UpdateStatus::Available {
                        version: version.to_string(),
                        kind,
                    },
                )
            });
        }
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
