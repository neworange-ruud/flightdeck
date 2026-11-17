//! About FlightDeck: the product, its version, who makes it, and its site.
//!
//! Two ways in, one card. The app menu's "About FlightDeck" opens
//! [`AboutWindow`], a small window of its own, so it works from the launcher
//! and from a worker window as well as from a local project. The palette's
//! "About FlightDeck" is the host's overlay ([`about_view`]). Both draw
//! [`about_body`], so they never disagree.
//!
//! The content is the desktop app's own, not the TUI's
//! [`AboutDoc`](flightdeck::tui::help::AboutDoc): the version is the desktop's
//! (its `desktop-v*` release line), and the credits and links are the ones
//! the desktop shows.

use gpui::{
    div, px, App, Context, Div, FocusHandle, FontWeight, InteractiveElement, IntoElement,
    KeyDownEvent, ParentElement, Render, StatefulInteractiveElement, Styled, Window,
};
use gpui_component::{h_flex, v_flex};

use flightdeck::host::{HostEvent, OverlayInput};

use super::help::{button, card, ButtonKind, MONO};
use super::Emit;
use crate::theme::Palette;

/// The product's name, as the About card shows it.
pub const PRODUCT_NAME: &str = "FlightDeck";
/// This build's version: the desktop app's own.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// Who creates and maintains FlightDeck.
pub const MAINTAINER: &str = "Ruud van Falier";
/// Where the maintainer's name links to.
pub const MAINTAINER_URL: &str = "mailto:ruud.vanfalier@neworange.agency";
/// The product's website.
pub const WEBSITE_URL: &str = "https://www.flightdeckai.app/";
/// The website as the card shows it.
const WEBSITE_LABEL: &str = "www.flightdeckai.app";

/// The standalone window's title.
pub const WINDOW_TITLE: &str = "About FlightDeck";

/// A link: accent ink, underlined on hover, opens `url` in the system's
/// handler (the browser; the mail app for `mailto:`).
fn link(id: &'static str, label: &'static str, url: &'static str, p: &Palette) -> impl IntoElement {
    div()
        .id(id)
        .debug_selector(move || id.into())
        .text_color(p.accent.hsla())
        .cursor_pointer()
        .hover(|s| s.underline())
        .on_click(move |_, _, cx| cx.open_url(url))
        .child(label)
}

/// What both the overlay and the window show: name and version, the credit
/// with the maintainer's mail link, and the website.
pub fn about_body(cx: &App) -> Div {
    let p = *Palette::global(cx);
    v_flex()
        .gap_3()
        .child(
            h_flex()
                .items_baseline()
                .gap_2()
                .child(
                    div()
                        .id("about-name")
                        .debug_selector(|| "about-name".into())
                        .text_xl()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(p.ink.hsla())
                        .child(PRODUCT_NAME),
                )
                .child(
                    div()
                        .id("about-version")
                        .debug_selector(|| "about-version".into())
                        .font_family(MONO)
                        .text_sm()
                        .text_color(p.muted.hsla())
                        .child(format!("Version {VERSION}")),
                ),
        )
        .child(
            h_flex()
                .flex_wrap()
                .gap_1()
                .text_sm()
                .child(
                    div()
                        .text_color(p.ink_2.hsla())
                        .child("Created and maintained by"),
                )
                .child(link("about-maintainer", MAINTAINER, MAINTAINER_URL, &p)),
        )
        .child(
            div()
                .text_sm()
                .child(link("about-url", WEBSITE_LABEL, WEBSITE_URL, &p)),
        )
}

/// The palette's About: the host's overlay card, with an OK button. OK, the
/// close button and Esc all answer [`OverlayInput::Cancel`].
pub fn about_view(emit: Emit, cx: &App) -> impl IntoElement {
    let p = *Palette::global(cx);
    let ok = {
        let emit = emit.clone();
        button(
            "about-ok",
            "OK",
            ButtonKind::Primary,
            &p,
            move |_, w, cx| emit(HostEvent::Overlay(OverlayInput::Cancel), w, cx),
        )
    };
    card("about-overlay", WINDOW_TITLE, px(420.), &p, &emit)
        .child(about_body(cx))
        .child(h_flex().justify_end().child(ok))
}

/// The app menu's About: a window of its own. OK, Esc and Enter close it.
pub struct AboutWindow {
    focus: FocusHandle,
}

