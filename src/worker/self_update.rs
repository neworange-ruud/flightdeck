//! The real [`WorkerUpdater`]: a worker replacing its own FlightDeck and
//! restarting into it, at a Desktop's request (`specs/WORKER_PRD.md` W20).
//!
//! **Updating** follows how this copy was installed, the same rule as
//! `flightdeck update` (SPECS §29): a Homebrew install is upgraded with
//! `brew` (so Homebrew's records stay true), an install by FlightDeck's own
//! installer is replaced in place through its receipt (axoupdater), and
//! anything else is refused with the reason.
//!
//! **Restarting** is `flightdeck worker restart` in a process that outlives
//! this one: it stops this worker (and its supervisor) and starts the new
//! binary, which resumes the agents. It is started from the stable path
//! ([`service::stable_exe`]: the Homebrew link, not the Cellar directory the
//! upgrade just removed). Under systemd it runs as a transient unit of its
//! own (`systemd-run --user`): a child of the worker's unit would be killed
//! with the unit's cgroup the moment the supervisor stops, half-way through
//! the restart. Elsewhere a new session (`setsid`) is enough.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::host::worker::self_update::{UpdateOutcome, WorkerUpdater};
use crate::worker::paths::WorkerPaths;
use crate::worker::{daemon, service};

/// How this copy of FlightDeck was installed, as far as updating it goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallSource {
    /// Under a Homebrew `Cellar`; `brew` is the prefix's.
    Homebrew { brew: PathBuf },
    /// Anything else: FlightDeck's installer, if its receipt says so.
    Other,
}

/// Where the binary at `canonical` (symlinks resolved) came from. Pure.
pub fn install_source(canonical: &Path) -> InstallSource {
    let parts: Vec<_> = canonical.components().collect();
    match parts.iter().rposition(|c| c.as_os_str() == "Cellar") {
        Some(at) if at > 0 => {
            let prefix: PathBuf = parts[..at].iter().collect();
            InstallSource::Homebrew {
                brew: prefix.join("bin").join("brew"),
            }
        }
        _ => InstallSource::Other,
    }
}

/// The version a `flightdeck --version` line names (`flightdeck 2.3.0`).
pub fn parse_version_line(text: &str) -> Option<String> {
    let version = text.split_whitespace().nth(1)?;
    version
        .split('.')
        .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
        .then(|| version.to_string())
}

/// The update, on this machine.
pub struct RealWorkerUpdater {
    paths: WorkerPaths,
}

impl RealWorkerUpdater {
    pub fn new(paths: WorkerPaths) -> Self {
        RealWorkerUpdater { paths }
    }

    /// The path a new process should start: the running binary's, made
    /// stable across an update.
    fn stable_exe() -> Result<PathBuf, String> {
        let exe = std::env::current_exe()
            .map_err(|e| format!("could not find the flightdeck binary ({e})"))?;
        Ok(service::stable_exe(&exe, &|p| p.exists()))
    }
}

