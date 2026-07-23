//! Zero-knowledge connection registry — the live routing table.
//!
//! This is the ephemeral side of the relay (contrast [`crate::store`], which
//! holds durable state). It maps each `pairing_id` to the currently-connected
//! `desktop` and/or `phone` legs so an [`EncryptedEnvelope`] from one leg can be
//! forwarded **verbatim** to the other. The registry only ever moves opaque
//! [`RelayFrame`]s between connection outboxes; it never deserializes, inspects,
//! or logs `ciphertext` (PRD §9.1 "blind pipe" — a hard invariant, not a style
//! choice).
//!
//! Each connection owns a bounded outbound `mpsc` channel drained by its writer
//! task; the registry stores the [`ConnHandle`] (the sender half). Forwarding to
//! a peer is therefore just a channel send, which applies natural back-pressure:
//! if a peer's outbox is full, the sender awaits rather than allocating without
//! bound.
//!
//! Routing is keyed purely by `(pairing_id, role)`, so one-phone-↔-many-Macs is
//! a UI addition later, not a change here (PRD §9.1, spec §10).
//!
//! [`EncryptedEnvelope`]: flightdeck_remote_protocol::EncryptedEnvelope

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;

use flightdeck_remote_protocol::{PairingId, RelayFrame, Role};
use tokio::sync::{mpsc, Notify};

/// Outcome of a non-blocking [`ConnHandle::try_send`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrySendOutcome {
    /// The frame was accepted into the peer's outbound channel.
    Sent,
    /// The peer's outbound channel is full — a slow or half-open (zombie) peer
    /// that is not draining. The frame was **not** enqueued.
    Full,
    /// The peer's writer has gone away (channel closed). The frame was **not**
    /// enqueued.
    Closed,
}

/// A handle to a live connection's outbound channel.
#[derive(Clone)]
pub struct ConnHandle {
    /// Opaque connection id, for logging.
    pub connection_id: String,
    /// Bounded sender into the connection's writer task.
    pub tx: mpsc::Sender<RelayFrame>,
    /// Fires when this connection is superseded by a newer leg for the same
    /// `(pairing, role)` and must tear itself down (remote-control-0ef.8).
    /// Shared with the owning [`crate::session::Connection`], which selects on
    /// it in its read loop. Cloned into every registry entry for the
    /// connection, so superseding any one of its legs signals the whole
    /// connection.
    pub shutdown: Arc<Notify>,
}

impl ConnHandle {
    /// Best-effort send of a frame to this connection. Returns `false` if the
    /// connection's writer has gone away (channel closed).
    ///
    /// Awaits if the outbound channel is full, applying natural back-pressure —
    /// appropriate for the connection's **own** low-volume control frames. Do
    /// **not** use it to forward a *peer's* traffic: a stuck receiver would then
    /// back-pressure the healthy sender's read loop (remote-control-0ef.6). Use
    /// [`Self::try_send`] on that path.
    pub async fn send(&self, frame: RelayFrame) -> bool {
        self.tx.send(frame).await.is_ok()
    }

    /// Non-blocking send. Never awaits: a full or closed channel returns
    /// immediately with the corresponding [`TrySendOutcome`] rather than
    /// parking the caller. Used on the peer-forward hot path so one dead/slow
    /// receiver cannot freeze a healthy sender (remote-control-0ef.6).
    pub fn try_send(&self, frame: RelayFrame) -> TrySendOutcome {
        match self.tx.try_send(frame) {
            Ok(()) => TrySendOutcome::Sent,
            Err(mpsc::error::TrySendError::Full(_)) => TrySendOutcome::Full,
            Err(mpsc::error::TrySendError::Closed(_)) => TrySendOutcome::Closed,
        }
    }

    /// Signal the owning connection to shut down because a newer leg has
    /// superseded it (remote-control-0ef.8). Idempotent and non-blocking:
    /// [`Notify::notify_one`] stores a permit if the connection is not yet
    /// awaiting, so the wakeup is never lost to a race.
    fn supersede(&self) {
        self.shutdown.notify_one();
    }
}

#[derive(Default)]
struct Slot {
    desktop: Option<ConnHandle>,
    phone: Option<ConnHandle>,
}

impl Slot {
    fn role_mut(&mut self, role: Role) -> &mut Option<ConnHandle> {
        match role {
            Role::Desktop => &mut self.desktop,
            Role::Phone => &mut self.phone,
        }
    }

