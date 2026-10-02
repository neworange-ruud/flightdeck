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

use std::rc::Rc;

use flightdeck::host::HostNotices;
use gpui::{
    div, App, Div, FontWeight, InteractiveElement, ParentElement, Stateful, Styled, Window,
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
}

/// What the banner says for `status`.
pub fn banner_copy(status: &UpdateStatus) -> BannerCopy {
    let copy = |headline: String, detail: &str, action| BannerCopy {
        headline,
        detail: detail.to_string(),
        action,
    };
    match status {
        UpdateStatus::Available { version, kind } => {
            let headline = format!("FlightDeck v{version} is available");
            match kind {
                InstallKind::Direct(_) => copy(
                    headline,
                    "Update now downloads and checks it, then asks you to restart.",
                    BannerAction::UpdateNow,
                ),
                InstallKind::Managed(manager) => BannerCopy {
                    headline,
                    detail: format!("Update with: {}", manager.update_hint()),
                    action: BannerAction::None,
                },
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
        },
        UpdateStatus::Failed { version, message } => copy(
            format!("Could not update to v{version}"),
            message,
            BannerAction::None,
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
                |_, _, cx| cx.restart(),
            ));
        }
        BannerAction::None => {}
    }
    row = row.child(button(
        "update-notes",
        "Release notes",
        ButtonKind::Secondary,
        &p,
        |_, _, cx| cx.open_url(RELEASES_URL),
    ));
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

        let failed = banner_copy(&UpdateStatus::Failed {
            version: "2.0.0".into(),
            message: "the download does not match its checksum".into(),
        });
        assert_eq!(failed.action, BannerAction::None);
        assert!(failed.detail.contains("checksum"));
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
