//! File-backed [`RelayStore`] implementation on top of a vendored SQLite
//! (`rusqlite`, `bundled` feature).
//!
//! **Why this exists.** [`super::InMemoryStore`] loses every device
//! registration, pairing, claim token, and per-pairing sequence high-water mark
//! on restart, so an Azure Container Apps redeploy repeatedly broke working
//! pairings: both endpoints keep their local pairing records but the relay no
//! longer recognizes them, auth fails with "unknown device", and both sides hang
//! until re-pairing (remote-control-b0f). This store persists that state to a
//! single SQLite file so a restart is transparent to already-paired endpoints.
//!
//! **Design.** The store holds one [`rusqlite::Connection`] behind a
//! [`std::sync::Mutex`], mirroring [`super::InMemoryStore`], which serializes all
//! its state behind a single mutex. The `async` trait methods complete
//! synchronously (SQLite calls are fast, local, and the relay's throughput is
//! bounded by network I/O, not the store); no async SQL driver is pulled in.
//! Multi-statement mutations (create/revoke pairing, enqueue-with-overflow,
//! redeem, ack) run inside a transaction so a mid-operation failure cannot leave
//! a half-applied write.
//!
//! **Semantics parity.** The queue and claim-token rules are the same ones
//! encoded by [`crate::queue::SenderQueue`] and [`crate::claims::ClaimTable`];
//! they are re-expressed here in SQL because those types own private state and
//! offer no way to rehydrate an arbitrary `(high_water, ack_cursor, buffer)`
//! snapshot from disk. Claim-token TTL is absolute wall-clock (`expires_at_ms`),
//! so a token that expired while the relay was down still fails redemption after
//! reload — expired tokens never resurrect.
//!
//! **Persistence caveat (out of scope here).** SQLite persists to whatever file
//! path it is given; whether that path survives a redeploy is a *deployment*
//! concern. On Azure Container Apps the container filesystem is ephemeral, so a
//! deployer must point `FLIGHTDECK_RELAY_STORE=sqlite:<path>` at a mounted
//! Azure Files volume (or move to a networked store) for state to outlive a
//! revision swap. This module only guarantees survival across a process restart
//! on stable storage.

use std::path::Path;
use std::sync::Mutex;

use async_trait::async_trait;
use flightdeck_remote_protocol::{ApnsEnvironment, DeviceId, EncryptedEnvelope, PairingId, Role};
use rusqlite::{params, Connection, OptionalExtension};

use crate::claims::{ClaimError, RedeemedClaim};
use crate::queue::{AppendOutcome, QueueError, ResumeOutcome};
use crate::store::{PairingMembers, RelayStore, RevokeOutcome, StoreError};

/// Schema for the relay's durable state. `IF NOT EXISTS` throughout so opening
/// an existing database is a no-op and opening a fresh one initializes it.
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS devices (
    device_id         TEXT PRIMARY KEY,
    public_key        TEXT,
    key_agreement_key TEXT
);
CREATE TABLE IF NOT EXISTS pairings (
    pairing_id   TEXT PRIMARY KEY,
    desktop      TEXT NOT NULL,
    phone        TEXT,
    machine_name TEXT
);
CREATE TABLE IF NOT EXISTS claims (
    token         TEXT PRIMARY KEY,
    pairing_id    TEXT NOT NULL,
    desktop       TEXT NOT NULL,
    expires_at_ms INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS queue_streams (
    pairing_id TEXT NOT NULL,
    sender     INTEGER NOT NULL,
    high_water INTEGER NOT NULL DEFAULT 0,
    ack_cursor INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (pairing_id, sender)
);
CREATE TABLE IF NOT EXISTS queue_envelopes (
    pairing_id  TEXT NOT NULL,
    sender      INTEGER NOT NULL,
    seq         INTEGER NOT NULL,
    sent_at_ms  INTEGER NOT NULL,
    nonce       TEXT NOT NULL,
    ciphertext  TEXT NOT NULL,
    PRIMARY KEY (pairing_id, sender, seq)
);
CREATE TABLE IF NOT EXISTS push_tokens (
    pairing_id TEXT PRIMARY KEY,
    token      TEXT NOT NULL,
    apns_env   TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value INTEGER NOT NULL
);
";

/// Map a sender [`Role`] to the small integer tag stored in the `sender`
/// columns. Mirrors `super::QueueKey`'s direction tag so desktop and phone
/// streams stay independent.
fn sender_tag(role: Role) -> i64 {
    match role {
        Role::Desktop => 0,
        Role::Phone => 1,
    }
}

/// Serialize an [`ApnsEnvironment`] for the `push_tokens.apns_env` column.
fn apns_env_str(env: ApnsEnvironment) -> &'static str {
    match env {
        ApnsEnvironment::Sandbox => "sandbox",
        ApnsEnvironment::Production => "production",
    }
}

