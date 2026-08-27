//! Tests for the browser access credentials.
//!
//! Everything runs against the [`FakeFs`] and [`FakeClock`] seams: no real file
//! is written under `~/.flightdeck`, and no test sleeps — the 120-second code
//! window and the 60-second lockout are both crossed by advancing the fake
//! clock. The one exception is the permissions test, which needs a real inode
//! and uses a `tempfile` directory.
//!
//! Refusal paths carry the weight here (SPECS §26): expired, wrong, replayed,
//! revoked, rotated and rate-limited each get their own test, because those are
//! the paths a browser and an attacker actually take.

use super::*;
use crate::testing::{FakeClock, FakeFs};

const ADDR_A: &str = "192.168.2.14";
const ADDR_B: &str = "192.168.2.99";
const PATH: &str = "/home/user/.flightdeck/web.json";

/// A store over in-memory seams, with the fake clock returned so a test can
/// move time.
fn store() -> (CredentialStore, Arc<FakeFs>, Arc<FakeClock>) {
    let fs = Arc::new(FakeFs::new());
    let clock = Arc::new(FakeClock::default());
    // Start the clock somewhere non-zero so a saturating_sub bug cannot hide
    // behind an origin of 0.
    clock.set_millis(1_000_000);
    clock.set_unix_secs(1_700_000_000);
    let s = CredentialStore::open(fs.clone(), clock.clone(), PATH);
    (s, fs, clock)
}

/// Mint a code and immediately exchange it, returning the token secret.
fn bootstrap(s: &mut CredentialStore, address: &str) -> String {
    let code = s.mint_bootstrap_code();
    let digits = code.reveal().to_string();
    s.exchange_code(address, &digits, Some("Test Browser"))
        .expect("exchange succeeds")
        .reveal()
        .to_string()
}

// -- happy path ------------------------------------------------------------

#[test]
fn mint_then_exchange_yields_a_usable_token() {
    let (mut s, _fs, _clock) = store();

    let code = s.mint_bootstrap_code();
    assert_eq!(code.reveal().len(), BOOTSTRAP_CODE_DIGITS);
    assert!(code.reveal().chars().all(|c| c.is_ascii_digit()));
    assert_eq!(s.bootstrap_seconds_remaining(), Some(120));

    let digits = code.reveal().to_string();
    let token = s
        .exchange_code(ADDR_A, &digits, Some("Safari on iPhone"))
        .expect("exchange");

    assert_eq!(s.active_tokens().count(), 1);
    assert_eq!(
        s.verify_token(ADDR_A, token.reveal()).expect("verify"),
        *token.id()
    );
    assert_eq!(
        s.records()[0].label.as_deref(),
        Some("Safari on iPhone"),
        "the label is kept for the desktop's browser list"
    );
}

#[test]
fn a_code_stops_being_offered_once_it_is_spent() {
    let (mut s, _fs, _clock) = store();
    let digits = s.mint_bootstrap_code().reveal().to_string();
    assert!(s.bootstrap_code().is_some());
    s.exchange_code(ADDR_A, &digits, None).expect("exchange");
    assert!(
        s.bootstrap_code().is_none(),
        "the overlay must stop showing a code that no longer works"
    );
}

#[test]
fn whitespace_and_separators_in_the_presented_code_are_tolerated() {
    let (mut s, _fs, _clock) = store();
    let digits = s.mint_bootstrap_code().reveal().to_string();
    let spaced = format!(
        " {} {}-{} {} ",
        &digits[0..1],
        &digits[1..2],
        &digits[2..3],
        &digits[3..4]
    );
    assert!(s.exchange_code(ADDR_A, &spaced, None).is_ok());
}

#[test]
fn multiple_browsers_hold_tokens_at_the_same_time() {
    let (mut s, _fs, _clock) = store();
    let phone = bootstrap(&mut s, ADDR_A);
    let laptop = bootstrap(&mut s, ADDR_B);
    assert_ne!(phone, laptop);
    assert_eq!(s.active_tokens().count(), 2);
    assert!(s.verify_token(ADDR_A, &phone).is_ok());
    assert!(s.verify_token(ADDR_B, &laptop).is_ok());
}

