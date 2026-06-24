//! Best-effort system-clipboard write for copied terminal selections.
//!
//! Tries the platform clipboard command first (`pbcopy` on macOS;
//! `wl-copy`/`xclip`/`xsel` on Linux), falling back to an OSC 52 escape
//! sequence written to the controlling terminal. The fallback works over SSH
//! and inside multiplexers that pass OSC 52 through, but is only reached when no
//! clipboard command is available, so it rarely perturbs the alternate screen.

use std::io::Write;
use std::process::{Command, Stdio};

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

/// Pipe `text` into the first available platform clipboard command. Returns
/// `true` once a command was spawned (regardless of whether it ultimately
/// succeeded), so the OSC 52 fallback is skipped when a native tool exists.
fn try_command_clipboard(text: &str) -> bool {
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

/// Write an OSC 52 clipboard-set sequence to stdout.
fn write_osc52(text: &str) -> std::io::Result<()> {
    let seq = format!("\x1b]52;c;{}\x07", base64_encode(text.as_bytes()));
    let mut out = std::io::stdout();
    out.write_all(seq.as_bytes())?;
    out.flush()
}

/// Minimal standard-alphabet base64 encoder (no padding-free shortcuts, no deps).
fn base64_encode(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
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
