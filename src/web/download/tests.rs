use std::path::Path;

use super::*;

fn file(name: &str, size: u64) -> DownloadGrantRequest {
    DownloadGrantRequest {
        path: Path::new("/home/u/proj").join(name),
        name: name.to_string(),
        size,
    }
}

fn grant_two(grants: &mut DownloadGrants, token: &TokenId, now_ms: i64) -> Vec<String> {
    grants.grant(
        token,
        Path::new("/home/u"),
        &[file("a.png", 10), file("b.png", 20)],
        now_ms,
    )
}

#[test]
fn every_file_gets_its_own_unguessable_url_safe_ticket() {
    let mut grants = DownloadGrants::default();
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
fn a_grant_is_spent_once_by_its_own_credential() {
    let mut grants = DownloadGrants::default();
    let token = TokenId::generate();
    let tickets = grant_two(&mut grants, &token, 1_000);
    let grant = grants.redeem(&tickets[1], &token, 1_000).expect("granted");
    assert_eq!(grant.name, "b.png");
    assert_eq!(grant.size, 20);
    assert_eq!(grant.path, Path::new("/home/u/proj/b.png"));
    assert_eq!(grant.home, Path::new("/home/u"));
    assert_eq!(grant.token, token);
    assert_eq!(grant.expires_ms, 1_000 + DOWNLOAD_GRANT_TTL_MS);
    assert_eq!(
        grants.redeem(&tickets[1], &token, 1_000),
        None,
        "single use"
    );
    assert_eq!(grants.len(), 1, "the other file's grant is untouched");
    assert_eq!(grants.redeem("no-such-ticket", &token, 1_000), None);
}

#[cfg(unix)]
#[test]
fn another_credential_is_told_nothing_and_spends_nothing() {
    let mut grants = DownloadGrants::default();
    let mine = TokenId::generate();
    let theirs = TokenId::generate();
    let tickets = grant_two(&mut grants, &mine, 0);
    assert_eq!(grants.redeem(&tickets[0], &theirs, 0), None);
    assert_eq!(grants.len(), 2, "kept for its owner");
    assert!(grants.redeem(&tickets[0], &mine, 0).is_some());
}

#[cfg(unix)]
#[test]
fn a_grant_lives_two_minutes_and_is_pruned_lazily() {
    assert_eq!(DOWNLOAD_GRANT_TTL_MS, 120_000);
    let mut grants = DownloadGrants::default();
    let token = TokenId::generate();
    let tickets = grant_two(&mut grants, &token, 0);
    // The last millisecond still counts.
    assert!(grants
        .redeem(&tickets[0], &token, DOWNLOAD_GRANT_TTL_MS)
        .is_some());
    assert_eq!(
        grants.redeem(&tickets[1], &token, DOWNLOAD_GRANT_TTL_MS + 1),
        None
    );
    assert!(grants.is_empty(), "pruned by the redemption");
}

#[test]
fn granting_prunes_what_has_expired() {
    let mut grants = DownloadGrants::default();
    let token = TokenId::generate();
    grant_two(&mut grants, &token, 0);
    grant_two(&mut grants, &token, DOWNLOAD_GRANT_TTL_MS + 1);
    assert_eq!(grants.len(), 2, "only the second pair is left");
    assert_eq!(grants.expire(2 * DOWNLOAD_GRANT_TTL_MS + 2), 2);
    assert!(grants.is_empty());
}

#[test]
fn a_plain_name_is_the_same_both_ways() {
    assert_eq!(
        content_disposition("report-2026_v1.txt"),
        "attachment; filename=\"report-2026_v1.txt\"; filename*=UTF-8''report-2026_v1.txt"
    );
}

#[test]
fn a_space_or_unicode_is_percent_encoded_and_replaced_in_the_fallback() {
    assert_eq!(
        content_disposition("résumé (final).pdf"),
        "attachment; filename=\"r_sum_ (final).pdf\"; \
         filename*=UTF-8''r%C3%A9sum%C3%A9%20%28final%29.pdf"
    );
    assert_eq!(
        content_disposition("日本.txt"),
        "attachment; filename=\"__.txt\"; filename*=UTF-8''%E6%97%A5%E6%9C%AC.txt"
    );
}

#[test]
fn quotes_backslashes_and_controls_cannot_break_the_header() {
    let header = content_disposition("a\"b\\c\td;e%f.txt");
    assert_eq!(
        header,
        "attachment; filename=\"a_b_c_d;e%f.txt\"; filename*=UTF-8''a%22b%5Cc%09d%3Be%25f.txt"
    );
    assert!(header
        .chars()
        .all(|c| c.is_ascii() && !c.is_ascii_control()));
}
