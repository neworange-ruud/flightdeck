//! A cleaned, phone-friendly transcript reconstructed from the agent's own
//! **session file** (its structured JSONL conversation log), tailed as it grows.
//!
//! ## Why not the PTY
//!
//! FlightDeck's primary agents (Claude Code, Codex, OpenCode) are full-screen
//! TUIs: they paint the alternate screen with absolute cursor positioning and
//! almost never emit `\n`-terminated lines. Reconstructing prose from the raw
//! PTY byte stream therefore yielded *nothing* for these agents — the mobile
//! chat stayed empty even though the agent was replying (remote-control-72k).
//!
//! Instead we read the agent's on-disk session transcript — the same JSONL
//! FlightDeck already locates for resume ([`crate::agents::resume`]) — which is
//! the authoritative, structured conversation: user prompts, assistant prose,
//! and tool calls, each with a stable id. We tail it (byte-offset cursor) and
//! translate each new record into a [`TranscriptItem`]:
//!
//! * a user text message → [`TranscriptItem::UserMessage`];
//! * an assistant text block → [`TranscriptItem::AgentMessage`];
//! * an assistant `tool_use` block → a collapsed [`TranscriptItem::Activity`]
//!   pill (summarised by tool);
//! * tool results, sidechain (subagent) turns, and meta records are skipped.
//!
//! Permission prompts are **not** in the session file; the bridge calls
//! [`TranscriptBuilder::on_needs_input`] when the status hook reports the agent
//! stopped for input, and we synthesize an inline
//! [`TranscriptItem::PermissionPrompt`] whose preview is the agent's last prose.
//!
//! v1 understands **Claude Code's** schema; [`crate::agents::resume`] returns no
//! session path for other agents yet, so those simply show no reconstructed
//! transcript (a follow-up, not a wrong transcript).
//!
//! Memory is capped to [`MAX_ITEMS`] most-recent items (a ring); pagination
//! honours the protocol's `from_index`.

use std::collections::{HashSet, VecDeque};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::Value;

use flightdeck_remote_protocol::{
    ActivityKind, PermissionChoice, PermissionOption, TranscriptFeed, TranscriptItem,
};
use flightdeck_remote_protocol::{ItemId, PromptId, SessionId};

/// Most-recent transcript items retained per session (ring buffer bound).
pub const MAX_ITEMS: usize = 500;
/// Max characters in a needs-input preview.
const PREVIEW_CAP: usize = 280;

/// Reconstructs one session's cleaned transcript by tailing its agent session
/// JSONL and translating each record into a [`TranscriptItem`].
pub struct TranscriptBuilder {
    session_id: SessionId,
    /// The session file currently being tailed (set on first successful sync).
    source: Option<PathBuf>,
    /// Byte offset up to which `source` has been consumed (only whole lines).
    read_offset: u64,
    /// Record uuids already ingested, so a re-read never double-appends.
    seen: HashSet<String>,
    /// The most recent assistant prose, used as the needs-input preview.
    last_agent_text: Option<String>,
    items: VecDeque<TranscriptItem>,
    /// Global ordinal of `items.front()` (items before it were evicted).
    base_index: u64,
    /// Total items ever produced (= base_index + items.len()).
    total: u64,
    /// Highest `total` already handed to [`Self::take_appended`].
    sent_upto: u64,
    /// Monotonic prompt counter for stable PromptIds.
    prompt_seq: u64,
}

impl TranscriptBuilder {
    /// A fresh builder for `session_id`.
    pub fn new(session_id: SessionId) -> Self {
        TranscriptBuilder {
            session_id,
            source: None,
            read_offset: 0,
            seen: HashSet::new(),
            last_agent_text: None,
            items: VecDeque::new(),
            base_index: 0,
            total: 0,
            sent_upto: 0,
            prompt_seq: 0,
        }
    }

