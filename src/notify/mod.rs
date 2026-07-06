//! OS notifications (SPECS §24): alert the user when an agent finishes a running
//! task even while their attention is elsewhere.
//!
//! [`SystemNotifier`] is the production [`Notifier`]. On macOS it posts a native
//! notification via `terminal-notifier`/`osascript`; on Linux it posts via
//! `notify-send` (libnotify); on every other platform (e.g. Windows) it is a
//! no-op, so the crate builds and runs everywhere.
//!
//! Delivery is best-effort and **never blocks the render loop**: each backend
//! spawns its command on a detached thread and ignores the result. A failed
//! notification must never disrupt the UI.

use crate::contracts::{Notification, Notifier};

/// The production notifier. Zero-sized; cheap to construct and pass by reference.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemNotifier;

impl Notifier for SystemNotifier {
    fn notify(&self, notification: &Notification) {
        #[cfg(target_os = "macos")]
        {
            post_macos(&notification.title, &notification.body);
        }
        #[cfg(target_os = "linux")]
        {
            post_linux(&notification.title, &notification.body);
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            // No-op on platforms without a notification backend (e.g. Windows).
            let _ = notification;
        }
    }
}

/// Post a native macOS notification on a detached thread so the render loop
/// never blocks; output is discarded and any error is ignored.
///
/// Prefers `terminal-notifier` when it is on `PATH`: it ships a real `.app`
/// bundle, so notifications register and are delivered reliably and it prompts
/// for permission on first use. Falls back to `osascript`, whose `display
/// notification` is attributed to "Script Editor" — which must be allowed in
/// System Settings → Notifications for banners to appear.
#[cfg(target_os = "macos")]
fn post_macos(title: &str, body: &str) {
    let title = title.to_string();
    let body = body.to_string();
    std::thread::spawn(move || {
        if post_via_terminal_notifier(&title, &body) {
            return;
        }
        post_via_osascript(&title, &body);
    });
}

/// Post a desktop notification on Linux via `notify-send` (libnotify) on a
/// detached thread so the render loop never blocks; output is discarded and any
/// error is ignored. If `notify-send` is not installed the spawn fails and the
/// notification is silently dropped — best-effort, exactly like the macOS path.
///
/// Arguments are passed as argv (no shell), so agent-controlled text cannot
/// inject anything. `notify-send <SUMMARY> <BODY>` takes the title as the first
/// positional argument and the body as the second.
#[cfg(target_os = "linux")]
fn post_linux(title: &str, body: &str) {
    let title = title.to_string();
    let body = body.to_string();
    std::thread::spawn(move || {
        let _ = std::process::Command::new("notify-send")
            .arg(&title)
            .arg(&body)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    });
}

/// Try to post via `terminal-notifier`. Returns `true` only if the command was
/// found and exited successfully; `false` (so the caller falls back) if it is
/// not installed or failed. Arguments are passed as argv — no escaping needed.
#[cfg(target_os = "macos")]
fn post_via_terminal_notifier(title: &str, body: &str) -> bool {
    std::process::Command::new("terminal-notifier")
        .arg("-title")
        .arg(title)
        .arg("-message")
        .arg(body)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Post via `osascript`'s `display notification`, building a safely-escaped
/// AppleScript command.
#[cfg(target_os = "macos")]
fn post_via_osascript(title: &str, body: &str) {
    let script = format!(
        "display notification {} with title {}",
        applescript_string(body),
        applescript_string(title),
    );
    let _ = std::process::Command::new("osascript")
        .arg("-e")
        .arg(script)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

/// Render `s` as a safe AppleScript double-quoted string literal: wrap in quotes,
/// escape backslashes and quotes, and flatten newlines to spaces (AppleScript
/// string literals cannot span lines). This prevents agent-controlled text from
/// breaking out of the literal and injecting AppleScript.
#[cfg(any(target_os = "macos", test))]
fn applescript_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' | '\r' => out.push(' '),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_plain_string_in_quotes() {
        assert_eq!(applescript_string("hello"), "\"hello\"");
    }

    #[test]
    fn escapes_embedded_quotes_and_backslashes() {
        assert_eq!(
            applescript_string("say \"hi\" \\ now"),
            "\"say \\\"hi\\\" \\\\ now\""
        );
    }

    #[test]
    fn flattens_newlines_to_spaces() {
        assert_eq!(applescript_string("line1\nline2\r\n"), "\"line1 line2  \"");
    }
}
