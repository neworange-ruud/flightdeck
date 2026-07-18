//! Resolve the args needed to relaunch an agent so it continues its session.
//!
//! The robust source is each agent's own on-disk session store (it exists no
//! matter how the agent exited — clean `/exit`, killed on shutdown, terminal
//! closed), keyed by the worktree directory:
//!   claude: `~/.claude/projects/<cwd with '/' and '.' → '-'>/<uuid>.jsonl`
//!   codex:  `~/.codex/sessions/**/rollout-*.jsonl`, each starting with a
//!           `session_meta` line carrying `payload.session_id` + `payload.cwd`.
//!
//! To support multiple agents sharing one worktree, the caller snapshots the
//! store's session ids at launch and later pins the id that newly appeared as
//! that tab's own session (see [`newest_new_session`]).
//!
//! [`parse_resume_args`] additionally parses the resume command an agent prints
//! on a clean exit, kept as a best-effort fallback.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// The on-disk record schema of an agent's session file. The remote transcript
/// builder parses each format's records differently (see
/// [`crate::remote::transcript`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionFormat {
    /// Claude Code: one record per line, `type=user|assistant`, `message.content`
    /// text / `tool_use` blocks, with `isMeta` / `isSidechain` filters.
    Claude,
    /// Codex: `session_meta` first, then `event_msg` (user/agent prose) and
    /// `response_item` (`function_call` tool activity) records.
    Codex,
}

/// Session ids in `agent_key`'s on-disk store for `cwd` (store rooted at `home`),
/// each with its file mtime. Empty for unknown agents or a missing store.
pub fn store_session_ids(agent_key: &str, cwd: &Path, home: &Path) -> Vec<(String, SystemTime)> {
    match agent_key {
        "claude" => claude_session_ids(cwd, home),
        "codex" => codex_session_ids(cwd, home),
        _ => Vec::new(),
    }
}

/// Absolute path to the **newest** session transcript file for `agent_key`
/// running in `cwd` (store rooted at `home`) together with its record
/// [`SessionFormat`], or `None` if none exists. This is the structured JSONL the
/// remote transcript is reconstructed from — the agent CLIs paint their UI on
/// the alt-screen, so scraping the PTY yields nothing; the session file is the
/// authoritative conversation (remote-control-72k).
///
/// Understands **Claude Code** and **Codex**, both of which append a JSONL
/// file. OpenCode keeps its conversation in a live SQLite DB (no tailable file),
/// so it returns `None` here and is resolved separately by
/// [`crate::remote::transcript::resolve_source`].
pub fn newest_session_path(
    agent_key: &str,
    cwd: &Path,
    home: &Path,
) -> Option<(PathBuf, SessionFormat)> {
    match agent_key {
        "claude" => {
            let mut ids = claude_session_ids(cwd, home);
            ids.sort_by_key(|(_, mtime)| *mtime);
            let (id, _) = ids.pop()?;
            let path = claude_project_dir(cwd, home).join(format!("{id}.jsonl"));
            Some((path, SessionFormat::Claude))
        }
        "codex" => {
            let mut files = codex_session_files(cwd, home);
            files.sort_by_key(|(_, mtime)| *mtime);
            let (path, _) = files.pop()?;
            Some((path, SessionFormat::Codex))
        }
        _ => None,
    }
}

/// The args to relaunch `agent_key` continuing session `id`, or `None` if the
/// agent has no known resume interface.
pub fn resume_args_for(agent_key: &str, id: &str) -> Option<Vec<String>> {
    match agent_key {
        "claude" => Some(vec!["--resume".to_string(), id.to_string()]),
        "codex" => Some(vec!["resume".to_string(), id.to_string()]),
        _ => None,
    }
}

/// The resume args to launch `agent_key` with so it continues a session: the
/// already-pinned `pinned` args if present (this tab's own session, resolved via
/// snapshot/pin — authoritative for multiple agents in one worktree), otherwise
/// the newest session in the store for `cwd` (fallback for the single-agent and
/// recovered cases). Empty = start a fresh session.
pub fn resolve_resume_args(
    agent_key: &str,
    cwd: &Path,
    home: &Path,
    pinned: &[String],
) -> Vec<String> {
    if !pinned.is_empty() {
        return pinned.to_vec();
    }
    let mut sessions = store_session_ids(agent_key, cwd, home);
    sessions.sort_by_key(|(_, t)| *t);
    match sessions.last() {
        Some((id, _)) => resume_args_for(agent_key, id).unwrap_or_default(),
        None => Vec::new(),
    }
}

/// The newest session id in `current` that is not in `snapshot` — i.e. the
/// session an agent created since launch. `None` until a new one appears.
pub fn newest_new_session(
    snapshot: &HashSet<String>,
    current: &[(String, SystemTime)],
) -> Option<String> {
    current
        .iter()
        .filter(|(id, _)| !snapshot.contains(id))
        .max_by_key(|(_, t)| *t)
        .map(|(id, _)| id.clone())
}

