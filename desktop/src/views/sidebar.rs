//! The 272px agents sidebar (design A1): an "AGENTS" header with a live
//! summary, one row per agent — monogram tile with its status badge, name,
//! status + elapsed, branch, diff and upstream, unread dot, Alt-N keycap — the
//! selected agent's terminals nested under it, and the New agent / New shell
//! footer.
//!
//! What each row says comes from the shared view model
//! ([`flightdeck::view::AgentRowView`]); this file only decides how it looks.
//! Clicks do what the TUI's clicks do: a row selects the agent and enters APP
//! mode; a terminal row focuses that terminal. Every button and context-menu
//! item performs its keymap entry through [`crate::commands`], so it is the
//! same action as its chord.

use flightdeck::app::commands::{Command, Selector};
use flightdeck::host::HostEvent;
use flightdeck::view::{
    format_elapsed, AgentBadge, AgentRowView, TerminalRef, TerminalView, UpstreamState,
};
use gpui::prelude::FluentBuilder;
use gpui::{
    div, px, AnyElement, App, Entity, FontWeight, InteractiveElement, IntoElement, MouseButton,
    ParentElement, Pixels, StatefulInteractiveElement, Styled,
};
use gpui_component::menu::{ContextMenuExt, PopupMenuItem};
use gpui_component::{h_flex, v_flex};

use crate::assets::icon;
use crate::commands::{disabled_in, keycap, keymap};
use crate::fonts::MONO_FAMILY;
use crate::host::HostModel;
use crate::surface::Surface;
use crate::theme::{Hex, Palette};
use crate::views::icons;

/// Width from the design brief (A1).
pub const SIDEBAR_WIDTH: Pixels = px(272.);

/// The sidebar's data for one frame.
pub struct SidebarData {
    pub rows: Vec<AgentRowView>,
    /// The selected agent's focused terminal: `None` for its agent, `Some(i)`
    /// for child `i` (the terminal row drawn raised).
    pub focused_child: Option<usize>,
    /// An `--isolated` run (SPECS §32): one session, so New agent is drawn
    /// disabled.
    pub isolated: bool,
    /// A remote window: the row the controlled instance itself is looking at
    /// (R7), marked `host`. Always `None` for the local workspace.
    pub host_marker: Option<usize>,
}

impl SidebarData {
    /// Read the active project's rows from the host.
    pub fn read(model: &HostModel) -> SidebarData {
        let host = model.host();
        SidebarData {
            isolated: host.is_isolated(),
            rows: host.agent_rows(),
            focused_child: host
                .active_state()
                .selected()
                .and_then(|tab| tab.session.selected_child()),
            host_marker: None,
        }
    }
}

/// Select agent `index` the way the TUI's sidebar click does: switch to it
/// (the Alt-N chord's command), then enter APP mode.
pub fn select_agent(host: &Entity<HostModel>, index: usize, cx: &mut App) {
    host.update(cx, |model, cx| {
        model.dispatch(
            HostEvent::Command(Command::SwitchAgentTab(Selector::Index(index))),
            cx,
        );
        model.dispatch(HostEvent::FocusApp, cx);
    });
}

/// Focus one of the selected agent's terminals and type into it, as the
/// TUI's terminal-tab click does.
pub fn focus_terminal(host: &Entity<HostModel>, target: TerminalRef, cx: &mut App) {
    host.update(cx, |model, cx| {
        match target {
            TerminalRef::Primary => {
                // No command selects the primary by index (its chords cycle);
                // the TUI's own click sets it directly, as does this.
                if let Some(tab) = model.host_mut().active_state_mut().selected_mut() {
                    tab.session.focus_primary();
                }
            }
            TerminalRef::Child(i) => model.dispatch(
                HostEvent::Command(Command::SwitchChildTerminal(Selector::Index(i))),
                cx,
            ),
        }
        model.dispatch(HostEvent::FocusTerminal, cx);
    });
}

