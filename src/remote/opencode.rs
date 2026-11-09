//! Read-only access to OpenCode's on-disk conversation store.
//!
//! Unlike Claude Code and Codex — which each append a JSONL session *file* the
//! transcript builder can tail — modern OpenCode keeps its whole conversation in
//! a single live **SQLite** database at `~/.local/share/opencode/opencode.db`
//! (WAL mode). Its older per-message JSON store under `storage/` was abandoned in
//! early 2026, so there is no file to tail; the transcript is reconstructed by
//! querying the DB (remote-control-fyj).
//!
//! Schema (the parts we read):
//! * `session(id, directory, agent, time_updated, …)` — one row per chat; we
//!   pick the newest whose `directory` is the tab's worktree.
//! * `message(id, session_id, data JSON{role: user|assistant, …}, …)`.
//! * `part(id, message_id, session_id, time_created, data JSON{type, …}, …)` —
//!   the ordered content stream: `text`, `tool`, `reasoning`, `step-*`, etc.
//!
//! We join each `part` to its `message.role`, ordered by `time_created` then
//! rowid, and hand the rows to [`crate::remote::transcript`] for translation.
//!
//! **OpenCode 2** keeps the same database file but new tables:
//! * `session_v2(id, parent_id, directory, time_updated, …)`;
//! * `session_message(id, session_id, type, seq, time_created, time_updated,
//!   data JSON)`: `type` is `user` (`data.text`), `assistant`
//!   (`data.content[]` of `text` / `reasoning` / `tool` items, and
//!   `data.time.completed` once the step is done) or bookkeeping such as `idle`.
//!
//! A 2.x message's content items are turned into the 1.x part shape the
//! translator already reads ([`parts_from_v2_message`]), so both versions share
//! one translation. A worktree's newest 2.x session wins; the 1.x tables are
//! only read when it has none (sessions from before the upgrade).
//!
//! **Platform**: correct reads of a live WAL database need real SQLite (recent
//! writes live in the `-wal` sidecar), so the query layer uses `rusqlite`
//! (bundled). To keep the released windows-msvc binary pure-Rust — as the
//! relay's TLS and the self-updater already are — the DB layer is compiled only
//! off Windows; on Windows [`latest_session_id`]/[`fetch_parts`] are inert stubs
//! and an OpenCode chat simply shows no reconstructed transcript. The connection
//! is opened **read-only**; FlightDeck never writes to OpenCode's database.

use std::path::{Path, PathBuf};

use serde_json::Value;

/// OpenCode's conversation database under `home`.
pub fn db_path(home: &Path) -> PathBuf {
    home.join(".local")
        .join("share")
        .join("opencode")
        .join("opencode.db")
}

/// One `part` row joined to its message role, ready for translation into a
/// [`crate::remote::transcript::TranscriptItem`].
#[derive(Debug, Clone)]
pub struct Part {
    /// The part's stable id (`prt_…`) — used to dedup across polls.
    pub id: String,
    /// The owning message's role: `"user"` or `"assistant"`.
    pub role: String,
    /// The part row's creation time in unix ms (its transcript timestamp).
    pub at_ms: i64,
    /// The part row's last update time in unix ms (the incremental poll cursor).
    pub updated_at_ms: i64,
    /// The parsed `part.data` JSON (`type`, and per-type fields).
    pub data: Value,
}

