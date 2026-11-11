//! What a worker needs to know about a PEM certificate without a TLS stack:
//! when it expires (SPECS §33).
//!
//! The worker serves HTTPS with a certificate `tailscale cert` issued and
//! reports its expiry in `worker status`. Reading `notAfter` needs an X.509
//! parser, and the crate has none: rustls deliberately exposes no
//! certificate fields, and `x509-parser`/`x509-cert` would be a new
//! dependency graph for one field. A certificate is DER, whose
//! tag-length-value framing is a dozen lines to walk, and `notAfter` sits at a
//! fixed position in it (RFC 5280 §4.1), so it is read here by hand. Anything
//! this cannot read is "validity unknown", never a guess.
//!
//! Platform-neutral on purpose: no rustls, so it compiles on Windows too,
//! where the worker does not run but the status rendering does.

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;

/// The DER bytes of the first `-----BEGIN <label>-----` block in `pem`.
pub fn first_pem_block(pem: &str, label: &str) -> Option<Vec<u8>> {
    let begin = format!("-----BEGIN {label}-----");
    let end = format!("-----END {label}-----");
    let mut body = String::new();
    let mut inside = false;
    for line in pem.lines().map(str::trim) {
        if !inside {
            inside = line == begin;
            continue;
        }
        if line == end {
            return STANDARD.decode(body.as_bytes()).ok();
        }
        body.push_str(line);
    }
    None
}

/// The leaf certificate's `notAfter` (the first certificate in `cert_pem`),
/// as unix seconds; `None` when it cannot be read.
pub fn certificate_not_after(cert_pem: &str) -> Option<u64> {
    not_after_der(&first_pem_block(cert_pem, "CERTIFICATE")?)
}

/// `notAfter` out of a DER `Certificate` (RFC 5280 §4.1):
///
/// ```text
/// Certificate  ::= SEQUENCE { tbsCertificate, signatureAlgorithm, signature }
/// TBSCertificate ::= SEQUENCE {
///     version [0] EXPLICIT Version DEFAULT v1,
///     serialNumber, signature, issuer,
///     validity SEQUENCE { notBefore Time, notAfter Time }, … }
/// ```
fn not_after_der(der: &[u8]) -> Option<u64> {
    const SEQUENCE: u8 = 0x30;
    const VERSION: u8 = 0xa0;
    let (tag, certificate, _) = tlv(der)?;
    if tag != SEQUENCE {
        return None;
    }
    let (tag, tbs, _) = tlv(certificate)?;
    if tag != SEQUENCE {
        return None;
    }
    let mut rest = tbs;
    let (tag, _, after) = tlv(rest)?;
    if tag == VERSION {
        rest = after;
    }
    // serialNumber, signature, issuer: skipped whatever they hold.
    for _ in 0..3 {
        rest = tlv(rest)?.2;
    }
    let (tag, validity, _) = tlv(rest)?;
    if tag != SEQUENCE {
        return None;
    }
    let (_, _, rest) = tlv(validity)?; // notBefore
    let (tag, not_after, _) = tlv(rest)?;
    parse_time(tag, not_after)
}

/// One DER tag-length-value: `(tag, contents, what follows)`. Single-byte
/// tags only (every tag a certificate's outer structure uses), definite
/// lengths only (DER has no other kind).
fn tlv(input: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, rest) = input.split_first()?;
    if tag & 0x1f == 0x1f {
        return None;
    }
    let (&first, rest) = rest.split_first()?;
    let (len, rest) = if first < 0x80 {
        (usize::from(first), rest)
    } else {
        let n = usize::from(first & 0x7f);
        if n == 0 || n > 4 || rest.len() < n {
            return None;
        }
        let len = rest[..n]
            .iter()
            .fold(0usize, |acc, &b| (acc << 8) | usize::from(b));
        (len, &rest[n..])
    };
    if rest.len() < len {
        return None;
    }
    Some((tag, &rest[..len], &rest[len..]))
}