/// Claude's project directory for `cwd`: the absolute path with every `/` and
/// `.` replaced by `-`, under `<home>/.claude/projects/`.
fn claude_project_dir(cwd: &Path, home: &Path) -> PathBuf {
    let mangled: String = cwd
        .to_string_lossy()
        .chars()
        .map(|c| if c == '/' || c == '.' { '-' } else { c })
        .collect();
    home.join(".claude").join("projects").join(mangled)
}

fn claude_session_ids(cwd: &Path, home: &Path) -> Vec<(String, SystemTime)> {
    let dir = claude_project_dir(cwd, home);
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return out;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if !is_uuid(stem) {
            continue;
        }
        let Ok(mtime) = entry.metadata().and_then(|m| m.modified()) else {
            continue;
        };
        out.push((stem.to_string(), mtime));
    }
    out
}

fn codex_session_ids(cwd: &Path, home: &Path) -> Vec<(String, SystemTime)> {
    codex_session_files(cwd, home)
        .into_iter()
        .filter_map(|(path, mtime)| codex_session_meta(&path).map(|(id, _)| (id, mtime)))
        .collect()
}

/// Every Codex rollout file whose `session_meta.cwd` matches `cwd`, with its
/// mtime. Codex nests rollouts under `~/.codex/sessions/<Y>/<M>/<D>/` and stamps
/// the worktree in the leading `session_meta` line, so we walk the tree and read
/// each file's first line to match (mirrors [`codex_session_meta`]).
fn codex_session_files(cwd: &Path, home: &Path) -> Vec<(PathBuf, SystemTime)> {
    let root = home.join(".codex").join("sessions");
    let target = cwd.to_string_lossy();
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            if let Some((_, session_cwd)) = codex_session_meta(&path) {
                if session_cwd == target {
                    if let Ok(mtime) = meta.modified() {
                        out.push((path, mtime));
                    }
                }
            }
        }
    }
    out
}