/// The sidebar column. `body` is the element that carries the "App" key
/// context and focus (see [`crate::shell`]).
pub fn sidebar(data: &SidebarData, host: &Surface, p: &Palette) -> gpui::Div {
    let working = data
        .rows
        .iter()
        .filter(|r| r.badge == AgentBadge::Working)
        .count();
    let waiting = data
        .rows
        .iter()
        .filter(|r| r.badge == AgentBadge::WaitingAttention)
        .count();
    let summary = match (working, waiting, data.rows.len()) {
        (_, _, 0) => "no agents".to_string(),
        (0, 0, n) => format!("{n} idle"),
        (w, 0, _) => format!("{w} working"),
        (0, n, _) => format!("{n} waiting"),
        (w, n, _) => format!("{w} working · {n} waiting"),
    };

    let list = v_flex().gap(px(2.)).px_2().children(
        data.rows
            .iter()
            .map(|row| agent_row(row, data.focused_child, data.host_marker, host, p)),
    );

    v_flex()
        .flex_shrink_0()
        .w(SIDEBAR_WIDTH)
        .h_full()
        .bg(p.surface_sidebar.hsla())
        .border_r_1()
        .border_color(p.hairline.hsla())
        .child(
            h_flex()
                .pt_4()
                .pb_2()
                .pl(px(18.))
                .pr_4()
                .justify_between()
                .text_size(px(11.))
                .child(
                    div()
                        .text_color(p.muted.hsla())
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("AGENTS"),
                )
                .child(
                    div()
                        .font_family(MONO_FAMILY)
                        .text_color(p.muted.hsla())
                        .child(summary),
                ),
        )
        .child(
            div()
                .id("agent-list")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .when(data.rows.is_empty(), |d| d.child(empty_state(p)))
                .child(list),
        )
        .child(footer(host, p, data.isolated))
}

fn empty_state(p: &Palette) -> impl IntoElement {
    v_flex()
        .px(px(18.))
        .pt_2()
        .gap_1()
        .text_sm()
        .child(div().text_color(p.ink_2.hsla()).child("No agents yet"))
        .child(div().text_xs().text_color(p.muted.hsla()).child(format!(
            "{} starts one in this project.",
            keycap("NewAgentTab").unwrap_or_default()
        )))
}

/// Two-letter monogram for an agent's display name: the initials of a
/// two-word name (`Claude Code` → `CC`), the capitals of a camel-case one
/// (`OpenCode` → `OC`), else first and last letter (`Codex` → `CX`).
pub fn monogram(agent_name: &str) -> String {
    let words: Vec<&str> = agent_name.split_whitespace().collect();
    let initials: String = match words.as_slice() {
        [] => "?".to_string(),
        [a, b, ..] => [a, b].iter().filter_map(|w| w.chars().next()).collect(),
        [one] => {
            let caps: String = one.chars().filter(|c| c.is_uppercase()).take(2).collect();
            if caps.chars().count() == 2 {
                caps
            } else {
                one.chars()
                    .next()
                    .into_iter()
                    .chain(one.chars().last().filter(|_| one.chars().count() > 1))
                    .collect()
            }
        }
    };
    initials.to_uppercase()
}

/// Which monogram ink an agent gets (`Palette::monogram_ink`).
pub fn monogram_ink(agent_name: &str, p: &Palette) -> Hex {
    let name = agent_name.to_lowercase();
    let slot = if name.contains("claude") {
        0
    } else if name.contains("codex") {
        1
    } else if name.contains("opencode") {
        2
    } else {
        3
    };
    p.monogram_ink[slot]
}

/// The words beside the name: the manual override's label when one is set,
/// else what the badge means, plus how long it has been so.
fn status_words(row: &AgentRowView) -> String {
    if row.creating {
        return "creating…".to_string();
    }
    let word = if row.manual_status.is_some() {
        row.status_text.as_str()
    } else {
        match row.badge {
            AgentBadge::Working => "working",
            AgentBadge::WaitingAttention => "waiting",
            AgentBadge::Idle => "idle",
            AgentBadge::Done => "done",
            AgentBadge::Error => "error",
        }
    };
    match row.status_since_secs {
        Some(secs) if row.badge != AgentBadge::Idle => {
            format!("{word} · {}", format_elapsed(secs))
        }
        _ => word.to_string(),
    }
}

