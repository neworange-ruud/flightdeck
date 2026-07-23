//! Persistence seam.
//!
//! All durable relay state lives behind the [`RelayStore`] trait: registered
//! device public keys, pairings and their membership, live claim tokens, the
//! per-`(pairing, sender)` envelope queues, and push tokens. Two
//! implementations ship: [`InMemoryStore`] (the default) keeps everything in
//! process memory behind a single mutex; [`SqliteStore`] persists the same state
//! to a file so it survives a restart/redeploy.
//!
//! **Why a trait.** A persistent implementation slots in behind the same async
//! interface so that device registrations, pairings, and queues survive relay
//! restarts and can later scale across replicas — see the "Relay team" notes in
//! `specs/REMOTE_PROTOCOL.md` §12 ("persist per-pairing queues and sequence
//! high-water marks so `resume` works across relay restarts"). The trait methods
//! are already `async` for exactly that future; both shipping impls complete
//! synchronously (the in-memory one over a mutex, the sqlite one over a mutexed
//! [`rusqlite::Connection`]).
//!
//! **Backend selection.** [`crate::config::StoreBackend`] chooses the impl from
//! `FLIGHTDECK_RELAY_STORE` (`memory` — the default — or `sqlite:<path>`).
//!
//! **In-memory limitation.** Because [`InMemoryStore`] is not persistent, a
//! relay restart drops all registrations, pairings, and queues. The desktop's
//! `pairing_offer` re-registers its key on reconnect, and the phone can re-pair,
//! but any envelopes queued for an offline peer at restart time are lost — the
//! sender is responsible for re-queuing (spec §6 amendment). Selecting
//! [`SqliteStore`] removes this limitation with no changes to the connection
//! state machine.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use flightdeck_remote_protocol::{ApnsEnvironment, DeviceId, EncryptedEnvelope, PairingId, Role};

use crate::claims::{ClaimError, ClaimTable, RedeemedClaim};
use crate::queue::{AppendOutcome, QueueError, ResumeOutcome, SenderQueue};

mod sqlite;
pub use sqlite::SqliteStore;

/// The desktop + (optional) phone device ids of a pairing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingMembers {
    /// The desktop device that created the pairing.
    pub desktop: DeviceId,
    /// The phone device, once it has claimed the pairing.
    pub phone: Option<DeviceId>,
    /// The desktop's last-announced, human-readable machine name (spec §10.1),
    /// re-sent on every desktop connect via `auth_response.machine_name` and
    /// forwarded to the phone. **Untrusted display text**, already length-bounded
    /// by the session layer. `None` until the desktop announces one; a relay
    /// restart clears it and the desktop repopulates it on its next connect.
    pub machine_name: Option<String>,
}

impl PairingMembers {
    /// Whether `device` is one of this pairing's members.
    pub fn contains(&self, device: &DeviceId) -> bool {
        self.desktop == *device || self.phone.as_ref() == Some(device)
    }
}

/// The result of a [`RelayStore::revoke_pairing`] attempt (spec §10.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RevokeOutcome {
    /// The pairing was removed. Carries the membership as it was **before**
    /// removal, so the caller can notify the (now former) peer.
    Removed(PairingMembers),
    /// The requester authenticated but is **not** a member of the pairing — the
    /// revoke is refused and nothing changed (security check, spec §10.2).
    NotMember,
    /// The pairing was already gone — an idempotent success no-op.
    AlreadyGone,
}

/// Why a store mutation failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreError {
    /// The referenced pairing id is not known to the relay.
    UnknownPairing,
}

/// Durable relay state. See the module docs for the persistence rationale.
#[async_trait]
pub trait RelayStore: Send + Sync {
    /// Register (or replace) a device's public key. Called from `pairing_offer`
    /// and `pairing_claim`, which self-register the connecting device's key.
    async fn register_device(&self, device: DeviceId, public_key_b64: String);

    /// Fetch a device's registered public key, if any.
    async fn device_public_key(&self, device: &DeviceId) -> Option<String>;

