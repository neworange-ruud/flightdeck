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
//! Answerable prompts surface two ways. A Claude `AskUserQuestion` **is** in the
//! session file (a tool_use), so it is emitted as an inline
//! [`TranscriptItem::PermissionPrompt`] the moment it is ingested — it does not
//! wait for a status edge, which for some agents/hooks never flips to `waiting`
//! and would otherwise leave the question invisible on the phone
//! (remote-control-z30). Binary permission prompts are **not** in the session
//! file; the bridge calls [`TranscriptBuilder::on_needs_input`] when the status
//! hook reports the agent stopped for input, and we synthesize an inline prompt
//! whose preview is the agent's last prose (OpenCode passes a captured
//! structured prompt through the same edge via [`TranscriptBuilder::set_structured_prompt`]).
//!
//! Three agents are supported, resolved by [`resolve_source`]:
//! * **Claude Code** — JSONL file, `type=user|assistant`, `message.content`
//!   blocks (tailed by byte offset, [`SessionFormat::Claude`]).
//! * **Codex** — JSONL file, `event_msg` prose + `response_item` tool activity
//!   ([`SessionFormat::Codex`]).
//! * **OpenCode** — a live SQLite DB rather than a tailable file, polled by
//!   session id ([`crate::remote::opencode`]); its rows mutate as the assistant
//!   streams, so a part is emitted only once final. The DB layer is compiled off
//!   Windows only, so on Windows an OpenCode chat shows no reconstructed
//!   transcript (as before).
//!
//! Memory is capped to [`MAX_ITEMS`] most-recent items (a ring); pagination
//! honours the protocol's `from_index`.

use std::collections::{HashSet, VecDeque};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::Value;

use flightdeck_remote_protocol::{
    ActivityKind, PermissionChoice, PermissionOption, PromptKind, PromptQuestion, TranscriptFeed,
    TranscriptItem,
};
use flightdeck_remote_protocol::{ItemId, PromptId, SessionId};

use crate::agents::resume::SessionFormat;

/// Most-recent transcript items retained per session (ring buffer bound).
pub const MAX_ITEMS: usize = 500;
/// Max characters in a needs-input preview.
const PREVIEW_CAP: usize = 280;

/// Where an agent's conversation is read from. Claude and Codex append a JSONL
/// session *file* that is tailed by byte offset; OpenCode keeps its conversation
/// in a live SQLite DB that is polled by session id.
#[derive(Debug, Clone)]
pub enum TranscriptSource {
    /// A tailable JSONL session file and the schema to parse it as.
    Jsonl {
        path: PathBuf,
        format: SessionFormat,
    },
    /// OpenCode's SQLite DB and the tab's worktree (its session's `directory`).
    OpenCode { db: PathBuf, directory: String },
}

/// Drop `.` (current-dir) components from a path. A base-branch agent
/// (`runs_on_base`) has `worktree_path_relative == "."`, so the desktop builds
/// its worktree as `repo_root.join(".")` → `…/repo/.`. Claude and OpenCode
/// canonicalize their cwd before recording a session, so they store it under the
/// clean `…/repo`. Left as-is, the string-mangled Claude project dir gains a
/// spurious trailing `-` (and the OpenCode `directory` a trailing `/.`) and never
/// matches what the agent wrote — so no session file is found and the transcript
/// stays permanently empty (remote-control-ou3). Stripping `CurDir` mirrors what
/// the agent's own `getcwd` does, without resolving symlinks (which would risk
/// diverging from the path real worktrees already match).
fn clean_cwd(cwd: &Path) -> PathBuf {
    let cleaned: PathBuf = cwd
        .components()
        .filter(|c| !matches!(c, std::path::Component::CurDir))
        .collect();
    if cleaned.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        cleaned
    }
}