/// A certificate `Time`: `UTCTime` (`YYMMDDHHMMSSZ`, years 1950–2049) or
/// `GeneralizedTime` (`YYYYMMDDHHMMSSZ`), always in UTC (RFC 5280 §4.1.2.5).
fn parse_time(tag: u8, bytes: &[u8]) -> Option<u64> {
    const UTC_TIME: u8 = 0x17;
    const GENERALIZED_TIME: u8 = 0x18;
    let text = std::str::from_utf8(bytes).ok()?;
    let digits = text.strip_suffix('Z')?;
    if !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let (year, rest) = match (tag, digits.len()) {
        (UTC_TIME, 12) => {
            let yy: i64 = digits[..2].parse().ok()?;
            (if yy >= 50 { 1900 + yy } else { 2000 + yy }, &digits[2..])
        }
        (GENERALIZED_TIME, 14) => (digits[..4].parse().ok()?, &digits[4..]),
        _ => return None,
    };
    let field = |i: usize| rest[i..i + 2].parse::<i64>().ok();
    let (month, day, hour, min, sec) = (field(0)?, field(2)?, field(4)?, field(6)?, field(8)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || min > 59 || sec > 60 {
        return None;
    }
    let days = crate::remote::transcript::days_from_civil(year, month, day);
    let secs = ((days * 24 + hour) * 60 + min) * 60 + sec;
    u64::try_from(secs).ok()
}

/// `YYYY-MM-DD` (UTC) for unix seconds: how `worker status` names a day.
pub fn format_date(unix_secs: u64) -> String {
    let mut stamp = crate::contracts::real::format_iso8601_utc(unix_secs);
    stamp.truncate(10);
    stamp
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The committed test certificate (`tests/fixtures/tls/README.md`).
    const FIXTURE: &str = include_str!("../../tests/fixtures/tls/cert.pem");
    /// Its notAfter, `2126-10-18 09:43:05 UTC`, as the README records it.
    const FIXTURE_NOT_AFTER: u64 = 4_947_990_185;

    #[test]
    fn the_fixture_expires_when_openssl_says_it_does() {
        assert_eq!(certificate_not_after(FIXTURE), Some(FIXTURE_NOT_AFTER));
        assert_eq!(format_date(FIXTURE_NOT_AFTER), "2126-10-18");
    }

    #[test]
    fn the_leaf_is_the_first_certificate_of_a_chain() {
        let chain =
            format!("{FIXTURE}-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n");
        assert_eq!(certificate_not_after(&chain), Some(FIXTURE_NOT_AFTER));
    }

    #[test]
    fn what_cannot_be_read_is_unknown_not_a_guess() {
        assert_eq!(certificate_not_after(""), None);
        assert_eq!(certificate_not_after("not pem"), None);
        assert_eq!(
            certificate_not_after("-----BEGIN CERTIFICATE-----\n!!!\n-----END CERTIFICATE-----\n"),
            None,
            "not base64"
        );
        assert_eq!(
            certificate_not_after("-----BEGIN CERTIFICATE-----\nMAA=\n-----END CERTIFICATE-----\n"),
            None,
            "an empty SEQUENCE"
        );
        // Truncated DER, at every length.
        let der = first_pem_block(FIXTURE, "CERTIFICATE").unwrap();
        for cut in 0..der.len().min(300) {
            let _ = not_after_der(&der[..cut]); // never panics
        }
    }

    #[test]
    fn both_time_encodings_are_read() {
        // UTCTime: 1950–2049.
        assert_eq!(parse_time(0x17, b"491231235959Z"), Some(2_524_607_999));
        assert_eq!(
            parse_time(0x17, b"500101000000Z"),
            None,
            "1950 is before the epoch: no unix second to give"
        );
        assert_eq!(parse_time(0x17, b"700101000000Z"), Some(0));
        assert_eq!(parse_time(0x18, b"20261111000000Z"), Some(1_794_355_200));
        // Not UTC, the wrong length or the wrong tag: unknown.
        assert_eq!(parse_time(0x17, b"700101000000+0100"), None);
        assert_eq!(parse_time(0x18, b"700101000000Z"), None);
        assert_eq!(parse_time(0x02, b"700101000000Z"), None);
        assert_eq!(parse_time(0x17, b"701301000000Z"), None, "month 13");
    }
}
