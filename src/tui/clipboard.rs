//! Best-effort system-clipboard write for copied terminal selections.
//!
//! Tries the platform clipboard command first (`pbcopy` on macOS; `clip` on
//! Windows; `wl-copy`/`xclip`/`xsel` on Linux), falling back to an OSC 52 escape
//! sequence written to the controlling terminal. The fallback works over SSH
//! and inside multiplexers that pass OSC 52 through, but is only reached when no
//! clipboard command is available, so it rarely perturbs the alternate screen.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

/// Copy `text` to the system clipboard (best effort; failures are silent).
pub fn copy(text: &str) {
    if text.is_empty() {
        return;
    }
    if try_command_clipboard(text) {
        return;
    }
    let _ = write_osc52(text);
}

/// If the system clipboard holds an image, write it to a temp file and return
/// its path; otherwise return `None`.
///
/// This is how FlightDeck delivers a pasted image to a hosted agent: the agent
/// runs in a PTY and cannot see the host clipboard's image flavour, but it does
/// understand a file-path reference (the same thing a terminal inserts when you
/// drag an image in). The caller sends the returned path to the PTY.
///
/// Best effort: any failure (no image, missing tool) yields `None` so the caller
/// can fall back to a normal paste.
pub fn save_clipboard_image() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        save_clipboard_image_macos()
    }
    #[cfg(target_os = "windows")]
    {
        save_clipboard_image_windows()
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        save_clipboard_image_linux()
    }
}

/// A unique path under the system temp dir for a pasted image, namespaced by pid
/// and a per-process counter so concurrent pastes never collide.
fn unique_image_path(ext: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    std::env::temp_dir().join(format!("flightdeck-paste-{pid}-{n}.{ext}"))
}

/// `true` if `path` now refers to a non-empty file.
fn wrote_non_empty(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|m| m.len() > 0)
        .unwrap_or(false)
}

/// macOS: read the clipboard image via `osascript`. Screenshots and most copied
/// images expose a `PNGf` flavour; we prefer it and fall back to `TIFF`.
#[cfg(target_os = "macos")]
fn save_clipboard_image_macos() -> Option<PathBuf> {
    // Inspect the available flavours first so we only spend the write attempt
    // when an image is actually present.
    let info = Command::new("osascript")
        .args(["-e", "clipboard info"])
        .output()
        .ok()?;
    let info = String::from_utf8_lossy(&info.stdout);
    let has_png = info.contains("PNGf");
    let has_tiff = info.contains("TIFF");
    if !has_png && !has_tiff {
        return None;
    }

    // Prefer PNG; coercion to PNGf succeeds for most image flavours on macOS.
    if has_png || has_tiff {
        let path = unique_image_path("png");
        if osascript_write_clipboard(&path, "«class PNGf»") && wrote_non_empty(&path) {
            return Some(path);
        }
        let _ = std::fs::remove_file(&path);
    }
    // Fall back to a raw TIFF dump when PNG coercion failed.
    if has_tiff {
        let path = unique_image_path("tiff");
        if osascript_write_clipboard(&path, "«class TIFF»") && wrote_non_empty(&path) {
            return Some(path);
        }
        let _ = std::fs::remove_file(&path);
    }
    None
}

