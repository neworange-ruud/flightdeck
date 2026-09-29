//! The About dialog, drawn from the [`AboutDoc`] the host builds.

use flightdeck::host::{HostEvent, OverlayInput};
use flightdeck::tui::help::AboutDoc;
use gpui::{
    div, px, App, FontWeight, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled,
};
use gpui_component::{h_flex, v_flex};

use super::help::{button, card, ButtonKind, MONO};
use super::Emit;
use crate::theme::Palette;

/// The About card: name and version, the tagline, who built it, the home page
/// (a link that opens in the browser) and an OK button.
pub fn about_view(doc: &AboutDoc, emit: Emit, cx: &App) -> impl IntoElement {
    let p = *Palette::global(cx);
    let url = doc.url.clone();

    let mut credits = v_flex().gap_1();
    for credit in &doc.credits {
        credits = credits.child(
            h_flex()
                .gap_1p5()
                .text_sm()
                .child(div().text_color(p.muted.hsla()).child(credit.role.clone()))
                .child(div().text_color(p.ink.hsla()).child(credit.name.clone())),
        );
    }

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

    card(
        "about-overlay",
        format!("About {}", doc.name),
        px(420.),
        &p,
        &emit,
    )
    .child(
        h_flex()
            .items_baseline()
            .gap_2()
            .child(
                div()
                    .text_xl()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(doc.name.clone()),
            )
            .child(
                div()
                    .font_family(MONO)
                    .text_sm()
                    .text_color(p.muted.hsla())
                    .child(format!("v{}", doc.version)),
            ),
    )
    .child(
        div()
            .text_sm()
            .text_color(p.ink_2.hsla())
            .child(doc.tagline.clone()),
    )
    .child(credits)
    .child(
        div()
            .id("about-url")
            .debug_selector(|| "about-url".into())
            .text_sm()
            .text_color(p.accent.hsla())
            .cursor_pointer()
            .hover(|s| s.underline())
            .on_click(move |_, _, cx| cx.open_url(&url))
            .child(doc.url.clone()),
    )
    .child(h_flex().justify_end().child(ok))
}

#[cfg(test)]
mod tests {
    use flightdeck::tui::help::about_doc;
    use gpui::TestAppContext;

    use super::super::help::testkit::{click, mount, recorder, take};
    use super::*;

    #[gpui::test]
    fn ok_and_close_dismiss_the_dialog(cx: &mut TestAppContext) {
        let doc = about_doc();
        let (emit, events) = recorder();
        let cx = mount(cx, move |_, cx| {
            about_view(&doc, emit.clone(), cx).into_any_element()
        });
        click(cx, "about-ok");
        click(cx, "overlay-close");
        let cancel = HostEvent::Overlay(OverlayInput::Cancel);
        assert_eq!(take(&events), vec![cancel.clone(), cancel]);
    }

    #[gpui::test]
    fn the_home_page_is_drawn_as_a_link(cx: &mut TestAppContext) {
        let doc = about_doc();
        let (emit, _) = recorder();
        let cx = mount(cx, move |_, cx| {
            about_view(&doc, emit.clone(), cx).into_any_element()
        });
        assert!(cx.debug_bounds("about-url").is_some());
    }
}