    /// Register (or replace) a device's **key-agreement** public key, carried
    /// alongside the identity key in `pairing_offer` / `pairing_claim`. Public
    /// keys are not secret; the relay stores it only to hand each endpoint its
    /// peer's KA key in `pairing_claimed` (spec §5.2 / §7.1). The relay never
    /// holds the private scalar.
    async fn register_key_agreement_key(&self, device: DeviceId, key_agreement_key_b64: String);

    /// Fetch a device's registered key-agreement public key, if any.
    async fn device_key_agreement_key(&self, device: &DeviceId) -> Option<String>;

    /// Create a new pairing owned by `desktop`, returning its fresh id.
    async fn create_pairing(&self, desktop: DeviceId) -> PairingId;

    /// Attach `phone` to an existing pairing, returning the desktop peer's id.
    async fn add_phone_to_pairing(
        &self,
        pairing: &PairingId,
        phone: DeviceId,
    ) -> Result<DeviceId, StoreError>;

    /// The members of a pairing, if it exists.
    async fn pairing_members(&self, pairing: &PairingId) -> Option<PairingMembers>;

    /// Record the desktop's announced machine name for `pairing` (spec §10.1).
    /// No-op if the pairing does not exist. The name is display-only and already
    /// length-bounded by the session layer.
    async fn set_machine_name(&self, pairing: &PairingId, machine_name: String);

    /// The desktop's last-announced machine name for `pairing`, if any.
    async fn machine_name(&self, pairing: &PairingId) -> Option<String>;

    /// Revoke a pairing on behalf of `requester` (spec §10.2). Verifies
    /// membership, then atomically removes the pairing **and** all of its state
    /// (membership, live claim tokens, both `(pairing, sender)` queues, and the
    /// push token). See [`RevokeOutcome`] for the three outcomes: removed,
    /// refused (non-member), or an idempotent no-op (already gone). The
    /// membership check and removal happen under one lock so there is no
    /// check-then-remove race.
    async fn revoke_pairing(&self, pairing: &PairingId, requester: &DeviceId) -> RevokeOutcome;

    /// Issue a single-use claim token bound to `pairing` / `desktop`.
    async fn issue_claim(
        &self,
        token: String,
        pairing: PairingId,
        desktop: DeviceId,
        expires_at_ms: i64,
    );

    /// Whether `token` is free to issue at wall-clock `now_ms` — i.e. not
    /// currently a **live** (issued, un-redeemed, un-expired) claim. Backs the
    /// `claim_token_hint` honoring in `pairing_offer` (spec §5.2). An expired
    /// entry that the sweep has not yet reaped does **not** count as live
    /// (remote-control-0ef.16).
    async fn claim_token_is_free(&self, token: &str, now_ms: i64) -> bool;

    /// Redeem a claim token at wall-clock `now_ms`.
    async fn redeem_claim(&self, token: &str, now_ms: i64) -> Result<RedeemedClaim, ClaimError>;

    /// Evict every claim-token entry that has expired as of `now_ms`, returning
    /// how many were removed. Called periodically by the relay's claim-sweep
    /// task so abandoned `pairing_offer` codes (never redeemed) do not leak
    /// forever (remote-control-0ef.16).
    async fn sweep_expired_claims(&self, now_ms: i64) -> usize;

    /// Append an outbound envelope to its `(pairing, sender)` queue, enforcing
    /// gapless sequencing and the bound. `Err` means the seq was invalid.
    async fn enqueue(&self, env: EncryptedEnvelope) -> Result<AppendOutcome, QueueError>;

    /// Service a `resume { from_seq }` against the `sender` stream of `pairing`.
    /// Returns the envelopes to replay (`seq > from_seq`, in order) or, when a
    /// drop-oldest overflow shed seqs the receiver still needs,
    /// [`ResumeOutcome::Resync`] (remote-control-0ef.7). An unknown queue is a
    /// clean, empty replay.
    async fn resume(&self, pairing: &PairingId, sender: Role, from_seq: u64) -> ResumeOutcome;

