//! Download, verify, and replace the running installation.
//!
//! ## Rules that make the swap safe
//!
//! - **Stage beside the target, never in a temp dir.** A rename is atomic only
//!   within one volume, and `/tmp` (or `%TEMP%`) is often another one. The
//!   staging directory is always a sibling of what it replaces ([`Layout`]).
//! - **Verify before anything is touched.** The installed copy is not modified
//!   until the download's SHA-256 has matched the published checksum.
//! - **Swap by rename, with the old copy kept until the new one is in.** If
//!   the second rename fails the first is undone ([`swap_one`]).
//! - **Windows cannot replace a running `.exe` but can rename it**, so the
//!   update is staged and applied at the *next* launch, before any window is
//!   created ([`finish_staged`]).
//!
//! Per install kind:
//!
//! | Kind | Staging | Swap | Then |
//! | --- | --- | --- | --- |
//! | macOS bundle | `<parent>/.FlightDeck-update-<v>/` | bundle to `previous.app`, new `.app` to the bundle path (two renames, rolled back on failure) | restart |
//! | AppImage | `<dir>/.<name>.update-<v>/` | one rename over the file (atomic; the running image keeps its old inode) | restart |
//! | Windows portable | `<dir>/.flightdeck-update/` | at next launch | staged, applied on start |

use std::io::Read;
use std::path::{Path, PathBuf};

use flightdeck::contracts::real::RealFs;
use flightdeck::contracts::FileSystem;

use super::kind::{Arch, DirectKind};
use super::release::{
    parse_checksum, select, sha256_hex, PackageFormat, ReleaseInfo, Selected, Version,
};
use super::source::{ReleaseSource, Unzip, UpdateError};

/// The filesystem operations an install needs beyond [`FileSystem`]. A trait
/// so the seam is explicit; the tests run it against temp directories.
pub trait UpdateFs: FileSystem {
    /// Rename `from` to `to` (a file or a directory), replacing a file `to`
    /// on Unix.
    fn rename(&self, from: &Path, to: &Path) -> Result<(), UpdateError>;
    /// Remove one file.
    fn remove_file(&self, path: &Path) -> Result<(), UpdateError>;
    /// Open a file for streaming reads (assets are tens of megabytes).
    fn open_read(&self, path: &Path) -> Result<Box<dyn Read>, UpdateError>;
    /// Mark a file executable (a no-op where there is no such bit).
    fn set_executable(&self, path: &Path) -> Result<(), UpdateError>;
}

impl UpdateFs for RealFs {
    fn rename(&self, from: &Path, to: &Path) -> Result<(), UpdateError> {
        std::fs::rename(from, to)
            .map_err(|e| UpdateError::Io(format!("{} to {}: {e}", from.display(), to.display())))
    }

    fn remove_file(&self, path: &Path) -> Result<(), UpdateError> {
        std::fs::remove_file(path).map_err(|e| UpdateError::Io(format!("{}: {e}", path.display())))
    }

    fn open_read(&self, path: &Path) -> Result<Box<dyn Read>, UpdateError> {
        let file = std::fs::File::open(path)
            .map_err(|e| UpdateError::Io(format!("{}: {e}", path.display())))?;
        Ok(Box::new(file))
    }

    fn set_executable(&self, path: &Path) -> Result<(), UpdateError> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
                .map_err(|e| UpdateError::Io(format!("{}: {e}", path.display())))
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            Ok(())
        }
    }
}

/// Every path an install of one kind and version touches. Pure, so the
/// placement rules are tested without a filesystem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    /// Created for the install and removed when it ends.
    pub staging: PathBuf,
    /// Where the asset is saved.
    pub download: PathBuf,
    /// Where its checksum file is saved.
    pub checksum: PathBuf,
    /// Where a zip is unpacked (unused for an AppImage).
    pub extracted: PathBuf,
    /// The installed thing the update replaces: the bundle, the AppImage, or
    /// the directory holding the portable `.exe`.
    pub target: PathBuf,
    /// Where the replaced copy is parked during the swap.
    pub aside: PathBuf,
}

/// The name the checksum file of `asset` is saved under while staged.
const CHECKSUM_FILE: &str = "asset.sha256";
/// Windows: the fixed staging directory, so a later launch can find it.
pub const WINDOWS_STAGING: &str = ".flightdeck-update";
/// Windows: where replaced files are parked; deleted on the next launch.
pub const WINDOWS_ASIDE: &str = ".flightdeck-old";
/// Windows: written last, after verification and unpacking, so a half-staged
/// update is never applied.
pub const READY_MARKER: &str = "READY";

