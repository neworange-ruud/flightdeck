//! A waiting tile's inline answer row (remote-control-bmej.5.6).
//!
//! What the row offers is the host's [`PromptView`]
//! ([`MissionTile::prompt`]), built from the prompt FlightDeck Remote detected
//! for the session: Approve / Deny for a permission, one button per option
//! for a short single-select question, and "Open to answer" for any other
//! shape — which opens the tile full size, exactly as Enter (or a double
//! click) does, so the agent's own TUI takes the answer.
//!
//! A button sends [`HostEvent::AnswerPrompt`]; the host runs it through the
//! phone's translator and keystroke queue and the input lock, so nothing here
//! knows which keys an agent wants. Clicks stop at the button: answering a
//! prompt does not also select (or, on a double click, open) the tile.
//!
//! [`HostEvent::AnswerPrompt`]: flightdeck::host::HostEvent::AnswerPrompt

use flightdeck::app::modes::InputMode;
use flightdeck::host::HostEvent;
use flightdeck::view::{MissionTile, PromptAnswer, PromptReply, PromptShape};
use gpui::{
    div, px, AnyElement, App, Entity, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled,
};
use gpui_component::h_flex;

use super::select_session;
use crate::commands::keycap;
use crate::host::HostModel;
use crate::theme::{Hex, Palette};
use crate::views::icons;

/// The row's height.
const ROW_HEIGHT: f32 = 34.;

/// The answer row for `t`, or `None` when it is not waiting on a detected
/// prompt. `index` is the tile's position, for element ids and the test
/// selectors (`mission-tile-{index}-approve`, `-deny`, `-option-{i}`,
/// `-open-to-answer`).
pub fn prompt_row(
    index: usize,
    t: &MissionTile,
    host: &Entity<HostModel>,
    p: &Palette,
) -> Option<AnyElement> {
    let prompt = t.prompt.as_ref()?;
    let mut buttons: Vec<AnyElement> = Vec::new();
    if prompt.shape == PromptShape::Unsupported {
        let host = host.clone();
        let key = t.key.clone();
        let tab_index = t.tab_index;
        buttons.push(
            button(
                (index, "open-to-answer".into()),
                "Open to answer".into(),
                Style::Neutral,
                p,
            )
            .child(icons::keycap(
                keycap("FocusTerminal").unwrap_or_default(),
                p.faint,
            ))
            .on_click(move |_, _, cx| {
                cx.stop_propagation();
                select_session(&host, &key, tab_index, InputMode::Terminal, cx);
            })
            .into_any_element(),
        );
    }
    for b in &prompt.buttons {
        let (selector, style) = match b.answer {
            PromptAnswer::Approve => ("approve".to_string(), Style::Primary),
            PromptAnswer::Deny => ("deny".to_string(), Style::Danger),
            PromptAnswer::Option(i) => (format!("option-{i}"), Style::Neutral),
        };
        let reply = PromptReply {
            key: t.key.clone(),
            prompt_id: prompt.prompt_id.clone(),
            answer: b.answer,
        };
        let host = host.clone();
        buttons.push(
            button((index, selector.into()), b.label.clone().into(), style, p)
                .on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    answer(&host, reply.clone(), cx);
                })
                .into_any_element(),
        );
    }
    Some(
        h_flex()
            .flex_shrink_0()
            .h(px(ROW_HEIGHT))
            .px(px(12.))
            .gap(px(6.))
            .border_t_1()
            .border_color(p.attention_border.hsla())
            .bg(p.attention_surface.hsla())
            .text_size(px(11.5))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(p.ink_2.hsla())
                    .child(prompt.text.lines().next().unwrap_or_default().to_string()),
            )
            .children(buttons)
            .into_any_element(),
    )
}

/// Send the answer through the host, as every other control's event.
fn answer(host: &Entity<HostModel>, reply: PromptReply, cx: &mut App) {
    host.update(cx, |model, cx| {
        model.dispatch(HostEvent::AnswerPrompt(reply), cx);
        cx.notify();
    });
}

#[derive(Clone, Copy)]
enum Style {
    Primary,
    Danger,
    Neutral,
}

/// One button of the row: a label (and whatever the caller adds) in a small
/// rounded box coloured by `style`.
fn button(
    (index, name): (usize, SharedString),
    label: SharedString,
    style: Style,
    p: &Palette,
) -> gpui::Stateful<gpui::Div> {
    let (bg, ink, border): (Hex, Hex, Hex) = match style {
        Style::Primary => (
            p.button_primary_bg,
            p.button_primary_ink,
            p.button_primary_bg,
        ),
        Style::Danger => (p.danger_bg, p.danger, p.danger_border),
        Style::Neutral => (p.surface_raised, p.ink_2, p.border),
    };
    let hover = p.border_strong;
    let selector = format!("mission-tile-{index}-{name}");
    h_flex()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .flex_none()
        .gap(px(6.))
        .px(px(9.))
        .h(px(22.))
        .rounded(px(5.))
        .border_1()
        .border_color(border.hsla())
        .bg(bg.hsla())
        .text_color(ink.hsla())
        .cursor_pointer()
        .hover(move |s| s.border_color(hover.hsla()))
        .child(label)
}