/// Turn one OpenCode 2 `session_message` row into transcript parts in the 1.x
/// `part` shape [`crate::remote::transcript`] reads:
///
/// * a `user` message is one `text` part;
/// * an `assistant` message gives one part per `content[]` item, id
///   `<message id>:<index>` (content only grows while the step streams, so the
///   ids are stable across polls). A `text` item carries `time.end` once the
///   message is completed, which is what releases streamed prose. A `tool`
///   item keeps its name in `tool` and its `state` (`status`, `input`) as is.
/// * anything else (`idle`, compaction markers, …) gives nothing.
///
/// Pure, so it is tested on every platform even though only the non-Windows
/// SQLite layer calls it.
#[cfg_attr(windows, allow(dead_code))]
pub fn parts_from_v2_message(
    id: &str,
    kind: &str,
    time_created: i64,
    time_updated: i64,
    data: &Value,
) -> Vec<Part> {
    let part = |id: String, role: &str, data: Value| Part {
        id,
        role: role.to_string(),
        at_ms: time_created,
        updated_at_ms: time_updated,
        data,
    };
    match kind {
        "user" => {
            let text = data.get("text").and_then(Value::as_str).unwrap_or("");
            vec![part(
                format!("{id}:0"),
                "user",
                serde_json::json!({ "type": "text", "text": text }),
            )]
        }
        "assistant" => {
            let completed = data
                .get("time")
                .and_then(|t| t.get("completed"))
                .filter(|v| !v.is_null())
                .cloned();
            let Some(items) = data.get("content").and_then(Value::as_array) else {
                return Vec::new();
            };
            items
                .iter()
                .enumerate()
                .filter_map(|(index, item)| {
                    let id = format!("{id}:{index}");
                    let converted = match item.get("type").and_then(Value::as_str)? {
                        "text" => serde_json::json!({
                            "type": "text",
                            "text": item.get("text").cloned().unwrap_or(Value::Null),
                            "time": { "end": completed.clone().unwrap_or(Value::Null) },
                        }),
                        "tool" => serde_json::json!({
                            "type": "tool",
                            "tool": item.get("name").cloned().unwrap_or(Value::Null),
                            "callID": item.get("id").cloned().unwrap_or(Value::Null),
                            "state": item.get("state").cloned().unwrap_or(Value::Null),
                        }),
                        other => serde_json::json!({ "type": other }),
                    };
                    Some(part(id, "assistant", converted))
                })
                .collect()
        }
        _ => Vec::new(),
    }
}

#[cfg(not(windows))]
mod imp {
    use super::{fetch_v2_parts, Part};
    use rusqlite::{Connection, OpenFlags};
    use std::path::Path;

    /// Open `db` read-only. Returns `None` if it cannot be opened (missing,
    /// locked in a way that blocks readers, or corrupt) — the caller then simply
    /// produces no transcript this tick and retries on the next.
    fn open_ro(db: &Path) -> Option<Connection> {
        Connection::open_with_flags(
            db,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .ok()
    }

    /// Whether the DB has OpenCode 2's tables.
    fn has_v2(conn: &Connection) -> bool {
        conn.query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'session_v2'",
            [],
            |_| Ok(()),
        )
        .is_ok()
    }

    /// The id of the newest OpenCode session whose worktree is `directory`, or
    /// `None` if the DB has none (or cannot be read). An OpenCode 2 session (a
    /// top-level one: subagent sessions share the directory) wins; the 1.x
    /// table is read only when there is none.
    pub fn latest_session_id(db: &Path, directory: &str) -> Option<String> {
        let conn = open_ro(db)?;
        if has_v2(&conn) {
            let v2 = conn
                .query_row(
                    "SELECT id FROM session_v2 WHERE directory = ?1 AND parent_id IS NULL \
                     ORDER BY time_updated DESC LIMIT 1",
                    [directory],
                    |row| row.get::<_, String>(0),
                )
                .ok();
            if v2.is_some() {
                return v2;
            }
        }
        conn.query_row(
            "SELECT id FROM session WHERE directory = ?1 \
             ORDER BY time_updated DESC LIMIT 1",
            [directory],
            |row| row.get::<_, String>(0),
        )
        .ok()
    }

