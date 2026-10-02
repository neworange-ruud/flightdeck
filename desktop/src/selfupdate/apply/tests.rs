//! The install and staged-swap paths, against a fake release source and real
//! temporary directories. Nothing here touches the network or spawns a tool;
//! the real `curl`/`ditto` are the unverified part (see `source`).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fs;

use super::super::release::{asset_name, checksum_name, ReleaseAsset};
use super::*;

pub fn v(text: &str) -> Version {
    Version::parse(text).unwrap()
}

/// A release whose assets are downloadable from `https://dl.test/<name>`.
pub fn release(version: &str, names: &[String]) -> ReleaseInfo {
    ReleaseInfo {
        version: v(version),
        page_url: "https://dl.test/release".into(),
        assets: names
            .iter()
            .map(|name| ReleaseAsset {
                name: name.clone(),
                url: format!("https://dl.test/{name}"),
            })
            .collect(),
    }
}

/// A fake release source: serves `files` by URL and counts downloads.
pub struct FakeSource {
    pub release: Option<ReleaseInfo>,
    pub files: RefCell<HashMap<String, Vec<u8>>>,
    pub downloads: Cell<u32>,
}

impl FakeSource {
    pub fn new(release: Option<ReleaseInfo>) -> FakeSource {
        FakeSource {
            release,
            files: RefCell::default(),
            downloads: Cell::new(0),
        }
    }

    /// Publish `asset` with `bytes` and a matching checksum file.
    pub fn publish(&self, asset: &str, bytes: &[u8]) {
        let digest = sha256_hex(bytes).unwrap();
        self.publish_raw(asset, bytes);
        self.publish_raw(
            &checksum_name(asset),
            format!("{digest}  {asset}\n").as_bytes(),
        );
    }

    pub fn publish_raw(&self, name: &str, bytes: &[u8]) {
        self.files
            .borrow_mut()
            .insert(format!("https://dl.test/{name}"), bytes.to_vec());
    }
}

impl ReleaseSource for FakeSource {
    fn latest(&self) -> Result<Option<ReleaseInfo>, UpdateError> {
        Ok(self.release.clone())
    }

    fn download(&self, url: &str, dest: &Path) -> Result<(), UpdateError> {
        self.downloads.set(self.downloads.get() + 1);
        let bytes = self
            .files
            .borrow()
            .get(url)
            .cloned()
            .ok_or_else(|| UpdateError::Network(format!("404 {url}")))?;
        fs::write(dest, bytes)?;
        Ok(())
    }
}

/// An "unzip" that ignores the archive and lays down a fixed tree.
pub struct FakeUnzip {
    pub files: Vec<(&'static str, &'static str)>,
}

impl Unzip for FakeUnzip {
    fn unzip(&self, _zip: &Path, dest: &Path) -> Result<(), UpdateError> {
        for (path, contents) in &self.files {
            let file = dest.join(path);
            fs::create_dir_all(file.parent().unwrap())?;
            fs::write(file, contents)?;
        }
        Ok(())
    }
}

const APP_EXE: &str = "FlightDeck.app/Contents/MacOS/flightdeck-desktop";

fn installer<'a>(
    fs: &'a dyn UpdateFs,
    source: &'a FakeSource,
    unzip: &'a FakeUnzip,
) -> Installer<'a> {
    Installer {
        fs,
        source,
        unzip,
        arch: Arch::Aarch64,
    }
}

fn read(path: impl AsRef<Path>) -> String {
    fs::read_to_string(path).unwrap()
}

fn entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

// --- placement ---------------------------------------------------------

#[test]
fn staging_is_always_beside_the_thing_it_replaces() {
    let mac = Layout::plan(
        &DirectKind::MacBundle {
            bundle: "/Applications/FlightDeck.app".into(),
        },
        v("1.4.0"),
        "a.zip",
    );
    assert_eq!(
        mac.staging,
        Path::new("/Applications/.FlightDeck-update-1.4.0")
    );
    assert_eq!(mac.target, Path::new("/Applications/FlightDeck.app"));
    assert!(
        mac.aside.starts_with("/Applications"),
        "parked on the same volume"
    );

    let image = Layout::plan(
        &DirectKind::AppImage {
            file: "/home/u/bin/FlightDeck.AppImage".into(),
        },
        v("1.4.0"),
        "a.AppImage",
    );
    assert_eq!(
        image.staging,
        Path::new("/home/u/bin/.FlightDeck.AppImage.update-1.4.0")
    );
    assert_eq!(image.download.parent(), Some(image.staging.as_path()));

    let win = Layout::plan(
        &DirectKind::WindowsPortable {
            exe: "D:/Tools/FD/flightdeck-desktop.exe".into(),
        },
        v("1.4.0"),
        "a.zip",
    );
    assert_eq!(win.staging, Path::new("D:/Tools/FD/.flightdeck-update"));
    assert_eq!(win.target, Path::new("D:/Tools/FD"));
    assert_eq!(win.aside, Path::new("D:/Tools/FD/.flightdeck-old"));
}

