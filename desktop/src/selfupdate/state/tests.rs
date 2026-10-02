//! "Update now" end to end, minus the real network and unzip tool.

use std::fs;

use super::super::apply::tests::{release, v, FakeSource, FakeUnzip};
use super::super::release::{asset_name, checksum_name, PackageFormat};
use super::*;

fn appimage() -> (tempfile::TempDir, DirectKind, String) {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("FlightDeck.AppImage");
    fs::write(&file, "old").unwrap();
    let name = asset_name(v("1.5.0"), PackageFormat::AppImage, Arch::X86_64);
    (dir, DirectKind::AppImage { file }, name)
}

const NO_UNZIP: FakeUnzip = FakeUnzip { files: vec![] };

#[test]
fn a_newer_release_is_installed_and_reported() {
    let (dir, kind, name) = appimage();
    let source = FakeSource::new(Some(release(
        "1.5.0",
        &[name.clone(), checksum_name(&name)],
    )));
    source.publish(&name, b"new");
    let (version, outcome) =
        update_to_latest(&source, &RealFs, &NO_UNZIP, Arch::X86_64, v("1.4.0"), &kind).unwrap();
    assert_eq!(version, v("1.5.0"));
    assert_eq!(outcome, Outcome::Replaced);
    assert_eq!(
        fs::read_to_string(dir.path().join("FlightDeck.AppImage")).unwrap(),
        "new"
    );
}

#[test]
fn nothing_newer_or_nothing_published_installs_nothing() {
    let (dir, kind, name) = appimage();
    let source = FakeSource::new(Some(release(
        "1.5.0",
        &[name.clone(), checksum_name(&name)],
    )));
    source.publish(&name, b"new");
    let err =
        update_to_latest(&source, &RealFs, &NO_UNZIP, Arch::X86_64, v("1.5.0"), &kind).unwrap_err();
    assert!(matches!(err, UpdateError::Release(_)), "{err:?}");
    assert_eq!(source.downloads.get(), 0);

    let none = FakeSource::new(None);
    assert!(update_to_latest(&none, &RealFs, &NO_UNZIP, Arch::X86_64, v("1.4.0"), &kind).is_err());
    assert_eq!(
        fs::read_to_string(dir.path().join("FlightDeck.AppImage")).unwrap(),
        "old"
    );
}
