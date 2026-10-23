//! A worker's row in the launcher's Workers section: the model
//! ([`WorkerRow`], built by [`super::links::WorkerLinks::rows`]), the words
//! and numbers it shows (pure, tested without a window), and the section
//! itself, drawn above the recent projects (`specs/WORKER_PRD.md` §4.5).

use flightdeck::web::protocol::Health;
use gpui::{
    div, px, App, FontWeight, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled,
};
use gpui_component::{h_flex, v_flex};

use crate::theme::{Hex, Palette};
use crate::views::icons;

/// How a worker's background link stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkerState {
    /// Dialling it for the first time since launch.
    Connecting,
    /// Attached: the row's numbers are live.
    Online,
    /// Unreachable (stopped, asleep, no network); the link keeps retrying.
    /// `last_seen` is when it was last reached, Unix seconds.
    Offline { last_seen: Option<u64> },
    /// The worker does not know this Desktop's token: pair it again.
    NotPaired,
    /// The worker withdrew this Desktop's access.
    Revoked,
    /// The two speak protocols too far apart; the text says which side.
    UpdateNeeded(String),
}

/// One saved worker, as the launcher lists it.
#[derive(Clone, Debug, PartialEq)]
pub struct WorkerRow {
    /// Its saved label.
    pub label: String,
    /// `host:port`, as saved.
    pub address: String,
    pub state: WorkerState,
    /// Sessions waiting for input or in error, while online.
    pub needs_you: usize,
    /// The worker's FlightDeck version, when known.
    pub version: Option<String>,
    /// The version differs from the core this app was built with.
    pub skew: bool,
    /// The latest health sample, while online.
    pub health: Option<Health>,
    /// Host messages no window has seen yet.
    pub unread: u32,
}

/// The status pill's word.
pub fn state_label(state: &WorkerState) -> &'static str {
    match state {
        WorkerState::Connecting => "connecting…",
        WorkerState::Online => "online",
        WorkerState::Offline { .. } => "offline",
        WorkerState::NotPaired => "not paired",
        WorkerState::Revoked => "access revoked",
        WorkerState::UpdateNeeded(_) => "update needed",
    }
}

/// The row's second line: health while online, otherwise what the state
/// means. `now` is the wall clock, Unix seconds.
pub fn detail_line(row: &WorkerRow, now: u64) -> String {
    match &row.state {
        WorkerState::Online => row
            .health
            .as_ref()
            .map(health_line)
            .filter(|line| !line.is_empty())
            .unwrap_or_else(|| row.address.clone()),
        WorkerState::Connecting => row.address.clone(),
        WorkerState::Offline {
            last_seen: Some(seen),
        } => format!("last seen {}", ago(now.saturating_sub(*seen))),
        WorkerState::Offline { last_seen: None } => row.address.clone(),
        WorkerState::NotPaired => {
            "This Desktop is not paired with it: Connect to remote… pairs it again.".to_string()
        }
        WorkerState::Revoked => "The worker revoked this Desktop's access.".to_string(),
        WorkerState::UpdateNeeded(why) => why.clone(),
    }
}

/// Health in one line: `load 0.42 · 18 GB free · 3 agents`. Parts the
/// worker could not measure are left out.
pub fn health_line(health: &Health) -> String {
    let mut parts = Vec::new();
    if let Some(load) = health.load_avg_1m_centi {
        parts.push(format!("load {}.{:02}", load / 100, load % 100));
    }
    if let Some(free) = health
        .disk_free_projects_root_bytes
        .or(health.disk_free_home_bytes)
    {
        parts.push(format!("{} free", bytes(free)));
    }
    match health.agents_total {
        0 => {}
        1 => parts.push("1 agent".to_string()),
        n => parts.push(format!("{n} agents")),
    }
    parts.join(" · ")
}

/// A size as people read a disk's: decimal units, whole numbers from 10 up.
fn bytes(n: u64) -> String {
    const GB: f64 = 1e9;
    const MB: f64 = 1e6;
    let n = n as f64;
    if n >= 10.0 * GB {
        format!("{:.0} GB", n / GB)
    } else if n >= GB {
        format!("{:.1} GB", n / GB)
    } else {
        format!("{:.0} MB", n / MB)
    }
}

/// How long ago, coarsely: `just now`, `5 min ago`, `3 h ago`, `2 d ago`.
fn ago(secs: u64) -> String {
    match secs {
        0..60 => "just now".to_string(),
        60..3_600 => format!("{} min ago", secs / 60),
        3_600..86_400 => format!("{} h ago", secs / 3_600),
        _ => format!("{} d ago", secs / 86_400),
    }
}

/// The pill's ink for a state.
fn state_ink(state: &WorkerState, p: &Palette) -> Hex {
    match state {
        WorkerState::Online => p.status_done,
        WorkerState::Connecting | WorkerState::Offline { .. } => p.muted,
        WorkerState::UpdateNeeded(_) => p.status_attention,
        WorkerState::NotPaired | WorkerState::Revoked => p.danger,
    }
}

