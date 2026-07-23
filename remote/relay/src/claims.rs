//! Pairing claim tokens (spec §5.2): short-TTL, single-use secrets minted by the
//! desktop's `pairing_offer` and redeemed by the phone's `pairing_claim`.
//!
//! A token binds to the `pairing_id` it was issued for and the desktop device
//! that offered it, so redemption can register the phone against the right
//! pairing and report the peer device id. Redemption is **single-use**
//! (the entry is removed whether it succeeds, is expired, or is a repeat) and
//! **time-bounded** (`expires_at_ms`). The clock is passed in explicitly so TTL
//! behavior is deterministically testable.

use std::collections::HashMap;

use flightdeck_remote_protocol::{DeviceId, PairingId};

/// A successfully redeemed claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedeemedClaim {
    /// The pairing the phone is joining.
    pub pairing_id: PairingId,
    /// The desktop device that offered the pairing (the phone's peer).
    pub desktop_device: DeviceId,
}

/// Why a `pairing_claim` was rejected. Both map to `pairing_claim_rejected` on
/// the wire, but the distinction is useful for logs/metrics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimError {
    /// No such token (never issued, or already redeemed — single-use).
    Unknown,
    /// The token existed but its TTL had elapsed.
    Expired,
}

#[derive(Debug, Clone)]
struct Entry {
    pairing_id: PairingId,
    desktop_device: DeviceId,
    expires_at_ms: i64,
}

/// A table of live claim tokens. Not thread-safe on its own; the store wraps it
/// in a lock.
#[derive(Debug, Default)]
pub struct ClaimTable {
    entries: HashMap<String, Entry>,
}

impl ClaimTable {
    /// Empty table.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a freshly-minted token. Overwrites any prior entry for the same
    /// token string (tokens are random, so collisions are not expected).
    pub fn issue(
        &mut self,
        token: impl Into<String>,
        pairing_id: PairingId,
        desktop_device: DeviceId,
        expires_at_ms: i64,
    ) {
        self.entries.insert(
            token.into(),
            Entry {
                pairing_id,
                desktop_device,
                expires_at_ms,
            },
        );
    }

    /// Redeem a token at wall-clock `now_ms`. Removes the entry (single-use)
    /// unless the token was simply unknown. An expired token is consumed on the
    /// attempt so it cannot be retried.
    pub fn redeem(&mut self, token: &str, now_ms: i64) -> Result<RedeemedClaim, ClaimError> {
        let entry = self.entries.remove(token).ok_or(ClaimError::Unknown)?;
        if now_ms > entry.expires_at_ms {
            return Err(ClaimError::Expired);
        }
        Ok(RedeemedClaim {
            pairing_id: entry.pairing_id,
            desktop_device: entry.desktop_device,
        })
    }

    /// Drop every live token bound to `pairing`. Used when a pairing is revoked
    /// (spec §10.2) so no dangling claim can later redeem into a gone pairing.
    /// A no-op when no token targets that pairing.
    pub fn remove_pairing(&mut self, pairing: &PairingId) {
        self.entries.retain(|_, e| e.pairing_id != *pairing);
    }

    /// Evict every entry whose TTL has elapsed as of `now_ms`, returning how
    /// many were removed. Backs the relay's periodic claim-sweep task so an
    /// abandoned `pairing_offer` code — issued but never redeemed — does not
    /// leak for the life of the process (remote-control-0ef.16). Redemption is
    /// still single-use and time-checked; this only bounds the table's growth.
    pub fn sweep_expired(&mut self, now_ms: i64) -> usize {
        let before = self.entries.len();
        self.entries.retain(|_, e| now_ms <= e.expires_at_ms);
        before - self.entries.len()
    }

    /// Whether `token` is currently a **live** claim at `now_ms`: present,
    /// un-redeemed, and not past its TTL. Used to decide whether a desktop's
    /// `claim_token_hint` is free to reuse (spec §5.2 amendment): a hint that
    /// collides with a live token is refused and a fresh random token is minted
    /// instead.
    ///
    /// An expired-but-unswept entry is treated as **absent** (not live): its TTL
    /// has elapsed, so no peer can still redeem it, and holding the string
    /// reserved would let an abandoned offer block hint reuse indefinitely. This
    /// closes the lazy-expiry gap that previously counted expired entries as
    /// present even before the sweep ran (remote-control-0ef.16).
    pub fn contains(&self, token: &str, now_ms: i64) -> bool {
        self.entries
            .get(token)
            .is_some_and(|e| now_ms <= e.expires_at_ms)
    }