// -- refusals: the bootstrap code -----------------------------------------

#[test]
fn an_expired_code_is_refused_without_sleeping() {
    let (mut s, _fs, clock) = store();
    let digits = s.mint_bootstrap_code().reveal().to_string();

    // One millisecond inside the window: still good.
    clock.advance_millis(BOOTSTRAP_CODE_TTL_MS - 1);
    assert_eq!(s.bootstrap_seconds_remaining(), Some(0));
    assert!(s.bootstrap_code().is_some());

    // One millisecond past it: dead, and the overlay stops offering it.
    clock.advance_millis(1);
    assert!(s.bootstrap_code().is_none());
    assert_eq!(
        s.exchange_code(ADDR_A, &digits, None),
        Err(AuthFailure::CodeExpired)
    );
}

#[test]
fn an_expired_code_is_not_a_guess_oracle() {
    // Past the window every input gets the same answer, so an attacker learns
    // nothing about whether their digits were right.
    let (mut s, _fs, clock) = store();
    let digits = s.mint_bootstrap_code().reveal().to_string();
    clock.advance_millis(BOOTSTRAP_CODE_TTL_MS);
    assert_eq!(
        s.exchange_code(ADDR_A, &digits, None),
        Err(AuthFailure::CodeExpired)
    );
    assert_eq!(
        s.exchange_code(ADDR_B, "0000", None),
        Err(AuthFailure::CodeExpired)
    );
}

#[test]
fn a_wrong_code_is_refused() {
    let (mut s, _fs, _clock) = store();
    let digits = s.mint_bootstrap_code().reveal().to_string();
    let wrong = if digits == "0000" { "1111" } else { "0000" };
    assert_eq!(
        s.exchange_code(ADDR_A, wrong, None),
        Err(AuthFailure::WrongCode)
    );
    // And the real code still works: a miss must not burn the user's code.
    assert!(s.exchange_code(ADDR_A, &digits, None).is_ok());
}

#[test]
fn a_code_cannot_be_exchanged_twice() {
    let (mut s, _fs, _clock) = store();
    let digits = s.mint_bootstrap_code().reveal().to_string();
    assert!(s.exchange_code(ADDR_A, &digits, None).is_ok());
    assert_eq!(
        s.exchange_code(ADDR_B, &digits, None),
        Err(AuthFailure::CodeAlreadyUsed),
        "replaying a spent code is distinguishable from guessing wrong"
    );
    assert_eq!(s.active_tokens().count(), 1, "no second token was issued");
}

#[test]
fn exchanging_when_no_code_was_minted_is_its_own_failure() {
    let (mut s, _fs, _clock) = store();
    assert_eq!(
        s.exchange_code(ADDR_A, "1234", None),
        Err(AuthFailure::NoCodeOutstanding)
    );
    assert_eq!(
        AuthFailure::NoCodeOutstanding.screen(),
        AccessScreen::CodeEntry
    );
}

#[test]
fn clearing_the_overlay_kills_the_code_immediately() {
    let (mut s, _fs, _clock) = store();
    let digits = s.mint_bootstrap_code().reveal().to_string();
    s.clear_bootstrap_code();
    assert_eq!(
        s.exchange_code(ADDR_A, &digits, None),
        Err(AuthFailure::NoCodeOutstanding)
    );
}

#[test]
fn minting_again_replaces_the_previous_code() {
    let (mut s, _fs, _clock) = store();
    let first = s.mint_bootstrap_code().reveal().to_string();
    let second = s.mint_bootstrap_code().reveal().to_string();
    // (A fresh mint could randomly repeat the digits; only assert when it did
    // not, so the test is not flaky.)
    if first != second {
        assert_eq!(
            s.exchange_code(ADDR_A, &first, None),
            Err(AuthFailure::WrongCode)
        );
    }
    assert!(s.exchange_code(ADDR_B, &second, None).is_ok());
}