/// The Workers section: a header and one row per saved worker. A click on a
/// row opens that worker's remote window ([`super::links::open`]).
pub fn section(rows: &[WorkerRow], p: &Palette, now: u64) -> impl IntoElement {
    v_flex()
        .id("workers")
        .debug_selector(|| "workers".into())
        .w_full()
        .gap(px(2.))
        .child(
            div()
                .px_3()
                .pb_1()
                .text_size(px(11.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(p.muted.hsla())
                .child("WORKERS"),
        )
        .children(
            rows.iter()
                .enumerate()
                .map(|(i, row)| worker_row(i, row, p, now)),
        )
}

/// One worker: name, status pill, needs-you count, unread dot and version
/// on the first line; health (or what the state means) under it.
fn worker_row(index: usize, row: &WorkerRow, p: &Palette, now: u64) -> impl IntoElement {
    let needs_you = (row.needs_you > 0).then(|| {
        h_flex()
            .debug_selector(move || format!("worker-{index}-needs-you"))
            .flex_none()
            .gap_1()
            .child(icons::icon(
                crate::assets::icon::STATUS_ATTENTION,
                px(11.),
                p.status_attention,
            ))
            .child(
                div()
                    .text_size(px(11.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(p.status_attention.hsla())
                    .child(row.needs_you.to_string()),
            )
    });
    let unread = (row.unread > 0).then(|| {
        div()
            .debug_selector(move || format!("worker-{index}-unread"))
            .flex_none()
            .size(px(7.))
            .rounded_full()
            .bg(p.accent.hsla())
    });
    let version = row.version.clone().map(|v| {
        div()
            .flex_none()
            .font_family(crate::fonts::MONO_FAMILY)
            .text_size(px(11.))
            .text_color(p.muted.hsla())
            .child(v)
    });
    let skew = row.skew.then(|| {
        icons::pill("version differs", p.status_attention, p)
            .debug_selector(move || format!("worker-{index}-skew"))
    });
    let address = row.address.clone();
    v_flex()
        .id(("worker", index))
        .debug_selector(move || format!("worker-{index}"))
        .w_full()
        .px_3()
        .py(px(7.))
        .gap(px(3.))
        .rounded(px(7.))
        .cursor_pointer()
        .hover(|s| s.bg(p.surface_raised.hsla()))
        .child(
            h_flex()
                .w_full()
                .gap_2()
                .child(icons::icon(crate::assets::icon::TERMINAL, px(14.), p.muted))
                .child(
                    div()
                        .flex_none()
                        .max_w(px(150.))
                        .truncate()
                        .text_size(px(13.))
                        .font_weight(FontWeight::MEDIUM)
                        .child(row.label.clone()),
                )
                .child(
                    icons::pill(state_label(&row.state), state_ink(&row.state, p), p)
                        .debug_selector(move || format!("worker-{index}-state")),
                )
                .children(needs_you)
                .children(unread)
                .child(div().flex_1())
                .children(version)
                .children(skew),
        )
        .child(
            div()
                .pl(px(22.))
                .truncate()
                .text_size(px(11.5))
                .text_color(p.muted.hsla())
                .child(detail_line(row, now)),
        )
        .on_click(move |_, _, cx: &mut App| super::links::open(&address, cx))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn health() -> Health {
        Health {
            sampled_at_ms: 0,
            uptime_secs: 60,
            version: "1.0.0".to_string(),
            load_avg_1m_centi: Some(42),
            cpu_count: 8,
            mem_total_bytes: None,
            mem_available_bytes: None,
            disk_free_home_bytes: Some(30_000_000_000),
            disk_free_projects_root_bytes: Some(18_400_000_000),
            agents_total: 3,
            agents_running: 2,
        }
    }

    fn row(state: WorkerState) -> WorkerRow {
        WorkerRow {
            label: "box".to_string(),
            address: "box.ts.net:7420".to_string(),
            state,
            needs_you: 0,
            version: None,
            skew: false,
            health: Some(health()),
            unread: 0,
        }
    }

    #[test]
    fn health_reads_as_one_line() {
        assert_eq!(health_line(&health()), "load 0.42 · 18 GB free · 3 agents");
        let mut h = health();
        h.load_avg_1m_centi = Some(1_205);
        h.disk_free_projects_root_bytes = None;
        h.agents_total = 1;
        assert_eq!(
            health_line(&h),
            "load 12.05 · 30 GB free · 1 agent",
            "$HOME's disk when the projects root's is unknown"
        );
        h.load_avg_1m_centi = None;
        h.disk_free_home_bytes = Some(4_520_000_000);
        h.agents_total = 0;
        assert_eq!(health_line(&h), "4.5 GB free");
        h.disk_free_home_bytes = Some(512_000_000);
        assert_eq!(health_line(&h), "512 MB free");
        h.disk_free_home_bytes = None;
        assert_eq!(health_line(&h), "");
    }

    #[test]
    fn the_second_line_says_what_the_state_means() {
        let now = 1_000_000;
        assert_eq!(
            detail_line(&row(WorkerState::Online), now),
            "load 0.42 · 18 GB free · 3 agents"
        );
        let mut no_health = row(WorkerState::Online);
        no_health.health = None;
        assert_eq!(detail_line(&no_health, now), "box.ts.net:7420");
        assert_eq!(
            detail_line(
                &row(WorkerState::Offline {
                    last_seen: Some(now - 3 * 3_600)
                }),
                now
            ),
            "last seen 3 h ago"
        );
        assert_eq!(
            detail_line(
                &row(WorkerState::Offline {
                    last_seen: Some(now - 10)
                }),
                now
            ),
            "last seen just now"
        );
        assert_eq!(
            detail_line(&row(WorkerState::Offline { last_seen: None }), now),
            "box.ts.net:7420"
        );
        assert_eq!(
            detail_line(&row(WorkerState::UpdateNeeded("update it".into())), now),
            "update it"
        );
    }

    #[test]
    fn coarse_ages() {
        assert_eq!(ago(59), "just now");
        assert_eq!(ago(60), "1 min ago");
        assert_eq!(ago(3_599), "59 min ago");
        assert_eq!(ago(7_200), "2 h ago");
        assert_eq!(ago(3 * 86_400), "3 d ago");
    }
}