    /// Number of live (issued, un-redeemed, un-expired at `now_ms`) tokens.
    /// Test/metric helper — expired-but-unswept entries are not counted.
    pub fn live_len(&self, now_ms: i64) -> usize {
        self.entries
            .values()
            .filter(|e| now_ms <= e.expires_at_ms)
            .count()
    }

    /// Total number of stored entries, including any expired-but-unswept ones.
    /// Test helper for asserting sweep behavior.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the table has no stored entries at all.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table_with_token(expires_at_ms: i64) -> ClaimTable {
        let mut t = ClaimTable::new();
        t.issue(
            "4729-Xk9Qa2Lm",
            PairingId::new("pair_1"),
            DeviceId::new("dev_mac"),
            expires_at_ms,
        );
        t
    }

    #[test]
    fn redeems_once_then_gone() {
        let mut t = table_with_token(10_000);
        let claim = t.redeem("4729-Xk9Qa2Lm", 5_000).expect("should redeem");
        assert_eq!(claim.pairing_id, PairingId::new("pair_1"));
        assert_eq!(claim.desktop_device, DeviceId::new("dev_mac"));
        // Single-use: a second attempt fails as unknown.
        assert_eq!(t.redeem("4729-Xk9Qa2Lm", 5_000), Err(ClaimError::Unknown));
        assert!(t.is_empty());
    }

    #[test]
    fn unknown_token_rejected() {
        let mut t = ClaimTable::new();
        assert_eq!(t.redeem("nope", 0), Err(ClaimError::Unknown));
    }

    #[test]
    fn expired_token_rejected_and_consumed() {
        let mut t = table_with_token(10_000);
        // now strictly greater than expiry.
        assert_eq!(t.redeem("4729-Xk9Qa2Lm", 10_001), Err(ClaimError::Expired));
        // Consumed even though expired: cannot be retried.
        assert_eq!(t.redeem("4729-Xk9Qa2Lm", 5_000), Err(ClaimError::Unknown));
    }

    #[test]
    fn boundary_exactly_at_expiry_is_valid() {
        let mut t = table_with_token(10_000);
        assert!(t.redeem("4729-Xk9Qa2Lm", 10_000).is_ok());
    }

    #[test]
    fn contains_treats_expired_entry_as_absent() {
        // remote-control-0ef.16: lazy expiry — an un-swept but expired entry
        // must not count as live, so a colliding hint can be reused.
        let t = table_with_token(10_000);
        assert!(t.contains("4729-Xk9Qa2Lm", 5_000), "live before TTL");
        assert!(t.contains("4729-Xk9Qa2Lm", 10_000), "live at the boundary");
        assert!(
            !t.contains("4729-Xk9Qa2Lm", 10_001),
            "expired entry counts as absent even before sweep"
        );
        // The unknown token is trivially absent.
        assert!(!t.contains("nope", 0));
    }

    #[test]
    fn sweep_expired_evicts_only_past_ttl_and_reports_count() {
        let mut t = ClaimTable::new();
        t.issue("a", PairingId::new("p1"), DeviceId::new("d1"), 10_000);
        t.issue("b", PairingId::new("p2"), DeviceId::new("d2"), 20_000);
        t.issue("c", PairingId::new("p3"), DeviceId::new("d3"), 30_000);

        // At now=15_000, only "a" (TTL 10_000) has elapsed.
        assert_eq!(t.live_len(15_000), 2);
        assert_eq!(t.sweep_expired(15_000), 1);
        assert_eq!(t.len(), 2);
        assert!(!t.contains("a", 15_000));
        assert!(t.contains("b", 15_000));
        assert!(t.contains("c", 15_000));

        // A second sweep at the same instant removes nothing more.
        assert_eq!(t.sweep_expired(15_000), 0);

        // Advancing past all TTLs sweeps the remainder.
        assert_eq!(t.sweep_expired(30_001), 2);
        assert!(t.is_empty());
    }
}