#[test]
fn a_code_is_burned_by_distributed_guessing_that_no_single_address_pays_for() {
    // The per-address budget cannot stop an attacker with many source
    // addresses, so the code itself has a global failure budget.
    let (mut s, _fs, _clock) = store();
    let digits = s.mint_bootstrap_code().reveal().to_string();
    let wrong = if digits == "0000" { "1111" } else { "0000" };

    for i in 0..BOOTSTRAP_CODE_MAX_FAILURES {
        // A different address each time, so no lockout ever trips.
        let addr = format!("10.0.0.{i}");
        assert_eq!(
            s.exchange_code(&addr, wrong, None),
            Err(AuthFailure::WrongCode),
            "attempt {i} should still be judged on the digits"
        );
    }

    assert!(s.bootstrap_code().is_none(), "the code is burned");
    assert_eq!(
        s.exchange_code("10.0.0.200", &digits, None),
        Err(AuthFailure::CodeExpired),
        "even the correct digits no longer work"
    );
}

// -- refusals: the persistent token ---------------------------------------

#[test]
fn an_unknown_token_is_refused() {
    let (mut s, _fs, _clock) = store();
    bootstrap(&mut s, ADDR_A);
    assert_eq!(
        s.verify_token(ADDR_B, "not-a-token-at-all"),
        Err(AuthFailure::UnknownToken)
    );
    assert_eq!(
        AuthFailure::UnknownToken.screen(),
        AccessScreen::CodeEntry,
        "an unrecognised cookie is the plain code-entry case, not an alarm"
    );
}

#[test]
fn a_revoked_token_is_refused_and_says_so() {
    let (mut s, _fs, _clock) = store();
    let secret = bootstrap(&mut s, ADDR_A);
    let id = s.verify_token(ADDR_A, &secret).expect("verify").clone();

    assert!(s.revoke(&id).expect("revoke persists"));
    assert_eq!(s.active_tokens().count(), 0);
    assert_eq!(
        s.verify_token(ADDR_A, &secret),
        Err(AuthFailure::TokenRevoked),
        "the browser needs the amber 'access revoked' screen, not code entry"
    );
    assert_eq!(AuthFailure::TokenRevoked.screen(), AccessScreen::Revoked);
}

#[test]
fn revoking_one_browser_leaves_the_others_alone() {
    let (mut s, _fs, _clock) = store();
    let phone = bootstrap(&mut s, ADDR_A);
    let laptop = bootstrap(&mut s, ADDR_B);
    let phone_id = s.verify_token(ADDR_A, &phone).expect("verify").clone();

    assert!(s.revoke(&phone_id).expect("revoke"));
    assert_eq!(
        s.verify_token(ADDR_A, &phone),
        Err(AuthFailure::TokenRevoked)
    );
    assert!(s.verify_token(ADDR_B, &laptop).is_ok());
}

#[test]
fn revoking_an_unknown_or_already_revoked_id_is_a_no_op() {
    let (mut s, _fs, _clock) = store();
    let secret = bootstrap(&mut s, ADDR_A);
    let id = s.verify_token(ADDR_A, &secret).expect("verify").clone();
    assert!(s.revoke(&id).expect("first revoke"));
    assert!(!s.revoke(&id).expect("second revoke"), "already revoked");
    assert!(!s.revoke(&TokenId::generate()).expect("unknown id"));
}

#[test]
fn revoke_all_withdraws_every_browser() {
    let (mut s, _fs, _clock) = store();
    let a = bootstrap(&mut s, ADDR_A);
    let b = bootstrap(&mut s, ADDR_B);
    assert_eq!(s.revoke_all().expect("revoke all"), 2);
    assert_eq!(s.revoke_all().expect("again"), 0);
    assert_eq!(s.verify_token(ADDR_A, &a), Err(AuthFailure::TokenRevoked));
    assert_eq!(s.verify_token(ADDR_B, &b), Err(AuthFailure::TokenRevoked));
}

