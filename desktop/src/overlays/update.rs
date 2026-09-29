//! The update-available notice, and the `ISOLATED` badge.
//!
//! SPECS §30: the update notice is "never a modal". It is a slim banner the
//! shell places above the content, dismissable for this session, that does not
//! take focus or block a keystroke. It reads [`HostNotices`], the same
//! read-out the TUI's status bar uses.

use std::rc::Rc;

use flightdeck::host::HostNotices;
use gpui::{
    div, App, Div, FontWeight, InteractiveElement, ParentElement, Stateful, Styled, Window,
};
use gpui_component::h_flex;

use super::help::{button, ButtonKind};
use crate::theme::Palette;

/// Where release notes live; the banner links here because the desktop does not
/// update itself, it only tells you a newer version exists.
pub const RELEASES_URL: &str = "https://github.com/neworange-ruud/flightdeck/releases";

/// Hides the banner. The banner is stateless, so the caller owns "dismissed
/// for this session" and stops rendering it when this fires.
pub type OnDismiss = Rc<dyn Fn(&mut Window, &mut App)>;

/// The banner, or `None` when no newer release is known. It emits no
/// [`flightdeck::host::HostEvent`]: there is no host action for "dismiss" or
/// "update" (the host only knows a newer version exists), so the one
/// interaction, dismissing, is a local callback.
pub fn update_banner(
    notices: &HostNotices,
    on_dismiss: Option<OnDismiss>,
    cx: &App,
) -> Option<Stateful<Div>> {
    let update = notices.update.as_ref()?;
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
                .child(format!(
                    "FlightDeck v{} is available",
                    update.latest_version
                )),
        )
        .child(
            div()
                .flex_1()
                .text_color(p.ink_2.hsla())
                .child("Update when it suits you; nothing is interrupted."),
        )
        .child(button(
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

    #[gpui::test]
    fn the_isolated_badge_follows_the_notice(cx: &mut TestAppContext) {
        mount(cx, |_, _| div().into_any_element());
        cx.update(|cx| {
            assert!(isolated_badge(&notices(None, true), cx).is_some());
            assert!(isolated_badge(&notices(None, false), cx).is_none());
        });
    }
}