    /// Prune the `sender` stream of `pairing` up to and including `cursor`.
    async fn ack(&self, pairing: &PairingId, sender: Role, cursor: u64);

    /// Store/refresh a pairing's APNs push token (opaque; never encrypted).
    async fn register_push_token(&self, pairing: PairingId, token: String, env: ApnsEnvironment);

    /// Remove a pairing's APNs push token, if any, **without** touching the
    /// pairing itself (spec §5.5). Backs [`crate::session`]'s
    /// `unregister_push_token` handling so a phone can mute this pairing's pushes
    /// while staying paired. **Idempotent**: removing an absent token is a no-op.
    /// Membership is verified by the session before this is called (this method
    /// only mutates the token slot).
    async fn unregister_push_token(&self, pairing: &PairingId);

    /// The pairing's registered APNs token + environment, if any. Read by the
    /// envelope path to wake an offline phone via push (spec §5.5/§11).
    async fn push_token(&self, pairing: &PairingId) -> Option<(String, ApnsEnvironment)>;
}

/// A queue key. `Role` is not `Hash` in the protocol crate, so we index the
/// sender direction by a small local tag rather than requiring a protocol
/// change.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct QueueKey(PairingId, u8);

impl QueueKey {
    fn new(pairing: PairingId, sender: Role) -> Self {
        let dir = match sender {
            Role::Desktop => 0,
            Role::Phone => 1,
        };
        QueueKey(pairing, dir)
    }
}

#[derive(Default)]
struct Inner {
    devices: HashMap<DeviceId, String>,
    /// Per-device key-agreement public keys (see [`RelayStore::register_key_agreement_key`]).
    key_agreement_keys: HashMap<DeviceId, String>,
    pairings: HashMap<PairingId, PairingMembers>,
    claims: ClaimTable,
    queues: HashMap<QueueKey, SenderQueue>,
    push_tokens: HashMap<PairingId, (String, ApnsEnvironment)>,
    /// Monotonic counter feeding readable pairing ids in tests/logs.
    pairing_counter: u64,
}

/// In-process, non-persistent [`RelayStore`]. Correct for a single replica;
/// state is lost on restart (see module docs).
pub struct InMemoryStore {
    inner: Mutex<Inner>,
    /// Per-`(pairing, sender)` queue bound.
    queue_max_per_pairing: usize,
}

impl InMemoryStore {
    /// Build an empty store whose queues each hold at most
    /// `queue_max_per_pairing` un-acked envelopes.
    pub fn new(queue_max_per_pairing: usize) -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
            queue_max_per_pairing: queue_max_per_pairing.max(1),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        // The lock is only ever held for the duration of a small synchronous
        // mutation; poisoning would mean a prior panic mid-mutation, which we
        // surface by unwrapping (a poisoned relay store is unrecoverable).
        self.inner.lock().expect("relay store mutex poisoned")
    }

    /// Physical number of claim-token entries currently stored, **including**
    /// expired-but-unswept ones. Distinct from the "live" count — a test hook
    /// for asserting that the background sweep physically evicted an entry
    /// (rather than it merely being lazily treated as absent).
    #[cfg(test)]
    pub fn claim_entry_count(&self) -> usize {
        self.lock().claims.len()
    }
}

#[async_trait]
impl RelayStore for InMemoryStore {
    async fn register_device(&self, device: DeviceId, public_key_b64: String) {
        self.lock().devices.insert(device, public_key_b64);
    }

    async fn device_public_key(&self, device: &DeviceId) -> Option<String> {
        self.lock().devices.get(device).cloned()
    }

    async fn register_key_agreement_key(&self, device: DeviceId, key_agreement_key_b64: String) {
        self.lock()
            .key_agreement_keys
            .insert(device, key_agreement_key_b64);
    }