// --- macOS -------------------------------------------------------------

fn mac_fixture() -> (tempfile::TempDir, DirectKind, FakeSource, ReleaseInfo) {
    let dir = tempfile::tempdir().unwrap();
    let apps = dir.path().join("Applications");
    fs::create_dir_all(apps.join("FlightDeck.app/Contents/MacOS")).unwrap();
    fs::write(apps.join(APP_EXE), "old").unwrap();
    let name = asset_name(v("1.4.0"), PackageFormat::MacZip, Arch::Aarch64);
    let rel = release("1.4.0", &[name.clone(), checksum_name(&name)]);
    let source = FakeSource::new(Some(rel.clone()));
    source.publish(&name, b"zip bytes");
    let kind = DirectKind::MacBundle {
        bundle: apps.join("FlightDeck.app"),
    };
    (dir, kind, source, rel)
}

fn new_app() -> FakeUnzip {
    FakeUnzip {
        files: vec![(APP_EXE, "new")],
    }
}

#[test]
fn a_verified_bundle_replaces_the_old_one_and_staging_is_gone() {
    let (dir, kind, source, rel) = mac_fixture();
    let outcome = installer(&RealFs, &source, &new_app())
        .install(&rel, &kind)
        .unwrap();
    assert_eq!(outcome, Outcome::Replaced);
    let apps = dir.path().join("Applications");
    assert_eq!(read(apps.join(APP_EXE)), "new");
    assert_eq!(
        entries(&apps),
        ["FlightDeck.app"],
        "no staging or previous copy left"
    );
}

#[test]
fn a_corrupt_download_never_touches_the_installed_bundle() {
    let (dir, kind, source, rel) = mac_fixture();
    let name = asset_name(v("1.4.0"), PackageFormat::MacZip, Arch::Aarch64);
    // The published checksum is for other bytes than what is served.
    let good = sha256_hex(&b"zip bytes"[..]).unwrap();
    source.publish_raw(&name, b"tampered");
    source.publish_raw(&checksum_name(&name), format!("{good}  {name}").as_bytes());

    let err = installer(&RealFs, &source, &new_app())
        .install(&rel, &kind)
        .unwrap_err();
    assert!(
        matches!(err, UpdateError::ChecksumMismatch { .. }),
        "{err:?}"
    );
    let apps = dir.path().join("Applications");
    assert_eq!(read(apps.join(APP_EXE)), "old");
    assert_eq!(entries(&apps), ["FlightDeck.app"]);
}

#[test]
fn a_release_without_a_checksum_downloads_nothing() {
    let (dir, kind, source, _) = mac_fixture();
    let name = asset_name(v("1.4.0"), PackageFormat::MacZip, Arch::Aarch64);
    let rel = release("1.4.0", &[name]);
    let err = installer(&RealFs, &source, &new_app())
        .install(&rel, &kind)
        .unwrap_err();
    assert!(matches!(err, UpdateError::Release(_)), "{err:?}");
    assert_eq!(source.downloads.get(), 0);
    assert_eq!(read(dir.path().join("Applications").join(APP_EXE)), "old");
}

#[test]
fn an_unreadable_checksum_file_refuses() {
    let (_dir, kind, source, rel) = mac_fixture();
    let name = asset_name(v("1.4.0"), PackageFormat::MacZip, Arch::Aarch64);
    source.publish_raw(&checksum_name(&name), b"<html>404</html>");
    let err = installer(&RealFs, &source, &new_app())
        .install(&rel, &kind)
        .unwrap_err();
    assert_eq!(err, UpdateError::BadChecksum);
}