/// Run an AppleScript that writes the clipboard, coerced to `class`, to `path`.
/// Returns whether the script exited successfully.
#[cfg(target_os = "macos")]
fn osascript_write_clipboard(path: &Path, class: &str) -> bool {
    // AppleScript string literals escape `\` and `"`; temp paths contain neither
    // in practice, but guard anyway.
    let p = path
        .to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    let script = format!(
        "set p to POSIX file \"{p}\"\n\
         set fh to open for access p with write permission\n\
         try\n\
         \tset eof fh to 0\n\
         \twrite (the clipboard as {class}) to fh\n\
         \tclose access fh\n\
         on error errMsg number errNum\n\
         \tclose access fh\n\
         \terror errMsg number errNum\n\
         end try"
    );
    Command::new("osascript")
        .arg("-e")
        .arg(script)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Windows: ask PowerShell to read an image off the clipboard and save it as a
/// PNG. `Clipboard::GetImage` returns null when no image is present, so the
/// script exits non-zero and we report `None`.
#[cfg(target_os = "windows")]
fn save_clipboard_image_windows() -> Option<PathBuf> {
    let path = unique_image_path("png");
    // PowerShell single-quoted literals only need `'` doubled; temp paths never
    // contain quotes in practice, but guard anyway.
    let p = path.to_string_lossy().replace('\'', "''");
    let script = format!(
        "Add-Type -AssemblyName System.Windows.Forms, System.Drawing; \
         $img = [System.Windows.Forms.Clipboard]::GetImage(); \
         if ($img -eq $null) {{ exit 1 }}; \
         $img.Save('{p}', [System.Drawing.Imaging.ImageFormat]::Png); exit 0"
    );
    let status = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    if matches!(status, Ok(s) if s.success()) && wrote_non_empty(&path) {
        return Some(path);
    }
    let _ = std::fs::remove_file(&path);
    None
}

/// Linux: try Wayland (`wl-paste`) then X11 (`xclip`) to pull a PNG off the
/// clipboard.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn save_clipboard_image_linux() -> Option<PathBuf> {
    use std::fs::File;

    // (command, args producing PNG bytes on stdout)
    let candidates: &[(&str, &[&str])] = &[
        ("wl-paste", &["--type", "image/png"]),
        (
            "xclip",
            &["-selection", "clipboard", "-t", "image/png", "-o"],
        ),
    ];
    for (cmd, args) in candidates {
        let path = unique_image_path("png");
        let Ok(file) = File::create(&path) else {
            continue;
        };
        let spawned = Command::new(cmd)
            .args(*args)
            .stdout(Stdio::from(file))
            .stderr(Stdio::null())
            .status();
        if matches!(spawned, Ok(s) if s.success()) && wrote_non_empty(&path) {
            return Some(path);
        }
        let _ = std::fs::remove_file(&path);
    }
    None
}

/// Pipe `text` into the first available platform clipboard command. On
/// Windows this returns PowerShell's actual success/failure so the OSC 52
/// fallback still runs when `Set-Clipboard` fails; elsewhere it returns
/// `true` once a command was spawned (regardless of whether it ultimately
/// succeeded), so the OSC 52 fallback is skipped when a native tool exists.
fn try_command_clipboard(text: &str) -> bool {
    #[cfg(target_os = "windows")]
    {
        try_windows_clipboard(text)
    }

    #[cfg(not(target_os = "windows"))]
    {
        let candidates: &[(&str, &[&str])] = if cfg!(target_os = "macos") {
            &[("pbcopy", &[])]
        } else {
            &[
                ("wl-copy", &[]),
                ("xclip", &["-selection", "clipboard"]),
                ("xsel", &["-ib"]),
            ]
        };

        for (cmd, args) in candidates {
            let spawned = Command::new(cmd)
                .args(*args)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
            if let Ok(mut child) = spawned {
                if let Some(mut stdin) = child.stdin.take() {
                    let _ = stdin.write_all(text.as_bytes());
                    // `stdin` drops here, closing the pipe so the command sees EOF.
                }
                let _ = child.wait();
                return true;
            }
        }
        false
    }
}

/// Windows: set the clipboard via PowerShell's `Set-Clipboard` instead of
/// piping into `clip`. `clip` decodes piped stdin using the process's
/// OEM/ANSI code page rather than UTF-8, so any non-ASCII text gets mangled
/// on the way in; base64-encoding the UTF-8 bytes and decoding them inside
/// PowerShell avoids that entirely (and sidesteps `-Command` quoting issues,
/// since the base64 alphabet never contains a `'`).
#[cfg(target_os = "windows")]
fn try_windows_clipboard(text: &str) -> bool {
    let b64 = base64_encode(text.as_bytes());
    let script = format!(
        "Set-Clipboard -Value ([System.Text.Encoding]::UTF8.GetString([System.Convert]::FromBase64String('{b64}')))"
    );
    Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Write an OSC 52 clipboard-set sequence to stdout.
fn write_osc52(text: &str) -> std::io::Result<()> {
    let seq = format!("\x1b]52;c;{}\x07", base64_encode(text.as_bytes()));
    let mut out = std::io::stdout();
    out.write_all(seq.as_bytes())?;
    out.flush()
}

/// Minimal standard-alphabet base64 encoder (no padding-free shortcuts, no deps).
fn base64_encode(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[((n >> 18) & 0x3f) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 0x3f) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[((n >> 6) & 0x3f) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(n & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::base64_encode;

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }
}