    async fn device_key_agreement_key(&self, device: &DeviceId) -> Option<String> {
        self.lock().key_agreement_keys.get(device).cloned()
    }

    async fn create_pairing(&self, desktop: DeviceId) -> PairingId {
        let mut inner = self.lock();
        inner.pairing_counter += 1;
        let id = PairingId::new(format!(
            "pair_{:04}_{}",
            inner.pairing_counter,
            crate::ids::random_suffix()
        ));
        inner.pairings.insert(
            id.clone(),
            PairingMembers {
                desktop,
                phone: None,
                machine_name: None,
            },
        );
        id
    }

    async fn add_phone_to_pairing(
        &self,
        pairing: &PairingId,
        phone: DeviceId,
    ) -> Result<DeviceId, StoreError> {
        let mut inner = self.lock();
        let members = inner
            .pairings
            .get_mut(pairing)
            .ok_or(StoreError::UnknownPairing)?;
        members.phone = Some(phone);
        Ok(members.desktop.clone())
    }

    async fn pairing_members(&self, pairing: &PairingId) -> Option<PairingMembers> {
        self.lock().pairings.get(pairing).cloned()
    }

    async fn set_machine_name(&self, pairing: &PairingId, machine_name: String) {
        if let Some(members) = self.lock().pairings.get_mut(pairing) {
            members.machine_name = Some(machine_name);
        }
    }

    async fn machine_name(&self, pairing: &PairingId) -> Option<String> {
        self.lock()
            .pairings
            .get(pairing)
            .and_then(|m| m.machine_name.clone())
    }

    async fn revoke_pairing(&self, pairing: &PairingId, requester: &DeviceId) -> RevokeOutcome {
        let mut inner = self.lock();
        let Some(members) = inner.pairings.get(pairing) else {
            // Idempotent: revoking an already-gone pairing is a success no-op.
            return RevokeOutcome::AlreadyGone;
        };
        if !members.contains(requester) {
            // Security (spec §10.2): only a member may revoke; refuse and leave
            // all state untouched.
            return RevokeOutcome::NotMember;
        }
        // Authorized: remove the pairing and every trace of it. Cloned members
        // are returned so the caller can notify the (now former) peer.
        let removed = inner
            .pairings
            .remove(pairing)
            .expect("pairing present under lock");
        inner.claims.remove_pairing(pairing);
        inner
            .queues
            .remove(&QueueKey::new(pairing.clone(), Role::Desktop));
        inner
            .queues
            .remove(&QueueKey::new(pairing.clone(), Role::Phone));
        inner.push_tokens.remove(pairing);

        // GC the identity + key-agreement keys of this pairing's members, but
        // only for a device no **surviving** pairing still references — a device
        // can belong to several pairings (a Mac paired with two phones, a phone
        // that re-paired), and those must keep their keys (remote-control-0ef.17).
        // Decide first (immutable read of the now-reduced `pairings`), then
        // remove, so the two field borrows do not overlap.
        let orphaned: Vec<DeviceId> = [Some(removed.desktop.clone()), removed.phone.clone()]
            .into_iter()
            .flatten()
            .filter(|device| !inner.pairings.values().any(|m| m.contains(device)))
            .collect();
        for device in orphaned {
            inner.devices.remove(&device);
            inner.key_agreement_keys.remove(&device);
        }
        RevokeOutcome::Removed(removed)
    }

    async fn issue_claim(
        &self,
        token: String,
        pairing: PairingId,
        desktop: DeviceId,
        expires_at_ms: i64,
    ) {
        self.lock()
            .claims
            .issue(token, pairing, desktop, expires_at_ms);
    }

    async fn claim_token_is_free(&self, token: &str, now_ms: i64) -> bool {
        !self.lock().claims.contains(token, now_ms)
    }

    async fn redeem_claim(&self, token: &str, now_ms: i64) -> Result<RedeemedClaim, ClaimError> {
        self.lock().claims.redeem(token, now_ms)
    }