#[test]
fn an_archive_that_is_not_a_bundle_is_refused() {
    let (dir, kind, source, rel) = mac_fixture();
    for files in [
        vec![("README.txt", "hi")],
        // An .app with no executable directory.
        vec![("FlightDeck.app/Contents/Info.plist", "x")],
    ] {
        let err = installer(&RealFs, &source, &FakeUnzip { files })
            .install(&rel, &kind)
            .unwrap_err();
        assert!(matches!(err, UpdateError::BadArchive(_)), "{err:?}");
        assert_eq!(read(dir.path().join("Applications").join(APP_EXE)), "old");
    }
}

/// Delegates to the real filesystem but fails the first rename that would put
/// something at `refuse`, as a permissions error or a full disk would. Only
/// the first, so the rollback that follows can be observed working.
struct FailingRename {
    refuse: PathBuf,
    armed: Cell<bool>,
}

impl FailingRename {
    fn new(refuse: PathBuf) -> FailingRename {
        FailingRename {
            refuse,
            armed: Cell::new(true),
        }
    }
}

impl FileSystem for FailingRename {
    fn exists(&self, p: &Path) -> bool {
        RealFs.exists(p)
    }
    fn is_dir(&self, p: &Path) -> bool {
        RealFs.is_dir(p)
    }
    fn create_dir_all(&self, p: &Path) -> flightdeck::contracts::Result<()> {
        RealFs.create_dir_all(p)
    }
    fn read_to_string(&self, p: &Path) -> flightdeck::contracts::Result<String> {
        RealFs.read_to_string(p)
    }
    fn write(&self, p: &Path, c: &str) -> flightdeck::contracts::Result<()> {
        RealFs.write(p, c)
    }
    fn symlink(&self, t: &Path, l: &Path) -> flightdeck::contracts::Result<()> {
        RealFs.symlink(t, l)
    }
    fn append_line(&self, p: &Path, l: &str) -> flightdeck::contracts::Result<()> {
        RealFs.append_line(p, l)
    }
    fn list_dir(&self, p: &Path) -> flightdeck::contracts::Result<Vec<PathBuf>> {
        RealFs.list_dir(p)
    }
    fn remove_dir_all(&self, p: &Path) -> flightdeck::contracts::Result<()> {
        RealFs.remove_dir_all(p)
    }
    fn try_lock_exclusive(
        &self,
        p: &Path,
    ) -> flightdeck::contracts::Result<Option<flightdeck::contracts::FileLock>> {
        RealFs.try_lock_exclusive(p)
    }
}

impl UpdateFs for FailingRename {
    fn rename(&self, from: &Path, to: &Path) -> Result<(), UpdateError> {
        if to == self.refuse && self.armed.replace(false) {
            return Err(UpdateError::Io("permission denied".into()));
        }
        RealFs.rename(from, to)
    }
    fn remove_file(&self, p: &Path) -> Result<(), UpdateError> {
        RealFs.remove_file(p)
    }
    fn open_read(&self, p: &Path) -> Result<Box<dyn Read>, UpdateError> {
        RealFs.open_read(p)
    }
    fn set_executable(&self, p: &Path) -> Result<(), UpdateError> {
        RealFs.set_executable(p)
    }
}

#[test]
fn a_failed_second_rename_puts_the_old_bundle_back() {
    let (dir, kind, source, rel) = mac_fixture();
    let apps = dir.path().join("Applications");
    let fs = FailingRename::new(apps.join("FlightDeck.app"));
    let err = installer(&fs, &source, &new_app())
        .install(&rel, &kind)
        .unwrap_err();
    assert!(matches!(err, UpdateError::Io(_)), "{err:?}");
    assert_eq!(read(apps.join(APP_EXE)), "old", "the app still works");
    assert_eq!(entries(&apps), ["FlightDeck.app"]);
}

#[test]
fn a_stale_staging_directory_does_not_poison_the_next_attempt() {
    let (dir, kind, source, rel) = mac_fixture();
    let stale = dir
        .path()
        .join("Applications/.FlightDeck-update-1.4.0/extracted");
    fs::create_dir_all(&stale).unwrap();
    fs::write(stale.join("junk"), "left by a crash").unwrap();
    installer(&RealFs, &source, &new_app())
        .install(&rel, &kind)
        .unwrap();
    assert_eq!(read(dir.path().join("Applications").join(APP_EXE)), "new");
}

// --- Linux -------------------------------------------------------------

