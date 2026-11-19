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

mod after_check {
    use super::super::{after_check, CheckKind, UpdateStatus};
    use crate::selfupdate::apply::tests::v;
    use crate::selfupdate::apply::Outcome;
    use crate::selfupdate::kind::InstallKind;
    use crate::selfupdate::source::UpdateError;

    const KIND: InstallKind = InstallKind::Unsupported;

    fn available(version: &str) -> UpdateStatus {
        UpdateStatus::Available {
            version: version.into(),
            kind: KIND,
        }
    }

    fn offline() -> Result<Option<crate::selfupdate::release::Version>, UpdateError> {
        Err(UpdateError::Network("offline".into()))
    }

    /// The background check speaks only when it finds something, and the
    /// same finding twice changes nothing (so a dismissed banner stays so).
    #[test]
    fn the_background_check_is_silent_unless_something_new_is_found() {
        let bg = CheckKind::Background;
        assert_eq!(after_check(None, Ok(None), bg, &KIND, v("1.4.0")), None);
        assert_eq!(after_check(None, offline(), bg, &KIND, v("1.4.0")), None);
        assert_eq!(
            after_check(None, Ok(Some(v("1.5.0"))), bg, &KIND, v("1.4.0")),
            Some(available("1.5.0"))
        );
        let shown = available("1.5.0");
        assert_eq!(
            after_check(Some(&shown), Ok(Some(v("1.5.0"))), bg, &KIND, v("1.4.0")),
            None
        );
        // A newer release since: the banner moves on to it.
        assert_eq!(
            after_check(Some(&shown), Ok(Some(v("1.6.0"))), bg, &KIND, v("1.4.0")),
            Some(available("1.6.0"))
        );
    }

    /// "Check for Updates…" always gets an answer.
    #[test]
    fn an_asked_for_check_always_answers() {
        let manual = CheckKind::Manual;
        let checking = UpdateStatus::Checking;
        assert_eq!(
            after_check(Some(&checking), Ok(None), manual, &KIND, v("1.4.0")),
            Some(UpdateStatus::UpToDate {
                version: "1.4.0".into()
            })
        );
        assert!(matches!(
            after_check(Some(&checking), offline(), manual, &KIND, v("1.4.0")),
            Some(UpdateStatus::CheckFailed { message }) if message.contains("offline")
        ));
        assert_eq!(
            after_check(
                Some(&checking),
                Ok(Some(v("1.5.0"))),
                manual,
                &KIND,
                v("1.4.0")
            ),
            Some(available("1.5.0"))
        );
    }

    /// Nothing a check finds replaces an update that is underway or done:
    /// the user would lose the Restart they are about to press.
    #[test]
    fn an_update_underway_is_never_replaced() {
        let installing = UpdateStatus::Installing {
            version: "1.5.0".into(),
        };
        let installed = UpdateStatus::Installed {
            version: "1.5.0".into(),
            outcome: Outcome::Replaced,
        };
        for current in [installing, installed] {
            for asked in [CheckKind::Background, CheckKind::Forced, CheckKind::Manual] {
                assert_eq!(
                    after_check(
                        Some(&current),
                        Ok(Some(v("1.6.0"))),
                        asked,
                        &KIND,
                        v("1.4.0")
                    ),
                    None
                );
            }
        }
    }
}