fn agent_row(
    row: &AgentRowView,
    focused_child: Option<usize>,
    host_marker: Option<usize>,
    host: &Surface,
    p: &Palette,
) -> AnyElement {
    let index = row.index;
    let selected = row.selected;
    let row_bg = if selected {
        p.surface_raised
    } else {
        p.surface_sidebar
    };
    let status_colour = icons::badge_colour(row.badge, p);

    // Monogram tile + the status badge on its corner.
    let tile = div()
        .relative()
        .flex_none()
        .size(px(30.))
        .rounded(px(7.))
        .bg(if selected {
            p.surface_raised_nested
        } else {
            p.surface_tile
        }
        .hsla())
        .flex()
        .items_center()
        .justify_center()
        .font_family(MONO_FAMILY)
        .text_size(px(10.5))
        .font_weight(FontWeight::MEDIUM)
        .text_color(monogram_ink(&row.agent_name, p).hsla())
        .child(monogram(&row.agent_name))
        .child(
            div()
                .absolute()
                .right(px(-4.))
                .bottom(px(-4.))
                .size(px(15.))
                .rounded_full()
                .bg(row_bg.hsla())
                .flex()
                .items_center()
                .justify_center()
                .child(icons::agent_glyph(
                    ("agent-badge", index),
                    row.badge,
                    px(11.),
                    p,
                )),
        );

    // Line 1: name + (unread dot | status words).
    let right: AnyElement = if row.unread && !selected {
        div()
            .flex_none()
            .size(px(7.))
            .rounded_full()
            .bg(p.accent.hsla())
            .into_any_element()
    } else {
        div()
            .flex_none()
            .text_size(px(11.))
            .text_color(status_colour.hsla())
            .child(status_words(row))
            .into_any_element()
    };
    let line1 = h_flex()
        .justify_between()
        .gap_2()
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_size(px(13.5))
                .font_weight(if selected {
                    FontWeight::SEMIBOLD
                } else {
                    FontWeight::MEDIUM
                })
                .text_color(p.ink.hsla())
                .child(row.name.clone()),
        )
        .when(host_marker == Some(index), |line| {
            // R7: the controlled instance is looking at this one.
            line.child(
                h_flex()
                    .id(("host-marker", index))
                    .debug_selector(move || format!("host-marker-{index}"))
                    .flex_none()
                    .gap(px(4.))
                    .text_size(px(10.5))
                    .text_color(p.accent.hsla())
                    .child(div().size(px(6.)).rounded_full().bg(p.accent.hsla()))
                    .child("host")
                    .tooltip(icons::tooltip("The host is looking at this agent")),
            )
        })
        .child(right);

    // Line 2: the branch.
    let line2 = div()
        .min_w_0()
        .truncate()
        .font_family(MONO_FAMILY)
        .text_size(px(11.))
        .text_color(p.muted.hsla())
        .child(format!("⎇ {}", row.branch));

    // Line 3: diff, file count, upstream; Alt-N on the right.
    let mut facts: Vec<AnyElement> = Vec::new();
    if let Some(changes) = row.changes {
        if !changes.is_clean() {
            facts.push(
                div()
                    .text_color(p.diff_added.hsla())
                    .child(format!("+{}", changes.lines_added))
                    .into_any_element(),
            );
            facts.push(
                div()
                    .text_color(p.diff_removed.hsla())
                    .child(format!("−{}", changes.lines_removed))
                    .into_any_element(),
            );
        }
    }
    let mut meta: Vec<String> = Vec::new();
    if let Some(changes) = row.changes {
        meta.push(match changes.files {
            0 => "clean".to_string(),
            1 => "1 file".to_string(),
            n => format!("{n} files"),
        });
    }
    match &row.upstream {
        UpstreamState::Unknown => {}
        UpstreamState::None => meta.push("no upstream".to_string()),
        UpstreamState::Tracking { ahead, behind, .. } => meta.push(format!("↑{ahead} ↓{behind}")),
    }
    if row.base_drift > 0 {
        meta.push(format!("target +{}", row.base_drift));
    }
    if !meta.is_empty() {
        facts.push(
            div()
                .min_w_0()
                .truncate()
                .text_color(p.muted.hsla())
                .child(meta.join(" · "))
                .into_any_element(),
        );
    }
    let line3 = h_flex()
        .gap_2()
        .font_family(MONO_FAMILY)
        .text_size(px(11.))
        .children(facts)
        .child(div().flex_1())
        .when_some(row.alt_index, |line, n| {
            line.child(icons::keycap(
                crate::commands::keycap(&format!("JumpToAgentTab{n}")).unwrap_or_default(),
                p.faint,
            ))
        });

    let head = h_flex()
        .id(("agent-row", index))
        .debug_selector(|| format!("agent-row-{index}"))
        .items_start()
        .gap(px(11.))
        .p(px(10.))
        .pb(px(if selected { 8. } else { 10. }))
        .cursor_pointer()
        .child(tile)
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap(px(3.))
                .child(line1)
                .child(line2)
                .child(line3),
        )
        .on_click({
            let host = host.clone();
            move |_, _, cx| host.select_agent(index, cx)
        });

    let menu_host = host.clone();
    let block = v_flex()
        .id(("agent-block", index))
        .rounded(px(8.))
        .when(selected, |b| {
            b.bg(p.surface_raised.hsla())
                .border_1()
                .border_color(p.border.hsla())
        })
        .when(!selected, |b| b.hover(|s| s.bg(p.surface_raised.hsla())))
        // Right-click selects first, as the TUI's `✕` does, so the menu's
        // item acts on the row it was opened on.
        .on_mouse_down(MouseButton::Right, {
            let host = host.clone();
            move |_, _, cx| {
                if !selected {
                    host.select_agent(index, cx);
                }
            }
        })
        .child(head)
        .when(selected, |b| {
            b.child(terminal_rows(&row.terminals, focused_child, host, p))
        })
        .context_menu(move |menu, _, _| agent_menu(menu, index, &menu_host));
    block.into_any_element()
}