/// Parse a stored `apns_env` value. Any unexpected value is treated as
/// production (the conservative choice — production tokens must never be sent to
/// the sandbox gateway); in practice only the two strings above are ever
/// written.
fn apns_env_from_str(s: &str) -> ApnsEnvironment {
    if s == "sandbox" {
        ApnsEnvironment::Sandbox
    } else {
        ApnsEnvironment::Production
    }
}

/// File-backed [`RelayStore`]. See the module docs for the rationale and the
/// deployment caveat.
pub struct SqliteStore {
    conn: Mutex<Connection>,
    /// Per-`(pairing, sender)` queue bound, mirroring
    /// [`super::InMemoryStore`]'s `queue_max_per_pairing`.
    queue_max_per_pairing: usize,
}

impl SqliteStore {
    /// Open (creating if absent) the SQLite database at `path` and ensure the
    /// schema exists. Queues each hold at most `queue_max_per_pairing` un-acked
    /// envelopes, matching [`super::InMemoryStore::new`].
    pub fn open(path: impl AsRef<Path>, queue_max_per_pairing: usize) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        // WAL keeps writes durable while allowing the single writer to proceed
        // without blocking readers; `synchronous = NORMAL` is the standard,
        // crash-safe pairing with WAL.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: Mutex::new(conn),
            queue_max_per_pairing: queue_max_per_pairing.max(1),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        // As with `InMemoryStore`, a poisoned lock means a prior panic
        // mid-mutation, which leaves the store unrecoverable; surface it.
        self.conn.lock().expect("relay sqlite store mutex poisoned")
    }
}

/// A SQLite failure inside a store method is unrecoverable at this layer (disk
/// gone, corruption) and the infallible trait has no channel to report it, so we
/// panic — matching `InMemoryStore`'s treatment of a poisoned mutex.
const DB_ERR: &str = "relay sqlite store operation failed";

#[async_trait]
impl RelayStore for SqliteStore {
    async fn register_device(&self, device: DeviceId, public_key_b64: String) {
        self.lock()
            .execute(
                "INSERT INTO devices (device_id, public_key) VALUES (?1, ?2)
                 ON CONFLICT(device_id) DO UPDATE SET public_key = excluded.public_key",
                params![device.as_str(), public_key_b64],
            )
            .expect(DB_ERR);
    }

