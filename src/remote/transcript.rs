//! A cleaned, phone-friendly transcript reconstructed from an agent terminal's
//! raw PTY byte stream.
//!
//! FlightDeck has no message model — the primary agent is just a terminal
//! painting a VT100 screen. To give the phone the PRD §5.3 "cleaned transcript"
//! (prose stays, noisy tool output collapses into activity pills, permission
//! asks inline) we tee the primary terminal's PTY bytes into a per-session
//! [`TranscriptBuilder`] and reconstruct a line-oriented view heuristically.
//!
//! ## Approach (and why)
//!
//! We **strip ANSI/control sequences and reassemble completed lines**, rather
//! than diffing a vt100 screen. The interactive agent CLIs (Claude Code, Codex,
//! OpenCode) repaint via cursor movement and full-screen redraws; diffing the
//! rendered grid would produce a churn of near-duplicate frames. A line stream
//! with a **sliding-window dedupe** absorbs redraws far more cheaply and keeps
//! the reconstruction honest: we never invent structure the bytes do not show.
//!
//! Completed lines are then classified:
//! * blank lines break prose paragraphs;
//! * lines with a strong tool marker (Claude's `⏺`/`⎿`, a bullet, `Ran `,
//!   `Edited `, a shell `$ ` prompt, a diff stat, …) open/extend an **activity
//!   pill** whose noisy body collapses under a one-line summary;
//! * everything else is **prose**, chunked into agent-message blocks.
//!
//! Weakly-matching lines are deliberately treated as prose — the design brief's
//! "useful and honest": a mis-summarised pill is worse than a plain line.
//!
//! Permission prompts are **not** parsed out of the stream (too unreliable);
//! the bridge calls [`TranscriptBuilder::on_needs_input`] when the status hook
//! reports the agent stopped for input, and we capture the tail lines as the
//! preview and emit an inline [`TranscriptItem::PermissionPrompt`].
//!
//! Memory is capped to [`MAX_ITEMS`] most-recent items (a ring); pagination
//! honours the protocol's `from_index`.

use std::collections::VecDeque;

use flightdeck_remote_protocol::{
    ActivityKind, PermissionChoice, PermissionOption, TranscriptFeed, TranscriptItem,
};
use flightdeck_remote_protocol::{ItemId, PromptId, SessionId};

/// Most-recent transcript items retained per session (ring buffer bound).
pub const MAX_ITEMS: usize = 500;
/// How many recently-finalised lines are kept to build a needs-input preview.
const RECENT_LINES: usize = 16;
/// How many recent identical lines suppress a redraw duplicate.
const DEDUPE_WINDOW: usize = 48;
/// Flush a prose block once it reaches this many lines (avoids giant blocks).
const PROSE_FLUSH_LINES: usize = 40;
/// Max characters in a needs-input preview.
const PREVIEW_CAP: usize = 280;

// ---------------------------------------------------------------------------
// ANSI / control stripping + line assembly
// ---------------------------------------------------------------------------

/// Incremental ANSI stripper + line assembler. Bytes go in; completed logical
/// lines come out (via the callback), with escape sequences removed and
/// carriage-return overwrites collapsed.
#[derive(Default)]
struct LineAssembler {
    /// The current (not-yet-terminated) line, already ANSI-stripped.
    cur: String,
    /// Parser state for multi-byte escape handling.
    esc: EscState,
}

#[derive(Default, PartialEq, Eq)]
enum EscState {
    /// Normal text.
    #[default]
    Text,
    /// Saw ESC; awaiting the sequence introducer.
    Escape,
    /// Inside a CSI (`ESC [`) sequence; consume until a final byte `@`..`~`.
    Csi,
    /// Inside an OSC (`ESC ]`) sequence; consume until BEL or ST.
    Osc,
    /// Saw ESC inside an OSC while looking for the ST terminator (`ESC \`).
    OscEsc,
}