impl Layout {
    /// The layout for updating `kind` to `version`, downloading `asset_name`.
    pub fn plan(kind: &DirectKind, version: Version, asset_name: &str) -> Layout {
        match kind {
            DirectKind::MacBundle { bundle } => {
                let parent = parent_of(bundle);
                let staging = parent.join(format!(".FlightDeck-update-{version}"));
                Layout {
                    download: staging.join(asset_name),
                    checksum: staging.join(CHECKSUM_FILE),
                    extracted: staging.join("extracted"),
                    aside: staging.join("previous.app"),
                    target: bundle.clone(),
                    staging,
                }
            }
            DirectKind::AppImage { file } => {
                let parent = parent_of(file);
                let name = file
                    .file_name()
                    .map_or_else(|| "FlightDeck".into(), |n| n.to_string_lossy().into_owned());
                let staging = parent.join(format!(".{name}.update-{version}"));
                Layout {
                    download: staging.join(asset_name),
                    checksum: staging.join(CHECKSUM_FILE),
                    extracted: staging.join("unused"),
                    aside: staging.join("previous"),
                    target: file.clone(),
                    staging,
                }
            }
            DirectKind::WindowsPortable { exe } => {
                let dir = parent_of(exe);
                let staging = dir.join(WINDOWS_STAGING);
                Layout {
                    download: staging.join(asset_name),
                    checksum: staging.join(CHECKSUM_FILE),
                    extracted: staging.join("payload"),
                    aside: dir.join(WINDOWS_ASIDE),
                    target: dir,
                    staging,
                }
            }
        }
    }
}

fn parent_of(path: &Path) -> PathBuf {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

/// How an install ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The new version is in place; restart to run it.
    Replaced,
    /// The new version is staged and replaces this one at the next launch.
    StagedForNextLaunch,
}

/// Everything an install needs from the outside.
pub struct Installer<'a> {
    pub fs: &'a dyn UpdateFs,
    pub source: &'a dyn ReleaseSource,
    pub unzip: &'a dyn Unzip,
    pub arch: Arch,
}

impl Installer<'_> {
    /// Update the installation described by `kind` to `release`.
    pub fn install(
        &self,
        release: &ReleaseInfo,
        kind: &DirectKind,
    ) -> Result<Outcome, UpdateError> {
        let format = PackageFormat::for_install(kind);
        let selected =
            select(release, format, self.arch).map_err(|e| UpdateError::Release(e.to_string()))?;
        let layout = Layout::plan(kind, release.version, &selected.asset.name);
        // Whatever happens, do not leave a half-filled staging directory for
        // the next attempt to trip on. A staged Windows update is the one
        // thing that stays, and only once it is complete.
        let result = self.stage_and_swap(kind, &selected, &layout);
        let keep = matches!(result, Ok(Outcome::StagedForNextLaunch));
        if !keep {
            let _ = self.fs.remove_dir_all(&layout.staging);
        }
        result
    }

    fn stage_and_swap(
        &self,
        kind: &DirectKind,
        selected: &Selected,
        layout: &Layout,
    ) -> Result<Outcome, UpdateError> {
        // A previous, interrupted attempt may have left the directory.
        if self.fs.exists(&layout.staging) {
            self.fs.remove_dir_all(&layout.staging)?;
        }
        self.fs.create_dir_all(&layout.staging)?;

        self.source
            .download(&selected.checksum.url, &layout.checksum)?;
        self.source
            .download(&selected.asset.url, &layout.download)?;
        self.verify(&selected.asset.name, layout)?;

        match kind {
            DirectKind::AppImage { file } => {
                self.fs.set_executable(&layout.download)?;
                // Atomic on one volume; the running process keeps the old inode.
                self.fs.rename(&layout.download, file)?;
                Ok(Outcome::Replaced)
            }
            DirectKind::MacBundle { bundle } => {
                self.fs.create_dir_all(&layout.extracted)?;
                self.unzip.unzip(&layout.download, &layout.extracted)?;
                let new_bundle = self.find_bundle(&layout.extracted)?;
                swap_one(self.fs, &new_bundle, bundle, &layout.aside)?;
                Ok(Outcome::Replaced)
            }
            DirectKind::WindowsPortable { .. } => {
                self.fs.create_dir_all(&layout.extracted)?;
                self.unzip.unzip(&layout.download, &layout.extracted)?;
                if self.fs.list_dir(&layout.extracted)?.is_empty() {
                    return Err(UpdateError::BadArchive("the archive is empty".into()));
                }
                // Last, so an interrupted staging is never applied.
                self.fs
                    .write(&layout.staging.join(READY_MARKER), "ready\n")?;
                Ok(Outcome::StagedForNextLaunch)
            }
        }
    }

    /// The downloaded file against the checksum published for it.
    fn verify(&self, asset_name: &str, layout: &Layout) -> Result<(), UpdateError> {
        let text = self.fs.read_to_string(&layout.checksum)?;
        let expected = parse_checksum(&text, asset_name).ok_or(UpdateError::BadChecksum)?;
        let actual = sha256_hex(self.fs.open_read(&layout.download)?)?;
        if actual == expected {
            Ok(())
        } else {
            Err(UpdateError::ChecksumMismatch { expected, actual })
        }
    }

    /// The `.app` directory an extracted bundle zip holds, checked to look
    /// like a bundle rather than trusted by name.
    fn find_bundle(&self, extracted: &Path) -> Result<PathBuf, UpdateError> {
        let bundle = self
            .fs
            .list_dir(extracted)?
            .into_iter()
            .find(|p| p.extension().is_some_and(|e| e == "app") && self.fs.is_dir(p))
            .ok_or_else(|| UpdateError::BadArchive("no .app in the archive".into()))?;
        if !self.fs.is_dir(&bundle.join("Contents").join("MacOS")) {
            return Err(UpdateError::BadArchive(
                "the .app has no Contents/MacOS".into(),
            ));
        }
        Ok(bundle)
    }
}