    async fn device_public_key(&self, device: &DeviceId) -> Option<String> {
        self.lock()
            .query_row(
                "SELECT public_key FROM devices WHERE device_id = ?1",
                params![device.as_str()],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .expect(DB_ERR)
            .flatten()
    }

    async fn register_key_agreement_key(&self, device: DeviceId, key_agreement_key_b64: String) {
        self.lock()
            .execute(
                "INSERT INTO devices (device_id, key_agreement_key) VALUES (?1, ?2)
                 ON CONFLICT(device_id) DO UPDATE SET key_agreement_key = excluded.key_agreement_key",
                params![device.as_str(), key_agreement_key_b64],
            )
            .expect(DB_ERR);
    }

    async fn device_key_agreement_key(&self, device: &DeviceId) -> Option<String> {
        self.lock()
            .query_row(
                "SELECT key_agreement_key FROM devices WHERE device_id = ?1",
                params![device.as_str()],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .expect(DB_ERR)
            .flatten()
    }

    async fn create_pairing(&self, desktop: DeviceId) -> PairingId {
        let conn = self.lock();
        // Persist the monotonic counter so pairing ids stay unique and readable
        // across restarts, matching `InMemoryStore`'s in-process counter.
        conn.execute(
            "INSERT INTO meta (key, value) VALUES ('pairing_counter', 1)
             ON CONFLICT(key) DO UPDATE SET value = value + 1",
            [],
        )
        .expect(DB_ERR);
        let counter: i64 = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'pairing_counter'",
                [],
                |row| row.get(0),
            )
            .expect(DB_ERR);
        let id = PairingId::new(format!(
            "pair_{:04}_{}",
            counter,
            crate::ids::random_suffix()
        ));
        conn.execute(
            "INSERT INTO pairings (pairing_id, desktop, phone, machine_name)
             VALUES (?1, ?2, NULL, NULL)",
            params![id.as_str(), desktop.as_str()],
        )
        .expect(DB_ERR);
        id
    }

    async fn add_phone_to_pairing(
        &self,
        pairing: &PairingId,
        phone: DeviceId,
    ) -> Result<DeviceId, StoreError> {
        let conn = self.lock();
        let desktop: Option<String> = conn
            .query_row(
                "SELECT desktop FROM pairings WHERE pairing_id = ?1",
                params![pairing.as_str()],
                |row| row.get(0),
            )
            .optional()
            .expect(DB_ERR);
        let Some(desktop) = desktop else {
            return Err(StoreError::UnknownPairing);
        };
        conn.execute(
            "UPDATE pairings SET phone = ?1 WHERE pairing_id = ?2",
            params![phone.as_str(), pairing.as_str()],
        )
        .expect(DB_ERR);
        Ok(DeviceId::new(desktop))
    }

    async fn pairing_members(&self, pairing: &PairingId) -> Option<PairingMembers> {
        self.lock()
            .query_row(
                "SELECT desktop, phone, machine_name FROM pairings WHERE pairing_id = ?1",
                params![pairing.as_str()],
                |row| {
                    Ok(PairingMembers {
                        desktop: DeviceId::new(row.get::<_, String>(0)?),
                        phone: row.get::<_, Option<String>>(1)?.map(DeviceId::new),
                        machine_name: row.get::<_, Option<String>>(2)?,
                    })
                },
            )
            .optional()
            .expect(DB_ERR)
    }

    async fn set_machine_name(&self, pairing: &PairingId, machine_name: String) {
        // No-op when the pairing does not exist (UPDATE matches zero rows),
        // matching `InMemoryStore`.
        self.lock()
            .execute(
                "UPDATE pairings SET machine_name = ?1 WHERE pairing_id = ?2",
                params![machine_name, pairing.as_str()],
            )
            .expect(DB_ERR);
    }

    async fn machine_name(&self, pairing: &PairingId) -> Option<String> {
        self.lock()
            .query_row(
                "SELECT machine_name FROM pairings WHERE pairing_id = ?1",
                params![pairing.as_str()],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .expect(DB_ERR)
            .flatten()
    }

    async fn revoke_pairing(&self, pairing: &PairingId, requester: &DeviceId) -> RevokeOutcome {
        let mut conn = self.lock();
        let members: Option<PairingMembers> = conn
            .query_row(
                "SELECT desktop, phone, machine_name FROM pairings WHERE pairing_id = ?1",
                params![pairing.as_str()],
                |row| {
                    Ok(PairingMembers {
                        desktop: DeviceId::new(row.get::<_, String>(0)?),
                        phone: row.get::<_, Option<String>>(1)?.map(DeviceId::new),
                        machine_name: row.get::<_, Option<String>>(2)?,
                    })
                },
            )
            .optional()
            .expect(DB_ERR);
        let Some(members) = members else {
            // Idempotent: revoking an already-gone pairing is a success no-op.
            return RevokeOutcome::AlreadyGone;
        };
        if !members.contains(requester) {
            // Security (spec §10.2): only a member may revoke.
            return RevokeOutcome::NotMember;
        }
        // Authorized: remove the pairing and every trace of it under one
        // transaction so the cleanup is atomic.
        let tx = conn.transaction().expect(DB_ERR);
        let p = pairing.as_str();
        tx.execute("DELETE FROM pairings WHERE pairing_id = ?1", params![p])
            .expect(DB_ERR);
        tx.execute("DELETE FROM claims WHERE pairing_id = ?1", params![p])
            .expect(DB_ERR);
        tx.execute(
            "DELETE FROM queue_streams WHERE pairing_id = ?1",
            params![p],
        )
        .expect(DB_ERR);
        tx.execute(
            "DELETE FROM queue_envelopes WHERE pairing_id = ?1",
            params![p],
        )
        .expect(DB_ERR);
        tx.execute("DELETE FROM push_tokens WHERE pairing_id = ?1", params![p])
            .expect(DB_ERR);
        tx.commit().expect(DB_ERR);
        RevokeOutcome::Removed(members)
    }

    async fn issue_claim(
        &self,
        token: String,
        pairing: PairingId,
        desktop: DeviceId,
        expires_at_ms: i64,
    ) {
        self.lock()
            .execute(
                "INSERT INTO claims (token, pairing_id, desktop, expires_at_ms)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(token) DO UPDATE SET
                    pairing_id = excluded.pairing_id,
                    desktop = excluded.desktop,
                    expires_at_ms = excluded.expires_at_ms",
                params![token, pairing.as_str(), desktop.as_str(), expires_at_ms],
            )
            .expect(DB_ERR);
    }

    async fn claim_token_is_free(&self, token: &str, now_ms: i64) -> bool {
        // A token is *taken* only while it is live: present AND not past its TTL
        // (`now_ms <= expires_at_ms`), mirroring `ClaimTable::contains`. An
        // expired-but-unswept entry counts as **free** so a colliding
        // `claim_token_hint` can be reused (remote-control-0ef.16); the entry
        // will be physically reaped by the periodic sweep.
        let live: i64 = self
            .lock()
            .query_row(
                "SELECT COUNT(*) FROM claims WHERE token = ?1 AND ?2 <= expires_at_ms",
                params![token, now_ms],
                |row| row.get(0),
            )
            .expect(DB_ERR);
        live == 0
    }

    async fn redeem_claim(&self, token: &str, now_ms: i64) -> Result<RedeemedClaim, ClaimError> {
        let mut conn = self.lock();
        let tx = conn.transaction().expect(DB_ERR);
        let row: Option<(String, String, i64)> = tx
            .query_row(
                "SELECT pairing_id, desktop, expires_at_ms FROM claims WHERE token = ?1",
                params![token],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .expect(DB_ERR);
        let Some((pairing_id, desktop, expires_at_ms)) = row else {
            // Unknown tokens are left untouched (nothing to consume).
            return Err(ClaimError::Unknown);
        };
        // Single-use: consume the entry whether it succeeds or is expired, so it
        // cannot be retried (matches `ClaimTable::redeem`).
        tx.execute("DELETE FROM claims WHERE token = ?1", params![token])
            .expect(DB_ERR);
        tx.commit().expect(DB_ERR);
        if now_ms > expires_at_ms {
            return Err(ClaimError::Expired);
        }
        Ok(RedeemedClaim {
            pairing_id: PairingId::new(pairing_id),
            desktop_device: DeviceId::new(desktop),
        })
    }

    async fn sweep_expired_claims(&self, now_ms: i64) -> usize {
        // Evict every entry past its TTL (`now_ms > expires_at_ms`) and report
        // how many were removed, matching `ClaimTable::sweep_expired` (which
        // retains `now_ms <= expires_at_ms`). An entry exactly at its boundary is
        // still live and kept. `Connection::execute` returns the affected count.
        self.lock()
            .execute(
                "DELETE FROM claims WHERE expires_at_ms < ?1",
                params![now_ms],
            )
            .expect(DB_ERR)
    }

    async fn enqueue(&self, env: EncryptedEnvelope) -> Result<AppendOutcome, QueueError> {
        let max = self.queue_max_per_pairing;
        let mut conn = self.lock();
        let tx = conn.transaction().expect(DB_ERR);
        let pairing = env.pairing_id.as_str();
        let tag = sender_tag(env.sender);

        let (high_water, ack_cursor): (u64, u64) = tx
            .query_row(
                "SELECT high_water, ack_cursor FROM queue_streams
                 WHERE pairing_id = ?1 AND sender = ?2",
                params![pairing, tag],
                |r| Ok((r.get::<_, i64>(0)? as u64, r.get::<_, i64>(1)? as u64)),
            )
            .optional()
            .expect(DB_ERR)
            .unwrap_or((0, 0));

        let expected = high_water + 1;
        if env.seq == expected {
            let buf_len: i64 = tx
                .query_row(
                    "SELECT COUNT(*) FROM queue_envelopes WHERE pairing_id = ?1 AND sender = ?2",
                    params![pairing, tag],
                    |r| r.get(0),
                )
                .expect(DB_ERR);
            let overflow = buf_len as usize >= max;
            if overflow {
                // Drop the oldest un-acked envelope (lowest seq) to make room,
                // matching `SenderQueue`'s drop-oldest overflow policy.
                tx.execute(
                    "DELETE FROM queue_envelopes
                     WHERE pairing_id = ?1 AND sender = ?2
                       AND seq = (SELECT MIN(seq) FROM queue_envelopes
                                  WHERE pairing_id = ?1 AND sender = ?2)",
                    params![pairing, tag],
                )
                .expect(DB_ERR);
            }
            tx.execute(
                "INSERT INTO queue_envelopes
                    (pairing_id, sender, seq, sent_at_ms, nonce, ciphertext)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    pairing,
                    tag,
                    env.seq as i64,
                    env.sent_at_ms,
                    env.nonce,
                    env.ciphertext
                ],
            )
            .expect(DB_ERR);
            tx.execute(
                "INSERT INTO queue_streams (pairing_id, sender, high_water, ack_cursor)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(pairing_id, sender) DO UPDATE SET high_water = excluded.high_water",
                params![pairing, tag, env.seq as i64, ack_cursor as i64],
            )
            .expect(DB_ERR);
            tx.commit().expect(DB_ERR);
            Ok(AppendOutcome::Accepted { overflow })
        } else if high_water > 0 && env.seq == high_water {
            // Idempotent re-send of the current head; nothing changes.
            Ok(AppendOutcome::Duplicate)
        } else {
            Err(QueueError::SeqViolation {
                expected,
                got: env.seq,
            })
        }
    }

    async fn resume(&self, pairing: &PairingId, sender: Role, from_seq: u64) -> ResumeOutcome {
        let conn = self.lock();
        let tag = sender_tag(sender);
        let p = pairing.as_str();

        // The stream row exists iff this `(pairing, sender)` queue was ever
        // written. Absent → a clean, empty replay (mirrors `InMemoryStore`'s
        // `queues.get(..).map(..).unwrap_or(Replay(empty))`).
        let ack_cursor: Option<u64> = conn
            .query_row(
                "SELECT ack_cursor FROM queue_streams WHERE pairing_id = ?1 AND sender = ?2",
                params![p, tag],
                |r| r.get::<_, i64>(0).map(|v| v as u64),
            )
            .optional()
            .expect(DB_ERR);
        let Some(ack_cursor) = ack_cursor else {
            return ResumeOutcome::Replay(Vec::new());
        };

        // Lowest retained (un-acked, un-dropped) seq. `MIN` over an empty buffer
        // is SQL `NULL` → `None`, meaning the buffer is empty (everything acked),
        // which is always a clean — possibly empty — replay.
        let front: Option<u64> = conn
            .query_row(
                "SELECT MIN(seq) FROM queue_envelopes WHERE pairing_id = ?1 AND sender = ?2",
                params![p, tag],
                |r| r.get::<_, Option<i64>>(0),
            )
            .expect(DB_ERR)
            .map(|v| v as u64);

        // Mirror `SenderQueue::resume` (remote-control-0ef.7): a hole that forces
        // a resync exists only when a drop-oldest overflow pushed the front above
        // `ack_cursor + 1` AND the receiver is asking for a seq below that front.
        // An ack-pruned front is contiguous (`front == ack_cursor + 1`), so a
        // resume from before it is a normal replay of the retained tail, never a
        // resync.
        if let Some(front) = front {
            let overflow_gap = front > ack_cursor + 1;
            if overflow_gap && from_seq + 1 < front {
                return ResumeOutcome::Resync;
            }
        }

        // Clean replay: every retained envelope with `seq > from_seq`, in order.
        let mut stmt = conn
            .prepare(
                "SELECT seq, sent_at_ms, nonce, ciphertext FROM queue_envelopes
                 WHERE pairing_id = ?1 AND sender = ?2 AND seq > ?3
                 ORDER BY seq ASC",
            )
            .expect(DB_ERR);
        let rows = stmt
            .query_map(params![p, tag, from_seq as i64], |row| {
                Ok(EncryptedEnvelope {
                    pairing_id: pairing.clone(),
                    seq: row.get::<_, i64>(0)? as u64,
                    sender,
                    sent_at_ms: row.get(1)?,
                    nonce: row.get(2)?,
                    ciphertext: row.get(3)?,
                })
            })
            .expect(DB_ERR);
        ResumeOutcome::Replay(rows.map(|r| r.expect(DB_ERR)).collect())
    }

    async fn ack(&self, pairing: &PairingId, sender: Role, cursor: u64) {
        let mut conn = self.lock();
        let tag = sender_tag(sender);
        let stream: Option<(u64, u64)> = conn
            .query_row(
                "SELECT high_water, ack_cursor FROM queue_streams
                 WHERE pairing_id = ?1 AND sender = ?2",
                params![pairing.as_str(), tag],
                |r| Ok((r.get::<_, i64>(0)? as u64, r.get::<_, i64>(1)? as u64)),
            )
            .optional()
            .expect(DB_ERR);
        let Some((high_water, ack_cursor)) = stream else {
            // Ack on an absent queue is a no-op, matching `InMemoryStore`.
            return;
        };
        let cursor = cursor.min(high_water);
        if cursor <= ack_cursor {
            return;
        }
        let tx = conn.transaction().expect(DB_ERR);
        tx.execute(
            "UPDATE queue_streams SET ack_cursor = ?1 WHERE pairing_id = ?2 AND sender = ?3",
            params![cursor as i64, pairing.as_str(), tag],
        )
        .expect(DB_ERR);
        tx.execute(
            "DELETE FROM queue_envelopes WHERE pairing_id = ?1 AND sender = ?2 AND seq <= ?3",
            params![pairing.as_str(), tag, cursor as i64],
        )
        .expect(DB_ERR);
        tx.commit().expect(DB_ERR);
    }

    async fn register_push_token(&self, pairing: PairingId, token: String, env: ApnsEnvironment) {
        self.lock()
            .execute(
                "INSERT INTO push_tokens (pairing_id, token, apns_env) VALUES (?1, ?2, ?3)
                 ON CONFLICT(pairing_id) DO UPDATE SET
                    token = excluded.token, apns_env = excluded.apns_env",
                params![pairing.as_str(), token, apns_env_str(env)],
            )
            .expect(DB_ERR);
    }

    async fn unregister_push_token(&self, pairing: &PairingId) {
        // Idempotent: DELETE of an absent row matches zero rows.
        self.lock()
            .execute(
                "DELETE FROM push_tokens WHERE pairing_id = ?1",
                params![pairing.as_str()],
            )
            .expect(DB_ERR);
    }

    async fn push_token(&self, pairing: &PairingId) -> Option<(String, ApnsEnvironment)> {
        self.lock()
            .query_row(
                "SELECT token, apns_env FROM push_tokens WHERE pairing_id = ?1",
                params![pairing.as_str()],
                |row| {
                    let token: String = row.get(0)?;
                    let env: String = row.get(1)?;
                    Ok((token, apns_env_from_str(&env)))
                },
            )
            .optional()
            .expect(DB_ERR)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(dir: &std::path::Path) -> SqliteStore {
        SqliteStore::open(dir.join("relay.db"), 1000).expect("open sqlite store")
    }

    fn env(pairing: &PairingId, sender: Role, seq: u64) -> EncryptedEnvelope {
        EncryptedEnvelope {
            pairing_id: pairing.clone(),
            seq,
            sender,
            sent_at_ms: 1000 + seq as i64,
            nonce: "bm9uY2U=".into(),
            ciphertext: format!("ciphertext-{seq}"),
        }
    }

    /// Unwrap a clean replay's envelopes, failing on a resync signal.
    fn replayed(outcome: ResumeOutcome) -> Vec<EncryptedEnvelope> {
        match outcome {
            ResumeOutcome::Replay(v) => v,
            ResumeOutcome::Resync => panic!("expected a clean replay, got Resync"),
        }
    }

    #[tokio::test]
    async fn device_registration_round_trips() {
        let tmp = tempdir();
        let s = store(&tmp);
        let dev = DeviceId::new("dev_1");
        assert_eq!(s.device_public_key(&dev).await, None);
        s.register_device(dev.clone(), "pk".into()).await;
        assert_eq!(s.device_public_key(&dev).await, Some("pk".into()));

        // Identity and key-agreement keys are stored independently on the same
        // device row without clobbering each other.
        s.register_key_agreement_key(dev.clone(), "ka".into()).await;
        assert_eq!(s.device_key_agreement_key(&dev).await, Some("ka".into()));
        assert_eq!(s.device_public_key(&dev).await, Some("pk".into()));
    }

    #[tokio::test]
    async fn ka_key_before_identity_key_does_not_clobber() {
        let tmp = tempdir();
        let s = store(&tmp);
        let dev = DeviceId::new("dev_1");
        s.register_key_agreement_key(dev.clone(), "ka".into()).await;
        s.register_device(dev.clone(), "pk".into()).await;
        assert_eq!(s.device_key_agreement_key(&dev).await, Some("ka".into()));
        assert_eq!(s.device_public_key(&dev).await, Some("pk".into()));
    }

    #[tokio::test]
    async fn claim_ttl_and_single_use() {
        let tmp = tempdir();
        let s = store(&tmp);
        let pairing = PairingId::new("pair_1");
        let desktop = DeviceId::new("dev_mac");
        s.issue_claim("tok".into(), pairing.clone(), desktop.clone(), 10_000)
            .await;
        assert!(!s.claim_token_is_free("tok", 5_000).await);
        // Redeems once at/under expiry.
        let claim = s.redeem_claim("tok", 10_000).await.expect("redeem");
        assert_eq!(claim.pairing_id, pairing);
        assert_eq!(claim.desktop_device, desktop);
        // Single-use: gone afterwards.
        assert_eq!(s.redeem_claim("tok", 5_000).await, Err(ClaimError::Unknown));
        assert!(s.claim_token_is_free("tok", 5_000).await);
    }

    #[tokio::test]
    async fn expired_claim_is_consumed_not_resurrected() {
        let tmp = tempdir();
        let s = store(&tmp);
        s.issue_claim(
            "tok".into(),
            PairingId::new("pair_1"),
            DeviceId::new("dev_mac"),
            10_000,
        )
        .await;
        assert_eq!(
            s.redeem_claim("tok", 10_001).await,
            Err(ClaimError::Expired)
        );
        // Consumed even though expired.
        assert_eq!(s.redeem_claim("tok", 5_000).await, Err(ClaimError::Unknown));
    }

    #[tokio::test]
    async fn queue_gapless_dedup_and_overflow() {
        let tmp = tempdir();
        let s = SqliteStore::open(tmp.join("relay.db"), 3).expect("open");
        let pairing = PairingId::new("pair_1");
        // Gapless from 1.
        for seq in 1..=3 {
            assert_eq!(
                s.enqueue(env(&pairing, Role::Desktop, seq)).await,
                Ok(AppendOutcome::Accepted { overflow: false })
            );
        }
        // Gap is rejected, high-water unchanged.
        assert_eq!(
            s.enqueue(env(&pairing, Role::Desktop, 5)).await,
            Err(QueueError::SeqViolation {
                expected: 4,
                got: 5
            })
        );
        // Duplicate of head is tolerated.
        assert_eq!(
            s.enqueue(env(&pairing, Role::Desktop, 3)).await,
            Ok(AppendOutcome::Duplicate)
        );
        // Fourth distinct push overflows: oldest (seq 1) dropped.
        assert_eq!(
            s.enqueue(env(&pairing, Role::Desktop, 4)).await,
            Ok(AppendOutcome::Accepted { overflow: true })
        );
        // The oldest (seq 1) was shed, so a resume from 0 asks for a lost seq and
        // must RESYNC rather than replay a hole (remote-control-0ef.7). Resuming
        // from seq 2 (the front's predecessor) replays the retained tail cleanly.
        assert_eq!(
            s.resume(&pairing, Role::Desktop, 0).await,
            ResumeOutcome::Resync
        );
        let seqs: Vec<u64> = replayed(s.resume(&pairing, Role::Desktop, 1).await)
            .iter()
            .map(|e| e.seq)
            .collect();
        assert_eq!(seqs, vec![2, 3, 4]);
    }

    #[tokio::test]
    async fn queues_isolated_per_sender_and_ack_prunes() {
        let tmp = tempdir();
        let s = store(&tmp);
        let pairing = PairingId::new("pair_1");
        s.enqueue(env(&pairing, Role::Desktop, 1)).await.unwrap();
        s.enqueue(env(&pairing, Role::Phone, 1)).await.unwrap();
        s.enqueue(env(&pairing, Role::Desktop, 2)).await.unwrap();

        let d: Vec<u64> = replayed(s.resume(&pairing, Role::Desktop, 0).await)
            .iter()
            .map(|e| e.seq)
            .collect();
        let p: Vec<u64> = replayed(s.resume(&pairing, Role::Phone, 0).await)
            .iter()
            .map(|e| e.seq)
            .collect();
        assert_eq!(d, vec![1, 2]);
        assert_eq!(p, vec![1]);

        s.ack(&pairing, Role::Desktop, 1).await;
        let d: Vec<u64> = replayed(s.resume(&pairing, Role::Desktop, 0).await)
            .iter()
            .map(|e| e.seq)
            .collect();
        assert_eq!(d, vec![2]);
    }

    #[tokio::test]
    async fn resume_signals_resync_after_drop_oldest() {
        // remote-control-0ef.7: a drop-oldest overflow leaves the buffer's front
        // above ack_cursor + 1, so a resume from before that front asks for shed
        // envelopes and must RESYNC rather than replay a hole. Mirrors the
        // `SenderQueue::resume` unit test, re-expressed over SQL state.
        let tmp = tempdir();
        let s = SqliteStore::open(tmp.join("relay.db"), 3).expect("open");
        let pairing = PairingId::new("pair_1");
        for seq in 1..=5 {
            s.enqueue(env(&pairing, Role::Desktop, seq)).await.unwrap();
        }
        // Buffer now holds seq 3,4,5 (1 and 2 dropped); ack_cursor still 0.
        assert_eq!(
            replayed(s.resume(&pairing, Role::Desktop, 2).await)
                .iter()
                .map(|e| e.seq)
                .collect::<Vec<_>>(),
            vec![3, 4, 5],
            "seq 3 is the retained front → clean replay"
        );
        // A fresh receiver (needs seq 1) and one that last saw seq 1 (needs seq 2)
        // both ask for shed seqs → resync.
        assert_eq!(
            s.resume(&pairing, Role::Desktop, 0).await,
            ResumeOutcome::Resync
        );
        assert_eq!(
            s.resume(&pairing, Role::Desktop, 1).await,
            ResumeOutcome::Resync
        );
        // An unknown queue is a clean, empty replay (no stream row).
        assert_eq!(
            s.resume(&PairingId::new("nope"), Role::Phone, 0).await,
            ResumeOutcome::Replay(vec![])
        );
    }

    #[tokio::test]
    async fn resume_from_before_an_acked_front_is_clean_not_resync() {
        // Regression (remote-control-0ef.7): ack-pruning advances the front
        // contiguously (front == ack_cursor + 1), so a resume from before it must
        // replay the retained tail, never be misread as an overflow gap.
        let tmp = tempdir();
        let s = store(&tmp);
        let pairing = PairingId::new("pair_1");
        for seq in 1..=3 {
            s.enqueue(env(&pairing, Role::Desktop, seq)).await.unwrap();
        }
        s.ack(&pairing, Role::Desktop, 1).await; // prune seq 1; front now seq 2.
        assert_eq!(
            replayed(s.resume(&pairing, Role::Desktop, 0).await)
                .iter()
                .map(|e| e.seq)
                .collect::<Vec<_>>(),
            vec![2, 3]
        );
    }

    #[tokio::test]
    async fn sweep_expired_claims_removes_only_expired() {
        let tmp = tempdir();
        let s = store(&tmp);
        s.issue_claim(
            "live".into(),
            PairingId::new("p1"),
            DeviceId::new("d1"),
            10_000,
        )
        .await;
        s.issue_claim(
            "stale".into(),
            PairingId::new("p2"),
            DeviceId::new("d2"),
            5_000,
        )
        .await;

        // Lazy expiry: past its TTL, "stale" already counts as free before sweep.
        assert!(!s.claim_token_is_free("stale", 4_000).await);
        assert!(s.claim_token_is_free("stale", 6_000).await);
        assert!(!s.claim_token_is_free("live", 6_000).await);

        // The sweep at now=6_000 reaps only "stale"; the boundary-safe "live"
        // stays and is still redeemable.
        assert_eq!(s.sweep_expired_claims(6_000).await, 1);
        assert!(s.redeem_claim("live", 6_000).await.is_ok());
        assert_eq!(
            s.redeem_claim("stale", 6_000).await,
            Err(ClaimError::Unknown)
        );
    }

    /// A temp directory that cleans itself up on drop, without pulling in the
    /// `tempfile` crate (kept out of the dependency set for a single test need).
    struct TempDir(std::path::PathBuf);
    impl std::ops::Deref for TempDir {
        type Target = std::path::Path;
        fn deref(&self) -> &std::path::Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn tempdir() -> TempDir {
        let mut buf = [0u8; 8];
        getrandom::getrandom(&mut buf).expect("csprng");
        let mut hex = String::new();
        for b in buf {
            hex.push_str(&format!("{b:02x}"));
        }
        let dir = std::env::temp_dir().join(format!("relay-sqlite-test-{hex}"));
        std::fs::create_dir_all(&dir).expect("mkdir temp");
        TempDir(dir)
    }
}
