//! Random identifier / token generation for the relay.
//!
//! All values come from the OS CSPRNG (`getrandom`). None of these are secrets
//! the relay must remember beyond their normal lifetime; claim tokens are the
//! only sensitive one and are single-use + short-TTL (see [`crate::claims`]).

/// Lowercase-hex a byte slice.
fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// A short random hex suffix (6 bytes → 12 hex chars) for human-readable ids.
pub fn random_suffix() -> String {
    let mut buf = [0u8; 6];
    getrandom::getrandom(&mut buf).expect("OS CSPRNG unavailable");
    hex(&buf)
}

/// An opaque per-connection id for logs/support (`conn_<hex>`).
pub fn connection_id() -> String {
    let mut buf = [0u8; 8];
    getrandom::getrandom(&mut buf).expect("OS CSPRNG unavailable");
    format!("conn_{}", hex(&buf))
}

/// A pairing claim token in the `NNNN-<hex>` shape the spec's fixtures show
/// (the 4 digits are the human-typeable part; the tail widens the space so a
/// token cannot be guessed within its short TTL).
pub fn claim_token() -> String {
    let mut buf = [0u8; 6];
    getrandom::getrandom(&mut buf).expect("OS CSPRNG unavailable");
    // 4 decimal digits from the first two bytes, then hex of the rest.
    let digits = u16::from_be_bytes([buf[0], buf[1]]) % 10_000;
    format!("{digits:04}-{}", hex(&buf[2..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_well_shaped() {
        assert!(connection_id().starts_with("conn_"));
        assert_ne!(connection_id(), connection_id());
        let t = claim_token();
        assert_eq!(t.as_bytes()[4], b'-', "NNNN- prefix");
        assert_ne!(claim_token(), claim_token());
    }
}
