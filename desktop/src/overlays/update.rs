//! The update-available notice, and the `ISOLATED` badge.
//!
//! SPECS §30: the update notice is "never a modal". It is a slim banner the
//! shell places above the content, dismissable for this session, that does not
//! take focus or block a keystroke. It reads [`HostNotices`], the same
//! read-out the TUI's status bar uses, and the desktop's own
//! [`UpdateStatus`](crate::selfupdate::UpdateStatus): what the desktop app can
//! do about an update depends on how it was installed, which the host does not
//! know.
//!
//! What the banner offers follows the install kind:
//!
//! - a package manager owns the app: the command to run, nothing more;
//! - the app was unpacked by the user: "Update now", which downloads, checks
//!   and replaces it in the background (the banner shows progress and never
//!   blocks), then offers "Restart";
//! - anywhere else: a pointer at the release page.
//!
//! "Check for Updates…" (the app menu and the palette) reports through the
//! same banner: "Checking…", then "up to date", a failure with "Check
//! again", or the update found. Once dismissed, a newer release found later
//! brings the banner back, and while one is known the status bar carries a
//! pill ([`update_indicator`]) that does too.

use std::rc::Rc;

use flightdeck::host::HostNotices;
use gpui::{
    div, App, Div, FontWeight, InteractiveElement, ParentElement, Stateful,
    StatefulInteractiveElement, Styled, Window,
};
use gpui_component::h_flex;

use super::help::{button, ButtonKind};
use crate::selfupdate::{self, InstallKind, Outcome, UpdateStatus};
use crate::theme::Palette;

/// Where release notes live, and where users of an install the app cannot
/// replace itself download the new version.
pub const RELEASES_URL: &str = "https://github.com/neworange-ruud/flightdeck/releases";

/// Hides the banner. The banner is stateless, so the caller owns "dismissed
/// for this session" and stops rendering it when this fires.
pub type OnDismiss = Rc<dyn Fn(&mut Window, &mut App)>;

/// The one button an update state offers besides "Release notes".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BannerAction {
    /// Download and install (direct installs only).
    UpdateNow,
    /// Restart into the new version.
    Restart,
    /// Ask GitHub again: after a failed check or a failed install.
    CheckAgain,
    /// Nothing to press.
    None,
}

/// The words and the action for an update state. Pure, so the copy for every
/// install kind is tested without a window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BannerCopy {
    pub headline: String,
    pub detail: String,
    pub action: BannerAction,
    /// Where "Release notes" goes, when there is a release to talk about.
    pub notes_url: Option<String>,
}

/// The page of the desktop release `version`.
pub fn release_notes_url(version: &str) -> String {
    format!("{RELEASES_URL}/tag/desktop-v{version}")
}

/// What the banner says for `status`.
pub fn banner_copy(status: &UpdateStatus) -> BannerCopy {
    let notes_url = status.newer_version().map(release_notes_url);
    let copy = |headline: String, detail: &str, action| BannerCopy {
        headline,
        detail: detail.to_string(),
        action,
        notes_url: notes_url.clone(),
    };
    match status {
        UpdateStatus::Checking => copy("Checking for updates…".to_string(), "", BannerAction::None),
        UpdateStatus::UpToDate { version } => copy(
            "FlightDeck is up to date".to_string(),
            &format!("v{version} is the newest release."),
            BannerAction::None,
        ),
        UpdateStatus::CheckFailed { message } => copy(
            "Could not check for updates".to_string(),
            message,
            BannerAction::CheckAgain,
        ),
        UpdateStatus::Available { version, kind } => {
            let headline = format!("FlightDeck v{version} is available");
            match kind {
                InstallKind::Direct(_) => copy(
                    headline,
                    "Update now downloads and checks it, then asks you to restart.",
                    BannerAction::UpdateNow,
                ),
                InstallKind::Managed(manager) => copy(
                    headline,
                    &format!("Update with: {}", manager.update_hint()),
                    BannerAction::None,
                ),
                InstallKind::Unsupported => copy(
                    headline,
                    "Download it from the releases page.",
                    BannerAction::None,
                ),
            }
        }
        UpdateStatus::Installing { version } => copy(
            format!("Updating FlightDeck to v{version}"),
            "Downloading and verifying. You can keep working.",
            BannerAction::None,
        ),
        UpdateStatus::Installed { version, outcome } => match outcome {
            Outcome::Replaced => copy(
                format!("FlightDeck v{version} is installed"),
                "Restart to use it.",
                BannerAction::Restart,
            ),
            Outcome::StagedForNextLaunch => copy(
                format!("FlightDeck v{version} is ready"),
                "It is applied the next time FlightDeck starts.",
                BannerAction::Restart,
            ),
            Outcome::RunInstaller { .. } => copy(
                format!("FlightDeck v{version} is downloaded"),
                "Restart to install it. Windows asks for permission first.",
                BannerAction::Restart,
            ),
        },
        UpdateStatus::Failed { version, message } => copy(
            format!("Could not update to v{version}"),
            message,
            BannerAction::CheckAgain,
        ),
    }
}

