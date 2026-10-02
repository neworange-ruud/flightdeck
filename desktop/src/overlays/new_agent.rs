//! The New Agent Session Tab form (SPECS §4, §22; beads
//! `remote-control-bmej.4.2`): which agent, where it runs (a new branch, an
//! existing branch, or the base branch with no worktree), and the branch name
//! or filter.
//!
//! The form's state is the host's [`NewAgentForm`]; every control answers
//! with the TUI's own keys, so the form cannot disagree with the TUI's:
//!
//! - an agent chip is `↑`/`↓` presses to that agent (the TUI's radio),
//! - a target segment is `Tab` presses to that target (the TUI's cycle, which
//!   skips "existing branch" when the project has no other local branch),
//! - a branch row is [`OverlayInput::SelectRow`],
//! - typing is the TUI's typing, and Create is Enter.
//!
//! Validation is the TUI's too: an empty name for a worktree target is not an
//! error message, the form simply stays open.

use flightdeck::host::{
    DialogView, HostEvent, NewAgentForm, NewAgentTarget, OverlayInput, OverlayKey,
};
use gpui::{
    div, prelude::FluentBuilder as _, px, AnyElement, Div, FontWeight, InteractiveElement as _,
    IntoElement, ParentElement, SharedString, StatefulInteractiveElement as _, Styled,
};

use super::{
    buttons, dialog_card, footer, heading, origin_line, overlay, section_label, split_hint,
    OverlayCx, MONO,
};
use crate::theme::Palette;

/// The debug selector on agent chip `index`, for tests.
pub fn agent_selector(index: usize) -> String {
    format!("new-agent-agent-{index}")
}

/// The debug selector on a target segment, for tests.
pub fn target_selector(target: NewAgentTarget) -> String {
    format!("new-agent-target-{}", target_slug(target))
}

fn target_slug(target: NewAgentTarget) -> &'static str {
    match target {
        NewAgentTarget::NewBranch => "new",
        NewAgentTarget::ExistingBranch => "existing",
        NewAgentTarget::Base => "base",
    }
}

/// The target `Tab` moves to from `target`: new → existing → base → new,
/// skipping existing when there is no other local branch (the TUI's cycle).
pub fn next_target(target: NewAgentTarget, has_existing_branches: bool) -> NewAgentTarget {
    match target {
        NewAgentTarget::NewBranch if has_existing_branches => NewAgentTarget::ExistingBranch,
        NewAgentTarget::NewBranch | NewAgentTarget::ExistingBranch => NewAgentTarget::Base,
        NewAgentTarget::Base => NewAgentTarget::NewBranch,
    }
}

/// The `Tab` presses that move the form to `to`, or `None` when the cycle
/// never reaches it (existing branch, with none to choose from).
pub fn tabs_to(form: &NewAgentForm, to: NewAgentTarget) -> Option<Vec<HostEvent>> {
    let mut at = form.target;
    let mut presses = Vec::new();
    for _ in 0..3 {
        if at == to {
            return Some(presses);
        }
        at = next_target(at, form.has_existing_branches);
        presses.push(overlay(OverlayInput::Key(OverlayKey::Tab)));
    }
    None
}

/// The `↑`/`↓` presses that choose agent `index`, or `None` while the form
/// targets an existing branch — there the arrows move the branch list, and
/// the TUI shows the agent as fixed (`Agent: …`).
pub fn arrows_to_agent(form: &NewAgentForm, index: usize) -> Option<Vec<HostEvent>> {
    if form.target == NewAgentTarget::ExistingBranch || index >= form.agents.len() {
        return None;
    }
    let (key, n) = if index < form.selected_agent {
        (OverlayKey::Up, form.selected_agent - index)
    } else {
        (OverlayKey::Down, index - form.selected_agent)
    };
    Some((0..n).map(|_| overlay(OverlayInput::Key(key))).collect())
}