/// Resolve where `agent`'s conversation for `cwd` lives (store rooted at
/// `home`), or `None` if there is nothing to read yet. Claude/Codex resolve to a
/// JSONL file via [`crate::agents::resume`]; OpenCode resolves to its SQLite DB
/// (remote-control-fyj) when that DB exists.
pub fn resolve_source(agent: &str, cwd: &Path, home: &Path) -> Option<TranscriptSource> {
    // Normalize away `.`/current-dir components so a base-branch agent's
    // `repo_root/.` cwd matches the clean path the agent recorded its session
    // under (remote-control-ou3).
    let cwd = clean_cwd(cwd);
    let cwd = cwd.as_path();
    if let Some((path, format)) = crate::agents::resume::newest_session_path(agent, cwd, home) {
        return Some(TranscriptSource::Jsonl { path, format });
    }
    if agent == "opencode" {
        let db = crate::remote::opencode::db_path(home);
        if db.exists() {
            return Some(TranscriptSource::OpenCode {
                db,
                directory: cwd.to_string_lossy().into_owned(),
            });
        }
    }
    None
}

/// A structured prompt captured during ingest (e.g. a Claude `AskUserQuestion`
/// tool call), surfaced by the next [`TranscriptBuilder::on_needs_input`] edge
/// instead of the synthesized binary allow/deny fallback.
#[derive(Clone, Debug)]
pub struct StructuredPrompt {
    /// Permission (binary) vs Question (N-option / free-text).
    pub kind: PromptKind,
    /// Question text (Question) or permission/action text (Permission).
    pub command: String,
    /// The offered options, already indexed `0..n`, with labels/descriptions.
    pub options: Vec<PermissionOption>,
    /// Whether the phone may submit a free-text answer ("Type your own answer").
    pub allow_free_text: bool,
    /// Whether the phone may select multiple options (a `multiSelect` /
    /// checklist question). `false` for permissions and single-select questions.
    pub multi_select: bool,
    /// All questions in the prompt's form, in tab order (a Claude
    /// `AskUserQuestion` can carry several). Empty for a permission prompt or a
    /// single implicit question whose flat fields above already describe it; the
    /// flat `command`/`options`/`multi_select` always mirror `questions[0]` when
    /// this is non-empty.
    pub questions: Vec<PromptQuestion>,
}

/// Reconstructs one session's cleaned transcript by tailing its agent session
/// JSONL and translating each record into a [`TranscriptItem`].
pub struct TranscriptBuilder {
    session_id: SessionId,
    /// Which record schema `source` is parsed as (set each sync; stable per
    /// session). Defaults to Claude until the first sync sets it.
    format: SessionFormat,
    /// The session file currently being tailed (set on first successful sync).
    source: Option<PathBuf>,
    /// For OpenCode sessions (DB-backed, no tailable file): the session id
    /// currently being polled, so a new conversation in the worktree resets.
    oc_session: Option<String>,
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
    /// A structured prompt captured during ingest, consumed (and cleared) by the
    /// next [`Self::on_needs_input`] edge in place of the binary fallback. Used
    /// by the OpenCode sidecar path, whose prompt is not in the tailed store.
    pending_structured: Option<StructuredPrompt>,
    /// Preview of a structured prompt already surfaced *at ingest time* (a Claude
    /// `AskUserQuestion`, which lives in the session file) and not yet answered.
    /// While `Some`, the needs-input edge must NOT synthesize a second (binary)
    /// prompt for the same wait — it just reports this preview (remote-control-z30).
    open_prompt: Option<String>,
}

impl TranscriptBuilder {
    /// A fresh builder for `session_id`.
    pub fn new(session_id: SessionId) -> Self {
        TranscriptBuilder {
            session_id,
            format: SessionFormat::Claude,
            source: None,
            oc_session: None,
            read_offset: 0,
            seen: HashSet::new(),
            last_agent_text: None,
            items: VecDeque::new(),
            base_index: 0,
            total: 0,
            sent_upto: 0,
            prompt_seq: 0,
            pending_structured: None,
            open_prompt: None,
        }
    }

    /// Capture a structured prompt to be surfaced by the next
    /// [`Self::on_needs_input`] edge (instead of the binary fallback).
    pub fn set_structured_prompt(&mut self, p: StructuredPrompt) {
        self.pending_structured = Some(p);
    }

    /// Whether a structured prompt is ready to surface right now — either
    /// captured and awaiting the needs-input edge (`pending_structured`, e.g. the
    /// OpenCode sidecar) or already emitted at ingest and unanswered
    /// (`open_prompt`, a Claude AskUserQuestion). The bridge uses this to decide
    /// whether to surface a prompt immediately or defer the binary fallback.
    pub fn has_structured_prompt(&self) -> bool {
        self.pending_structured.is_some() || self.open_prompt.is_some()
    }