/// The banner, or `None` when no newer release is known. The desktop's own
/// update state ([`selfupdate::status`]) wins; failing that, the host's
/// notice (the TUI's release line) gets the plain "release notes" banner.
///
/// "Update now" and "Restart" act through [`selfupdate`] directly, so the
/// shell passes nothing new; dismissing is a local callback.
pub fn update_banner(
    notices: &HostNotices,
    on_dismiss: Option<OnDismiss>,
    cx: &App,
) -> Option<Stateful<Div>> {
    let copy = match selfupdate::status(cx) {
        Some(status) => banner_copy(&status),
        None => {
            let update = notices.update.as_ref()?;
            BannerCopy {
                headline: format!("FlightDeck v{} is available", update.latest_version),
                detail: "Update when it suits you; nothing is interrupted.".to_string(),
                action: BannerAction::None,
                notes_url: Some(RELEASES_URL.to_string()),
            }
        }
    };
    let p = *Palette::global(cx);
    let mut row = h_flex()
        .id("update-banner")
        .w_full()
        .px_4()
        .py_1p5()
        .gap_3()
        .items_center()
        .bg(p.status_attention_bg.hsla())
        .border_b_1()
        .border_color(p.status_attention.hsla())
        .text_sm()
        .child(
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(p.status_attention.hsla())
                .child(copy.headline),
        )
        .child(div().flex_1().text_color(p.ink_2.hsla()).child(copy.detail));
    match copy.action {
        BannerAction::UpdateNow => {
            row = row.child(button(
                "update-now",
                "Update now",
                ButtonKind::Primary,
                &p,
                |_, _, cx| selfupdate::begin_install(cx),
            ));
        }
        BannerAction::Restart => {
            row = row.child(button(
                "update-restart",
                "Restart",
                ButtonKind::Primary,
                &p,
                |_, _, cx| selfupdate::restart(cx),
            ));
        }
        BannerAction::CheckAgain => {
            row = row.child(button(
                "update-check-again",
                "Check again",
                ButtonKind::Primary,
                &p,
                |_, _, cx| selfupdate::check_for_updates(cx),
            ));
        }
        BannerAction::None => {}
    }
    if let Some(url) = copy.notes_url {
        row = row.child(button(
            "update-notes",
            "Release notes",
            ButtonKind::Secondary,
            &p,
            move |_, _, cx| cx.open_url(&url),
        ));
    }
    if let Some(dismiss) = on_dismiss {
        row = row.child(button(
            "update-dismiss",
            "Dismiss",
            ButtonKind::Quiet,
            &p,
            move |_, w, cx| dismiss(w, cx),
        ));
    }
    Some(row)
}

/// What the version-skew banner of a worker window offers when the worker
/// runs a newer FlightDeck than this app. The full update banner lives in
/// the main window only, so the skew banner carries the same actions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkewOffer {
    /// A few words on where the update stands, when there is anything to say.
    pub note: Option<String>,
    /// "Update now", "Restart", or nothing.
    pub action: BannerAction,
    /// The label of the button that opens the releases page.
    pub releases_label: &'static str,
}