    /// Every `part` of `session_id` updated at or after `updated_since`, joined to
    /// its message role and ordered as it was written (`time_created`, then rowid
    /// to break ties within a message). The inclusive boundary deliberately
    /// revisits the newest row so an in-flight part remains observable until it
    /// finalizes. Returns empty on any read error.
    pub fn fetch_parts(db: &Path, session_id: &str, updated_since: Option<i64>) -> Vec<Part> {
        let Some(conn) = open_ro(db) else {
            return Vec::new();
        };
        if has_v2(&conn)
            && conn
                .query_row(
                    "SELECT 1 FROM session_v2 WHERE id = ?1",
                    [session_id],
                    |_| Ok(()),
                )
                .is_ok()
        {
            return fetch_v2_parts(&conn, session_id, updated_since);
        }
        // `json_extract` is available because the bundled SQLite ships JSON1.
        let mut stmt = match conn.prepare(
            "SELECT p.id, json_extract(m.data, '$.role'), p.time_created, \
                    p.time_updated, p.data \
             FROM part p JOIN message m ON p.message_id = m.id \
             WHERE p.session_id = ?1 AND (?2 IS NULL OR p.time_updated >= ?2) \
             ORDER BY p.time_created ASC, p.rowid ASC",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map(rusqlite::params![session_id, updated_since], |row| {
            let id: String = row.get(0)?;
            let role: String = row.get::<_, Option<String>>(1)?.unwrap_or_default();
            let at_ms: i64 = row.get(2)?;
            let updated_at_ms: i64 = row.get(3)?;
            let data: String = row.get(4)?;
            Ok((id, role, at_ms, updated_at_ms, data))
        });
        let Ok(rows) = rows else {
            return Vec::new();
        };
        rows.flatten()
            .filter_map(|(id, role, at_ms, updated_at_ms, data)| {
                serde_json::from_str::<serde_json::Value>(&data)
                    .ok()
                    .map(|data| Part {
                        id,
                        role,
                        at_ms,
                        updated_at_ms,
                        data,
                    })
            })
            .collect()
    }
}

#[cfg(not(windows))]
use imp_v2::fetch_v2_parts;

#[cfg(not(windows))]
mod imp_v2 {
    use super::{parts_from_v2_message, Part};
    use rusqlite::Connection;

    /// An OpenCode 2 session's messages updated at or after `updated_since`
    /// (inclusive, like the 1.x query, so an in-flight step is revisited until
    /// it completes), in `seq` order, as transcript parts.
    pub fn fetch_v2_parts(
        conn: &Connection,
        session_id: &str,
        updated_since: Option<i64>,
    ) -> Vec<Part> {
        let mut stmt = match conn.prepare(
            "SELECT id, type, time_created, time_updated, data FROM session_message \
             WHERE session_id = ?1 AND (?2 IS NULL OR time_updated >= ?2) \
             ORDER BY seq ASC",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map(rusqlite::params![session_id, updated_since], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
            ))
        });
        let Ok(rows) = rows else {
            return Vec::new();
        };
        rows.flatten()
            .flat_map(|(id, kind, created, updated, data)| {
                serde_json::from_str::<serde_json::Value>(&data)
                    .map(|data| parts_from_v2_message(&id, &kind, created, updated, &data))
                    .unwrap_or_default()
            })
            .collect()
    }
}

#[cfg(windows)]
mod imp {
    use super::Part;
    use std::path::Path;

    // On Windows the SQLite layer is not compiled (the released binary stays
    // pure-Rust), so OpenCode transcript reconstruction is unavailable and these
    // are inert — the chat shows no reconstructed transcript, as it did before.
    pub fn latest_session_id(_db: &Path, _directory: &str) -> Option<String> {
        None
    }
    pub fn fetch_parts(_db: &Path, _session_id: &str, _updated_since: Option<i64>) -> Vec<Part> {
        Vec::new()
    }
}

pub use imp::{fetch_parts, latest_session_id};

// The DB layer is compiled (and thus testable) only off Windows. These tests
// build a temp SQLite DB with the subset of OpenCode's schema we read, in WAL
// mode, and assert the read-only open + role-join query behave — the crux being
// that a read-only connection reads a live WAL database (recent writes live in
// the `-wal` sidecar).
#[cfg(all(test, not(windows)))]
mod tests {
    use super::*;
    use rusqlite::Connection;