/// Replace `current` by `new`, parking the old copy at `aside`, and undo the
/// first rename if the second fails. `aside` must be on the same volume.
fn swap_one(
    fs: &dyn UpdateFs,
    new: &Path,
    current: &Path,
    aside: &Path,
) -> Result<(), UpdateError> {
    let had_current = fs.exists(current);
    if had_current {
        if fs.exists(aside) {
            remove_any(fs, aside)?;
        }
        fs.rename(current, aside)?;
    }
    if let Err(e) = fs.rename(new, current) {
        if had_current {
            // Put the old copy back; if even that fails the error below is the
            // more useful one to show, and `aside` still holds the old copy.
            let _ = fs.rename(aside, current);
        }
        return Err(e);
    }
    Ok(())
}

fn remove_any(fs: &dyn UpdateFs, path: &Path) -> Result<(), UpdateError> {
    if fs.is_dir(path) {
        fs.remove_dir_all(path)?;
    } else {
        fs.remove_file(path)?;
    }
    Ok(())
}

/// Windows, at launch: apply a staged update. Every top-level entry of the
/// staged payload replaces the same-named entry beside the executable (the old
/// one is renamed into the aside directory, which Windows allows even for the
/// running `.exe`). All or nothing: a failure puts back what was already moved.
///
/// Returns `Ok(true)` when an update was applied and the process should
/// re-launch itself; `Ok(false)` when nothing was staged (also the case on
/// every launch after the first, when the aside directory is cleaned up).
pub fn finish_staged(fs: &dyn UpdateFs, exe: &Path) -> Result<bool, UpdateError> {
    let layout = Layout::plan(
        &DirectKind::WindowsPortable {
            exe: exe.to_path_buf(),
        },
        Version::new(0, 0, 0),
        "",
    );
    // The previous update's parked files are dead weight now (best effort: the
    // old executable may still be open, in which case the next launch tries
    // again).
    if fs.exists(&layout.aside) && !fs.exists(&layout.staging.join(READY_MARKER)) {
        let _ = fs.remove_dir_all(&layout.aside);
    }
    if !fs.exists(&layout.staging.join(READY_MARKER)) {
        // A staging directory without the marker is an interrupted download.
        if fs.exists(&layout.staging) {
            let _ = fs.remove_dir_all(&layout.staging);
        }
        return Ok(false);
    }
    // Files parked by an earlier update that could not be deleted then.
    let _ = fs.remove_dir_all(&layout.aside);
    fs.create_dir_all(&layout.aside)?;
    let mut done: Vec<(PathBuf, PathBuf, PathBuf)> = Vec::new();
    // A fixed order, so an interrupted swap behaves the same way every time.
    let mut staged_entries = fs.list_dir(&layout.extracted)?;
    staged_entries.sort();
    for staged in staged_entries {
        let Some(name) = staged.file_name() else {
            continue;
        };
        let current = layout.target.join(name);
        let aside = layout.aside.join(name);
        if let Err(e) = swap_one(fs, &staged, &current, &aside) {
            // Undo the entries already swapped, newest first.
            for (current, aside, staged) in done.iter().rev() {
                let _ = fs.rename(current, staged);
                if fs.exists(aside) {
                    let _ = fs.rename(aside, current);
                }
            }
            return Err(e);
        }
        done.push((current, aside, staged));
    }
    fs.remove_dir_all(&layout.staging)?;
    Ok(true)
}

#[cfg(test)]
pub(crate) mod tests;