impl AboutWindow {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        AboutWindow { focus }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(event.keystroke.key.as_str(), "escape" | "enter") {
            cx.stop_propagation();
            window.remove_window();
        }
    }
}

impl Render for AboutWindow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = *Palette::global(cx);
        v_flex()
            .id("about-window")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key_down))
            .size_full()
            .justify_between()
            .gap_4()
            .p_5()
            .bg(p.surface_window.hsla())
            .child(about_body(cx))
            .child(h_flex().justify_end().child(button(
                "about-ok",
                "OK",
                ButtonKind::Primary,
                &p,
                |_, window, _| window.remove_window(),
            )))
    }
}

#[cfg(test)]
mod tests {
    use gpui::{TestAppContext, VisualTestContext};

    use super::super::help::testkit::{click, mount, recorder, take};
    use super::*;

    #[test]
    fn the_card_names_the_desktop_build_and_its_maintainer() {
        assert_eq!(PRODUCT_NAME, "FlightDeck");
        assert_eq!(VERSION, env!("CARGO_PKG_VERSION"));
        assert_eq!(MAINTAINER, "Ruud van Falier");
        assert_eq!(MAINTAINER_URL, "mailto:ruud.vanfalier@neworange.agency");
        assert_eq!(WEBSITE_URL, "https://www.flightdeckai.app/");
    }

    #[gpui::test]
    fn ok_and_close_dismiss_the_overlay(cx: &mut TestAppContext) {
        let (emit, events) = recorder();
        let cx = mount(cx, move |_, cx| {
            about_view(emit.clone(), cx).into_any_element()
        });
        click(cx, "about-ok");
        click(cx, "overlay-close");
        let cancel = HostEvent::Overlay(OverlayInput::Cancel);
        assert_eq!(take(&events), vec![cancel.clone(), cancel]);
    }

    #[gpui::test]
    fn the_overlay_shows_name_version_credit_and_site(cx: &mut TestAppContext) {
        let (emit, _) = recorder();
        let cx = mount(cx, move |_, cx| {
            about_view(emit.clone(), cx).into_any_element()
        });
        for selector in [
            "about-name",
            "about-version",
            "about-maintainer",
            "about-url",
        ] {
            assert!(cx.debug_bounds(selector).is_some(), "{selector} is drawn");
        }
    }

    #[gpui::test]
    fn the_maintainers_name_opens_a_mail(cx: &mut TestAppContext) {
        let (emit, _) = recorder();
        let cx = mount(cx, move |_, cx| {
            about_view(emit.clone(), cx).into_any_element()
        });
        click(cx, "about-maintainer");
        assert_eq!(cx.opened_url().as_deref(), Some(MAINTAINER_URL));
    }

    #[gpui::test]
    fn the_website_link_opens_the_site(cx: &mut TestAppContext) {
        let (emit, _) = recorder();
        let cx = mount(cx, move |_, cx| {
            about_view(emit.clone(), cx).into_any_element()
        });
        click(cx, "about-url");
        assert_eq!(cx.opened_url().as_deref(), Some(WEBSITE_URL));
    }

    fn open_window(cx: &mut TestAppContext) -> &mut VisualTestContext {
        cx.update(|cx| cx.set_global(Palette::dark()));
        let (_, vcx) = cx.add_window_view(AboutWindow::new);
        vcx.run_until_parked();
        vcx
    }

    #[gpui::test]
    fn the_window_shows_the_same_card(cx: &mut TestAppContext) {
        let vcx = open_window(cx);
        for selector in [
            "about-name",
            "about-version",
            "about-maintainer",
            "about-url",
        ] {
            assert!(vcx.debug_bounds(selector).is_some(), "{selector} is drawn");
        }
    }

    #[gpui::test]
    fn ok_closes_the_window(cx: &mut TestAppContext) {
        let vcx = open_window(cx);
        click(vcx, "about-ok");
        vcx.run_until_parked();
        assert!(vcx.windows().is_empty());
    }

    #[gpui::test]
    fn escape_closes_the_window(cx: &mut TestAppContext) {
        let vcx = open_window(cx);
        vcx.simulate_keystrokes("escape");
        vcx.run_until_parked();
        assert!(vcx.windows().is_empty());
    }
}