impl LineAssembler {
    /// Feed bytes, invoking `on_line` for every completed logical line.
    fn feed(&mut self, bytes: &[u8], mut on_line: impl FnMut(&str)) {
        // Work over chars; PTY output is UTF-8 in practice. Lossy decode keeps
        // us panic-free on partial multibyte reads (rare, and cosmetic only).
        let text = String::from_utf8_lossy(bytes);
        for ch in text.chars() {
            match self.esc {
                EscState::Text => self.feed_text(ch, &mut on_line),
                EscState::Escape => self.feed_escape(ch),
                EscState::Csi => {
                    // CSI ends on a byte in the range '@'..='~'.
                    if ('@'..='~').contains(&ch) {
                        self.esc = EscState::Text;
                    }
                }
                EscState::Osc => match ch {
                    '\u{07}' => self.esc = EscState::Text, // BEL terminates OSC
                    '\u{1b}' => self.esc = EscState::OscEsc,
                    _ => {}
                },
                EscState::OscEsc => {
                    // ESC \ (ST) terminates; any other byte stays in OSC.
                    self.esc = if ch == '\\' {
                        EscState::Text
                    } else {
                        EscState::Osc
                    };
                }
            }
        }
    }

    fn feed_escape(&mut self, ch: char) {
        self.esc = match ch {
            '[' => EscState::Csi,
            ']' => EscState::Osc,
            // Other 2-byte escapes (charset selection, etc.): swallow one byte.
            _ => EscState::Text,
        };
    }

    fn feed_text(&mut self, ch: char, on_line: &mut impl FnMut(&str)) {
        match ch {
            '\u{1b}' => self.esc = EscState::Escape,
            '\n' => {
                let line = std::mem::take(&mut self.cur);
                on_line(line.trim_end_matches('\r'));
            }
            '\r' => {
                // Carriage return without newline: the line is about to be
                // overwritten (progress bars, spinners). Drop what we have so
                // only the final paint of that row survives.
                self.cur.clear();
            }
            '\t' => self.cur.push_str("    "),
            // Skip other C0 control chars.
            c if (c as u32) < 0x20 => {}
            c => self.cur.push(c),
        }
    }

    /// Emit any buffered partial line as a completed line (used before a
    /// preview capture so a trailing prompt without a newline is not missed).
    fn flush(&mut self, on_line: impl FnOnce(&str)) {
        if !self.cur.is_empty() {
            let line = std::mem::take(&mut self.cur);
            on_line(&line);
        }
    }
}

// ---------------------------------------------------------------------------
// Line classification
// ---------------------------------------------------------------------------

/// What a finalised line represents in the cleaned transcript.
enum LineClass {
    /// An empty line — a paragraph/section break.
    Blank,
    /// Plain prose.
    Prose,
    /// The header line of a tool activity, with its kind.
    PillHeader(ActivityKind),
    /// A continuation/result line that attaches to the current activity.
    Continuation,
}

/// Leading markers that open a tool-activity pill.
const PILL_MARKERS: &[char] = &['\u{23fa}', '\u{25cf}', '\u{2022}', '\u{25e6}'];
/// Leading markers for a tool result / continuation line.
const CONT_MARKERS: &[char] = &['\u{23bf}', '\u{2514}', '\u{251c}', '\u{2570}', '\u{2502}'];

/// Classify a trimmed line. Only strong signals become pills; anything
/// ambiguous stays prose (honest over clever).
fn classify(line: &str) -> LineClass {
    let t = line.trim();
    if t.is_empty() {
        return LineClass::Blank;
    }
    let first = t.chars().next().unwrap_or(' ');
    if CONT_MARKERS.contains(&first) {
        return LineClass::Continuation;
    }
    if PILL_MARKERS.contains(&first) {
        return LineClass::PillHeader(kind_of(t));
    }
    if looks_like_tool(t) {
        return LineClass::PillHeader(kind_of(t));
    }
    LineClass::Prose
}

/// Heuristic: does this line read like a tool invocation / shell command?
fn looks_like_tool(t: &str) -> bool {
    // A shell prompt line.
    if let Some(rest) = t.strip_prefix("$ ") {
        return !rest.trim().is_empty();
    }
    // A leading verb naming a tool action.
    const VERBS: &[&str] = &[
        "Ran ",
        "Running ",
        "Edited ",
        "Editing ",
        "Wrote ",
        "Writing ",
        "Created ",
        "Creating ",
        "Read ",
        "Reading ",
        "Searched ",
        "Searching ",
        "Fetched ",
        "Fetching ",
        "Listing ",
        "Deleted ",
        "Moved ",
        "Renamed ",
    ];
    if VERBS.iter().any(|v| t.starts_with(v)) {
        return true;
    }
    // A tool call rendered as `Tool(arg)` (Claude Code style).
    const TOOLS: &[&str] = &[
        "Bash(",
        "Edit(",
        "Read(",
        "Write(",
        "Grep(",
        "Glob(",
        "Task(",
        "Update(",
        "MultiEdit(",
        "WebFetch(",
        "WebSearch(",
        "NotebookEdit(",
    ];
    if TOOLS.iter().any(|p| t.starts_with(p)) {
        return true;
    }
    // A standalone diff stat, e.g. `+18 -4` or `3 additions, 1 removal`.
    is_diff_stat(t)
}