    /// Whether an ingest-time structured prompt (a Claude AskUserQuestion) is
    /// currently emitted and unanswered — the signal that a deferred binary
    /// fallback must be abandoned because the real question has now arrived.
    pub fn has_open_prompt(&self) -> bool {
        self.open_prompt.is_some()
    }

    /// The preview (question text) of the currently open ingest-time prompt.
    pub fn open_prompt_preview(&self) -> Option<String> {
        self.open_prompt.clone()
    }

    /// Clear the open-prompt dedup guard (and any captured-but-unemitted
    /// prompt) once the current prompt has been answered — i.e. the agent left
    /// needs-input. Without this, the guard that stops ONE question emitting
    /// twice (ingest vs sidecar) also suppresses the NEXT question in the
    /// session as a false duplicate, so a follow-up question reuses the old
    /// answered frame instead of surfacing anew (remote-control-dc9). Claude
    /// also clears `open_prompt` when the answer's user-turn record is ingested;
    /// OpenCode has no such record, so the bridge calls this on the status edge.
    pub fn clear_open_prompt(&mut self) {
        self.open_prompt = None;
        self.pending_structured = None;
    }

    /// Ingest any new conversation from `source`, dispatching to the file-tail
    /// (Claude/Codex) or DB-poll (OpenCode) path. Cheap and safe to call every
    /// tick.
    pub fn sync(&mut self, source: &TranscriptSource, now_ms: i64) {
        match source {
            TranscriptSource::Jsonl { path, format } => self.sync_jsonl(path, *format, now_ms),
            TranscriptSource::OpenCode { db, directory } => {
                self.sync_opencode(db, directory, now_ms)
            }
        }
    }

    /// Poll OpenCode's SQLite DB for the newest session in `directory` and ingest
    /// any parts not yet seen. Unlike the append-only JSONL tail, OpenCode
    /// mutates rows as the assistant streams, so a part is emitted only once it
    /// is *final* (an assistant text part has `time.end`; a tool part has reached
    /// `completed`/`error`) — an in-flight part is left unseen and retried next
    /// tick. Dedup is by part id (rows are re-read every poll).
    pub fn sync_opencode(&mut self, db: &Path, directory: &str, now_ms: i64) {
        let Some(sid) = crate::remote::opencode::latest_session_id(db, directory) else {
            return;
        };
        // A different session id means a new conversation in this worktree —
        // start over so `load` replaces cleanly (mirrors the file-switch reset).
        if self.oc_session.as_deref() != Some(sid.as_str()) {
            self.oc_session = Some(sid.clone());
            self.reset();
        }
        for part in crate::remote::opencode::fetch_parts(db, &sid) {
            if self.seen.contains(&part.id) {
                continue;
            }
            self.ingest_opencode(&part, now_ms);
        }
    }

    /// Tail the agent session file at `path`, appending any newly-written
    /// records as transcript items. Cheap and safe to call every tick: it only
    /// reads bytes past the last consumed offset, and only consumes whole
    /// (newline-terminated) lines so a half-written record is never parsed.
    /// `now_ms` stamps items whose record carries no parseable timestamp.
    /// `format` selects the record parser (Claude vs Codex); it is stable for a
    /// given session file.
    pub fn sync_jsonl(&mut self, path: &Path, format: SessionFormat, now_ms: i64) {
        self.format = format;
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
        self.pending_structured = None;
        self.open_prompt = None;
        // `prompt_seq` intentionally keeps advancing so PromptIds stay unique.
    }

    /// Translate one session-file record into 0+ transcript items, dispatching
    /// on the session's [`SessionFormat`].
    fn ingest(&mut self, o: &Value, now_ms: i64) {
        match self.format {
            SessionFormat::Claude => self.ingest_claude(o, now_ms),
            SessionFormat::Codex => self.ingest_codex(o, now_ms),
        }
    }