#[test]
fn an_appimage_is_replaced_in_place_and_stays_executable() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("FlightDeck.AppImage");
    fs::write(&file, "old image").unwrap();
    let name = asset_name(v("1.4.0"), PackageFormat::AppImage, Arch::X86_64);
    let rel = release("1.4.0", &[name.clone(), checksum_name(&name)]);
    let source = FakeSource::new(Some(rel.clone()));
    source.publish(&name, b"new image");

    let unzip = new_app();
    let mut with = installer(&RealFs, &source, &unzip);
    with.arch = Arch::X86_64;
    let outcome = with
        .install(&rel, &DirectKind::AppImage { file: file.clone() })
        .unwrap();
    assert_eq!(outcome, Outcome::Replaced);
    assert_eq!(read(&file), "new image");
    assert_eq!(entries(dir.path()), ["FlightDeck.AppImage"]);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o111, 0o111, "the image must be executable");
    }
}

// --- Windows -----------------------------------------------------------

fn windows_fixture() -> (tempfile::TempDir, PathBuf, FakeSource, ReleaseInfo) {
    let dir = tempfile::tempdir().unwrap();
    let exe = dir.path().join("flightdeck-desktop.exe");
    fs::write(&exe, "old exe").unwrap();
    fs::write(dir.path().join("notes.txt"), "user file").unwrap();
    let name = asset_name(v("1.4.0"), PackageFormat::WindowsZip, Arch::Aarch64);
    let rel = release("1.4.0", &[name.clone(), checksum_name(&name)]);
    let source = FakeSource::new(Some(rel.clone()));
    source.publish(&name, b"zip bytes");
    (dir, exe, source, rel)
}

fn new_portable() -> FakeUnzip {
    FakeUnzip {
        files: vec![
            ("flightdeck-desktop.exe", "new exe"),
            ("assets/x.dat", "data"),
        ],
    }
}

#[test]
fn a_windows_update_is_staged_then_applied_at_the_next_launch() {
    let (dir, exe, source, rel) = windows_fixture();
    let kind = DirectKind::WindowsPortable { exe: exe.clone() };
    let outcome = installer(&RealFs, &source, &new_portable())
        .install(&rel, &kind)
        .unwrap();
    assert_eq!(outcome, Outcome::StagedForNextLaunch);
    // The running installation is untouched until the next launch.
    assert_eq!(read(&exe), "old exe");
    assert!(dir.path().join(WINDOWS_STAGING).join(READY_MARKER).exists());

    assert!(finish_staged(&RealFs, &exe).unwrap());
    assert_eq!(read(&exe), "new exe");
    assert_eq!(read(dir.path().join("assets/x.dat")), "data");
    assert_eq!(
        read(dir.path().join("notes.txt")),
        "user file",
        "unrelated files stay"
    );
    assert_eq!(
        read(
            dir.path()
                .join(WINDOWS_ASIDE)
                .join("flightdeck-desktop.exe")
        ),
        "old exe",
        "the old exe is parked, not deleted"
    );
    assert!(!dir.path().join(WINDOWS_STAGING).exists());

    // The launch after that clears the parked files and does nothing else.
    assert!(!finish_staged(&RealFs, &exe).unwrap());
    assert!(!dir.path().join(WINDOWS_ASIDE).exists());
    assert_eq!(read(&exe), "new exe");
}

#[test]
fn a_half_staged_windows_update_is_never_applied() {
    let (dir, exe, _source, _) = windows_fixture();
    let staging = dir.path().join(WINDOWS_STAGING);
    fs::create_dir_all(staging.join("payload")).unwrap();
    fs::write(staging.join("payload/flightdeck-desktop.exe"), "partial").unwrap();
    // No READY marker: the download or unpack was interrupted.
    assert!(!finish_staged(&RealFs, &exe).unwrap());
    assert_eq!(read(&exe), "old exe");
    assert!(!staging.exists(), "the leftovers are cleaned up");
}

#[test]
fn a_failure_part_way_through_the_windows_swap_restores_everything() {
    let (dir, exe, source, rel) = windows_fixture();
    fs::write(dir.path().join("helper.dll"), "old dll").unwrap();
    let kind = DirectKind::WindowsPortable { exe: exe.clone() };
    let unzip = FakeUnzip {
        files: vec![
            ("flightdeck-desktop.exe", "new exe"),
            ("helper.dll", "new dll"),
        ],
    };
    installer(&RealFs, &source, &unzip)
        .install(&rel, &kind)
        .unwrap();

    let failing = FailingRename::new(dir.path().join("helper.dll"));
    assert!(finish_staged(&failing, &exe).is_err());
    assert_eq!(read(&exe), "old exe");
    assert_eq!(read(dir.path().join("helper.dll")), "old dll");
}