/// A line that is (mostly) a diff/edit statistic.
fn is_diff_stat(t: &str) -> bool {
    let lower = t.to_ascii_lowercase();
    if lower.contains("addition") || lower.contains("removal") || lower.contains("insertion") {
        return true;
    }
    // `+N -M` or `+N −M` (ASCII hyphen or Unicode minus) as the whole line.
    let compact: String = t.split_whitespace().collect::<Vec<_>>().join(" ");
    let ok = compact
        .split(' ')
        .all(|tok| tok.starts_with('+') || tok.starts_with('-') || tok.starts_with('\u{2212}'))
        && (compact.contains('+') || compact.contains('-') || compact.contains('\u{2212}'));
    ok && compact.chars().any(|c| c.is_ascii_digit())
}

/// Pick the activity kind for a pill header from keywords.
fn kind_of(t: &str) -> ActivityKind {
    let l = strip_markers(t).to_ascii_lowercase();
    if l.contains("test") || l.contains("passed") || l.contains("failed") || l.contains("pytest") {
        ActivityKind::Test
    } else if l.starts_with("edit")
        || l.starts_with("wrote")
        || l.starts_with("writing")
        || l.starts_with("creat")
        || l.starts_with("update")
        || l.starts_with("multiedit")
        || l.starts_with("delet")
        || l.starts_with("renamed")
        || l.starts_with("moved")
    {
        ActivityKind::Edit
    } else if l.starts_with("search")
        || l.starts_with("grep")
        || l.starts_with("glob")
        || l.starts_with("read")
        || l.starts_with("listing")
    {
        ActivityKind::Search
    } else if l.starts_with("ran ")
        || l.starts_with("running")
        || l.starts_with("bash(")
        || l.starts_with("$ ")
    {
        ActivityKind::Command
    } else {
        ActivityKind::Other
    }
}

/// Strip leading bullet/tree markers and whitespace from a line.
fn strip_markers(t: &str) -> String {
    t.trim_start_matches(|c: char| {
        PILL_MARKERS.contains(&c) || CONT_MARKERS.contains(&c) || c.is_whitespace()
    })
    .to_string()
}

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

/// A block currently being accumulated.
enum Block {
    None,
    /// Prose lines destined for an agent-message item.
    Prose(Vec<String>),
    /// A tool activity: (summary header, body lines, kind).
    Activity {
        summary: String,
        body: Vec<String>,
        kind: ActivityKind,
    },
}

