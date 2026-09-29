//! Getting the user's attention: the "needs you" count on the app's icon, and
//! a window-attention request when a new agent starts waiting.
//!
//! ## What goes where
//!
//! - **OS notifications** (a banner and the completion / input-required
//!   sound) are the core's: `AppHost::pump` posts every finish through the
//!   `Notifier` the app was opened with, and [`crate::host::RealServices`]
//!   hands it the TUI's own `SystemNotifier`, so the GUI's events, titles and
//!   sounds are the TUI's by construction (the same `take_finish_notifications`
//!   feeds both). Nothing in this module posts one.
//! - **Unread dots** clear when a session is viewed; that is core state the
//!   sidebar already draws.
//! - **The badge** is this module's: [`badge_label`] turns the count into the
//!   text, [`set_dock_badge`] shows it.
//!
//! ## Per OS
//!
//! | OS | Count on the icon | Window attention |
//! | --- | --- | --- |
//! | macOS | `NSApp.dockTile.badgeLabel` (below) | GPUI's `Window::request_attention` (bounces the Dock icon while the app is inactive) |
//! | Windows | not done: GPUI has no taskbar-overlay API. The equivalent is `ITaskbarList3::SetOverlayIcon` on the window's `HWND` | `request_attention` flashes the taskbar button |
//! | Linux | not done: there is no portable badge (`com.canonical.Unity.LauncherEntry` covers Unity/Plasma docks only) | `request_attention` sets the window-manager urgency hint |

use gpui::App;

/// The largest count spelled out; more reads `99+`, as OS badges do.
const BADGE_MAX: usize = 99;

/// The badge text for `count` agents needing the user: `None` (clear the
/// badge) for zero.
pub fn badge_label(count: usize) -> Option<String> {
    match count {
        0 => None,
        n if n > BADGE_MAX => Some(format!("{BADGE_MAX}+")),
        n => Some(n.to_string()),
    }
}

/// What a new reading of the needs-you count means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Observation {
    /// The badge text changed: set it.
    pub badge_changed: bool,
    /// More agents are waiting than before: someone new needs the user.
    pub new_arrivals: bool,
}

/// The last count shown, so the badge is only touched when it changes and the
/// window is only nagged for a genuinely new arrival (an answered agent
/// lowering the count is not one).
#[derive(Debug, Default)]
pub struct AttentionState {
    count: usize,
}

impl AttentionState {
    /// Record a new reading of the count.
    pub fn observe(&mut self, count: usize) -> Observation {
        let previous = std::mem::replace(&mut self.count, count);
        Observation {
            badge_changed: badge_label(previous) != badge_label(count),
            new_arrivals: count > previous,
        }
    }
}

/// Apply a reading: update the badge when its text changed, and ask the OS to
/// draw attention to a window that is not active when someone new is
/// waiting. Call from the main thread (the host's turn is).
pub fn apply(state: &mut AttentionState, count: usize, cx: &mut App) {
    let seen = state.observe(count);
    if seen.badge_changed {
        set_dock_badge(badge_label(count).as_deref());
    }
    if seen.new_arrivals {
        for window in cx.windows() {
            // A closed or released window is simply skipped.
            let _ = window.update(cx, |_, window, _| {
                if !window.is_window_active() {
                    window.request_attention();
                }
            });
        }
    }
}

/// Show `label` on the Dock icon, or clear the badge for `None`.
#[cfg(target_os = "macos")]
pub fn set_dock_badge(label: Option<&str>) {
    use objc2_app_kit::NSApplication;
    use objc2_foundation::{MainThreadMarker, NSString};

    // GPUI runs the app on the main thread; off it there is no dock tile to
    // touch, and doing nothing is right.
    let Some(main) = MainThreadMarker::new() else {
        return;
    };
    let tile = NSApplication::sharedApplication(main).dockTile();
    let text = label.map(NSString::from_str);
    tile.setBadgeLabel(text.as_deref());
}

/// No app-icon badge API on this OS (see the module docs).
#[cfg(not(target_os = "macos"))]
pub fn set_dock_badge(label: Option<&str>) {
    let _ = label;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_badge_is_the_count_and_clears_at_zero() {
        assert_eq!(badge_label(0), None);
        assert_eq!(badge_label(1).as_deref(), Some("1"));
        assert_eq!(badge_label(99).as_deref(), Some("99"));
        assert_eq!(badge_label(100).as_deref(), Some("99+"));
        assert_eq!(badge_label(5000).as_deref(), Some("99+"));
    }

    #[test]
    fn the_badge_is_only_touched_when_its_text_changes() {
        let mut state = AttentionState::default();
        assert_eq!(
            state.observe(0),
            Observation {
                badge_changed: false,
                new_arrivals: false
            },
            "nothing waiting at launch"
        );
        assert_eq!(
            state.observe(1),
            Observation {
                badge_changed: true,
                new_arrivals: true
            }
        );
        assert_eq!(
            state.observe(1),
            Observation {
                badge_changed: false,
                new_arrivals: false
            },
            "the same count every turn changes nothing"
        );
        assert_eq!(
            state.observe(0),
            Observation {
                badge_changed: true,
                new_arrivals: false
            },
            "answering clears the badge without nagging the window"
        );
        // 100 and 101 both read "99+": no redundant badge write.
        state.observe(100);
        assert!(!state.observe(101).badge_changed);
    }
}
