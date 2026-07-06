//! Parse an agent's on-exit "resume" hint into the args needed to relaunch it.
//!
//! Agents print a resume command when they quit, e.g.:
//!   claude: "Resume this session with:  claude --resume <uuid>"
//!   codex:  "To continue this session, run codex resume <uuid>"
//!
//! We hardcode the known formats (claude, codex), extract the session UUID, and
//! return the full arg vector to relaunch with. Unknown agents yield `None`.

/// Replay args for `agent_key` if `text` contains its resume hint, else `None`.
/// `text` should be plain (ANSI already stripped by the caller).
pub fn parse_resume_args(agent_key: &str, text: &str) -> Option<Vec<String>> {
    match agent_key {
        "claude" => uuid_after(text, "claude --resume").map(|id| vec!["--resume".to_string(), id]),
        "codex" => uuid_after(text, "codex resume").map(|id| vec!["resume".to_string(), id]),
        _ => None,
    }
}

/// Remove ANSI escape sequences (CSI `ESC[…`, OSC `ESC]…BEL/ST`, and lone ESC)
/// so plain-text matching works on styled terminal output.
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            // CSI: consume params/intermediates until a final byte 0x40..=0x7e.
            Some('[') => {
                chars.next();
                for pc in chars.by_ref() {
                    if ('\x40'..='\x7e').contains(&pc) {
                        break;
                    }
                }
            }
            // OSC: consume until BEL or ST (`ESC \`).
            Some(']') => {
                chars.next();
                while let Some(pc) = chars.next() {
                    if pc == '\x07' {
                        break;
                    }
                    if pc == '\x1b' {
                        if chars.peek() == Some(&'\\') {
                            chars.next();
                        }
                        break;
                    }
                }
            }
            // Lone ESC (or ESC + other): drop the ESC only.
            _ => {}
        }
    }
    out
}

/// Find `needle` in `text` and return the following UUID token, if valid.
fn uuid_after(text: &str, needle: &str) -> Option<String> {
    let start = text.find(needle)? + needle.len();
    let rest = text[start..].trim_start();
    let token: String = rest
        .chars()
        .take_while(|c| c.is_ascii_hexdigit() || *c == '-')
        .collect();
    is_uuid(&token).then_some(token)
}

/// Whether `s` has the canonical 8-4-4-4-12 hex UUID shape.
fn is_uuid(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    let lens = [8usize, 4, 4, 4, 12];
    parts.len() == 5
        && parts
            .iter()
            .zip(lens)
            .all(|(p, n)| p.len() == n && p.chars().all(|c| c.is_ascii_hexdigit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_claude_resume_line() {
        let text =
            "Resume this session with:\n  claude --resume 3d74d44d-e9e7-407f-9938-c59ef4045e3f\n";
        assert_eq!(
            parse_resume_args("claude", text),
            Some(vec![
                "--resume".to_string(),
                "3d74d44d-e9e7-407f-9938-c59ef4045e3f".to_string()
            ])
        );
    }

    #[test]
    fn parses_codex_resume_line() {
        let text =
            "To continue this session, run codex resume 019f378e-76e9-7de3-a1db-41a027b7b719";
        assert_eq!(
            parse_resume_args("codex", text),
            Some(vec![
                "resume".to_string(),
                "019f378e-76e9-7de3-a1db-41a027b7b719".to_string()
            ])
        );
    }

    #[test]
    fn uuid_followed_by_trailing_text_is_still_captured() {
        let text = "claude --resume 3d74d44d-e9e7-407f-9938-c59ef4045e3f. Bye.";
        assert_eq!(
            parse_resume_args("claude", text),
            Some(vec![
                "--resume".to_string(),
                "3d74d44d-e9e7-407f-9938-c59ef4045e3f".to_string()
            ])
        );
    }

    #[test]
    fn no_hint_yields_none() {
        assert_eq!(parse_resume_args("claude", "just some normal output"), None);
        assert_eq!(parse_resume_args("codex", "nothing to resume here"), None);
    }

    #[test]
    fn unknown_agent_yields_none() {
        let text = "claude --resume 3d74d44d-e9e7-407f-9938-c59ef4045e3f";
        assert_eq!(parse_resume_args("opencode", text), None);
    }

    #[test]
    fn rejects_malformed_uuid() {
        let text = "claude --resume not-a-uuid";
        assert_eq!(parse_resume_args("claude", text), None);
    }

    #[test]
    fn strip_ansi_removes_csi_and_osc_sequences() {
        assert_eq!(strip_ansi("\x1b[1mhello\x1b[0m"), "hello");
        assert_eq!(strip_ansi("\x1b]0;window title\x07body"), "body");
    }

    #[test]
    fn parses_resume_from_ansi_styled_output() {
        let styled = "\x1b[2m  claude --resume 3d74d44d-e9e7-407f-9938-c59ef4045e3f\x1b[0m";
        assert_eq!(
            parse_resume_args("claude", &strip_ansi(styled)),
            Some(vec![
                "--resume".to_string(),
                "3d74d44d-e9e7-407f-9938-c59ef4045e3f".to_string()
            ])
        );
    }
}