/// Reconstructs one session's cleaned transcript from teed PTY bytes.
pub struct TranscriptBuilder {
    session_id: SessionId,
    asm: LineAssembler,
    dedupe: VecDeque<String>,
    recent: VecDeque<String>,
    block: Block,
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
            asm: LineAssembler::default(),
            dedupe: VecDeque::new(),
            recent: VecDeque::new(),
            block: Block::None,
            items: VecDeque::new(),
            base_index: 0,
            total: 0,
            sent_upto: 0,
            prompt_seq: 0,
        }
    }

    /// Feed a chunk of raw primary-terminal PTY bytes.
    pub fn push_bytes(&mut self, bytes: &[u8], at_ms: i64) {
        // Collect finalised lines first (the assembler borrows `self.asm`).
        let mut lines: Vec<String> = Vec::new();
        self.asm.feed(bytes, |line| lines.push(line.to_string()));
        for line in lines {
            self.ingest_line(&line, at_ms);
        }
    }

    /// Ingest one finalised, ANSI-stripped line.
    fn ingest_line(&mut self, line: &str, at_ms: i64) {
        let trimmed = line.trim_end();
        // Redraw dedupe: drop an exact, non-blank repeat seen very recently.
        if !trimmed.trim().is_empty() {
            if self.dedupe.iter().any(|l| l == trimmed) {
                return;
            }
            self.dedupe.push_back(trimmed.to_string());
            if self.dedupe.len() > DEDUPE_WINDOW {
                self.dedupe.pop_front();
            }
            self.recent.push_back(trimmed.trim().to_string());
            if self.recent.len() > RECENT_LINES {
                self.recent.pop_front();
            }
        }

        match classify(trimmed) {
            LineClass::Blank => self.flush_block(at_ms),
            LineClass::Prose => self.push_prose(trimmed, at_ms),
            LineClass::PillHeader(kind) => self.open_activity(trimmed, kind, at_ms),
            LineClass::Continuation => self.push_continuation(trimmed, at_ms),
        }
    }

    fn push_prose(&mut self, line: &str, at_ms: i64) {
        if let Block::Prose(lines) = &mut self.block {
            lines.push(line.to_string());
            if lines.len() >= PROSE_FLUSH_LINES {
                self.flush_block(at_ms);
            }
        } else {
            self.flush_block(at_ms);
            self.block = Block::Prose(vec![line.to_string()]);
        }
    }

    fn open_activity(&mut self, header: &str, kind: ActivityKind, at_ms: i64) {
        self.flush_block(at_ms);
        self.block = Block::Activity {
            summary: strip_markers(header),
            body: Vec::new(),
            kind,
        };
    }

    fn push_continuation(&mut self, line: &str, at_ms: i64) {
        if let Block::Activity { body, .. } = &mut self.block {
            body.push(strip_markers(line));
        } else {
            // A continuation marker with no open activity: treat as prose.
            self.push_prose(line, at_ms);
        }
    }

    /// Flush the current block into a finished item.
    fn flush_block(&mut self, at_ms: i64) {
        match std::mem::replace(&mut self.block, Block::None) {
            Block::None => {}
            Block::Prose(lines) => {
                let text = lines.join("\n");
                if !text.trim().is_empty() {
                    let item_id = self.next_item_id();
                    self.push_item(TranscriptItem::AgentMessage {
                        item_id,
                        text,
                        at_ms,
                    });
                }
            }
            Block::Activity {
                summary,
                body,
                kind,
            } => {
                let (summary, detail) = split_detail(&summary);
                let body_text = if body.is_empty() {
                    None
                } else {
                    Some(body.join("\n"))
                };
                let item_id = self.next_item_id();
                self.push_item(TranscriptItem::Activity {
                    item_id,
                    summary,
                    detail,
                    body: body_text,
                    kind,
                    at_ms,
                });
            }
        }
    }

    /// The bridge calls this on the working/idle → needs-input edge: flush any
    /// open block, capture the tail lines as the pending-question preview, and
    /// append an inline permission prompt. Returns the preview (if any).
    pub fn on_needs_input(&mut self, at_ms: i64) -> Option<String> {
        // Fold any trailing partial line (a prompt printed without a newline).
        let mut pending: Option<String> = None;
        self.asm.flush(|l| pending = Some(l.to_string()));
        if let Some(p) = pending {
            self.ingest_line(&p, at_ms);
        }
        self.flush_block(at_ms);

        let preview = self.build_preview();
        self.prompt_seq += 1;
        let prompt_id = PromptId::new(format!("{}:p{}", self.session_id, self.prompt_seq));
        let item_id = self.next_item_id();
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

    /// Build a preview from the most recent non-blank lines.
    fn build_preview(&self) -> Option<String> {
        let tail: Vec<&str> = self
            .recent
            .iter()
            .rev()
            .take(6)
            .map(|s| s.as_str())
            .collect();
        if tail.is_empty() {
            return None;
        }
        let mut text = tail
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string();
        if text.is_empty() {
            return None;
        }
        if text.len() > PREVIEW_CAP {
            text.truncate(PREVIEW_CAP);
            text.push('…');
        }
        Some(text)
    }

    fn next_item_id(&self) -> ItemId {
        ItemId::new(format!("{}:{}", self.session_id, self.total))
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

/// Split a trailing `· detail` (or a diff stat) off a pill summary, so a pill
/// renders as `Ran npm test` + detail `42 passed`.
fn split_detail(summary: &str) -> (String, Option<String>) {
    if let Some((head, tail)) = summary.split_once(" · ") {
        let tail = tail.trim();
        if !tail.is_empty() {
            return (head.trim().to_string(), Some(tail.to_string()));
        }
    }
    (summary.trim().to_string(), None)
}

#[cfg(test)]
mod tests;
