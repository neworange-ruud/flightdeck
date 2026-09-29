//! The network and archive tools behind two small traits.
//!
//! Everything that decides (which release, which asset, whether the download
//! is intact, where files go) is pure code elsewhere in this module. What is
//! left is "ask GitHub for the release list", "save a URL to a file" and
//! "unzip", which sit behind [`ReleaseSource`] and [`Unzip`] so tests use fakes
//! and never touch the network.
//!
//! The real implementations shell out to tools every supported OS ships
//! (`curl`, `ditto`, `tar`, `unzip`), by argv and never through a shell. That
//! keeps the desktop crate free of an HTTP/TLS client and a zip library (the
//! `ditto` extraction also preserves what a signed bundle needs). The trade is
//! that a machine without `curl` reports "could not check" and shows no
//! notice, which is the same silent failure the TUI's check has offline.

use std::fmt;
use std::path::Path;
use std::process::Command;

use super::kind::Os;
use super::release::{latest_release, ReleaseInfo};

/// Why an update step failed. The message is shown in the banner, so it is
/// written for the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateError {
    /// Talking to GitHub or downloading failed.
    Network(String),
    /// The release lacks the asset or checksum for this machine.
    Release(String),
    /// The downloaded file does not match its published checksum.
    ChecksumMismatch { expected: String, actual: String },
    /// The checksum file could not be understood.
    BadChecksum,
    /// The archive did not contain what an install of this kind needs.
    BadArchive(String),
    /// A filesystem step failed (permissions, a full disk, …).
    Io(String),
}

impl fmt::Display for UpdateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UpdateError::Network(m) => write!(f, "could not download the update ({m})"),
            UpdateError::Release(m) => write!(f, "{m}"),
            UpdateError::ChecksumMismatch { .. } => {
                write!(
                    f,
                    "the download does not match its checksum, so it was discarded"
                )
            }
            UpdateError::BadChecksum => write!(f, "the release's checksum file is unreadable"),
            UpdateError::BadArchive(m) => {
                write!(f, "the download is not a valid FlightDeck package ({m})")
            }
            UpdateError::Io(m) => write!(f, "could not install the update ({m})"),
        }
    }
}

impl std::error::Error for UpdateError {}

impl From<flightdeck::contracts::FlightDeckError> for UpdateError {
    fn from(e: flightdeck::contracts::FlightDeckError) -> Self {
        UpdateError::Io(e.to_string())
    }
}

impl From<std::io::Error> for UpdateError {
    fn from(e: std::io::Error) -> Self {
        UpdateError::Io(e.to_string())
    }
}

/// Where releases come from.
pub trait ReleaseSource {
    /// The newest published desktop release, or `None` if there is none.
    fn latest(&self) -> Result<Option<ReleaseInfo>, UpdateError>;
    /// Save the file at `url` to `dest`.
    fn download(&self, url: &str, dest: &Path) -> Result<(), UpdateError>;
}

/// Unpacks a `.zip`.
pub trait Unzip {
    /// Extract `zip` into the existing directory `dest`.
    fn unzip(&self, zip: &Path, dest: &Path) -> Result<(), UpdateError>;
}

/// GitHub Releases through `curl`. Not exercised by the tests (no network in
/// them); see the module docs.
pub struct GithubReleases {
    owner: &'static str,
    repo: &'static str,
}

impl GithubReleases {
    /// The FlightDeck repository, the same one the TUI's check queries
    /// (`src/update.rs`).
    pub const fn flightdeck() -> GithubReleases {
        GithubReleases {
            owner: "neworange-ruud",
            repo: "flightdeck",
        }
    }
}

/// The transfer limits: give up on a stalled connection, never on a slow one.
/// `--fail` turns an HTTP error into a non-zero exit instead of saving the
/// error page as the "update".
const CURL_COMMON: [&str; 8] = [
    "--fail",
    "--silent",
    "--show-error",
    "--location",
    "--proto",
    "=https",
    "--connect-timeout",
    "15",
];

fn run(mut command: Command) -> Result<Vec<u8>, UpdateError> {
    let output = command
        .output()
        .map_err(|e| UpdateError::Network(format!("{:?}: {e}", command.get_program())))?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(UpdateError::Network(stderr.trim().to_string()))
    }
}

impl ReleaseSource for GithubReleases {
    fn latest(&self) -> Result<Option<ReleaseInfo>, UpdateError> {
        let mut curl = Command::new("curl");
        curl.args(CURL_COMMON)
            .args(["--max-time", "30"])
            .args(["--header", "Accept: application/vnd.github+json"])
            .args(["--header", "User-Agent: flightdeck-desktop-updater"])
            // 100 is the API's page maximum; desktop releases are a small
            // share of the repository's, and the newest come first.
            .arg(format!(
                "https://api.github.com/repos/{}/{}/releases?per_page=100",
                self.owner, self.repo
            ));
        let body = run(curl)?;
        latest_release(&String::from_utf8_lossy(&body)).map_err(UpdateError::Network)
    }

    fn download(&self, url: &str, dest: &Path) -> Result<(), UpdateError> {
        let mut curl = Command::new("curl");
        curl.args(CURL_COMMON)
            .args(["--header", "User-Agent: flightdeck-desktop-updater"])
            .arg("--output")
            .arg(dest)
            .arg(url);
        run(curl).map(|_| ())
    }
}

/// Unzip with the OS's own tool: `ditto` on macOS (keeps permissions,
/// symlinks and extended attributes of a bundle), `tar` (bsdtar, which reads
/// zip) on Windows 10+, `unzip` elsewhere.
pub struct SystemUnzip {
    pub os: Os,
}

impl Unzip for SystemUnzip {
    fn unzip(&self, zip: &Path, dest: &Path) -> Result<(), UpdateError> {
        let mut command = match self.os {
            Os::MacOs => {
                let mut c = Command::new("ditto");
                c.args(["-x", "-k"]).arg(zip).arg(dest);
                c
            }
            Os::Windows => {
                let mut c = Command::new("tar");
                c.arg("-xf").arg(zip).arg("-C").arg(dest);
                c
            }
            Os::Linux => {
                let mut c = Command::new("unzip");
                c.arg("-q").arg(zip).arg("-d").arg(dest);
                c
            }
        };
        run_extract(&mut command)
    }
}

fn run_extract(command: &mut Command) -> Result<(), UpdateError> {
    let output = command
        .output()
        .map_err(|e| UpdateError::BadArchive(format!("{:?}: {e}", command.get_program())))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(UpdateError::BadArchive(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ))
    }
}