    async fn sweep_expired_claims(&self, now_ms: i64) -> usize {
        self.lock().claims.sweep_expired(now_ms)
    }

    async fn enqueue(&self, env: EncryptedEnvelope) -> Result<AppendOutcome, QueueError> {
        let mut inner = self.lock();
        let max = self.queue_max_per_pairing;
        let key = QueueKey::new(env.pairing_id.clone(), env.sender);
        inner
            .queues
            .entry(key)
            .or_insert_with(|| SenderQueue::new(max))
            .append(env)
    }

    async fn resume(&self, pairing: &PairingId, sender: Role, from_seq: u64) -> ResumeOutcome {
        self.lock()
            .queues
            .get(&QueueKey::new(pairing.clone(), sender))
            .map(|q| q.resume(from_seq))
            .unwrap_or(ResumeOutcome::Replay(Vec::new()))
    }

    async fn ack(&self, pairing: &PairingId, sender: Role, cursor: u64) {
        if let Some(q) = self
            .lock()
            .queues
            .get_mut(&QueueKey::new(pairing.clone(), sender))
        {
            q.ack(cursor);
        }
    }

    async fn register_push_token(&self, pairing: PairingId, token: String, env: ApnsEnvironment) {
        self.lock().push_tokens.insert(pairing, (token, env));
    }

    async fn unregister_push_token(&self, pairing: &PairingId) {
        // Idempotent: `HashMap::remove` on an absent key is a no-op.
        self.lock().push_tokens.remove(pairing);
    }

    async fn push_token(&self, pairing: &PairingId) -> Option<(String, ApnsEnvironment)> {
        self.lock().push_tokens.get(pairing).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairing: &str, sender: Role, seq: u64) -> EncryptedEnvelope {
        EncryptedEnvelope {
            pairing_id: PairingId::new(pairing),
            seq,
            sender,
            sent_at_ms: 0,
            nonce: "bm9uY2U=".into(),
            ciphertext: "opaque".into(),
        }
    }

    /// Unwrap a clean replay's envelopes, failing the test on a resync signal.
    fn replayed(outcome: ResumeOutcome) -> Vec<EncryptedEnvelope> {
        match outcome {
            ResumeOutcome::Replay(v) => v,
            ResumeOutcome::Resync => panic!("expected a clean replay, got Resync"),
        }
    }

    #[tokio::test]
    async fn device_registration_round_trips() {
        let store = InMemoryStore::new(1000);
        let dev = DeviceId::new("dev_1");
        assert_eq!(store.device_public_key(&dev).await, None);
        store.register_device(dev.clone(), "pk".into()).await;
        assert_eq!(store.device_public_key(&dev).await, Some("pk".into()));
    }

    #[tokio::test]
    async fn pairing_membership_and_peer_lookup() {
        let store = InMemoryStore::new(1000);
        let desktop = DeviceId::new("dev_mac");
        let pairing = store.create_pairing(desktop.clone()).await;

        let members = store.pairing_members(&pairing).await.unwrap();
        assert_eq!(members.desktop, desktop);
        assert_eq!(members.phone, None);
        assert!(members.contains(&desktop));

        let phone = DeviceId::new("dev_phone");
        let peer = store
            .add_phone_to_pairing(&pairing, phone.clone())
            .await
            .unwrap();
        assert_eq!(peer, desktop);
        let members = store.pairing_members(&pairing).await.unwrap();
        assert_eq!(members.phone, Some(phone.clone()));
        assert!(members.contains(&phone));
    }

    #[tokio::test]
    async fn add_phone_to_unknown_pairing_errors() {
        let store = InMemoryStore::new(1000);
        assert_eq!(
            store
                .add_phone_to_pairing(&PairingId::new("nope"), DeviceId::new("p"))
                .await,
            Err(StoreError::UnknownPairing)
        );
    }

