//! Confirmations for destructive and git operations (beads
//! `remote-control-bmej.4.3`): abandon worktree, close session, the sidebar's
//! close menu, close terminal, finish / local merge, rebase worktree, push
//! with uncommitted changes, close project, quit, unpair.
//!
//! The question is the host's title, verbatim — SPECS §5's guard copy
//! ("Rewrites history; aborts on conflict.") is part of it — and the buttons
//! are the host's, in its roles: a [`ButtonRole::Destructive`] answer is drawn
//! in the danger colours, and none of these dialogs has a default button, so
//! Enter never confirms them (see [`super::dialog::key_events`]). Esc cancels.
//!
//! Pull base has no confirmation in the core (it only fast-forwards the base),
//! so there is none here either.

use flightdeck::host::{ButtonRole, DialogKind, DialogView};
use gpui::{div, px, AnyElement, Div, FontWeight, IntoElement, ParentElement, Styled};

use super::{button_row, dialog_card, heading, origin_line, OverlayCx, MONO};
use crate::theme::Palette;

/// Whether `kind` is drawn as a confirmation.
pub fn handles(kind: &DialogKind) -> bool {
    matches!(
        kind,
        DialogKind::CloseSession { .. }
            | DialogKind::CloseTerminal { .. }
            | DialogKind::CloseSessionChoice { .. }
            | DialogKind::ConfirmPush
            | DialogKind::ConfirmAbandon { .. }
            | DialogKind::ConfirmMerge { .. }
            | DialogKind::ConfirmRebase { .. }
            | DialogKind::CloseProject { .. }
            | DialogKind::UnpairPhone
            | DialogKind::ConfirmQuit
    )
}

/// Draw a confirmation.
pub fn render(view: &DialogView, cx: &OverlayCx) -> AnyElement {
    let p = cx.palette;
    let destructive = view
        .buttons
        .iter()
        .any(|b| b.role == ButtonRole::Destructive);

    let mark = mark(p, destructive);
    let text = div()
        .flex_1()
        // Without a zero minimum the column grows to its longest line and a
        // long question (the rebase guard) runs off the card instead of wrapping.
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(12.))
        .child(heading(p, &view.title, &view.body))
        .children(facts(p, &view.kind))
        .children(origin_line(p, view.origin_label.as_deref()));

    // The close menu offers four answers; give them one row.
    dialog_card(p, if view.buttons.len() > 3 { 640. } else { 460. })
        .child(
            div()
                .flex()
                .flex_row()
                .items_start()
                .gap(px(14.))
                .p(px(20.))
                .child(mark)
                .child(text),
        )
        .child(button_row(p, &view.buttons, cx))
        .into_any_element()
}

/// The badge beside the question: a warning mark when an answer destroys
/// work, a plain question mark otherwise.
fn mark(p: &Palette, destructive: bool) -> Div {
    let (bg, ink, glyph) = if destructive {
        (p.danger_bg, p.danger, "!")
    } else {
        (p.surface_raised, p.ink_2, "?")
    };
    div()
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .size(px(30.))
        .rounded(px(15.))
        .bg(bg.hsla())
        .text_color(ink.hsla())
        .font_weight(FontWeight::BOLD)
        .text_size(px(15.))
        .child(glyph)
}

/// The typed facts behind the question, as scannable chips: which branch goes
/// where, how far the base moved, whether an agent is running. They restate
/// the title's own facts; the title stays the authority.
fn facts(p: &Palette, kind: &DialogKind) -> Option<Div> {
    let mut chips: Vec<Div> = Vec::new();
    match kind {
        DialogKind::ConfirmMerge {
            agent_branch,
            base_branch,
            primary_running,
        } => {
            chips.push(branch_chip(p, agent_branch, base_branch));
            if *primary_running {
                chips.push(chip(p, "agent running", true));
            }
        }
        DialogKind::ConfirmRebase {
            agent_branch,
            base_branch,
            drift,
            primary_running,
        } => {
            chips.push(branch_chip(p, agent_branch, base_branch));
            if *drift > 0 {
                chips.push(chip(
                    p,
                    &match drift {
                        1 => "base +1 commit".to_string(),
                        n => format!("base +{n} commits"),
                    },
                    false,
                ));
            }
            if *primary_running {
                chips.push(chip(p, "agent running", true));
            }
        }
        DialogKind::ConfirmAbandon { dirty: true } | DialogKind::ConfirmPush => {
            chips.push(chip(p, "uncommitted changes", true));
        }
        DialogKind::CloseTerminal { label } => chips.push(chip(p, label, false)),
        _ => {}
    }
    (!chips.is_empty()).then(|| {
        div()
            .flex()
            .flex_row()
            .flex_wrap()
            .gap(px(6.))
            .children(chips)
    })
}

fn chip(p: &Palette, text: &str, attention: bool) -> Div {
    let (bg, ink) = if attention {
        (p.status_attention_bg, p.status_attention)
    } else {
        (p.surface_raised, p.ink_2)
    };
    div()
        .px(px(7.))
        .py(px(2.))
        .rounded(px(5.))
        .bg(bg.hsla())
        .text_color(ink.hsla())
        .text_size(px(11.5))
        .child(text.to_string())
}

/// `⎇ flightdeck/x → main`.
fn branch_chip(p: &Palette, from: &str, to: &str) -> Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.))
        .px(px(7.))
        .py(px(2.))
        .rounded(px(5.))
        .border_1()
        .border_color(p.border.hsla())
        .bg(p.surface_terminal.hsla())
        .font_family(MONO)
        .text_size(px(11.))
        .text_color(p.ink_2.hsla())
        .child(format!("⎇ {from}"))
        .child(div().text_color(p.separator.hsla()).child("→"))
        .child(to.to_string())
}