    fn role(&self, role: Role) -> &Option<ConnHandle> {
        match role {
            Role::Desktop => &self.desktop,
            Role::Phone => &self.phone,
        }
    }

    fn is_empty(&self) -> bool {
        self.desktop.is_none() && self.phone.is_none()
    }
}

/// The live routing table. Cheap to clone-wrap in an `Arc`; internally a mutex
/// held only for the duration of a map lookup/insert (never across an `.await`).
#[derive(Default)]
pub struct Registry {
    slots: Mutex<HashMap<PairingId, Slot>>,
}

impl Registry {
    /// Empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<PairingId, Slot>> {
        self.slots.lock().expect("registry mutex poisoned")
    }

    /// Attach a connection's leg for `pairing` under `role`. Returns the peer
    /// leg's handle if the peer is already connected (so the caller can announce
    /// presence in both directions). Replaces any stale handle for the same
    /// `(pairing, role)` — the newest connection wins.
    ///
    /// If a *different* connection already held this `(pairing, role)` slot, it
    /// is a superseded (likely half-open) leg: [`ConnHandle::supersede`] signals
    /// it to tear down rather than leaving two same-role legs to coexist
    /// (remote-control-0ef.8). Re-attaching the *same* connection (e.g. an
    /// already-authed desktop offering another pairing on its live socket) never
    /// signals shutdown.
    pub fn attach(
        &self,
        pairing: &PairingId,
        role: Role,
        handle: ConnHandle,
    ) -> Option<ConnHandle> {
        let mut slots = self.lock();
        let slot = slots.entry(pairing.clone()).or_default();
        let new_id = handle.connection_id.clone();
        if let Some(previous) = slot.role_mut(role).replace(handle) {
            if previous.connection_id != new_id {
                previous.supersede();
            }
        }
        slot.role(role.peer()).clone()
    }

    /// Detach a connection's leg, but only if the stored handle is still *this*
    /// connection (identified by `connection_id`) — so a reconnect that already
    /// replaced the handle is not torn down by the old connection's cleanup.
    /// Returns the peer handle **only when this connection actually owned the
    /// slot**, for a disconnect presence announcement.
    ///
    /// When the slot has already been taken over by a newer leg (the superseded
    /// case, remote-control-0ef.8), the peer is now talking to that newer leg,
    /// so the old connection's cleanup must **not** announce a disconnect —
    /// hence `None`. The newer leg already announced its own `Connected`.
    pub fn detach(
        &self,
        pairing: &PairingId,
        role: Role,
        connection_id: &str,
    ) -> Option<ConnHandle> {
        let mut slots = self.lock();
        let slot = slots.get_mut(pairing)?;
        let still_ours = slot
            .role(role)
            .as_ref()
            .is_some_and(|h| h.connection_id == connection_id);
        let peer = if still_ours {
            *slot.role_mut(role) = None;
            slot.role(role.peer()).clone()
        } else {
            // A newer leg owns this slot; leave it — and its peer — untouched.
            None
        };
        if slot.is_empty() {
            slots.remove(pairing);
        }
        peer
    }

    /// Remove a pairing's slot entirely, dropping **both** legs' routing handles
    /// (spec §10.2, revoke). The underlying connections stay alive for their
    /// other pairings; they simply can no longer route this one, which no longer
    /// exists in the store. A no-op when the pairing is not in the table.
    pub fn remove(&self, pairing: &PairingId) {
        self.lock().remove(pairing);
    }

    /// The peer leg's handle for `pairing`, if the peer of `role` is connected.
    pub fn peer(&self, pairing: &PairingId, role: Role) -> Option<ConnHandle> {
        self.lock()
            .get(pairing)
            .and_then(|s| s.role(role.peer()).clone())
    }
}

/// The opposite role. A pairing has exactly two legs, so "the peer" of a
/// connection is simply the other role.
pub fn peer_role(role: Role) -> Role {
    match role {
        Role::Desktop => Role::Phone,
        Role::Phone => Role::Desktop,
    }
}

/// Role adjacency sugar so `role.peer()` reads naturally inside this module.
trait Peer {
    fn peer(self) -> Role;
}