/// Draw the form.
pub fn render(view: &DialogView, form: &NewAgentForm, cx: &OverlayCx) -> AnyElement {
    let p = cx.palette;
    let (question, hint) = split_hint(&view.title);

    let agents_fixed = form.target == NewAgentTarget::ExistingBranch;
    let agents = div().flex().flex_row().flex_wrap().gap(px(6.)).children(
        form.agents.iter().enumerate().map(|(i, agent)| {
            let selected = i == form.selected_agent;
            let selector = agent_selector(i);
            let click = arrows_to_agent(form, i);
            div()
                .id(SharedString::from(selector.clone()))
                .debug_selector(move || selector)
                .flex()
                .flex_row()
                .items_center()
                .gap(px(7.))
                .h(px(30.))
                .px(px(11.))
                .rounded(px(7.))
                .border_1()
                .text_size(px(12.5))
                .map(|d| {
                    if selected {
                        d.bg(p.surface_raised_nested.hsla())
                            .border_color(p.border_strong.hsla())
                            .text_color(p.ink.hsla())
                    } else {
                        d.bg(p.surface_raised.hsla())
                            .border_color(p.border.hsla())
                            .text_color(p.ink_2.hsla())
                    }
                })
                .when(agents_fixed && !selected, |d| d.opacity(0.45))
                .map(|d| match click {
                    Some(events) if !agents_fixed => d
                        .cursor_pointer()
                        .hover(|s| s.border_color(p.border_strong.hsla()))
                        .on_click(cx.on_click_emit(events)),
                    _ => d,
                })
                .child(radio(p, selected))
                .child(agent.name.clone())
        }),
    );

    let targets = segmented(p, form, cx);

    let detail: Div = match form.target {
        NewAgentTarget::NewBranch => field_block(p, "Task name", cx.field.clone()),
        NewAgentTarget::ExistingBranch => {
            let rows = super::dialog::render_rows(view, true, false, cx);
            field_block(
                p,
                "Branch",
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(cx.field.clone())
                    .child(rows),
            )
        }
        // The body line says it: "Runs on base branch 'main' in the project
        // root — no worktree."
        NewAgentTarget::Base => div(),
    };

    let body = div()
        .flex()
        .flex_col()
        .gap(px(16.))
        .p(px(20.))
        .child(heading(p, question, &view.body))
        .children(origin_line(p, view.origin_label.as_deref()))
        .child(field_block(p, "Agent", agents))
        .child(field_block(p, "Runs on", targets))
        .child(detail)
        .children(hint.map(|h| {
            div()
                .text_size(px(11.5))
                .text_color(p.muted.hsla())
                .child(h.to_string())
        }));

    // The target button (`Tab`) is the segmented control above; the rest are
    // the host's, verbatim.
    let answer: Vec<_> = view
        .buttons
        .iter()
        .filter(|b| b.id != "Tab")
        .cloned()
        .collect();
    let footer = footer(p).justify_end().children(buttons(p, &answer, cx));

    dialog_card(p, 540.)
        .child(body)
        .child(footer)
        .into_any_element()
}

/// A labelled group.
fn field_block(p: &Palette, label: &str, content: impl IntoElement) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(7.))
        .child(section_label(label.to_string(), p))
        .child(content)
}

/// A radio mark: a ring, filled with the accent when chosen.
fn radio(p: &Palette, on: bool) -> Div {
    div()
        .size(px(12.))
        .rounded(px(6.))
        .border_1()
        .flex()
        .items_center()
        .justify_center()
        .map(|d| {
            if on {
                d.border_color(p.accent.hsla())
                    .child(div().size(px(6.)).rounded(px(3.)).bg(p.accent.hsla()))
            } else {
                d.border_color(p.faint.hsla())
            }
        })
}

/// New branch | Existing branch | Base (main).
fn segmented(p: &Palette, form: &NewAgentForm, cx: &OverlayCx) -> Div {
    let options = [
        (NewAgentTarget::NewBranch, "New branch".to_string()),
        (
            NewAgentTarget::ExistingBranch,
            "Existing branch".to_string(),
        ),
        (NewAgentTarget::Base, format!("Base · {}", form.base_branch)),
    ];
    div()
        .flex()
        .flex_row()
        .p(px(3.))
        .gap(px(2.))
        .rounded(px(8.))
        .border_1()
        .border_color(p.border.hsla())
        .bg(p.surface_input.hsla())
        .children(options.into_iter().map(|(target, label)| {
            let selected = target == form.target;
            let selector = target_selector(target);
            let presses = tabs_to(form, target);
            div()
                .id(SharedString::from(selector.clone()))
                .debug_selector(move || selector)
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .h(px(26.))
                .px(px(10.))
                .rounded(px(5.))
                .text_size(px(12.))
                .when(target == NewAgentTarget::Base, |d| {
                    d.font_family(MONO).text_size(px(11.5))
                })
                .map(|d| {
                    if selected {
                        d.bg(p.surface_raised_nested.hsla())
                            .text_color(p.ink.hsla())
                            .font_weight(FontWeight::MEDIUM)
                    } else {
                        d.text_color(p.muted.hsla())
                    }
                })
                .map(|d| match presses {
                    Some(events) if !selected => d
                        .cursor_pointer()
                        .hover(|s| s.text_color(p.ink.hsla()))
                        .on_click(cx.on_click_emit(events)),
                    Some(_) => d,
                    // No other local branch: the TUI's Tab skips this target.
                    None => d.opacity(0.4),
                })
                .child(label)
        }))
}
