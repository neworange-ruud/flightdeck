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

use crate::contracts::{Notification, NotificationSound, Notifier};

/// The distinctive two-note "ding" chime played when an agent finishes its turn
/// (SPECS §24). A custom bell so it never collides with a system sound. Embedded
/// in the binary so playback needs no external asset.
const DING_WAV: &[u8] = include_bytes!("../../assets/notify-ding.wav");

/// The input-required alert is synthesized into a WAV on first use. Its three
/// quick ascending tones are intentionally unlike the completion ding, so a
/// user can tell an agent is blocked without looking at FlightDeck.
const INPUT_SAMPLE_RATE: u32 = 22_050;
const INPUT_PIP_SAMPLES: u32 = 2_200;
const INPUT_GAP_SAMPLES: u32 = 700;
const INPUT_PIP_COUNT: u32 = 3;
const INPUT_WAV_LEN: u64 = 44
    + ((INPUT_PIP_SAMPLES * INPUT_PIP_COUNT + INPUT_GAP_SAMPLES * (INPUT_PIP_COUNT - 1)) * 2)
        as u64;

/// The production notifier. Zero-sized; cheap to construct and pass by reference.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemNotifier;

impl Notifier for SystemNotifier {
    fn notify(&self, notification: &Notification) {
        if notification.sound != NotificationSound::None {
            play_alert(notification.sound);
        }
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

/// Play an alert sound on a detached thread so the render loop never blocks.
/// Best-effort: the selected WAV is materialized to a stable temp file once,
/// then handed to the platform's audio player. Any failure — no player, no
/// audio device, write error — is silently ignored, exactly like the
/// visual-notification path.
fn play_alert(sound: NotificationSound) {
    std::thread::spawn(move || {
        let path = match sound {
            NotificationSound::None => None,
            NotificationSound::Completion => completion_file_path(),
            NotificationSound::InputRequired => input_required_file_path(),
        };
        if let Some(path) = path {
            play_wav(&path);
        }
    });
}

/// Materialize the embedded chime to `<temp>/flightdeck-notify-ding.wav` and
/// return its path. Written once and reused; rewritten if the on-disk size no
/// longer matches (e.g. after an upgrade changed the asset). Returns `None` if
/// the file cannot be written.
fn completion_file_path() -> Option<std::path::PathBuf> {
    let path = std::env::temp_dir().join("flightdeck-notify-ding.wav");
    let up_to_date = std::fs::metadata(&path)
        .map(|m| m.len() == DING_WAV.len() as u64)
        .unwrap_or(false);
    if !up_to_date && std::fs::write(&path, DING_WAV).is_err() {
        return None;
    }
    Some(path)
}

/// Materialize the synthesized input-required alert to a stable file in the
/// OS temp directory. Its expected length lets us reuse the file until an
/// updated version of FlightDeck changes the signal.
fn input_required_file_path() -> Option<std::path::PathBuf> {
    let path = std::env::temp_dir().join("flightdeck-notify-input.wav");
    let up_to_date = std::fs::metadata(&path)
        .map(|m| m.len() == INPUT_WAV_LEN)
        .unwrap_or(false);
    if !up_to_date && std::fs::write(&path, input_required_wav()).is_err() {
        return None;
    }
    Some(path)
}

/// Build a compact PCM WAV containing three ascending tones. The short gaps
/// and rising pitch make this immediately different from the completion chime.
fn input_required_wav() -> Vec<u8> {
    let sample_count =
        INPUT_PIP_SAMPLES * INPUT_PIP_COUNT + INPUT_GAP_SAMPLES * (INPUT_PIP_COUNT - 1);
    let data_len = sample_count * 2;
    let mut wav = Vec::with_capacity(44 + data_len as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes()); // PCM header size
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1u16.to_le_bytes()); // mono
    wav.extend_from_slice(&INPUT_SAMPLE_RATE.to_le_bytes());
    wav.extend_from_slice(&(INPUT_SAMPLE_RATE * 2).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes()); // block alignment
    wav.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());

    for sample_index in 0..sample_count {
        let span = INPUT_PIP_SAMPLES + INPUT_GAP_SAMPLES;
        let pip = sample_index / span;
        let within_pip = sample_index % span;
        let sample = if within_pip >= INPUT_PIP_SAMPLES {
            0
        } else {
            let frequency = [880.0_f32, 1_176.0, 1_568.0][pip as usize];
            // A tiny fade avoids a click at the start and end of each pulse.
            let fade_samples = 96;
            let envelope = if within_pip < fade_samples {
                within_pip as f32 / fade_samples as f32
            } else if within_pip + fade_samples > INPUT_PIP_SAMPLES {
                (INPUT_PIP_SAMPLES - within_pip) as f32 / fade_samples as f32
            } else {
                1.0
            };
            let phase =
                std::f32::consts::TAU * frequency * sample_index as f32 / INPUT_SAMPLE_RATE as f32;
            (phase.sin() * envelope * i16::MAX as f32 * 0.28) as i16
        };
        wav.extend_from_slice(&sample.to_le_bytes());
    }
    wav
}

/// Play a WAV file via the platform's audio CLI. macOS uses `afplay`; Linux
/// tries PulseAudio (`paplay`) then ALSA (`aplay`); Windows uses PowerShell's
/// `SoundPlayer`. Best-effort — output is discarded and errors are ignored.
#[cfg(target_os = "macos")]
fn play_wav(path: &std::path::Path) {
    let _ = std::process::Command::new("afplay")
        .arg(path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

#[cfg(target_os = "linux")]
fn play_wav(path: &std::path::Path) {
    for (player, pre_args) in [("paplay", &[][..]), ("aplay", &["-q"][..])] {
        let ok = std::process::Command::new(player)
            .args(pre_args)
            .arg(path)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            return;
        }
    }
}

#[cfg(target_os = "windows")]
fn play_wav(path: &std::path::Path) {
    // Single-quoted PowerShell literal; double any embedded quote so a path
    // under a username with a `'` cannot break out. The filename is ours.
    let literal = path.display().to_string().replace('\'', "''");
    let script = format!("(New-Object System.Media.SoundPlayer '{literal}').PlaySync();");
    let _ = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", &script])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn play_wav(path: &std::path::Path) {
    // No audio backend on this platform; drop the request.
    let _ = path;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ding_asset_is_a_nonempty_wav() {
        assert!(DING_WAV.len() > 44, "WAV must exceed its 44-byte header");
        assert_eq!(&DING_WAV[0..4], b"RIFF");
        assert_eq!(&DING_WAV[8..12], b"WAVE");
    }

    #[test]
    fn input_required_alert_is_a_distinct_valid_wav() {
        let wav = input_required_wav();
        assert_eq!(wav.len() as u64, INPUT_WAV_LEN);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_ne!(wav.as_slice(), DING_WAV, "alerts must not share a sound");
    }

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