    /// Create a WAL-mode DB with OpenCode's session/message/part shape and seed
    /// rows, then close the writer (leaving the DB for a read-only reader).
    fn seed(path: &Path) {
        let conn = Connection::open(path).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        conn.execute_batch(
            "CREATE TABLE session (id TEXT PRIMARY KEY, directory TEXT, time_updated INTEGER);
             CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT, data TEXT);
             CREATE TABLE part (id TEXT PRIMARY KEY, message_id TEXT, session_id TEXT,
                                 time_created INTEGER, time_updated INTEGER, data TEXT);
             INSERT INTO session VALUES ('ses_old','/repo/wt',100);
             INSERT INTO session VALUES ('ses_new','/repo/wt',200);
             INSERT INTO session VALUES ('ses_other','/elsewhere',300);
             INSERT INTO message VALUES ('m1','ses_new','{\"role\":\"user\"}');
             INSERT INTO message VALUES ('m2','ses_new','{\"role\":\"assistant\"}');
             -- Inserted out of time order to prove the query sorts by time_created.
             INSERT INTO part VALUES ('p2','m2','ses_new',20,200,'{\"type\":\"text\",\"text\":\"hi back\"}');
             INSERT INTO part VALUES ('p1','m1','ses_new',10,100,'{\"type\":\"text\",\"text\":\"hi\"}');
             -- A part in another session must never be returned.
             INSERT INTO message VALUES ('m3','ses_other','{\"role\":\"user\"}');
             INSERT INTO part VALUES ('p3','m3','ses_other',10,300,'{\"type\":\"text\",\"text\":\"nope\"}');",
        )
        .unwrap();
    }

    #[test]
    fn latest_session_id_picks_newest_for_directory() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("opencode.db");
        seed(&db);
        assert_eq!(
            latest_session_id(&db, "/repo/wt").as_deref(),
            Some("ses_new"),
            "newest by time_updated within the directory"
        );
        assert_eq!(latest_session_id(&db, "/no/such").as_deref(), None);
    }

    #[test]
    fn fetch_parts_joins_role_and_orders_by_time() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("opencode.db");
        seed(&db);
        let parts = fetch_parts(&db, "ses_new", None);
        let got: Vec<(&str, &str, &str)> = parts
            .iter()
            .map(|p| {
                (
                    p.id.as_str(),
                    p.role.as_str(),
                    p.data.get("text").and_then(|v| v.as_str()).unwrap_or(""),
                )
            })
            .collect();
        assert_eq!(
            got,
            vec![("p1", "user", "hi"), ("p2", "assistant", "hi back")],
            "ordered by time_created, role joined from message, other session excluded"
        );
    }

    #[test]
    fn fetch_parts_filters_on_inclusive_update_cursor() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("opencode.db");
        seed(&db);

        let parts = fetch_parts(&db, "ses_new", Some(200));
        assert_eq!(
            parts.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
            vec!["p2"],
            "the newest update boundary is revisited without rereading history"
        );
    }

    /// OpenCode 2's tables (the subset read), with rows shaped like a real
    /// 2.2.6 run: a user prompt, a step with reasoning and a `shell` tool call,
    /// a final text step, the `idle` marker, plus a subagent session and an
    /// unfinished step in another session.
    fn seed_v2(path: &Path) {
        let conn = Connection::open(path).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        conn.execute_batch(
            r#"CREATE TABLE session_v2 (id TEXT PRIMARY KEY, parent_id TEXT, directory TEXT, time_updated INTEGER);
             CREATE TABLE session_message (id TEXT PRIMARY KEY, session_id TEXT, type TEXT, seq INTEGER,
                                           time_created INTEGER, time_updated INTEGER, data TEXT);
             INSERT INTO session_v2 VALUES ('ses_v2', NULL, '/repo/wt', 500);
             INSERT INTO session_v2 VALUES ('ses_child', 'ses_v2', '/repo/wt', 900);
             INSERT INTO session_v2 VALUES ('ses_live', NULL, '/repo/live', 600);
             INSERT INTO session_message VALUES ('msg_u','ses_v2','user',4,100,100,
               '{"time":{"created":100},"text":"Run echo hi","files":[]}');
             INSERT INTO session_message VALUES ('msg_a1','ses_v2','assistant',7,110,130,
               '{"time":{"created":110,"completed":130},"content":[{"type":"reasoning","text":"I will run it."},{"type":"tool","id":"call_1","name":"shell","state":{"status":"completed","input":{"command":"echo hi"}}}]}');
             INSERT INTO session_message VALUES ('msg_a2','ses_v2','assistant',16,140,160,
               '{"time":{"created":140,"completed":160},"content":[{"type":"text","text":"done"}]}');
             INSERT INTO session_message VALUES ('msg_i','ses_v2','idle',21,170,170,
               '{"time":{"created":170},"outcome":"succeeded"}');
             INSERT INTO session_message VALUES ('msg_s','ses_live','assistant',3,200,210,
               '{"time":{"created":200},"content":[{"type":"text","text":"still typ"}]}');"#,
        )
        .unwrap();
    }

    #[test]
    fn v2_latest_session_is_the_newest_top_level_one() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("opencode.db");
        seed_v2(&db);
        assert_eq!(
            latest_session_id(&db, "/repo/wt").as_deref(),
            Some("ses_v2"),
            "a newer subagent session in the same worktree is not the conversation"
        );
    }

    #[test]
    fn v2_session_wins_and_1x_sessions_are_the_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("opencode.db");
        // Both schemas in one file, as after an upgrade.
        seed(&db);
        seed_v2(&db);
        assert_eq!(
            latest_session_id(&db, "/repo/wt").as_deref(),
            Some("ses_v2")
        );
        assert_eq!(
            latest_session_id(&db, "/elsewhere").as_deref(),
            Some("ses_other"),
            "a worktree with only 1.x sessions still finds them"
        );
        assert_eq!(
            fetch_parts(&db, "ses_new", None).len(),
            2,
            "a 1.x session is still read from the 1.x tables"
        );
    }

    #[test]
    fn v2_messages_become_1x_shaped_parts_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("opencode.db");
        seed_v2(&db);
        let parts = fetch_parts(&db, "ses_v2", None);
        let got: Vec<(&str, &str, &str)> = parts
            .iter()
            .map(|p| {
                (
                    p.id.as_str(),
                    p.role.as_str(),
                    p.data.get("type").and_then(Value::as_str).unwrap_or(""),
                )
            })
            .collect();
        assert_eq!(
            got,
            vec![
                ("msg_u:0", "user", "text"),
                ("msg_a1:0", "assistant", "reasoning"),
                ("msg_a1:1", "assistant", "tool"),
                ("msg_a2:0", "assistant", "text"),
            ],
            "seq order, one part per content item, the idle marker skipped"
        );
        assert_eq!(parts[0].data["text"], "Run echo hi");
        assert_eq!(parts[2].data["tool"], "shell");
        assert_eq!(parts[2].data["state"]["input"]["command"], "echo hi");
        assert_eq!(
            parts[3].data["time"]["end"], 160,
            "completed text is released"
        );
        assert_eq!(parts[3].at_ms, 140);
        assert_eq!(parts[3].updated_at_ms, 160);
    }

    #[test]
    fn v2_unfinished_text_has_no_end_and_the_cursor_revisits_it() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("opencode.db");
        seed_v2(&db);
        let parts = fetch_parts(&db, "ses_live", Some(210));
        assert_eq!(
            parts.len(),
            1,
            "the inclusive cursor revisits the in-flight step"
        );
        assert!(
            parts[0].data["time"]["end"].is_null(),
            "streamed prose waits until the step completes"
        );
        assert!(fetch_parts(&db, "ses_v2", Some(161))
            .iter()
            .all(|p| p.id == "msg_i:0"));
    }

    #[test]
    fn missing_db_is_a_safe_empty() {
        let db = Path::new("/no/such/opencode.db");
        assert_eq!(latest_session_id(db, "/repo/wt"), None);
        assert!(fetch_parts(db, "ses_new", None).is_empty());
    }
}
