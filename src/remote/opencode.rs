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
    /// The parsed `part.data` JSON (`type`, and per-type fields).
    pub data: Value,
}

#[cfg(not(windows))]
mod imp {
    use super::Part;
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

    /// The id of the newest OpenCode session whose worktree is `directory`, or
    /// `None` if the DB has none (or cannot be read).
    pub fn latest_session_id(db: &Path, directory: &str) -> Option<String> {
        let conn = open_ro(db)?;
        conn.query_row(
            "SELECT id FROM session WHERE directory = ?1 \
             ORDER BY time_updated DESC LIMIT 1",
            [directory],
            |row| row.get::<_, String>(0),
        )
        .ok()
    }

    /// Every `part` of `session_id`, joined to its message role and ordered as it
    /// was written (`time_created`, then rowid to break ties within a message).
    /// Returns empty on any read error.
    pub fn fetch_parts(db: &Path, session_id: &str) -> Vec<Part> {
        let Some(conn) = open_ro(db) else {
            return Vec::new();
        };
        // `json_extract` is available because the bundled SQLite ships JSON1.
        let mut stmt = match conn.prepare(
            "SELECT p.id, json_extract(m.data, '$.role'), p.time_created, p.data \
             FROM part p JOIN message m ON p.message_id = m.id \
             WHERE p.session_id = ?1 \
             ORDER BY p.time_created ASC, p.rowid ASC",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map([session_id], |row| {
            let id: String = row.get(0)?;
            let role: String = row.get::<_, Option<String>>(1)?.unwrap_or_default();
            let at_ms: i64 = row.get(2)?;
            let data: String = row.get(3)?;
            Ok((id, role, at_ms, data))
        });
        let Ok(rows) = rows else {
            return Vec::new();
        };
        rows.flatten()
            .filter_map(|(id, role, at_ms, data)| {
                serde_json::from_str::<serde_json::Value>(&data)
                    .ok()
                    .map(|data| Part {
                        id,
                        role,
                        at_ms,
                        data,
                    })
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
    pub fn fetch_parts(_db: &Path, _session_id: &str) -> Vec<Part> {
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
                                time_created INTEGER, data TEXT);
             INSERT INTO session VALUES ('ses_old','/repo/wt',100);
             INSERT INTO session VALUES ('ses_new','/repo/wt',200);
             INSERT INTO session VALUES ('ses_other','/elsewhere',300);
             INSERT INTO message VALUES ('m1','ses_new','{\"role\":\"user\"}');
             INSERT INTO message VALUES ('m2','ses_new','{\"role\":\"assistant\"}');
             -- Inserted out of time order to prove the query sorts by time_created.
             INSERT INTO part VALUES ('p2','m2','ses_new',20,'{\"type\":\"text\",\"text\":\"hi back\"}');
             INSERT INTO part VALUES ('p1','m1','ses_new',10,'{\"type\":\"text\",\"text\":\"hi\"}');
             -- A part in another session must never be returned.
             INSERT INTO message VALUES ('m3','ses_other','{\"role\":\"user\"}');
             INSERT INTO part VALUES ('p3','m3','ses_other',10,'{\"type\":\"text\",\"text\":\"nope\"}');",
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
        let parts = fetch_parts(&db, "ses_new");
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
    fn missing_db_is_a_safe_empty() {
        let db = Path::new("/no/such/opencode.db");
        assert_eq!(latest_session_id(db, "/repo/wt"), None);
        assert!(fetch_parts(db, "ses_new").is_empty());
    }
}