/// The row's context menu. Each item selects the row, then performs the
/// keymap entry its chord performs (Rename has no chord; it is the palette's
/// Rename, which opens the same prompt).
fn agent_menu(
    menu: gpui_component::menu::PopupMenu,
    index: usize,
    host: &Surface,
) -> gpui_component::menu::PopupMenu {
    let item = |label: &'static str, id: Option<&'static str>| {
        let host = host.clone();
        let text = match id.and_then(keycap) {
            Some(cap) => format!("{label}    {cap}"),
            None => label.to_string(),
        };
        PopupMenuItem::new(text).on_click(move |_, _, cx| {
            host.select_agent(index, cx);
            match id {
                Some(id) => host.perform_id(id, cx),
                None => host.dispatch(
                    HostEvent::Command(Command::RenameAgentTab {
                        new_name: String::new(),
                    }),
                    cx,
                ),
            }
        })
    };
    menu.item(item("Rename…", None))
        .item(item("Set status…", Some("SetManualStatus")))
        .item(item("Restart agent", Some("RestartAgent")))
        .item(item(
            "Open in file manager",
            Some("OpenWorktreeInFileManager"),
        ))
        .separator()
        .item(item("Close agent…", Some("CloseAgentTab")))
}

/// The selected agent's terminals, indented under it.
fn terminal_rows(
    terminals: &[TerminalView],
    focused_child: Option<usize>,
    host: &Surface,
    p: &Palette,
) -> impl IntoElement {
    v_flex()
        .pl(px(40.))
        .pr_2()
        .pb_2()
        .gap(px(1.))
        .text_size(px(12.))
        .children(terminals.iter().enumerate().map(|(i, term)| {
            let focused = match term.target {
                TerminalRef::Primary => focused_child.is_none(),
                TerminalRef::Child(c) => focused_child == Some(c),
            };
            let target = term.target;
            let host = host.clone();
            h_flex()
                .id(("terminal-row", i))
                .debug_selector(move || format!("terminal-row-{i}"))
                .h(px(26.))
                .px_2()
                .gap_2()
                .rounded(px(5.))
                .cursor_pointer()
                .when(focused, |r| {
                    r.bg(p.surface_raised_nested.hsla())
                        .text_color(p.ink.hsla())
                })
                .when(!focused, |r| {
                    r.text_color(p.ink_2.hsla())
                        .hover(|s| s.bg(p.surface_raised_nested.hsla()))
                })
                .child(icons::icon(
                    icon::TERMINAL,
                    px(12.),
                    if focused { p.ink } else { p.ink_2 },
                ))
                .child(div().flex_none().child(term.label.clone()))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_right()
                        .font_family(MONO_FAMILY)
                        .text_size(px(10.5))
                        .text_color(p.muted.hsla())
                        .child(command_hint(&term.title)),
                )
                .on_click(move |_, _, cx| host.focus_terminal(target, cx))
        }))
}