    #[tokio::test]
    async fn push_token_round_trips_and_refreshes() {
        let store = InMemoryStore::new(1000);
        let pairing = PairingId::new("pair");
        assert_eq!(store.push_token(&pairing).await, None);

        store
            .register_push_token(pairing.clone(), "tok_a".into(), ApnsEnvironment::Sandbox)
            .await;
        assert_eq!(
            store.push_token(&pairing).await,
            Some(("tok_a".into(), ApnsEnvironment::Sandbox))
        );

        // A refresh replaces the token + environment.
        store
            .register_push_token(pairing.clone(), "tok_b".into(), ApnsEnvironment::Production)
            .await;
        assert_eq!(
            store.push_token(&pairing).await,
            Some(("tok_b".into(), ApnsEnvironment::Production))
        );
    }

    #[tokio::test]
    async fn unregister_push_token_removes_and_is_idempotent() {
        let store = InMemoryStore::new(1000);
        let pairing = PairingId::new("pair");

        // Unregistering when nothing is stored is a success no-op.
        store.unregister_push_token(&pairing).await;
        assert_eq!(store.push_token(&pairing).await, None);

        // Register, then remove.
        store
            .register_push_token(pairing.clone(), "tok".into(), ApnsEnvironment::Production)
            .await;
        assert!(store.push_token(&pairing).await.is_some());
        store.unregister_push_token(&pairing).await;
        assert_eq!(store.push_token(&pairing).await, None);

        // A second removal of the now-absent token is still a no-op.
        store.unregister_push_token(&pairing).await;
        assert_eq!(store.push_token(&pairing).await, None);
    }

    #[tokio::test]
    async fn machine_name_set_and_get_round_trips() {
        let store = InMemoryStore::new(1000);
        let pairing = store.create_pairing(DeviceId::new("dev_mac")).await;

        // Absent until announced.
        assert_eq!(store.machine_name(&pairing).await, None);
        assert_eq!(
            store.pairing_members(&pairing).await.unwrap().machine_name,
            None
        );

        store
            .set_machine_name(&pairing, "Ruud's MacBook Pro".into())
            .await;
        assert_eq!(
            store.machine_name(&pairing).await,
            Some("Ruud's MacBook Pro".into())
        );

        // A re-announce (e.g. a rename on reconnect) replaces the stored value.
        store.set_machine_name(&pairing, "Work Mac".into()).await;
        assert_eq!(store.machine_name(&pairing).await, Some("Work Mac".into()));

        // Setting a name on an unknown pairing is a silent no-op.
        store
            .set_machine_name(&PairingId::new("nope"), "x".into())
            .await;
        assert_eq!(store.machine_name(&PairingId::new("nope")).await, None);
    }

    /// Build a fully-populated pairing (phone joined, a live claim, both queues,
    /// a push token) so revoke-cleanup can be asserted end to end.
    async fn populated_pairing(store: &InMemoryStore) -> (PairingId, DeviceId, DeviceId) {
        let desktop = DeviceId::new("dev_mac");
        let phone = DeviceId::new("dev_phone");
        let pairing = store.create_pairing(desktop.clone()).await;
        store
            .add_phone_to_pairing(&pairing, phone.clone())
            .await
            .unwrap();
        store
            .issue_claim("tok_live".into(), pairing.clone(), desktop.clone(), 10_000)
            .await;
        // Populate this pairing's own queues (env() uses a fixed pairing id, so
        // enqueue against the real pairing id directly).
        store
            .enqueue(EncryptedEnvelope {
                pairing_id: pairing.clone(),
                seq: 1,
                sender: Role::Desktop,
                sent_at_ms: 0,
                nonce: "bm9uY2U=".into(),
                ciphertext: "opaque".into(),
            })
            .await
            .unwrap();
        store
            .enqueue(EncryptedEnvelope {
                pairing_id: pairing.clone(),
                seq: 1,
                sender: Role::Phone,
                sent_at_ms: 0,
                nonce: "bm9uY2U=".into(),
                ciphertext: "opaque".into(),
            })
            .await
            .unwrap();
        store
            .register_push_token(pairing.clone(), "apns".into(), ApnsEnvironment::Sandbox)
            .await;
        (pairing, desktop, phone)
    }