/// The skew banner's offer for the desktop's update state (`None`: no newer
/// release known yet, so the releases page is the way to get one). Pure.
pub fn skew_offer(status: Option<&UpdateStatus>) -> SkewOffer {
    let nothing_known = SkewOffer {
        note: None,
        action: BannerAction::None,
        releases_label: "Download",
    };
    let Some(status) = status else {
        return nothing_known;
    };
    let copy = banner_copy(status);
    let note = match status {
        // Nothing newer than this app is published yet (the worker may run
        // a CLI release the desktop has not had): the releases page.
        UpdateStatus::UpToDate { .. } => return nothing_known,
        UpdateStatus::Checking => Some(copy.headline),
        UpdateStatus::CheckFailed { .. } => Some(format!("{}: {}", copy.headline, copy.detail)),
        UpdateStatus::Available {
            kind: InstallKind::Direct(_),
            ..
        } => None,
        UpdateStatus::Available {
            kind: InstallKind::Managed(_),
            ..
        } => Some(copy.detail),
        UpdateStatus::Available { .. }
        | UpdateStatus::Installing { .. }
        | UpdateStatus::Installed { .. } => Some(copy.headline),
        UpdateStatus::Failed { .. } => Some(format!("{}: {}", copy.headline, copy.detail)),
    };
    let releases_label = match copy.action {
        BannerAction::None | BannerAction::CheckAgain => "Download",
        BannerAction::UpdateNow | BannerAction::Restart => "Release notes",
    };
    SkewOffer {
        note,
        action: copy.action,
        releases_label,
    }
}

/// The version-skew banner's actions: [`skew_offer`] for the current update
/// state, as a row of a note and buttons.
pub fn skew_actions(cx: &App) -> Div {
    let offer = skew_offer(selfupdate::status(cx).as_ref());
    let p = *Palette::global(cx);
    let mut row = h_flex().flex_none().gap_2().items_center().children(
        offer
            .note
            .map(|note| div().text_color(p.status_attention.hsla()).child(note)),
    );
    match offer.action {
        BannerAction::UpdateNow => {
            row = row.child(button(
                "skew-update-now",
                "Update now",
                ButtonKind::Primary,
                &p,
                |_, _, cx| selfupdate::begin_install(cx),
            ));
        }
        BannerAction::Restart => {
            row = row.child(button(
                "skew-update-restart",
                "Restart",
                ButtonKind::Primary,
                &p,
                |_, _, cx| selfupdate::restart(cx),
            ));
        }
        BannerAction::CheckAgain => {
            row = row.child(button(
                "skew-check-again",
                "Check again",
                ButtonKind::Primary,
                &p,
                |_, _, cx| selfupdate::check_for_updates(cx),
            ));
        }
        BannerAction::None => {}
    }
    row.child(button(
        "skew-releases",
        offer.releases_label,
        ButtonKind::Secondary,
        &p,
        |_, _, cx| cx.open_url(RELEASES_URL),
    ))
}

/// A worker window's "Update worker" (an older worker that can update
/// itself, protocol v14), or while it updates, a note saying so.
/// `on_update` sends the request.
pub fn worker_update_actions(
    updating: bool,
    on_update: impl Fn(&mut Window, &mut App) + 'static,
    cx: &App,
) -> Div {
    let p = *Palette::global(cx);
    let row = h_flex().flex_none().gap_2().items_center();
    if updating {
        return row.child(
            div()
                .debug_selector(|| "worker-updating".into())
                .text_color(p.status_attention.hsla())
                .child("Updating the worker… it restarts by itself"),
        );
    }
    row.child(button(
        "update-worker",
        "Update worker",
        ButtonKind::Primary,
        &p,
        move |_, w, cx| on_update(w, cx),
    ))
}

/// The status bar's update pill, while a newer release is known or being
/// installed: the way back to the banner after it was dismissed. Its words
/// are [`indicator_label`]'s; a click brings the banner back
/// ([`selfupdate::reshow`]), where the actions are.
pub fn update_indicator(cx: &App) -> Option<Stateful<Div>> {
    let label = indicator_label(selfupdate::status(cx).as_ref()?)?;
    let p = Palette::global(cx);
    Some(
        div()
            .id("update-indicator")
            .debug_selector(|| "update-indicator".into())
            .flex_none()
            .px_1p5()
            .rounded_sm()
            .bg(p.status_attention_bg.hsla())
            .text_color(p.status_attention.hsla())
            .text_xs()
            .font_weight(FontWeight::SEMIBOLD)
            .cursor_pointer()
            .child(label)
            .on_click(|_, _, cx| selfupdate::reshow(cx)),
    )
}