#[test]
fn rotate_invalidates_every_prior_token_and_offers_a_fresh_code() {
    let (mut s, _fs, _clock) = store();
    let old = bootstrap(&mut s, ADDR_A);

    let (code, error) = s.rotate();
    assert!(error.is_none(), "the fake filesystem persists fine");
    assert_eq!(
        s.verify_token(ADDR_A, &old),
        Err(AuthFailure::TokenRevoked),
        "a rotation must not leave the old bookmark working"
    );
    assert_eq!(s.active_tokens().count(), 0);

    // The fresh code works and yields a token that is not the old one.
    let digits = code.reveal().to_string();
    let new = s.exchange_code(ADDR_A, &digits, None).expect("exchange");
    assert_ne!(new.reveal(), old);
    assert!(s.verify_token(ADDR_A, new.reveal()).is_ok());
}

// -- rate limiting ---------------------------------------------------------

#[test]
fn the_limiter_trips_on_the_fourth_failed_attempt_and_releases_after_sixty_seconds() {
    let (mut s, _fs, clock) = store();
    let digits = s.mint_bootstrap_code().reveal().to_string();
    let wrong = if digits == "0000" { "1111" } else { "0000" };

    // Artboard 2b's footer copy starts here.
    assert_eq!(s.attempts_remaining(ADDR_A), 3);

    for expected_left in [2, 1, 0] {
        assert_eq!(
            s.exchange_code(ADDR_A, wrong, None),
            Err(AuthFailure::WrongCode),
            "the first three attempts are judged on their merits"
        );
        assert_eq!(s.attempts_remaining(ADDR_A), expected_left);
    }

    // The fourth attempt is refused by the limiter, not the credential — and
    // even the *correct* code does not get through.
    let failure = s.exchange_code(ADDR_A, &digits, None).expect_err("locked");
    assert_eq!(
        failure,
        AuthFailure::RateLimited {
            retry_after_ms: RATE_LIMIT_LOCKOUT_MS
        }
    );
    assert!(failure.is_rate_limited());
    assert_eq!(failure.screen(), AccessScreen::RateLimited);
    assert_eq!(s.lockout_remaining_ms(ADDR_A), Some(RATE_LIMIT_LOCKOUT_MS));

    // One millisecond short of 60s: still locked, and the countdown is honest.
    clock.advance_millis(RATE_LIMIT_LOCKOUT_MS - 1);
    assert_eq!(s.lockout_remaining_ms(ADDR_A), Some(1));
    assert!(s
        .exchange_code(ADDR_A, &digits, None)
        .expect_err("still locked")
        .is_rate_limited());

    // 60s served: full budget again, and the code works.
    clock.advance_millis(1);
    assert_eq!(s.lockout_remaining_ms(ADDR_A), None);
    assert_eq!(s.attempts_remaining(ADDR_A), 3);
    assert!(s.exchange_code(ADDR_A, &digits, None).is_ok());
}

#[test]
fn the_limiter_is_per_address_not_global() {
    // The whole point: a phone guessing on the guest wifi must not lock the
    // user's own desktop browser out of their own machine.
    let (mut s, _fs, _clock) = store();
    let digits = s.mint_bootstrap_code().reveal().to_string();
    let wrong = if digits == "0000" { "1111" } else { "0000" };

    for _ in 0..RATE_LIMIT_MAX_FAILURES {
        let _ = s.exchange_code(ADDR_A, wrong, None);
    }
    assert!(s
        .exchange_code(ADDR_A, wrong, None)
        .expect_err("A is locked")
        .is_rate_limited());

    assert_eq!(s.attempts_remaining(ADDR_B), 3, "B is untouched");
    assert!(
        s.exchange_code(ADDR_B, &digits, None).is_ok(),
        "B must still be able to get in"
    );
}