    /// Tail the agent session file at `path`, appending any newly-written
    /// records as transcript items. Cheap and safe to call every tick: it only
    /// reads bytes past the last consumed offset, and only consumes whole
    /// (newline-terminated) lines so a half-written record is never parsed.
    /// `now_ms` stamps items whose record carries no parseable timestamp.
    pub fn sync_jsonl(&mut self, path: &Path, now_ms: i64) {
        // A different path means a different session (a resume reuses the same
        // file) — start the transcript over so `load` replaces cleanly.
        if self.source.as_deref() != Some(path) {
            self.source = Some(path.to_path_buf());
            self.reset();
        }

        let Ok(mut f) = std::fs::File::open(path) else {
            return;
        };
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        if len < self.read_offset {
            // Truncated / rotated in place — re-read from the top.
            self.reset();
        }
        if len == self.read_offset {
            return;
        }
        if f.seek(SeekFrom::Start(self.read_offset)).is_err() {
            return;
        }
        let mut buf = String::new();
        if f.read_to_string(&mut buf).is_err() {
            return; // non-UTF-8 tail (shouldn't happen for JSONL); retry next tick
        }
        // Consume only up to the last complete line; keep any partial tail.
        let Some(last_nl) = buf.rfind('\n') else {
            return;
        };
        let complete = &buf[..=last_nl];
        for line in complete.lines() {
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(v) = serde_json::from_str::<Value>(line) {
                self.ingest(&v, now_ms);
            }
        }
        self.read_offset += complete.len() as u64;
    }

    /// Clear all accumulated transcript state (on a session-file switch/rotate).
    fn reset(&mut self) {
        self.read_offset = 0;
        self.seen.clear();
        self.last_agent_text = None;
        self.items.clear();
        self.base_index = 0;
        self.total = 0;
        self.sent_upto = 0;
        // `prompt_seq` intentionally keeps advancing so PromptIds stay unique.
    }

    /// Translate one session-file record into 0+ transcript items.
    fn ingest(&mut self, o: &Value, now_ms: i64) {
        let uuid = o
            .get("uuid")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        // Dedup by record uuid (records without one fall back to append order).
        if !uuid.is_empty() && !self.seen.insert(uuid.clone()) {
            return;
        }
        // Subagent (sidechain) turns are not part of the main conversation.
        if o.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            return;
        }
        let at_ms = o
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(parse_iso8601_ms)
            .unwrap_or(now_ms);