/// Read a codex session file's leading `session_meta` line, returning
/// `(session_id, cwd)` if present.
fn codex_session_meta(path: &Path) -> Option<(String, String)> {
    let content = std::fs::read_to_string(path).ok()?;
    let first = content.lines().next()?;
    let value: serde_json::Value = serde_json::from_str(first).ok()?;
    let payload = value.get("payload")?;
    let id = payload.get("session_id")?.as_str()?.to_string();
    let cwd = payload.get("cwd")?.as_str()?.to_string();
    Some((id, cwd))
}

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

    fn touch(path: &std::path::Path, contents: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    #[test]
    fn claude_store_lists_session_ids_for_cwd() {
        let home = tempfile::tempdir().unwrap();
        let cwd = std::path::Path::new("/home/u/Repos/proj/.flightdeck/worktrees/feat");
        // Mangled dir: '/' and '.' → '-'.
        let dir = home
            .path()
            .join(".claude/projects/-home-u-Repos-proj--flightdeck-worktrees-feat");
        touch(
            &dir.join("3d74d44d-e9e7-407f-9938-c59ef4045e3f.jsonl"),
            "{}\n",
        );
        touch(&dir.join("not-a-uuid.jsonl"), "{}\n"); // ignored
        touch(&dir.join("readme.txt"), "x"); // ignored

        let ids: Vec<String> = store_session_ids("claude", cwd, home.path())
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        assert_eq!(
            ids,
            vec!["3d74d44d-e9e7-407f-9938-c59ef4045e3f".to_string()]
        );
    }

    #[test]
    fn codex_store_matches_session_by_cwd() {
        let home = tempfile::tempdir().unwrap();
        let cwd = std::path::Path::new("/home/u/Repos/proj/wt");
        let sessions = home.path().join(".codex/sessions/2026/07/06");
        touch(
            &sessions.join("rollout-a-019f378e-76e9-7de3-a1db-41a027b7b719.jsonl"),
            "{\"type\":\"session_meta\",\"payload\":{\"session_id\":\"019f378e-76e9-7de3-a1db-41a027b7b719\",\"cwd\":\"/home/u/Repos/proj/wt\"}}\n",
        );
        // Different cwd → must be excluded.
        touch(
            &sessions.join("rollout-b-11111111-1111-1111-1111-111111111111.jsonl"),
            "{\"type\":\"session_meta\",\"payload\":{\"session_id\":\"11111111-1111-1111-1111-111111111111\",\"cwd\":\"/somewhere/else\"}}\n",
        );

        let ids: Vec<String> = store_session_ids("codex", cwd, home.path())
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        assert_eq!(
            ids,
            vec!["019f378e-76e9-7de3-a1db-41a027b7b719".to_string()]
        );
    }

    #[test]
    fn newest_session_path_locates_claude_and_codex() {
        let home = tempfile::tempdir().unwrap();
        let cwd = std::path::Path::new("/home/u/wt");

        // Claude: newest .jsonl under the mangled project dir.
        let claude_dir = home.path().join(".claude/projects/-home-u-wt");
        touch(
            &claude_dir.join("11111111-1111-1111-1111-111111111111.jsonl"),
            "{}\n",
        );
        let (path, fmt) = newest_session_path("claude", cwd, home.path()).unwrap();
        assert_eq!(fmt, SessionFormat::Claude);
        assert!(path.ends_with("11111111-1111-1111-1111-111111111111.jsonl"));

        // Codex: the rollout file whose session_meta.cwd matches, newest wins.
        let sessions = home.path().join(".codex/sessions/2026/07/18");
        touch(
            &sessions.join("rollout-old-019f378e-76e9-7de3-a1db-41a027b7b719.jsonl"),
            "{\"type\":\"session_meta\",\"payload\":{\"session_id\":\"019f378e-76e9-7de3-a1db-41a027b7b719\",\"cwd\":\"/home/u/wt\"}}\n",
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
        touch(
            &sessions.join("rollout-new-019f4ce2-7973-73c1-add9-840090982b86.jsonl"),
            "{\"type\":\"session_meta\",\"payload\":{\"session_id\":\"019f4ce2-7973-73c1-add9-840090982b86\",\"cwd\":\"/home/u/wt\"}}\n",
        );
        let (path, fmt) = newest_session_path("codex", cwd, home.path()).unwrap();
        assert_eq!(fmt, SessionFormat::Codex);
        assert!(
            path.ends_with("rollout-new-019f4ce2-7973-73c1-add9-840090982b86.jsonl"),
            "newest rollout by mtime, got {path:?}"
        );

        // OpenCode (and unknown agents) have no tailable file → None.
        assert!(newest_session_path("opencode", cwd, home.path()).is_none());
        assert!(newest_session_path("codex", std::path::Path::new("/nope"), home.path()).is_none());
    }

    #[test]
    fn newest_new_session_picks_id_absent_from_snapshot() {
        let t0 = SystemTime::UNIX_EPOCH;
        let t1 = t0 + std::time::Duration::from_secs(10);
        let t2 = t0 + std::time::Duration::from_secs(20);
        let snapshot: HashSet<String> = ["old".to_string()].into_iter().collect();
        let current = vec![
            ("old".to_string(), t0),
            ("newer".to_string(), t1),
            ("newest".to_string(), t2),
        ];
        assert_eq!(
            newest_new_session(&snapshot, &current),
            Some("newest".to_string())
        );
        // Nothing new yet → None.
        let all_known: HashSet<String> = ["old".into(), "newer".into(), "newest".into()]
            .into_iter()
            .collect();
        assert_eq!(newest_new_session(&all_known, &current), None);
    }

    #[test]
    fn resolve_prefers_pinned_over_store_fallback() {
        let home = tempfile::tempdir().unwrap();
        let cwd = std::path::Path::new("/x/y");
        // Pinned wins even if the store has a different newest session.
        let pinned = vec!["--resume".to_string(), "pinned-id".to_string()];
        assert_eq!(
            resolve_resume_args("claude", cwd, home.path(), &pinned),
            pinned
        );
    }

    #[test]
    fn resolve_falls_back_to_newest_store_session_when_unpinned() {
        let home = tempfile::tempdir().unwrap();
        let cwd = std::path::Path::new("/home/u/wt");
        let dir = home.path().join(".claude/projects/-home-u-wt");
        touch(
            &dir.join("11111111-1111-1111-1111-111111111111.jsonl"),
            "{}\n",
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
        touch(
            &dir.join("22222222-2222-2222-2222-222222222222.jsonl"),
            "{}\n",
        ); // newer
        assert_eq!(
            resolve_resume_args("claude", cwd, home.path(), &[]),
            vec![
                "--resume".to_string(),
                "22222222-2222-2222-2222-222222222222".to_string()
            ]
        );
    }

    #[test]
    fn resolve_is_empty_when_no_session_and_unpinned() {
        let home = tempfile::tempdir().unwrap();
        let cwd = std::path::Path::new("/no/sessions/here");
        assert!(resolve_resume_args("claude", cwd, home.path(), &[]).is_empty());
    }

    #[test]
    fn resume_args_for_known_agents() {
        assert_eq!(
            resume_args_for("claude", "abc"),
            Some(vec!["--resume".to_string(), "abc".to_string()])
        );
        assert_eq!(
            resume_args_for("codex", "abc"),
            Some(vec!["resume".to_string(), "abc".to_string()])
        );
        assert_eq!(resume_args_for("opencode", "abc"), None);
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