#[test]
fn a_successful_exchange_resets_the_counter() {
    let (mut s, _fs, _clock) = store();
    let digits = s.mint_bootstrap_code().reveal().to_string();
    let wrong = if digits == "0000" { "1111" } else { "0000" };

    let _ = s.exchange_code(ADDR_A, wrong, None);
    let _ = s.exchange_code(ADDR_A, wrong, None);
    assert_eq!(s.attempts_remaining(ADDR_A), 1);

    assert!(s.exchange_code(ADDR_A, &digits, None).is_ok());
    assert_eq!(
        s.attempts_remaining(ADDR_A),
        3,
        "getting it right wipes the slate"
    );
}

#[test]
fn a_successful_token_verify_resets_the_counter() {
    let (mut s, _fs, _clock) = store();
    let secret = bootstrap(&mut s, ADDR_A);
    let _ = s.verify_token(ADDR_A, "garbage");
    let _ = s.verify_token(ADDR_A, "garbage");
    assert_eq!(s.attempts_remaining(ADDR_A), 1);
    assert!(s.verify_token(ADDR_A, &secret).is_ok());
    assert_eq!(s.attempts_remaining(ADDR_A), 3);
}

#[test]
fn an_unknown_token_spends_the_budget_but_a_revoked_one_does_not() {
    let (mut s, _fs, _clock) = store();
    let secret = bootstrap(&mut s, ADDR_A);
    let id = s.verify_token(ADDR_A, &secret).expect("verify").clone();
    s.revoke(&id).expect("revoke");

    // The user's own stale cookie, presented repeatedly by a reloading tab:
    // it must not lock that browser out of the code-entry screen.
    for _ in 0..10 {
        assert_eq!(
            s.verify_token(ADDR_A, &secret),
            Err(AuthFailure::TokenRevoked)
        );
    }
    assert_eq!(s.attempts_remaining(ADDR_A), 3);

    // A token the host never issued is a different matter.
    let _ = s.verify_token(ADDR_A, "forged");
    assert_eq!(s.attempts_remaining(ADDR_A), 2);
}

#[test]
fn a_locked_address_is_refused_before_the_credential_is_even_looked_at() {
    let (mut s, _fs, _clock) = store();
    let secret = bootstrap(&mut s, ADDR_A);
    for _ in 0..RATE_LIMIT_MAX_FAILURES {
        let _ = s.verify_token(ADDR_A, "forged");
    }
    assert!(s
        .verify_token(ADDR_A, &secret)
        .expect_err("locked out even with a valid token")
        .is_rate_limited());
}

// -- persistence -----------------------------------------------------------

#[test]
fn persistence_round_trips_through_serde() {
    let fs = Arc::new(FakeFs::new());
    let clock = Arc::new(FakeClock::default());
    clock.set_millis(1_000_000);
    clock.set_unix_secs(1_700_000_000);

    let secret = {
        let mut s = CredentialStore::open(fs.clone(), clock.clone(), PATH);
        let secret = bootstrap(&mut s, ADDR_A);
        s.save().expect("save");
        secret
    };

    // A brand-new store over the same file — as after a host restart. The
    // bookmark must keep working (D10).
    let mut reopened = CredentialStore::open(fs.clone(), clock.clone(), PATH);
    assert!(reopened.load_error().is_none());
    assert_eq!(reopened.active_tokens().count(), 1);
    assert!(reopened.verify_token(ADDR_A, &secret).is_ok());
    assert_eq!(reopened.records()[0].label.as_deref(), Some("Test Browser"));
    assert_eq!(reopened.records()[0].created_unix_secs, 1_700_000_000);
}

#[test]
fn the_exchange_persists_before_the_cookie_is_handed_out() {
    // A host that dies right after the exchange must still honour the cookie
    // the browser just stored, so exchange_code writes the file itself.
    let (mut s, fs, _clock) = store();
    bootstrap(&mut s, ADDR_A);
    assert!(
        fs.file_contents(Path::new(PATH)).is_some(),
        "exchange_code did not persist"
    );
    assert!(s.last_persist_error().is_none());
}

#[test]
fn the_file_never_contains_the_token_itself() {
    let (mut s, fs, _clock) = store();
    let secret = bootstrap(&mut s, ADDR_A);
    let json = fs.file_contents(Path::new(PATH)).expect("written");
    assert!(
        !json.contains(&secret),
        "web.json must hold only the hash of the token"
    );
    assert!(json.contains(&token_hash(&secret)));
}