    #[tokio::test]
    async fn revoke_by_member_removes_pairing_and_all_state() {
        let store = InMemoryStore::new(1000);
        let (pairing, _desktop, phone) = populated_pairing(&store).await;

        let outcome = store.revoke_pairing(&pairing, &phone).await;
        match outcome {
            RevokeOutcome::Removed(members) => {
                assert_eq!(members.desktop, DeviceId::new("dev_mac"));
                assert_eq!(members.phone, Some(phone.clone()));
            }
            other => panic!("expected Removed, got {other:?}"),
        }

        // Pairing and every trace of it are gone.
        assert_eq!(store.pairing_members(&pairing).await, None);
        assert!(store.claim_token_is_free("tok_live", 0).await);
        assert!(replayed(store.resume(&pairing, Role::Desktop, 0).await).is_empty());
        assert!(replayed(store.resume(&pairing, Role::Phone, 0).await).is_empty());
        assert_eq!(store.push_token(&pairing).await, None);
    }

    #[tokio::test]
    async fn revoke_by_desktop_member_also_allowed() {
        // Revocation is role-agnostic: any member may revoke.
        let store = InMemoryStore::new(1000);
        let (pairing, desktop, _phone) = populated_pairing(&store).await;
        assert!(matches!(
            store.revoke_pairing(&pairing, &desktop).await,
            RevokeOutcome::Removed(_)
        ));
        assert_eq!(store.pairing_members(&pairing).await, None);
    }

    #[tokio::test]
    async fn revoke_by_non_member_is_refused_and_changes_nothing() {
        let store = InMemoryStore::new(1000);
        let (pairing, _desktop, _phone) = populated_pairing(&store).await;

        let stranger = DeviceId::new("dev_stranger");
        assert_eq!(
            store.revoke_pairing(&pairing, &stranger).await,
            RevokeOutcome::NotMember
        );

        // Nothing was removed.
        assert!(store.pairing_members(&pairing).await.is_some());
        assert!(!store.claim_token_is_free("tok_live", 0).await);
        assert_eq!(
            replayed(store.resume(&pairing, Role::Desktop, 0).await).len(),
            1,
            "queue must survive a refused revoke"
        );
        assert!(store.push_token(&pairing).await.is_some());
    }

    #[tokio::test]
    async fn revoke_is_idempotent_for_a_gone_pairing() {
        let store = InMemoryStore::new(1000);
        let (pairing, _desktop, phone) = populated_pairing(&store).await;
        assert!(matches!(
            store.revoke_pairing(&pairing, &phone).await,
            RevokeOutcome::Removed(_)
        ));
        // A second revoke of the same (now-gone) pairing is a success no-op.
        assert_eq!(
            store.revoke_pairing(&pairing, &phone).await,
            RevokeOutcome::AlreadyGone
        );
    }

    #[tokio::test]
    async fn queues_are_isolated_per_sender_direction() {
        let store = InMemoryStore::new(1000);
        // Desktop stream and phone stream have independent seq spaces.
        store.enqueue(env("pair", Role::Desktop, 1)).await.unwrap();
        store.enqueue(env("pair", Role::Phone, 1)).await.unwrap();
        store.enqueue(env("pair", Role::Desktop, 2)).await.unwrap();

        let d = replayed(
            store
                .resume(&PairingId::new("pair"), Role::Desktop, 0)
                .await,
        );
        let p = replayed(store.resume(&PairingId::new("pair"), Role::Phone, 0).await);
        assert_eq!(d.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![1, 2]);
        assert_eq!(p.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![1]);

        store.ack(&PairingId::new("pair"), Role::Desktop, 1).await;
        let d = replayed(
            store
                .resume(&PairingId::new("pair"), Role::Desktop, 0)
                .await,
        );
        assert_eq!(d.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![2]);
    }