/// The pill's words for `status`, or `None` when there is nothing to update
/// to. Pure.
pub fn indicator_label(status: &UpdateStatus) -> Option<String> {
    match status {
        UpdateStatus::Available { version, .. } => Some(format!("Update to v{version}")),
        UpdateStatus::Installing { .. } => Some("Updating…".to_string()),
        UpdateStatus::Installed { .. } => Some("Restart to update".to_string()),
        UpdateStatus::Failed { .. } => Some("Update failed".to_string()),
        UpdateStatus::Checking
        | UpdateStatus::UpToDate { .. }
        | UpdateStatus::CheckFailed { .. } => None,
    }
}

/// The permanent `ISOLATED` badge (SPECS §32), or `None` for an ordinary run.
pub fn isolated_badge(notices: &HostNotices, cx: &App) -> Option<Div> {
    if !notices.isolated {
        return None;
    }
    let p = Palette::global(cx);
    Some(
        div()
            .debug_selector(|| "isolated-badge".into())
            .px_1p5()
            .rounded_sm()
            .bg(p.status_attention_bg.hsla())
            .text_color(p.status_attention.hsla())
            .text_xs()
            .font_weight(FontWeight::SEMIBOLD)
            .child("ISOLATED"),
    )
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use flightdeck::web::protocol::UpdateNotice;
    use gpui::{IntoElement, TestAppContext};

    use super::super::help::testkit::{click, mount};
    use super::*;

    fn notices(update: Option<&str>, isolated: bool) -> HostNotices {
        HostNotices {
            update: update.map(|v| UpdateNotice {
                latest_version: v.to_string(),
            }),
            isolated,
        }
    }

    /// The banner exists only when the host knows of a newer release, and it is
    /// a plain row: nothing in it takes focus or covers the content.
    #[gpui::test]
    fn the_banner_is_drawn_only_for_a_known_update(cx: &mut TestAppContext) {
        let with = notices(Some("9.9.9"), false);
        let without = notices(None, false);
        let cx = mount(cx, move |_, cx| {
            update_banner(&with, None, cx)
                .map(IntoElement::into_any_element)
                .unwrap_or_else(|| div().into_any_element())
        });
        assert!(cx.debug_bounds("update-notes").is_some());
        assert!(
            cx.debug_bounds("update-dismiss").is_none(),
            "no callback, no button"
        );
        cx.update(|_, cx| assert!(update_banner(&without, None, cx).is_none()));
    }

    #[gpui::test]
    fn dismissing_calls_back_and_nothing_else(cx: &mut TestAppContext) {
        use std::rc::Rc;
        let dismissed = Rc::new(Cell::new(0));
        let seen = dismissed.clone();
        let with = notices(Some("9.9.9"), false);
        let cx = mount(cx, move |_, cx| {
            let seen = seen.clone();
            let dismiss: OnDismiss = Rc::new(move |_, _| seen.set(seen.get() + 1));
            update_banner(&with, Some(dismiss), cx)
                .map(IntoElement::into_any_element)
                .unwrap_or_else(|| div().into_any_element())
        });
        click(cx, "update-dismiss");
        assert_eq!(dismissed.get(), 1);
    }

    fn available(kind: InstallKind) -> UpdateStatus {
        UpdateStatus::Available {
            version: "2.0.0".into(),
            kind,
        }
    }

    /// The banner offers "Update now" only where the app may replace itself;
    /// a package manager's install is told the command instead.
    #[test]
    fn the_copy_follows_the_install_kind() {
        use crate::selfupdate::kind::{DirectKind, PackageManager};

        let direct = banner_copy(&available(InstallKind::Direct(DirectKind::AppImage {
            file: "/x.AppImage".into(),
        })));
        assert_eq!(direct.action, BannerAction::UpdateNow);
        assert_eq!(direct.headline, "FlightDeck v2.0.0 is available");

        let managed = banner_copy(&available(InstallKind::Managed(PackageManager::Homebrew)));
        assert_eq!(managed.action, BannerAction::None);
        assert!(managed.detail.contains("brew"), "{}", managed.detail);

        let other = banner_copy(&available(InstallKind::Unsupported));
        assert_eq!(other.action, BannerAction::None);
        assert!(other.detail.contains("releases page"));
    }

    /// A worker newer than the app: its banner always offers a way to the
    /// new release, "Update now" where the app may replace itself.
    #[test]
    fn the_skew_banner_always_offers_a_way_to_update() {
        use crate::selfupdate::kind::{DirectKind, PackageManager};

        let unknown = skew_offer(None);
        assert_eq!(unknown.action, BannerAction::None);
        assert_eq!(unknown.note, None);
        assert_eq!(unknown.releases_label, "Download");

        let direct = skew_offer(Some(&available(InstallKind::Direct(
            DirectKind::MacBundle {
                bundle: "/Applications/FlightDeck.app".into(),
            },
        ))));
        assert_eq!(direct.action, BannerAction::UpdateNow);
        assert_eq!(direct.note, None, "the button says it");
        assert_eq!(direct.releases_label, "Release notes");

        let managed = skew_offer(Some(&available(InstallKind::Managed(
            PackageManager::Homebrew,
        ))));
        assert_eq!(managed.action, BannerAction::None);
        assert!(managed.note.unwrap().contains("brew upgrade"));
        assert_eq!(managed.releases_label, "Download");

        let installing = skew_offer(Some(&UpdateStatus::Installing {
            version: "2.1.0".into(),
        }));
        assert_eq!(installing.action, BannerAction::None);
        assert_eq!(
            installing.note.as_deref(),
            Some("Updating FlightDeck to v2.1.0")
        );

        let installed = skew_offer(Some(&UpdateStatus::Installed {
            version: "2.1.0".into(),
            outcome: Outcome::Replaced,
        }));
        assert_eq!(installed.action, BannerAction::Restart);

        let failed = skew_offer(Some(&UpdateStatus::Failed {
            version: "2.1.0".into(),
            message: "checksum mismatch".into(),
        }));
        assert_eq!(failed.action, BannerAction::CheckAgain);
        assert_eq!(
            failed.note.as_deref(),
            Some("Could not update to v2.1.0: checksum mismatch")
        );
        assert_eq!(failed.releases_label, "Download");
    }

    #[test]
    fn progress_and_results_are_worded_for_the_user() {
        let working = banner_copy(&UpdateStatus::Installing {
            version: "2.0.0".into(),
        });
        assert_eq!(working.action, BannerAction::None);

        let done = |outcome| {
            banner_copy(&UpdateStatus::Installed {
                version: "2.0.0".into(),
                outcome,
            })
        };
        assert_eq!(done(Outcome::Replaced).action, BannerAction::Restart);
        let staged = done(Outcome::StagedForNextLaunch);
        assert_eq!(staged.action, BannerAction::Restart);
        assert!(staged.detail.contains("next time"), "{}", staged.detail);
        let msi = done(Outcome::RunInstaller {
            msi: "C:/x/update/a.msi".into(),
        });
        assert_eq!(msi.action, BannerAction::Restart);
        assert!(msi.detail.contains("permission"), "{}", msi.detail);

        let failed = banner_copy(&UpdateStatus::Failed {
            version: "2.0.0".into(),
            message: "the download does not match its checksum".into(),
        });
        assert_eq!(failed.action, BannerAction::CheckAgain);
        assert!(failed.detail.contains("checksum"));
    }

    /// "Check for Updates…" always answers: checking, up to date (no
    /// release notes to read), or why it failed with a way to try again.
    #[test]
    fn an_asked_for_check_always_answers() {
        let checking = banner_copy(&UpdateStatus::Checking);
        assert_eq!(checking.headline, "Checking for updates…");
        assert_eq!(checking.action, BannerAction::None);
        assert_eq!(checking.notes_url, None);

        let current = banner_copy(&UpdateStatus::UpToDate {
            version: "2.2.0".into(),
        });
        assert_eq!(current.headline, "FlightDeck is up to date");
        assert!(current.detail.contains("v2.2.0"), "{}", current.detail);
        assert_eq!(current.notes_url, None);

        let failed = banner_copy(&UpdateStatus::CheckFailed {
            message: "could not download the update (timeout)".into(),
        });
        assert_eq!(failed.action, BannerAction::CheckAgain);
        assert!(failed.detail.contains("timeout"));

        // Release notes go to the release found, not the list of all.
        let found = banner_copy(&available(InstallKind::Unsupported));
        assert_eq!(
            found.notes_url.as_deref(),
            Some("https://github.com/neworange-ruud/flightdeck/releases/tag/desktop-v2.0.0")
        );
    }

    /// The status bar pill shows only while there is something to update to.
    #[test]
    fn the_pill_shows_while_an_update_is_known() {
        assert_eq!(
            indicator_label(&available(InstallKind::Unsupported)).as_deref(),
            Some("Update to v2.0.0")
        );
        let installed = UpdateStatus::Installed {
            version: "2.0.0".into(),
            outcome: Outcome::Replaced,
        };
        assert_eq!(
            indicator_label(&installed).as_deref(),
            Some("Restart to update")
        );
        for quiet in [
            UpdateStatus::Checking,
            UpdateStatus::UpToDate {
                version: "2.0.0".into(),
            },
            UpdateStatus::CheckFailed {
                message: "offline".into(),
            },
        ] {
            assert_eq!(indicator_label(&quiet), None, "{quiet:?}");
        }
    }

    /// The pill brings a dismissed banner back: it bumps the generation the
    /// window compares its dismissal with.
    #[gpui::test]
    fn the_pill_brings_the_banner_back(cx: &mut TestAppContext) {
        let cx = mount(cx, move |_, cx| {
            update_indicator(cx)
                .map(IntoElement::into_any_element)
                .unwrap_or_else(|| div().into_any_element())
        });
        assert!(cx.debug_bounds("update-indicator").is_none());
        cx.update(|_, cx| selfupdate::set_status(cx, available(InstallKind::Unsupported)));
        cx.run_until_parked();
        let before = cx.update(|_, cx| selfupdate::generation(cx));
        click(cx, "update-indicator");
        assert!(cx.update(|_, cx| selfupdate::generation(cx)) > before);
    }

    /// The desktop's own status draws the banner even when the host knows of
    /// no update, and only a direct install gets the button.
    #[gpui::test]
    fn the_banner_follows_the_desktop_status(cx: &mut TestAppContext) {
        use crate::selfupdate::kind::DirectKind;

        let none = notices(None, false);
        let cx = mount(cx, move |_, cx| {
            update_banner(&none, None, cx)
                .map(IntoElement::into_any_element)
                .unwrap_or_else(|| div().into_any_element())
        });
        assert!(cx.debug_bounds("update-notes").is_none(), "nothing known");

        cx.update(|_, cx| {
            selfupdate::set_status(
                cx,
                available(InstallKind::Direct(DirectKind::AppImage {
                    file: "/x.AppImage".into(),
                })),
            )
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("update-now").is_some());
        assert!(cx.debug_bounds("update-notes").is_some());

        cx.update(|_, cx| selfupdate::set_status(cx, available(InstallKind::Unsupported)));
        cx.run_until_parked();
        assert!(cx.debug_bounds("update-now").is_none());
        assert!(cx.debug_bounds("update-notes").is_some());

        cx.update(|_, cx| {
            selfupdate::set_status(
                cx,
                UpdateStatus::Installed {
                    version: "2.0.0".into(),
                    outcome: Outcome::Replaced,
                },
            )
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("update-restart").is_some());
    }

    #[gpui::test]
    fn the_isolated_badge_follows_the_notice(cx: &mut TestAppContext) {
        mount(cx, |_, _| div().into_any_element());
        cx.update(|cx| {
            assert!(isolated_badge(&notices(None, true), cx).is_some());
            assert!(isolated_badge(&notices(None, false), cx).is_none());
        });
    }
}