#[test]
fn the_file_never_contains_the_bootstrap_code() {
    let (mut s, fs, _clock) = store();
    let digits = s.mint_bootstrap_code().reveal().to_string();
    s.exchange_code(ADDR_A, &digits, None).expect("exchange");
    let json = fs.file_contents(Path::new(PATH)).expect("written");
    assert!(
        !json.contains(&digits),
        "the bootstrap code is memory-only and must not be persisted"
    );
}

#[test]
fn a_partial_file_loads_with_the_rest_defaulted() {
    // A hand-written or older file must not brick the store.
    let fs = Arc::new(FakeFs::new().with_file(PATH, r#"{"version":1}"#));
    let clock = Arc::new(FakeClock::default());
    let s = CredentialStore::open(fs, clock, PATH);
    assert!(s.load_error().is_none());
    assert_eq!(s.records().len(), 0);
}

#[test]
fn a_corrupt_file_is_reported_rather_than_silently_discarded() {
    let fs = Arc::new(FakeFs::new().with_file(PATH, "{ not json"));
    let clock = Arc::new(FakeClock::default());
    let s = CredentialStore::open(fs, clock, PATH);
    assert!(
        s.load_error().is_some(),
        "an unreadable file that exists is worth telling the user about"
    );
    assert_eq!(s.active_tokens().count(), 0);
}

#[test]
fn a_missing_file_is_not_an_error() {
    let (s, _fs, _clock) = store();
    assert!(s.load_error().is_none());
    assert_eq!(s.active_tokens().count(), 0);
}

#[test]
fn a_stale_file_read_cannot_resurrect_a_revoked_token() {
    let (mut s, fs, _clock) = store();
    let secret = bootstrap(&mut s, ADDR_A);
    let stale = fs.file_contents(Path::new(PATH)).expect("written");

    let id = s.verify_token(ADDR_A, &secret).expect("verify").clone();
    s.revoke(&id).expect("revoke");

    // Something puts the pre-revocation file back: a restored backup, a synced
    // home directory, a second FlightDeck saving an older token set.
    fs.write(Path::new(PATH), &stale).expect("overwrite");
    s.reload().expect("reload");

    assert_eq!(
        s.verify_token(ADDR_A, &secret),
        Err(AuthFailure::TokenRevoked),
        "revocation is one-way for the life of the process"
    );
    assert_eq!(s.active_tokens().count(), 0);
}

#[test]
fn reload_picks_up_a_token_added_out_of_band() {
    // The flip side of the test above: reload is still a real reload.
    let (mut s, fs, clock) = store();
    let secret = {
        let mut other = CredentialStore::open(fs.clone(), clock.clone(), PATH);
        bootstrap(&mut other, ADDR_B)
    };
    assert_eq!(s.active_tokens().count(), 0);
    s.reload().expect("reload");
    assert!(s.verify_token(ADDR_B, &secret).is_ok());
}

#[test]
fn tombstones_are_capped_so_the_file_cannot_grow_without_bound() {
    let (mut s, _fs, clock) = store();
    for i in 0..(REVOKED_TOMBSTONE_CAP + 5) {
        clock.set_unix_secs(1_700_000_000 + i as u64);
        let secret = bootstrap(&mut s, ADDR_A);
        let id = s.verify_token(ADDR_A, &secret).expect("verify").clone();
        s.revoke(&id).expect("revoke");
    }
    assert_eq!(s.records().len(), REVOKED_TOMBSTONE_CAP);
    assert_eq!(s.active_tokens().count(), 0);
}

#[cfg(unix)]
#[test]
fn the_credential_file_is_owner_only_on_unix() {
    use crate::contracts::{RealClock, RealFs};
    use std::os::unix::fs::PermissionsExt;

    // A real inode is needed to observe a real mode, so this one test uses a
    // temporary directory — never the developer's `~/.flightdeck`.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(".flightdeck").join("web.json");

    let mut s = CredentialStore::open(Arc::new(RealFs), Arc::new(RealClock), path.clone());
    bootstrap(&mut s, ADDR_A);

    let mode = std::fs::metadata(&path)
        .expect("metadata")
        .permissions()
        .mode();
    assert_eq!(
        mode & 0o777,
        0o600,
        "web.json lists every browser with access and must not be world-readable"
    );
}

// -- secrets stay out of logs ---------------------------------------------

#[test]
fn debug_output_never_contains_a_secret() {
    let (mut s, _fs, _clock) = store();

    let code = s.mint_bootstrap_code();
    let digits = code.reveal().to_string();
    let rendered = format!("{code:?}");
    assert!(
        !rendered.contains(&digits),
        "BootstrapCode Debug leaked the code: {rendered}"
    );
    assert!(rendered.contains("<redacted>"));

    let token = s.exchange_code(ADDR_A, &digits, None).expect("exchange");
    let rendered = format!("{token:?}");
    assert!(
        !rendered.contains(token.reveal()),
        "AccessToken Debug leaked the token: {rendered}"
    );
    assert!(rendered.contains("<redacted>"));

    // And nothing that merely *holds* the code can print it either — the whole
    // reason the store has a hand-written Debug.
    let mut fresh = s;
    let code = fresh.mint_bootstrap_code();
    let rendered = format!("{fresh:?}");
    assert!(
        !rendered.contains(code.reveal()),
        "CredentialStore Debug leaked the code: {rendered}"
    );
}

#[test]
fn a_token_id_is_not_derived_from_its_secret() {
    // The id is public (it is what revoke takes and what the desktop lists), so
    // it must carry no information about the token it names.
    let (mut s, _fs, _clock) = store();
    let digits = s.mint_bootstrap_code().reveal().to_string();
    let token = s.exchange_code(ADDR_A, &digits, None).expect("exchange");
    let secret = token.reveal();
    let id = token.id().as_str();
    assert!(!secret.contains(id));
    assert!(!token_hash(secret).contains(id));
}

// -- small units ----------------------------------------------------------

#[test]
fn secret_eq_matches_only_identical_bytes() {
    assert!(secret_eq(b"1234", b"1234"));
    assert!(!secret_eq(b"1234", b"1235"));
    assert!(!secret_eq(b"1234", b"12345"), "differing lengths");
    assert!(!secret_eq(b"1234", b""), "empty input");
    assert!(secret_eq(b"", b""));
}

#[test]
fn generated_codes_are_the_right_shape_and_not_constant() {
    let mut seen = std::collections::HashSet::new();
    for _ in 0..200 {
        let code = random_code();
        assert_eq!(code.len(), BOOTSTRAP_CODE_DIGITS);
        assert!(code.chars().all(|c| c.is_ascii_digit()));
        seen.insert(code);
    }
    // 200 draws from 10 000 values collapsing to one would mean a broken RNG.
    assert!(seen.len() > 100, "codes are not varying: {}", seen.len());
}

#[test]
fn failure_spellings_are_stable_and_carry_nothing_secret() {
    for (failure, spelling, screen) in [
        (
            AuthFailure::NoCodeOutstanding,
            "no_code_outstanding",
            AccessScreen::CodeEntry,
        ),
        (AuthFailure::WrongCode, "wrong_code", AccessScreen::Rejected),
        (
            AuthFailure::CodeExpired,
            "code_expired",
            AccessScreen::Rejected,
        ),
        (
            AuthFailure::CodeAlreadyUsed,
            "code_already_used",
            AccessScreen::Rejected,
        ),
        (
            AuthFailure::UnknownToken,
            "unknown_token",
            AccessScreen::CodeEntry,
        ),
        (
            AuthFailure::TokenRevoked,
            "token_revoked",
            AccessScreen::Revoked,
        ),
        (
            AuthFailure::RateLimited { retry_after_ms: 42 },
            "rate_limited",
            AccessScreen::RateLimited,
        ),
    ] {
        assert_eq!(failure.as_str(), spelling);
        assert_eq!(failure.screen(), screen);
    }
}

#[test]
fn the_limiter_prunes_stale_addresses() {
    let mut limiter = AddressLimiter::new();
    for i in 0..RATE_LIMIT_MAX_TRACKED_ADDRESSES {
        limiter.record_failure(&format!("10.1.{}.{}", i / 256, i % 256), 0);
    }
    // Long after every lockout has been served, one more failure prunes the
    // dead entries instead of growing the map forever.
    limiter.record_failure("10.9.9.9", RATE_LIMIT_LOCKOUT_MS * 10);
    assert!(limiter.entries.len() < RATE_LIMIT_MAX_TRACKED_ADDRESSES);
    assert_eq!(
        limiter.attempts_remaining("10.9.9.9", RATE_LIMIT_LOCKOUT_MS * 10),
        RATE_LIMIT_MAX_FAILURES - 1
    );
}

#[test]
fn the_default_path_sits_beside_remote_json() {
    // Only the shape is asserted; the env var may be anything on a CI box.
    if let Some(path) = web_credentials_path() {
        assert!(path.ends_with(".flightdeck/web.json"));
    }
}

// -- the debug-only test seam ----------------------------------------------
//
// `mint_fixed_bootstrap_code` exists so the Playwright suite (D15) can drive the
// real exchange endpoint in a real browser; the tests below pin the two
// properties that keep it from being a credential bypass. It is
// `#[cfg(debug_assertions)]`, so a release build has no such method — and these
// tests, which are also debug-only, are how we know the shape it does have.

#[test]
fn a_fixed_test_code_is_exchanged_by_the_ordinary_path() {
    let (mut s, _fs, _clock) = store();

    let code = s
        .mint_fixed_bootstrap_code("8419")
        .expect("four digits mint a code");
    assert_eq!(code.reveal(), "8419");
    // It is the live code, and it behaves like any other: same TTL, and the
    // exchange is the shipped one.
    assert_eq!(s.bootstrap_code().map(|c| c.reveal()), Some("8419"));
    assert_eq!(s.bootstrap_seconds_remaining(), Some(120));
    let token = s
        .exchange_code(ADDR_A, "8419", Some("Chromium"))
        .expect("exchange succeeds");
    assert!(!token.reveal().is_empty());
    // Single use, exactly as a random code is: the second attempt is refused
    // and there is no live code left to try again with.
    assert!(s.bootstrap_code().is_none());
    assert!(s.exchange_code(ADDR_B, "8419", None).is_err());
}

#[test]
fn a_fixed_test_code_still_expires_and_is_still_rate_limited() {
    let (mut s, _fs, clock) = store();

    s.mint_fixed_bootstrap_code("8419").expect("mint");
    clock.set_millis(1_000_000 + BOOTSTRAP_CODE_TTL_MS + 1);
    assert!(
        s.bootstrap_code().is_none(),
        "the seam must not hand out a code that outlives the TTL"
    );
    assert!(s.exchange_code(ADDR_A, "8419", None).is_err());

    // And a wrong guess against a fixed code spends the address's budget the
    // same way, so the seam cannot be used to sidestep the limiter either.
    s.mint_fixed_bootstrap_code("8419").expect("mint");
    let before = s.attempts_remaining(ADDR_A);
    let _ = s.exchange_code(ADDR_A, "0000", None);
    assert!(s.attempts_remaining(ADDR_A) < before);
}

#[test]
fn a_fixed_test_code_that_is_not_four_digits_mints_nothing() {
    let (mut s, _fs, _clock) = store();

    for bad in ["", "84", "84190", "84a9", "８４１９"] {
        assert!(
            s.mint_fixed_bootstrap_code(bad).is_none(),
            "{bad:?} must not mint a code"
        );
        assert!(
            s.bootstrap_code().is_none(),
            "{bad:?} left a code behind — a typo must not install one the \
             exchange endpoint can never match"
        );
    }
}
