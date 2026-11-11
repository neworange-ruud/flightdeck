use std::path::Path;

use super::*;

fn file(name: &str, size: u64) -> UploadGrantRequest {
    UploadGrantRequest {
        final_name: name.to_string(),
        size,
        replace: false,
    }
}

fn grant_two(grants: &mut UploadGrants, token: &TokenId, now_ms: i64) -> Vec<String> {
    let (tickets, expired) = grants.grant(
        token,
        7,
        Path::new("/home/u"),
        Path::new("/home/u/assets"),
        &[file("a.png", 10), file("b.png", 20)],
        now_ms,
    );
    assert!(expired.is_empty());
    tickets
}

#[test]
fn every_file_gets_its_own_unguessable_url_safe_ticket() {
    let mut grants = UploadGrants::default();
    let tickets = grant_two(&mut grants, &TokenId::generate(), 0);
    assert_eq!(tickets.len(), 2);
    assert_ne!(tickets[0], tickets[1]);
    for ticket in &tickets {
        assert_eq!(ticket.len(), 22, "128 bits, base64url, no padding");
        assert!(
            ticket
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "{ticket}"
        );
    }
    assert_eq!(grants.len(), 2);
}

#[cfg(unix)]
#[test]
fn a_ticket_is_spent_by_its_first_redemption() {
    let token = TokenId::generate();
    let mut grants = UploadGrants::default();
    let tickets = grant_two(&mut grants, &token, 1_000);

    let (first, _) = grants.redeem(&tickets[0], &token, 10, 1_500);
    let Redeem::Granted(grant) = first else {
        panic!("granted: {first:?}");
    };
    assert_eq!(grant.final_path(), Path::new("/home/u/assets/a.png"));
    assert_eq!(grant.batch, 7);
    assert_eq!(grant.expires_ms, 1_000 + UPLOAD_GRANT_TTL_MS);

    let (again, _) = grants.redeem(&tickets[0], &token, 10, 1_600);
    assert_eq!(again, Redeem::Invalid, "single use");
    assert_eq!(grants.len(), 1, "the other file's grant is untouched");
}

#[cfg(unix)]
#[test]
fn another_credential_is_told_nothing_and_spends_nothing() {
    let token = TokenId::generate();
    let mut grants = UploadGrants::default();
    let tickets = grant_two(&mut grants, &token, 0);
    let (verdict, _) = grants.redeem(&tickets[0], &TokenId::generate(), 10, 1);
    assert_eq!(verdict, Redeem::Invalid);
    // Not even the size is revealed to a stranger.
    let (verdict, _) = grants.redeem(&tickets[0], &TokenId::generate(), 999, 1);
    assert_eq!(verdict, Redeem::Invalid);
    let (verdict, _) = grants.redeem("no-such-ticket", &token, 10, 1);
    assert_eq!(verdict, Redeem::Invalid);
    assert_eq!(grants.len(), 2);
    let (verdict, _) = grants.redeem(&tickets[0], &token, 10, 2);
    assert!(matches!(verdict, Redeem::Granted(_)), "{verdict:?}");
}

#[cfg(unix)]
#[test]
fn a_wrong_length_is_refused_and_the_grant_kept() {
    let token = TokenId::generate();
    let mut grants = UploadGrants::default();
    let tickets = grant_two(&mut grants, &token, 0);
    let (verdict, _) = grants.redeem(&tickets[1], &token, 21, 1);
    assert_eq!(verdict, Redeem::SizeMismatch { expected: 20 });
    let (verdict, _) = grants.redeem(&tickets[1], &token, 20, 2);
    assert!(matches!(verdict, Redeem::Granted(_)), "{verdict:?}");
}

#[test]
fn an_unused_grant_expires_after_two_minutes_and_is_reported_once() {
    let token = TokenId::generate();
    let mut grants = UploadGrants::default();
    let tickets = grant_two(&mut grants, &token, 0);
    assert!(grants.expire(UPLOAD_GRANT_TTL_MS).is_empty(), "not yet");
    let expired = grants.expire(UPLOAD_GRANT_TTL_MS + 1);
    assert_eq!(expired.len(), 2);
    assert_eq!(expired[0].final_name, "a.png");
    let failed = expired[0].failed("expired", 0);
    assert!(!failed.ok);
    assert_eq!(failed.batch, 7);
    assert_eq!(failed.path, Path::new("/home/u/assets/a.png"));
    assert!(grants.is_empty());
    assert!(grants.expire(UPLOAD_GRANT_TTL_MS * 10).is_empty(), "once");
    #[cfg(unix)]
    {
        let (verdict, _) = grants.redeem(&tickets[0], &token, 10, UPLOAD_GRANT_TTL_MS + 2);
        assert_eq!(verdict, Redeem::Invalid);
    }
    drop(tickets);
}

#[cfg(unix)]
#[test]
fn a_redemption_prunes_and_returns_what_expired_meanwhile() {
    let token = TokenId::generate();
    let mut grants = UploadGrants::default();
    let old = grant_two(&mut grants, &token, 0);
    let (fresh, expired) = grants.grant(
        &token,
        8,
        Path::new("/home/u"),
        Path::new("/home/u"),
        &[file("c", 1)],
        UPLOAD_GRANT_TTL_MS - 10,
    );
    assert!(expired.is_empty());
    let (verdict, expired) = grants.redeem(&fresh[0], &token, 1, UPLOAD_GRANT_TTL_MS + 5);
    assert!(matches!(verdict, Redeem::Granted(_)), "{verdict:?}");
    assert_eq!(expired.len(), 2, "the old batch's two grants");
    let (verdict, _) = grants.redeem(&old[0], &token, 10, UPLOAD_GRANT_TTL_MS + 6);
    assert_eq!(verdict, Redeem::Invalid);
}

#[test]
fn sizes_read_like_a_person_would_say_them() {
    assert_eq!(size_label(0), "0 B");
    assert_eq!(size_label(1023), "1023 B");
    assert_eq!(size_label(1024), "1 KB");
    assert_eq!(size_label(3_500), "3.4 KB");
    assert_eq!(size_label(256 * 1024 * 1024), "256 MB");
    assert_eq!(size_label(1_610_612_736), "1.5 GB");
}