/// The command a terminal runs, as its short name (`/usr/local/bin/claude
/// --resume` → `claude`).
pub fn command_hint(title: &str) -> String {
    let first = title.split_whitespace().next().unwrap_or("");
    first
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(first)
        .to_string()
}

/// New agent (⌃N) and New shell (⌃T): the keymap's own entries. New agent is
/// drawn disabled, with no click handler at all, in an isolated run.
fn footer(host: &Surface, p: &Palette, isolated: bool) -> impl IntoElement {
    let new_agent_disabled = keymap()
        .entry("NewAgentTab")
        .is_some_and(|entry| disabled_in(entry, isolated));
    let new_agent = {
        let host = host.clone();
        h_flex()
            .id("new-agent")
            .debug_selector(|| "new-agent".into())
            .flex_1()
            .h(px(34.))
            .justify_center()
            .gap_2()
            .rounded(px(7.))
            .border_1()
            .border_color(p.border_strong.hsla())
            .bg(p.surface_raised.hsla())
            .hover(|s| s.bg(p.surface_raised_nested.hsla()))
            .text_size(px(12.5))
            .font_weight(FontWeight::MEDIUM)
            .text_color(p.ink.hsla())
            .cursor_pointer()
            .child(icons::icon(icon::PLUS, px(13.), p.ink))
            .child("New agent")
            .child(icons::keycap(
                keycap("NewAgentTab").unwrap_or_default(),
                p.muted,
            ))
            .when(new_agent_disabled, |d| {
                d.opacity(0.45)
                    .cursor_default()
                    .tooltip(icons::tooltip("Not available in an isolated run"))
            })
            .when(!new_agent_disabled, |d| {
                d.on_click(move |_, _, cx| host.perform_id("NewAgentTab", cx))
            })
    };
    let new_shell = {
        let host = host.clone();
        div()
            .id("new-shell")
            .debug_selector(|| "new-shell".into())
            .flex_none()
            .size(px(34.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(7.))
            .border_1()
            .border_color(p.border_strong.hsla())
            .bg(p.surface_raised.hsla())
            .hover(|s| s.bg(p.surface_raised_nested.hsla()))
            .cursor_pointer()
            .child(icons::icon(icon::TERMINAL, px(14.), p.ink_2))
            .tooltip(icons::tooltip(format!(
                "New shell  {}",
                keycap("NewChildTerminal").unwrap_or_default()
            )))
            .on_click(move |_, _, cx| host.perform_id("NewChildTerminal", cx))
    };
    h_flex()
        .p_3()
        .gap_2()
        .border_t_1()
        .border_color(p.hairline.hsla())
        .child(new_agent)
        .child(new_shell)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monograms_follow_the_mockup() {
        assert_eq!(monogram("Claude Code"), "CC");
        assert_eq!(monogram("OpenCode"), "OC");
        assert_eq!(monogram("Codex"), "CX");
        assert_eq!(monogram("x"), "X");
        assert_eq!(monogram(""), "?");
    }

    #[test]
    fn command_hints_are_the_programs_short_name() {
        assert_eq!(command_hint("/usr/local/bin/claude --resume"), "claude");
        assert_eq!(command_hint("cargo test"), "cargo");
        assert_eq!(command_hint(r"C:\bin\codex.exe"), "codex.exe");
        assert_eq!(command_hint(""), "");
    }
}