    /// Translate one **Claude Code** record into 0+ transcript items.
    fn ingest_claude(&mut self, o: &Value, now_ms: i64) {
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
                // A user turn answers any prompt that was awaiting input, so an
                // open ingest-time question is now resolved (remote-control-z30).
                self.open_prompt = None;
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
                            // A Claude `AskUserQuestion` is an answerable prompt, not
                            // an activity. It lives in the session file, so surface it
                            // as a Question prompt IMMEDIATELY rather than waiting for a
                            // needs-input status edge — that edge depends on a status
                            // hook flipping to `waiting`, which does not fire for
                            // AskUserQuestion on every agent, leaving the question
                            // invisible on the phone (remote-control-z30). Emit no
                            // activity pill. If the JSON is missing/oddly-typed, fall
                            // through to the pill.
                            if name == "AskUserQuestion" {
                                if let Some(sp) = parse_ask_user_question(input) {
                                    // Skip if this question was already surfaced
                                    // (the PreToolUse sidecar delivers it before the
                                    // JSONL record lands) so it is not shown twice
                                    // (remote-control-qa1). Either way, emit no pill.
                                    if self.open_prompt.is_none() {
                                        self.emit_structured_prompt(sp, at_ms);
                                    }
                                    continue;
                                }
                            }
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

    /// Translate one **Codex** rollout record into 0+ transcript items.
    ///
    /// Codex writes two interleaved streams (in file/timestamp order):
    /// * `event_msg` — the display stream: `user_message` / `agent_message` carry
    ///   the user's and the agent's prose (`payload.message`, a string). We take
    ///   prose from here (not the duplicate `response_item` `message` records).
    /// * `response_item` — model I/O: `function_call` / `custom_tool_call` carry
    ///   tool activity. We take tool pills from here.
    ///
    /// Everything else (`reasoning`, developer/injected `message`s, tool output,
    /// `session_meta`, `world_state`, token counts, task lifecycle) is skipped.
    /// Records carry no per-record uuid, so tool items key off the tool `call_id`
    /// and prose falls back to the running ordinal (see [`Self::item_id`]); the
    /// byte-offset tail never re-reads a line, so no uuid dedup is needed.
    fn ingest_codex(&mut self, o: &Value, now_ms: i64) {
        let at_ms = o
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(parse_iso8601_ms)
            .unwrap_or(now_ms);
        let Some(payload) = o.get("payload") else {
            return;
        };
        let payload_type = payload.get("type").and_then(Value::as_str);

        match o.get("type").and_then(Value::as_str) {
            Some("event_msg") => match payload_type {
                Some("user_message") => {
                    if let Some(text) = payload
                        .get("message")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|t| !t.is_empty())
                    {
                        let item_id = self.item_id("", 0);
                        self.push_item(TranscriptItem::UserMessage {
                            item_id,
                            text: text.to_string(),
                            at_ms,
                        });
                    }
                }
                Some("agent_message") => {
                    if let Some(text) = payload
                        .get("message")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|t| !t.is_empty())
                    {
                        self.last_agent_text = Some(text.to_string());
                        let item_id = self.item_id("", 0);
                        self.push_item(TranscriptItem::AgentMessage {
                            item_id,
                            text: text.to_string(),
                            at_ms,
                        });
                    }
                }
                _ => {}
            },
            Some("response_item") => match payload_type {
                Some("function_call") | Some("custom_tool_call") => {
                    let name = payload
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("tool");
                    let call_id = payload.get("call_id").and_then(Value::as_str).unwrap_or("");
                    let (summary, detail, body, kind) = codex_tool_activity(name, payload);
                    let item_id = self.item_id(call_id, 0);
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
            },
            _ => {}
        }
    }

