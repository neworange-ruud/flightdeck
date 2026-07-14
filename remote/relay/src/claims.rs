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

    /// Whether `token` is currently present (issued and not yet redeemed).
    /// Used to decide whether a desktop's `claim_token_hint` is free to reuse
    /// (spec §5.2 amendment): a hint that collides with a live token is refused
    /// and a fresh random token is minted instead. An expired-but-unredeemed
    /// entry still counts as present (it is only removed on a redeem attempt),
    /// which is the safe answer — the relay never re-issues a token string that
    /// another pairing could still be trying to redeem.
    pub fn contains(&self, token: &str) -> bool {
        self.entries.contains_key(token)
    }

    /// Number of live (issued, un-redeemed) tokens. Test/metric helper.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the table has no live tokens.
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
}