/// Run `argv`, quietly; the error is its last line of output.
fn run(program: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new(program)
        .args(args)
        // Homebrew: no hints or analytics notices in what we capture.
        .env("HOMEBREW_NO_ENV_HINTS", "1")
        .env("HOMEBREW_NO_INSTALL_FROM_API_HINTS", "1")
        .output()
        .map_err(|e| format!("could not run {} ({e})", program.display()))?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    if output.status.success() {
        return Ok(stdout);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let last = stderr
        .lines()
        .chain(stdout.lines())
        .rfind(|l| !l.trim().is_empty())
        .unwrap_or("no output")
        .trim()
        .to_string();
    Err(format!(
        "`{} {}` failed: {last}",
        program.display(),
        args.join(" ")
    ))
}

fn installer_update() -> Result<Option<String>, String> {
    #[cfg(all(feature = "self-update", not(windows)))]
    {
        crate::update::update_installer_quietly()
    }
    #[cfg(not(all(feature = "self-update", not(windows))))]
    {
        Err("this build was compiled without self-update support".to_string())
    }
}

impl WorkerUpdater for RealWorkerUpdater {
    fn update(&self) -> Result<UpdateOutcome, String> {
        let exe = Self::stable_exe()?;
        let canonical = std::fs::canonicalize(&exe).unwrap_or_else(|_| exe.clone());
        match install_source(&canonical) {
            InstallSource::Homebrew { brew } => {
                if !brew.exists() {
                    return Err(format!(
                        "this FlightDeck is a Homebrew install, but {} is missing",
                        brew.display()
                    ));
                }
                // `brew update` first: `upgrade` only auto-updates the tap
                // once a day, and the release may be minutes old.
                run(&brew, &["update", "--quiet"])?;
                run(&brew, &["upgrade", "flightdeck"])?;
                let version = run(&exe, &["--version"])
                    .ok()
                    .and_then(|out| parse_version_line(&out))
                    .ok_or("Homebrew upgraded FlightDeck, but the new version does not run")?;
                Ok(if version == crate::VERSION {
                    UpdateOutcome::UpToDate
                } else {
                    UpdateOutcome::Updated { version }
                })
            }
            InstallSource::Other => Ok(match installer_update()? {
                Some(version) => UpdateOutcome::Updated { version },
                None => UpdateOutcome::UpToDate,
            }),
        }
    }

    fn restart(&self) -> Result<(), String> {
        let exe = Self::stable_exe()?;
        let exe_text = exe.to_string_lossy().into_owned();
        // systemd sets INVOCATION_ID for every process of a unit it started.
        let under_systemd = std::env::var_os("INVOCATION_ID").is_some();
        let systemd_run = ["/usr/bin/systemd-run", "/bin/systemd-run"]
            .into_iter()
            .map(Path::new)
            .find(|p| p.exists());
        let spawned = match (under_systemd, systemd_run) {
            (true, Some(systemd_run)) => daemon::spawn_detached(
                systemd_run,
                &[
                    "--user",
                    "--collect",
                    "--quiet",
                    "--unit",
                    &format!("flightdeck-worker-update-{}", std::process::id()),
                    &exe_text,
                    "worker",
                    "restart",
                ],
                &self.paths.home,
                Some(&self.paths.log_file),
            ),
            _ => daemon::spawn_detached(
                &exe,
                &["worker", "restart"],
                &self.paths.home,
                Some(&self.paths.log_file),
            ),
        };
        spawned
            .map(|_| ())
            .map_err(|e| format!("could not start `flightdeck worker restart` ({e})"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cellar_binary_is_homebrews_and_names_its_brew() {
        assert_eq!(
            install_source(Path::new(
                "/opt/homebrew/Cellar/flightdeck/2.2.0/bin/flightdeck"
            )),
            InstallSource::Homebrew {
                brew: PathBuf::from("/opt/homebrew/bin/brew")
            }
        );
        assert_eq!(
            install_source(Path::new(
                "/home/linuxbrew/.linuxbrew/Cellar/flightdeck/2.2.0/bin/flightdeck"
            )),
            InstallSource::Homebrew {
                brew: PathBuf::from("/home/linuxbrew/.linuxbrew/bin/brew")
            }
        );
        for other in [
            "/home/u/.cargo/bin/flightdeck",
            "/usr/local/bin/flightdeck",
            "/home/u/.local/bin/flightdeck",
        ] {
            assert_eq!(install_source(Path::new(other)), InstallSource::Other);
        }
    }

    #[test]
    fn the_version_line_is_read_strictly() {
        assert_eq!(
            parse_version_line("flightdeck 2.3.0\n").as_deref(),
            Some("2.3.0")
        );
        assert_eq!(parse_version_line("flightdeck"), None);
        assert_eq!(parse_version_line("flightdeck 2.3.0-rc1"), None);
        assert_eq!(parse_version_line(""), None);
    }
}
