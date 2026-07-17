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
use std::sync::Mutex;

use flightdeck_remote_protocol::{PairingId, RelayFrame, Role};
use tokio::sync::mpsc;

/// A handle to a live connection's outbound channel.
#[derive(Clone)]
pub struct ConnHandle {
    /// Opaque connection id, for logging.
    pub connection_id: String,
    /// Bounded sender into the connection's writer task.
    pub tx: mpsc::Sender<RelayFrame>,
}

impl ConnHandle {
    /// Best-effort send of a frame to this connection. Returns `false` if the
    /// connection's writer has gone away (channel closed).
    pub async fn send(&self, frame: RelayFrame) -> bool {
        self.tx.send(frame).await.is_ok()
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
    pub fn attach(
        &self,
        pairing: &PairingId,
        role: Role,
        handle: ConnHandle,
    ) -> Option<ConnHandle> {
        let mut slots = self.lock();
        let slot = slots.entry(pairing.clone()).or_default();
        *slot.role_mut(role) = Some(handle);
        slot.role(role.peer()).clone()
    }

    /// Detach a connection's leg, but only if the stored handle is still *this*
    /// connection (identified by `connection_id`) — so a reconnect that already
    /// replaced the handle is not torn down by the old connection's cleanup.
    /// Returns the peer handle if one is present, for a disconnect presence
    /// announcement.
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
        if still_ours {
            *slot.role_mut(role) = None;
        }
        let peer = slot.role(role.peer()).clone();
        if slot.is_empty() {
            slots.remove(pairing);
        }
        peer
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
}