    #[tokio::test]
    async fn sweep_expired_claims_removes_only_expired() {
        let store = InMemoryStore::new(1000);
        store
            .issue_claim(
                "live".into(),
                PairingId::new("p1"),
                DeviceId::new("d1"),
                10_000,
            )
            .await;
        store
            .issue_claim(
                "stale".into(),
                PairingId::new("p2"),
                DeviceId::new("d2"),
                5_000,
            )
            .await;

        // Before its TTL, an un-redeemed token is not free (still live).
        assert!(!store.claim_token_is_free("stale", 4_000).await);

        // Lazy expiry: past the TTL the entry no longer counts as live even
        // before the sweep reaps it (remote-control-0ef.16).
        assert!(store.claim_token_is_free("stale", 6_000).await);
        assert!(!store.claim_token_is_free("live", 6_000).await);

        // The sweep at now=6_000 reaps only the expired "stale" entry.
        assert_eq!(store.sweep_expired_claims(6_000).await, 1);
        // "live" is still redeemable; "stale" is gone.
        assert!(store.redeem_claim("live", 6_000).await.is_ok());
        assert_eq!(
            store.redeem_claim("stale", 6_000).await,
            Err(ClaimError::Unknown)
        );
    }

    #[tokio::test]
    async fn revoke_gcs_device_keys_only_when_no_pairing_references_them() {
        // remote-control-0ef.17: a device shared by two pairings keeps its
        // identity + key-agreement keys until the LAST referencing pairing is
        // revoked.
        let store = InMemoryStore::new(1000);
        let desktop = DeviceId::new("dev_mac");
        let phone_a = DeviceId::new("dev_phone_a");
        let phone_b = DeviceId::new("dev_phone_b");

        // One Mac paired with two phones (desktop is shared across pairings).
        store
            .register_device(desktop.clone(), "pk_mac".into())
            .await;
        store
            .register_key_agreement_key(desktop.clone(), "ka_mac".into())
            .await;
        let pairing_a = store.create_pairing(desktop.clone()).await;
        let pairing_b = store.create_pairing(desktop.clone()).await;

        store.register_device(phone_a.clone(), "pk_a".into()).await;
        store
            .register_key_agreement_key(phone_a.clone(), "ka_a".into())
            .await;
        store
            .add_phone_to_pairing(&pairing_a, phone_a.clone())
            .await
            .unwrap();
        store.register_device(phone_b.clone(), "pk_b".into()).await;
        store
            .register_key_agreement_key(phone_b.clone(), "ka_b".into())
            .await;
        store
            .add_phone_to_pairing(&pairing_b, phone_b.clone())
            .await
            .unwrap();

        // Revoke the first pairing: phone_a is orphaned and reaped, but the
        // shared desktop is still referenced by pairing_b, so its keys stay.
        assert!(matches!(
            store.revoke_pairing(&pairing_a, &desktop).await,
            RevokeOutcome::Removed(_)
        ));
        assert_eq!(store.device_public_key(&phone_a).await, None);
        assert_eq!(store.device_key_agreement_key(&phone_a).await, None);
        assert_eq!(
            store.device_public_key(&desktop).await,
            Some("pk_mac".into())
        );
        assert_eq!(
            store.device_key_agreement_key(&desktop).await,
            Some("ka_mac".into())
        );
        // phone_b's keys are untouched.
        assert_eq!(store.device_public_key(&phone_b).await, Some("pk_b".into()));

        // Revoke the last pairing: now the desktop is unreferenced and reaped.
        assert!(matches!(
            store.revoke_pairing(&pairing_b, &desktop).await,
            RevokeOutcome::Removed(_)
        ));
        assert_eq!(store.device_public_key(&desktop).await, None);
        assert_eq!(store.device_key_agreement_key(&desktop).await, None);
        assert_eq!(store.device_public_key(&phone_b).await, None);
        assert_eq!(store.device_key_agreement_key(&phone_b).await, None);
    }
}