        match o.get("type").and_then(Value::as_str) {
            Some("user") => {
                // System-injected meta records are not user prose.
                if o.get("isMeta").and_then(Value::as_bool) == Some(true) {
                    return;
                }
                let content = o.get("message").and_then(|m| m.get("content"));
                if let Some(text) = extract_user_text(content) {
                    let text = text.trim().to_string();
                    if !text.is_empty() {
                        let item_id = self.item_id(&uuid, 0);
                        self.push_item(TranscriptItem::UserMessage {
                            item_id,
                            text,
                            at_ms,
                        });
                    }
                }
            }
            Some("assistant") => {
                let Some(blocks) = o
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(Value::as_array)
                else {
                    return;
                };
                for (i, b) in blocks.iter().enumerate() {
                    match b.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            let text = b
                                .get("text")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .trim()
                                .to_string();
                            if !text.is_empty() {
                                self.last_agent_text = Some(text.clone());
                                let item_id = self.item_id(&uuid, i);
                                self.push_item(TranscriptItem::AgentMessage {
                                    item_id,
                                    text,
                                    at_ms,
                                });
                            }
                        }
                        Some("tool_use") => {
                            let name = b.get("name").and_then(Value::as_str).unwrap_or("tool");
                            let empty = Value::Null;
                            let input = b.get("input").unwrap_or(&empty);
                            let (summary, detail, body, kind) = tool_activity(name, input);
                            let item_id = self.item_id(&uuid, i);
                            self.push_item(TranscriptItem::Activity {
                                item_id,
                                summary,
                                detail,
                                body,
                                kind,
                                at_ms,
                            });
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    /// The bridge calls this on the working/idle → needs-input edge: synthesize
    /// an inline permission prompt whose preview is the agent's last prose (the
    /// session file has no permission records). Returns the preview (if any).
    pub fn on_needs_input(&mut self, at_ms: i64) -> Option<String> {
        let preview = self.build_preview();
        self.prompt_seq += 1;
        let prompt_id = PromptId::new(format!("{}:p{}", self.session_id, self.prompt_seq));
        let item_id = ItemId::new(format!("{}:prompt:{}", self.session_id, self.prompt_seq));
        self.push_item(TranscriptItem::PermissionPrompt {
            item_id,
            prompt_id,
            command: preview.clone().unwrap_or_default(),
            options: vec![
                PermissionOption {
                    choice: PermissionChoice::AllowOnce,
                    label: "Allow once".to_string(),
                },
                PermissionOption {
                    choice: PermissionChoice::Deny,
                    label: "Deny".to_string(),
                },
            ],
            at_ms,
        });
        preview
    }

    /// The agent's last prose, truncated, as the needs-input preview.
    fn build_preview(&self) -> Option<String> {
        let mut text = self.last_agent_text.as_ref()?.trim().to_string();
        if text.is_empty() {
            return None;
        }
        if text.chars().count() > PREVIEW_CAP {
            text = text.chars().take(PREVIEW_CAP).collect();
            text.push('…');
        }
        Some(text)
    }

    /// A stable item id from the record uuid + block index, falling back to the
    /// running ordinal when a record has no uuid.
    fn item_id(&self, uuid: &str, block: usize) -> ItemId {
        if uuid.is_empty() {
            ItemId::new(format!("{}:{}", self.session_id, self.total))
        } else {
            ItemId::new(format!("{uuid}:{block}"))
        }
    }

    /// Append an item to the ring, evicting the oldest past [`MAX_ITEMS`].
    fn push_item(&mut self, item: TranscriptItem) {
        self.items.push_back(item);
        self.total += 1;
        while self.items.len() > MAX_ITEMS {
            self.items.pop_front();
            self.base_index += 1;
        }
    }

    /// Items appended since the last call (for `TranscriptAppend`). The
    /// `from_index` of the returned feed is the global ordinal of the first
    /// item; `replace` is false (append semantics).
    pub fn take_appended(&mut self) -> Option<TranscriptFeed> {
        if self.total <= self.sent_upto {
            return None;
        }
        let start = self.sent_upto.max(self.base_index);
        let skip = (start - self.base_index) as usize;
        let items: Vec<TranscriptItem> = self.items.iter().skip(skip).cloned().collect();
        self.sent_upto = self.total;
        if items.is_empty() {
            return None;
        }
        Some(TranscriptFeed {
            session_id: self.session_id.clone(),
            from_index: start,
            replace: false,
            items,
        })
    }

    /// A full (or from-cursor) transcript load for `RequestTranscript`. When
    /// `from_index` is beyond what is retained, the retained window is returned
    /// with `replace = true` from its true base.
    pub fn load(&self, from_index: Option<u64>) -> TranscriptFeed {
        let requested = from_index.unwrap_or(0);
        let start = requested.max(self.base_index);
        let skip = start.saturating_sub(self.base_index) as usize;
        let items: Vec<TranscriptItem> = self.items.iter().skip(skip).cloned().collect();
        TranscriptFeed {
            session_id: self.session_id.clone(),
            from_index: start,
            replace: true,
            items,
        }
    }

    /// Total items ever produced (for tests / cursor math).
    pub fn total(&self) -> u64 {
        self.total
    }

    /// The id of the most recently minted permission prompt, if any — the
    /// pending one while the session is waiting for input. The command bridge
    /// checks a phone `permission_decision` against this so a stale decision
    /// (already answered on the desktop, or superseded by a newer prompt) is
    /// rejected instead of injecting a keystroke into the wrong prompt.
    pub fn last_prompt_id(&self) -> Option<PromptId> {
        (self.prompt_seq > 0)
            .then(|| PromptId::new(format!("{}:p{}", self.session_id, self.prompt_seq)))
    }
}

// ---------------------------------------------------------------------------
// Record → item helpers
// ---------------------------------------------------------------------------

/// Extract displayable prose from a `user` record's `content`: a bare string is
/// the prompt; an array yields its `text` blocks joined. A tool-result-only
/// array (no text) yields `None` — tool output is not shown as user prose.
fn extract_user_text(content: Option<&Value>) -> Option<String> {
    match content {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Array(arr)) => {
            let parts: Vec<&str> = arr
                .iter()
                .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect();
            (!parts.is_empty()).then(|| parts.join("\n"))
        }
        _ => None,
    }
}

/// Summarise an assistant `tool_use` block into `(summary, detail, body, kind)`
/// for a collapsed activity pill.
fn tool_activity(
    name: &str,
    input: &Value,
) -> (String, Option<String>, Option<String>, ActivityKind) {
    let s = |k: &str| input.get(k).and_then(Value::as_str);
    match name {
        "Bash" => {
            let cmd = s("command").unwrap_or("");
            let summary = s("description")
                .map(str::to_string)
                .unwrap_or_else(|| format!("Ran {}", first_line(cmd)));
            let body = (!cmd.is_empty()).then(|| cmd.to_string());
            (summary, None, body, ActivityKind::Command)
        }
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => {
            let f = s("file_path").or_else(|| s("notebook_path")).unwrap_or("");
            (
                format!("Edited {}", basename(f)),
                None,
                None,
                ActivityKind::Edit,
            )
        }
        "Read" => (
            format!("Read {}", basename(s("file_path").unwrap_or(""))),
            None,
            None,
            ActivityKind::Search,
        ),
        "Grep" | "Glob" => (
            format!(
                "Searched {}",
                s("pattern").or_else(|| s("query")).unwrap_or("")
            ),
            None,
            None,
            ActivityKind::Search,
        ),
        "Task" => (
            format!("Task: {}", s("description").unwrap_or("subtask")),
            None,
            None,
            ActivityKind::Other,
        ),
        "TodoWrite" => (
            "Updated the task list".to_string(),
            None,
            None,
            ActivityKind::Other,
        ),
        "WebFetch" | "WebSearch" => (
            format!("{name} {}", s("url").or_else(|| s("query")).unwrap_or("")),
            None,
            None,
            ActivityKind::Search,
        ),
        other => (other.to_string(), None, None, ActivityKind::Other),
    }
}

/// The first non-empty line of `s`, trimmed (for a one-line command summary).
fn first_line(s: &str) -> &str {
    s.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
}

/// The final path component of `p` (its file name), or `p` itself.
fn basename(p: &str) -> &str {
    p.rsplit(['/', '\\'])
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(p)
}

/// Parse an ISO-8601 UTC timestamp (`YYYY-MM-DDTHH:MM:SS[.fff]Z`) to unix ms.
/// Ignores any timezone offset (the agents write UTC `Z`); returns `None` on a
/// shape it does not recognise so the caller can fall back to wall-clock.
fn parse_iso8601_ms(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 19
        || b[4] != b'-'
        || b[7] != b'-'
        || (b[10] != b'T' && b[10] != b' ')
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let num = |r: std::ops::Range<usize>| s.get(r).and_then(|x| x.parse::<i64>().ok());
    let (year, month, day) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (hour, min, sec) = (num(11..13)?, num(14..16)?, num(17..19)?);
    let mut ms = 0i64;
    if b.get(19) == Some(&b'.') {
        let frac: String = s[20..]
            .chars()
            .take_while(char::is_ascii_digit)
            .take(3)
            .collect();
        if !frac.is_empty() {
            ms = format!("{frac:0<3}").parse().unwrap_or(0);
        }
    }
    let days = days_from_civil(year, month, day);
    Some((((days * 24 + hour) * 60 + min) * 60 + sec) * 1000 + ms)
}

/// Days since the unix epoch for a proleptic-Gregorian y/m/d (Hinnant's
/// `days_from_civil`).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

#[cfg(test)]
mod tests;