    /// Translate one **OpenCode** `part` (already joined to its message role)
    /// into 0+ transcript items. Marks the part `seen` once it is handled —
    /// either emitted or permanently skipped — but leaves a not-yet-final part
    /// unseen so a later poll re-checks it.
    fn ingest_opencode(&mut self, part: &crate::remote::opencode::Part, now_ms: i64) {
        let at_ms = if part.at_ms > 0 { part.at_ms } else { now_ms };
        match part.data.get("type").and_then(Value::as_str) {
            Some("text") => {
                let text = part
                    .data
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if part.role == "user" {
                    // User prose is written whole (not streamed) — always final.
                    if !text.is_empty() {
                        let item_id = self.item_id(&part.id, 0);
                        self.push_item(TranscriptItem::UserMessage {
                            item_id,
                            text,
                            at_ms,
                        });
                    }
                    self.seen.insert(part.id.clone());
                } else {
                    // Assistant prose streams token-by-token; wait for `time.end`.
                    let finished = part
                        .data
                        .get("time")
                        .and_then(|t| t.get("end"))
                        .is_some_and(|v| !v.is_null());
                    if !finished {
                        return; // in-flight — retry on the next poll
                    }
                    if !text.is_empty() {
                        self.last_agent_text = Some(text.clone());
                        let item_id = self.item_id(&part.id, 0);
                        self.push_item(TranscriptItem::AgentMessage {
                            item_id,
                            text,
                            at_ms,
                        });
                    }
                    self.seen.insert(part.id.clone());
                }
            }
            Some("tool") => {
                let status = part
                    .data
                    .get("state")
                    .and_then(|s| s.get("status"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if status != "completed" && status != "error" {
                    return; // pending/running — retry once the call settles
                }
                let tool = part
                    .data
                    .get("tool")
                    .and_then(Value::as_str)
                    .unwrap_or("tool");
                let null = Value::Null;
                let input = part
                    .data
                    .get("state")
                    .and_then(|s| s.get("input"))
                    .unwrap_or(&null);
                let (summary, detail, body, kind) = opencode_tool_activity(tool, input);
                let item_id = self.item_id(&part.id, 0);
                self.push_item(TranscriptItem::Activity {
                    item_id,
                    summary,
                    detail,
                    body,
                    kind,
                    at_ms,
                });
                self.seen.insert(part.id.clone());
            }
            // reasoning, step-start/step-finish, patch, file, compaction, subtask
            // and any future part type are not conversation items.
            _ => {
                self.seen.insert(part.id.clone());
            }
        }
    }

    /// Emit a structured prompt as an inline [`TranscriptItem::PermissionPrompt`]
    /// now, minting a fresh prompt id, and remember its preview as the currently
    /// open prompt. Shared by the ingest-time AskUserQuestion capture and the
    /// needs-input edge (OpenCode sidecar). Returns the preview.
    fn emit_structured_prompt(&mut self, sp: StructuredPrompt, at_ms: i64) -> Option<String> {
        self.prompt_seq += 1;
        let prompt_id = PromptId::new(format!("{}:p{}", self.session_id, self.prompt_seq));
        let item_id = ItemId::new(format!("{}:prompt:{}", self.session_id, self.prompt_seq));
        let preview = truncate_preview(&sp.command);
        self.push_item(TranscriptItem::PermissionPrompt {
            item_id,
            prompt_id,
            kind: sp.kind,
            command: sp.command,
            options: sp.options,
            allow_free_text: sp.allow_free_text,
            multi_select: sp.multi_select,
            questions: sp.questions,
            at_ms,
        });
        self.open_prompt = preview.clone();
        preview
    }

    /// The bridge calls this on the working/idle → needs-input edge: synthesize
    /// an inline permission prompt whose preview is the agent's last prose (the
    /// session file has no permission records). Returns the preview (if any).
    pub fn on_needs_input(&mut self, at_ms: i64) -> Option<String> {
        // If a structured prompt was already surfaced (a Claude AskUserQuestion
        // emitted at ingest), do NOT emit anything else for the same wait — and
        // discard any duplicate captured prompt (e.g. the sidecar for the SAME
        // question) so the phone never sees it twice (remote-control-qa1).
        if let Some(preview) = self.open_prompt.clone() {
            self.pending_structured = None;
            return Some(preview);
        }
        // Otherwise a captured structured prompt (the OpenCode sidecar, or the
        // Claude AskUserQuestion sidecar) supplants the binary allow/deny fallback.
        if let Some(sp) = self.pending_structured.take() {
            return self.emit_structured_prompt(sp, at_ms);
        }
        self.prompt_seq += 1;
        let prompt_id = PromptId::new(format!("{}:p{}", self.session_id, self.prompt_seq));
        let item_id = ItemId::new(format!("{}:prompt:{}", self.session_id, self.prompt_seq));
        let preview = self.build_preview();
        self.push_item(TranscriptItem::PermissionPrompt {
            item_id,
            prompt_id,
            kind: PromptKind::Permission,
            command: preview.clone().unwrap_or_default(),
            options: vec![
                PermissionOption {
                    index: 0,
                    choice: Some(PermissionChoice::AllowOnce),
                    label: "Allow once".to_string(),
                    description: None,
                },
                PermissionOption {
                    index: 1,
                    choice: Some(PermissionChoice::Deny),
                    label: "Deny".to_string(),
                    description: None,
                },
            ],
            allow_free_text: false,
            multi_select: false,
            questions: Vec::new(),
            at_ms,
        });
        preview
    }

    /// The agent's last prose, truncated, as the needs-input preview.
    fn build_preview(&self) -> Option<String> {
        truncate_preview(self.last_agent_text.as_ref()?)
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

/// Trim `text` and cap it at [`PREVIEW_CAP`] chars (appending `…` when cut),
/// or `None` when empty. Shared by the last-prose preview and structured-prompt
/// previews.
fn truncate_preview(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if text.chars().count() > PREVIEW_CAP {
        let mut out: String = text.chars().take(PREVIEW_CAP).collect();
        out.push('…');
        Some(out)
    } else {
        Some(text.to_string())
    }
}

/// Parse a Claude `AskUserQuestion` tool_use `input` into a [`StructuredPrompt`],
/// capturing ALL of its `questions` (each with its header, question text,
/// options, and `multiSelect` flag) — not just the first. A single
/// `AskUserQuestion` can carry several questions rendered as a tabbed form with
/// a final Confirm tab, so every question is preserved in
/// [`StructuredPrompt::questions`]; the flat `command`/`options`/`multi_select`
/// fields mirror the FIRST question so a pre-multi-question consumer (and the
/// preview) still work.
///
/// Returns `None` — so the caller falls back to a normal activity pill — when
/// the shape is missing or oddly typed (no first question with text, or a
/// question with no options with labels).
pub(crate) fn parse_ask_user_question(input: &Value) -> Option<StructuredPrompt> {
    let raw_questions = input.get("questions").and_then(Value::as_array)?;
    let mut questions: Vec<PromptQuestion> = Vec::with_capacity(raw_questions.len());
    for q in raw_questions {
        let question = q
            .get("question")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|t| !t.is_empty())?
            .to_string();
        let options: Vec<PermissionOption> = q
            .get("options")
            .and_then(Value::as_array)?
            .iter()
            .enumerate()
            .filter_map(|(i, o)| {
                let label = o.get("label").and_then(Value::as_str)?.to_string();
                let description = o
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                Some(PermissionOption {
                    index: i as u32,
                    choice: None,
                    label,
                    description,
                })
            })
            .collect();
        if options.is_empty() {
            return None;
        }
        let header = q
            .get("header")
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|h| !h.is_empty());
        let multi_select = q
            .get("multiSelect")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        questions.push(PromptQuestion {
            header,
            question,
            options,
            multi_select,
        });
    }
    // The first question feeds the flat single-question fields (preview + any
    // consumer that ignores the `questions` list).
    let first = questions.first()?;
    Some(StructuredPrompt {
        kind: PromptKind::Question,
        command: first.question.clone(),
        options: first.options.clone(),
        allow_free_text: true,
        multi_select: first.multi_select,
        questions,
    })
}

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

/// Summarise a Codex tool record into `(summary, detail, body, kind)` for a
/// collapsed activity pill. Handles `function_call` (whose `arguments` is a
/// JSON *string*) and `custom_tool_call` (whose `input` is a raw code string).
fn codex_tool_activity(
    name: &str,
    payload: &Value,
) -> (String, Option<String>, Option<String>, ActivityKind) {
    // `function_call.arguments` is a JSON-encoded string; parse it if present.
    let args = payload
        .get("arguments")
        .and_then(Value::as_str)
        .and_then(|s| serde_json::from_str::<Value>(s).ok());
    let a = args.as_ref();
    match name {
        // Codex's exec tool: `command` is an argv array; `["bash","-lc", SCRIPT]`
        // carries the real script in its last element.
        "shell" | "local_shell" | "container.exec" => {
            let cmd = a.and_then(codex_command_string).unwrap_or_default();
            let summary = if cmd.is_empty() {
                "Ran a command".to_string()
            } else {
                format!("Ran {}", first_line(&cmd))
            };
            let body = (!cmd.is_empty()).then_some(cmd);
            (summary, None, body, ActivityKind::Command)
        }
        "apply_patch" => {
            let patch = a
                .and_then(|v| v.get("input").or_else(|| v.get("patch")))
                .and_then(Value::as_str)
                .unwrap_or("");
            let summary = match codex_patch_file(patch) {
                Some(f) => format!("Edited {}", basename(&f)),
                None => "Applied a patch".to_string(),
            };
            (summary, None, None, ActivityKind::Edit)
        }
        "update_plan" => (
            "Updated the plan".to_string(),
            None,
            None,
            ActivityKind::Other,
        ),
        "view_image" => (
            "Viewed an image".to_string(),
            None,
            None,
            ActivityKind::Other,
        ),
        // `custom_tool_call` (e.g. `exec`): the payload carries a raw `input`
        // string rather than JSON `arguments`.
        _ => {
            if let Some(input) = payload.get("input").and_then(Value::as_str) {
                let input = input.trim();
                let summary = format!("Ran {name}");
                let body = (!input.is_empty()).then(|| input.to_string());
                (summary, None, body, ActivityKind::Command)
            } else {
                (name.to_string(), None, None, ActivityKind::Other)
            }
        }
    }
}

/// The command string from a Codex `shell` call's parsed `arguments`: the
/// `["bash","-lc", SCRIPT]` script when present, else the argv joined by spaces.
fn codex_command_string(args: &Value) -> Option<String> {
    let arr = args.get("command")?.as_array()?;
    let parts: Vec<&str> = arr.iter().filter_map(Value::as_str).collect();
    if parts.is_empty() {
        return None;
    }
    if let [shell, flag, script] = parts.as_slice() {
        if (*shell == "bash" || *shell == "sh" || *shell == "zsh") && flag.starts_with('-') {
            return Some(script.to_string());
        }
    }
    Some(parts.join(" "))
}

/// The first file path named in a Codex `apply_patch` body (`*** Add File: p`,
/// `*** Update File: p`, `*** Delete File: p`), if any.
fn codex_patch_file(patch: &str) -> Option<String> {
    patch.lines().find_map(|line| {
        let line = line.trim();
        for tag in ["*** Add File:", "*** Update File:", "*** Delete File:"] {
            if let Some(rest) = line.strip_prefix(tag) {
                let p = rest.trim();
                if !p.is_empty() {
                    return Some(p.to_string());
                }
            }
        }
        None
    })
}

/// Summarise an OpenCode `tool` part into `(summary, detail, body, kind)` for a
/// collapsed activity pill. `input` is the tool's `state.input` object; OpenCode
/// tool names are lowercase and MCP tools are `server_tool`-prefixed.
fn opencode_tool_activity(
    tool: &str,
    input: &Value,
) -> (String, Option<String>, Option<String>, ActivityKind) {
    let s = |k: &str| input.get(k).and_then(Value::as_str);
    match tool {
        "bash" => {
            let cmd = s("command").unwrap_or("");
            let summary = s("description")
                .map(str::to_string)
                .unwrap_or_else(|| format!("Ran {}", first_line(cmd)));
            let body = (!cmd.is_empty()).then(|| cmd.to_string());
            (summary, None, body, ActivityKind::Command)
        }
        "edit" | "write" | "patch" => (
            format!("Edited {}", basename(s("filePath").unwrap_or(""))),
            None,
            None,
            ActivityKind::Edit,
        ),
        "read" => (
            format!("Read {}", basename(s("filePath").unwrap_or(""))),
            None,
            None,
            ActivityKind::Search,
        ),
        "grep" | "glob" => (
            format!("Searched {}", s("pattern").unwrap_or("")),
            None,
            None,
            ActivityKind::Search,
        ),
        "webfetch" => (
            format!("Fetched {}", s("url").unwrap_or("")),
            None,
            None,
            ActivityKind::Search,
        ),
        "task" => (
            format!("Task: {}", s("description").unwrap_or("subtask")),
            None,
            None,
            ActivityKind::Other,
        ),
        "todowrite" => (
            "Updated the task list".to_string(),
            None,
            None,
            ActivityKind::Other,
        ),
        "skill" => (
            format!("Skill: {}", s("name").unwrap_or("")),
            None,
            None,
            ActivityKind::Other,
        ),
        // Built-in fallbacks and MCP tools (`server_tool`) show the tool name.
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