impl Peer for Role {
    fn peer(self) -> Role {
        peer_role(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handle(id: &str) -> (ConnHandle, mpsc::Receiver<RelayFrame>) {
        let (tx, rx) = mpsc::channel(8);
        (
            ConnHandle {
                connection_id: id.into(),
                tx,
                shutdown: Arc::new(Notify::new()),
            },
            rx,
        )
    }

    #[test]
    fn attach_reports_peer_presence_both_ways() {
        let reg = Registry::new();
        let pairing = PairingId::new("pair");
        let (desktop, _dr) = handle("conn_d");
        let (phone, _pr) = handle("conn_p");

        // Desktop first: no peer yet.
        assert!(reg.attach(&pairing, Role::Desktop, desktop).is_none());
        // Phone second: sees the desktop peer.
        let peer = reg.attach(&pairing, Role::Phone, phone);
        assert_eq!(peer.map(|h| h.connection_id), Some("conn_d".to_string()));
        // Desktop can now see the phone.
        assert_eq!(
            reg.peer(&pairing, Role::Desktop).map(|h| h.connection_id),
            Some("conn_p".to_string())
        );
    }

    #[test]
    fn detach_only_removes_own_connection() {
        let reg = Registry::new();
        let pairing = PairingId::new("pair");
        let (d1, _r1) = handle("conn_d1");
        reg.attach(&pairing, Role::Desktop, d1);

        // A reconnect replaces the desktop handle.
        let (d2, _r2) = handle("conn_d2");
        reg.attach(&pairing, Role::Desktop, d2);

        // The *old* connection's cleanup must not evict the new handle.
        reg.detach(&pairing, Role::Desktop, "conn_d1");
        assert_eq!(
            reg.peer(&pairing, Role::Phone).map(|h| h.connection_id),
            Some("conn_d2".to_string())
        );

        // The current connection's cleanup does evict, and empties the slot.
        reg.detach(&pairing, Role::Desktop, "conn_d2");
        assert!(reg.peer(&pairing, Role::Phone).is_none());
    }

    #[tokio::test]
    async fn attach_supersedes_the_previous_same_role_connection() {
        // A second attach for the same (pairing, role) must signal the first
        // connection to shut down (remote-control-0ef.8) instead of leaving two
        // same-role legs to coexist.
        let reg = Registry::new();
        let pairing = PairingId::new("pair");

        let (d1, _r1) = handle("conn_d1");
        let d1_shutdown = d1.shutdown.clone();
        reg.attach(&pairing, Role::Desktop, d1);

        // A reconnect (different connection id) takes over the slot.
        let (d2, _r2) = handle("conn_d2");
        reg.attach(&pairing, Role::Desktop, d2);

        // The superseded connection was signalled. `notify_one` leaves a stored
        // permit, so `notified()` resolves immediately (bounded by a timeout so
        // a regression fails the test rather than hanging).
        tokio::time::timeout(std::time::Duration::from_secs(1), d1_shutdown.notified())
            .await
            .expect("superseded connection must be signalled to shut down");
    }

    #[tokio::test]
    async fn reattaching_same_connection_does_not_signal_shutdown() {
        // An already-authed connection re-attaching itself (e.g. offering an
        // extra pairing on its live socket) must not be told to shut down.
        let reg = Registry::new();
        let pairing = PairingId::new("pair");

        let (d1, _r1) = handle("conn_d1");
        let d1_shutdown = d1.shutdown.clone();
        reg.attach(&pairing, Role::Desktop, d1.clone());
        reg.attach(&pairing, Role::Desktop, d1);

        // No shutdown permit stored → `notified()` does not resolve.
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(100),
                d1_shutdown.notified()
            )
            .await
            .is_err(),
            "re-attaching the same connection must not signal shutdown"
        );
    }

    #[test]
    fn remove_drops_both_legs() {
        let reg = Registry::new();
        let pairing = PairingId::new("pair");
        let (desktop, _dr) = handle("conn_d");
        let (phone, _pr) = handle("conn_p");
        reg.attach(&pairing, Role::Desktop, desktop);
        reg.attach(&pairing, Role::Phone, phone);

        reg.remove(&pairing);
        assert!(reg.peer(&pairing, Role::Desktop).is_none());
        assert!(reg.peer(&pairing, Role::Phone).is_none());

        // Removing an absent pairing is a harmless no-op.
        reg.remove(&PairingId::new("gone"));
    }
}
