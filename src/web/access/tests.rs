//! Tests for the access overlay's state machine (design `2a`, D5, Q1, Q7).
//!
//! Everything runs against [`FakeFs`], [`FakeClock`] and
//! [`FakeInterfaceEnumerator`], so the 120-second countdown is crossed by
//! advancing a clock rather than sleeping, and the address picker is fed
//! macOS-, Linux- and Windows-shaped interface names without touching a real
//! NIC.
//!
//! The load-bearing assertions here are the ones about what is *not* on screen:
//! State A never yields a visible code, `r` takes the QR with it, and an
//! expired code is reported rather than silently replaced.

use std::sync::Arc;

use super::*;
use crate::testing::{FakeClock, FakeFs};
use crate::web::credentials::BOOTSTRAP_CODE_TTL_MS;
use crate::web::interfaces::FakeInterfaceEnumerator;

const PATH: &str = "/home/user/.flightdeck/web.json";

/// A store over in-memory seams, with the clock returned so a test can expire a
/// code without sleeping.
fn store() -> (CredentialStore, Arc<FakeClock>) {
    let fs = Arc::new(FakeFs::new());
    let clock = Arc::new(FakeClock::default());
    clock.set_millis(1_000_000);
    clock.set_unix_secs(1_700_000_000);
    let s = CredentialStore::open(fs, clock.clone(), PATH);
    (s, clock)
}

fn loopback() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 7420))
}

fn wildcard() -> SocketAddr {
    SocketAddr::from(([0, 0, 0, 0], 7420))
}

/// The three-interface fixture artboard 2a's State B draws: a wifi adapter, a
/// VM bridge and a tunnel.
fn three_interfaces() -> FakeInterfaceEnumerator {
    FakeInterfaceEnumerator::new()
        .with_interface("en0", Ipv4Addr::new(192, 168, 2, 14))
        .with_interface("bridge100", Ipv4Addr::new(192, 168, 64, 1))
        .with_interface("tailscale0", Ipv4Addr::new(100, 87, 14, 3))
}

/// A QR encoder stand-in: records nothing, returns art whose "rows" are the
/// payload itself, so a test can assert *what was encoded* without asserting
/// anything about half-block glyphs.
fn echo_qr(payload: &str) -> Option<(Vec<String>, usize)> {
    Some((vec![payload.to_string()], payload.chars().count()))
}

/// A local-only overlay over a fresh store.
fn local() -> (WebAccess, CredentialStore, Arc<FakeClock>) {
    let (mut s, clock) = store();
    let access = WebAccess::open(
        &mut s,
        &FakeInterfaceEnumerator::new(),
        loopback(),
        BindExposure::Loopback,
    );
    (access, s, clock)
}

/// A network-enabled overlay over a fresh store, with the three-row picker.
fn network() -> (WebAccess, CredentialStore, Arc<FakeClock>) {
    let (mut s, clock) = store();
    let access = WebAccess::open(
        &mut s,
        &three_interfaces(),
        wildcard(),
        BindExposure::Routable,
    );
    (access, s, clock)
}

// -- minting: the bug this module exists to fix ---------------------------

#[test]
fn opening_the_overlay_mints_a_code_that_can_be_exchanged() {
    let (_access, mut s, _clock) = local();
    let digits = s
        .bootstrap_code()
        .expect("opening the overlay mints a code")
        .reveal()
        .to_string();
    assert_eq!(digits.len(), 4);
    assert!(digits.bytes().all(|b| b.is_ascii_digit()));
    s.exchange_code("127.0.0.1", &digits, Some("Test Browser"))
        .expect("the minted code is exchangeable over the production path");
}

#[test]
fn the_countdown_runs_down_and_then_reports_expiry_rather_than_reminting() {
    let (access, s, clock) = network();
    let view = access.view(&s, echo_qr);
    assert_eq!(view.seconds_remaining, Some(BOOTSTRAP_CODE_TTL_MS / 1000));
    assert!(!view.code_expired);

    clock.set_millis(1_000_000 + BOOTSTRAP_CODE_TTL_MS / 2);
    let view = access.view(&s, echo_qr);
    assert_eq!(view.seconds_remaining, Some(60));

    clock.set_millis(1_000_000 + BOOTSTRAP_CODE_TTL_MS + 1);
    let view = access.view(&s, echo_qr);
    assert_eq!(view.seconds_remaining, None);
    assert!(view.code_expired, "an expired code is reported, not hidden");
    assert_eq!(view.code, None);
    assert!(view.qr_rows.is_empty(), "no code, no QR");
}

#[test]
fn space_mints_a_new_code_over_an_expired_one_without_restarting_anything() {
    let (mut access, mut s, clock) = network();
    let first = s.bootstrap_code().expect("minted").reveal().to_string();
    clock.set_millis(1_000_000 + BOOTSTRAP_CODE_TTL_MS + 1);

    assert_eq!(
        access.handle_key(AccessKey::Space, &mut s),
        AccessOutcome::Handled
    );
    let second = s
        .bootstrap_code()
        .expect("Space mints a replacement")
        .reveal()
        .to_string();
    assert_eq!(
        s.bootstrap_seconds_remaining(),
        Some(BOOTSTRAP_CODE_TTL_MS / 1000)
    );
    // The old one is gone from the store: exchanging it must fail even though
    // the digits could coincidentally match.
    if first != second {
        s.exchange_code("192.168.2.20", &first, None)
            .expect_err("the previous code no longer works");
    }
}

#[test]
fn rotate_revokes_every_browser_and_invalidates_the_previous_code() {
    let (mut access, mut s, _clock) = network();
    let first = s.bootstrap_code().expect("minted").reveal().to_string();
    s.exchange_code("192.168.2.20", &first, Some("Safari/iOS"))
        .expect("a browser gets in");
    assert_eq!(s.active_tokens().count(), 1);

    assert_eq!(
        access.handle_key(AccessKey::Char('x'), &mut s),
        AccessOutcome::Revoked,
        "the credentials are gone, but the sockets holding them are the event \
         loop's to close — `Handled` would be this overlay claiming an eviction \
         it cannot perform (§6.5 R20)"
    );
    assert_eq!(
        s.active_tokens().count(),
        0,
        "D5's rotate locks every browser out"
    );
    let second = s
        .bootstrap_code()
        .expect("rotate mints")
        .reveal()
        .to_string();
    if first != second {
        s.exchange_code("192.168.2.20", &first, None)
            .expect_err("the rotated-away code no longer works");
    }
    let view = access.view(&s, echo_qr);
    assert_eq!(
        view.notice.as_deref(),
        Some("1 browser revoked — new code issued.")
    );
    assert!(view.browsers.is_empty());
}

// -- who holds access, and revoking one of them (`remote-control-gk94`) ----

/// Let `count` browsers in, each from its own address, and return the digits
/// spent. The user-agents are the raw strings a real browser sends, because
/// coarsening them is one of the things under test.
fn let_browsers_in(s: &mut CredentialStore, agents: &[(&str, &str)]) {
    for (address, agent) in agents {
        let code = s.mint_bootstrap_code().reveal().to_string();
        s.exchange_code(address, &code, Some(agent))
            .expect("the code was just minted");
    }
}

/// The line 2a State B draws — `● 1 browser holds access · 192.168.2.20 ·
/// Safari/iOS · 14m` — needs three facts per holder, and every one of them has
/// to come out of the store rather than out of the renderer's imagination.
#[test]
fn each_holder_is_named_from_what_the_host_stored_about_it() {
    let (access, mut s, clock) = network();
    let_browsers_in(
        &mut s,
        &[(
            "192.168.2.20",
            "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 \
             (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1",
        )],
    );
    clock.set_unix_secs(1_700_000_000 + 14 * 60);

    let view = access.view(&s, echo_qr);
    let [row] = view.browsers.as_slice() else {
        panic!("one browser holds access: {:?}", view.browsers);
    };
    assert_eq!(
        row.address.as_deref(),
        Some("192.168.2.20"),
        "the address is the host's observation off the socket, not a claim"
    );
    assert_eq!(
        row.browser.as_deref(),
        Some("Safari on iOS"),
        "the user-agent is coarsened the same way the viewer chip coarsens it"
    );
    assert_eq!(
        row.granted_secs_ago,
        14 * 60,
        "dated against the store's own clock — the one that stamped the record"
    );
    assert_eq!(row.key, Some('1'), "the first row is revoked by `1`");
}

/// The whole point of the row: two browsers are two rows, and a digit takes
/// exactly one of them out.
#[test]
fn a_digit_revokes_the_browser_on_that_row_and_leaves_the_other_alone() {
    let (mut access, mut s, _clock) = network();
    let_browsers_in(
        &mut s,
        &[
            ("192.168.2.20", "Safari/604.1 iPhone"),
            ("192.168.2.31", "Chrome/120.0 Macintosh"),
        ],
    );
    let second_id = s
        .active_tokens()
        .nth(1)
        .expect("two browsers hold access")
        .id
        .clone();

    assert_eq!(
        access.handle_key(AccessKey::Char('2'), &mut s),
        AccessOutcome::Revoked,
        "the credential is withdrawn here; the socket is the event loop's to \
         close, exactly as `x` is (§6.5 R20)"
    );

    let view = access.view(&s, echo_qr);
    let [row] = view.browsers.as_slice() else {
        panic!("one browser is left: {:?}", view.browsers);
    };
    assert_eq!(row.address.as_deref(), Some("192.168.2.20"));
    assert!(
        !s.is_token_active(&second_id),
        "the browser the digit named is the one that lost access"
    );
    assert_eq!(
        view.notice.as_deref(),
        Some("192.168.2.31 · Chrome on macOS is locked out — its access was revoked."),
        "the notice names the browser, which is what the numbered rows are for"
    );
    assert_eq!(
        s.active_tokens().count(),
        1,
        "one digit, one credential — not a rotation"
    );
}

/// A digit past the end of the list is not "the last one".
#[test]
fn a_digit_with_no_row_under_it_revokes_nothing() {
    let (mut access, mut s, _clock) = network();
    let_browsers_in(&mut s, &[("192.168.2.20", "Safari/604.1 iPhone")]);

    assert_eq!(
        access.handle_key(AccessKey::Char('4'), &mut s),
        AccessOutcome::Ignored,
        "nothing was on row 4, so nothing may be revoked"
    );
    assert_eq!(s.active_tokens().count(), 1);
    assert_eq!(
        access.view(&s, echo_qr).notice,
        None,
        "a key the overlay never offered is silent, not a refusal"
    );
}

/// R12's rule, on the second surface that shows a user-agent: the string is
/// attacker-supplied, so it is reduced and never parsed — a separator inside it
/// cannot become a field boundary, and a control character cannot reach the
/// terminal.
#[test]
fn a_hostile_user_agent_cannot_forge_a_field_or_reach_the_terminal() {
    let (access, mut s, _clock) = network();
    let_browsers_in(
        &mut s,
        &[("192.168.2.20", "\u{1b}[2J · 10.0.0.1 · Administrator")],
    );

    let view = access.view(&s, echo_qr);
    let [row] = view.browsers.as_slice() else {
        panic!("one browser holds access");
    };
    assert_eq!(
        row.address.as_deref(),
        Some("192.168.2.20"),
        "the address row still reads the address the socket reported"
    );
    let browser = row.browser.as_deref().expect("something was claimed");
    assert!(
        !browser.contains('\u{1b}'),
        "no escape sequence survives into a drawn row: {browser:?}"
    );
    assert!(
        browser.contains(" · 10.0.0.1 · "),
        "the claim is carried whole and unparsed — the separators inside it are \
         text, not field boundaries, which is why it can never become a second \
         address: {browser:?}"
    );
    assert_eq!(
        view.browsers.len(),
        1,
        "one credential is one row, however many separators its claim contains"
    );
}

/// The digits and `x` are two different reaches, and adding the first must not
/// have quietly narrowed the second (D5: one command locks everyone out).
#[test]
fn x_still_means_everyone_and_the_footer_still_says_so() {
    let (mut access, mut s, _clock) = network();
    let_browsers_in(
        &mut s,
        &[
            ("192.168.2.20", "Safari/604.1 iPhone"),
            ("192.168.2.31", "Chrome/120.0 Macintosh"),
            ("192.168.2.44", "Firefox/121.0 X11; Linux"),
        ],
    );

    assert_eq!(
        access.handle_key(AccessKey::Char('x'), &mut s),
        AccessOutcome::Revoked
    );
    assert_eq!(s.active_tokens().count(), 0, "all three, not the first");
    assert!(
        access.view(&s, echo_qr).keys.contains(&("x", "revoke")),
        "2a's footer is unchanged"
    );
}

/// A record from before the address was stored is still a real credential.
///
/// The store is opened over a `web.json` written by an **older build** — no
/// `address`, no `label` — because that is the file every upgrading install
/// already has on disk, and serde's defaults are the only thing standing
/// between it and a row that invents what it was never told.
#[test]
fn a_record_from_an_older_build_is_still_listed_and_still_revocable() {
    let fs = Arc::new(FakeFs::new().with_file(
        PATH,
        r#"{"version":1,"tokens":[{"id":"tok_old","token_sha256":"abc",
            "created_unix_secs":1700000000,"last_seen_unix_secs":1700000000}]}"#,
    ));
    let clock = Arc::new(FakeClock::default());
    clock.set_millis(1_000_000);
    clock.set_unix_secs(1_700_000_000);
    let mut s = CredentialStore::open(fs, clock, PATH);
    assert_eq!(s.load_error(), None, "the older file still loads");
    let mut access = WebAccess::open(
        &mut s,
        &three_interfaces(),
        wildcard(),
        BindExposure::Routable,
    );

    let view = access.view(&s, echo_qr);
    let [row] = view.browsers.as_slice() else {
        panic!("one browser holds access");
    };
    assert_eq!(row.address, None, "no placeholder stands in for it");
    assert_eq!(row.browser, None);
    assert_eq!(
        access.handle_key(AccessKey::Char('1'), &mut s),
        AccessOutcome::Revoked
    );
    assert_eq!(
        access.view(&s, echo_qr).notice.as_deref(),
        Some("That browser is locked out — its access was revoked."),
        "it says what it did without naming what it cannot"
    );
}

/// `age_label` is what turns a stored timestamp into 2a's `14m`.
#[test]
fn an_age_is_rounded_down_so_it_never_claims_more_time_than_has_passed() {
    assert_eq!(age_label(0), "0s");
    assert_eq!(age_label(59), "59s");
    assert_eq!(age_label(60), "1m");
    assert_eq!(age_label(14 * 60 + 59), "14m");
    assert_eq!(age_label(3600), "1h");
    assert_eq!(age_label(47 * 3600), "1d");
}

// -- State A: the common case never shows a code ---------------------------

#[test]
fn the_local_state_draws_no_code_and_no_qr() {
    let (access, s, _clock) = local();
    let view = access.view(&s, echo_qr);
    assert_eq!(view.mode, Some(AccessMode::LocalOnly));
    assert_eq!(view.code, None);
    assert!(view.qr_rows.is_empty());
    assert!(
        !view.code_hidden,
        "there is nothing hidden — there is nothing"
    );
    assert!(!view.code_expired);
    assert_eq!(view.url, "http://127.0.0.1:7420");
    assert_eq!(
        view.exposure_line,
        "loopback only — nothing off this machine can reach it"
    );
    assert!(view.addresses.is_empty());
}

#[test]
fn enter_hands_the_browser_a_loopback_url_carrying_the_code_in_its_fragment() {
    let (mut access, mut s, _clock) = local();
    let digits = s.bootstrap_code().expect("minted").reveal().to_string();
    let outcome = access.handle_key(AccessKey::Enter, &mut s);
    assert_eq!(
        outcome,
        AccessOutcome::OpenBrowser(format!("http://127.0.0.1:7420/#{digits}"))
    );
    // The fragment is the whole point: nothing before the `#` carries it.
    let AccessOutcome::OpenBrowser(url) = outcome else {
        unreachable!("asserted above")
    };
    let (before, after) = url.split_once('#').expect("a fragment");
    assert!(!before.contains(&digits));
    assert_eq!(after, digits);
}

#[test]
fn enter_after_the_invisible_code_expired_still_opens_a_working_browser() {
    let (mut access, mut s, clock) = local();
    clock.set_millis(1_000_000 + BOOTSTRAP_CODE_TTL_MS + 1);
    assert!(s.bootstrap_code().is_none());

    let AccessOutcome::OpenBrowser(url) = access.handle_key(AccessKey::Enter, &mut s) else {
        panic!("Enter must open a browser rather than report an expiry State A never showed");
    };
    let digits = url.split_once('#').expect("a fragment").1.to_string();
    s.exchange_code("127.0.0.1", &digits, None)
        .expect("the re-minted code is live");
}

#[test]
fn copy_url_carries_a_code_so_the_second_browser_is_authenticated_too() {
    let (mut access, mut s, _clock) = local();
    let digits = s.bootstrap_code().expect("minted").reveal().to_string();
    assert_eq!(
        access.handle_key(AccessKey::Char('c'), &mut s),
        AccessOutcome::CopyUrl(format!("http://127.0.0.1:7420/#{digits}"))
    );
}

#[test]
fn the_local_footer_is_the_artboards_and_its_keys_are_the_bound_ones() {
    let (mut access, mut s, _clock) = local();
    assert_eq!(
        access.view(&s, echo_qr).keys,
        vec![
            ("Enter", "open"),
            ("c", "copy"),
            ("n", "network access"),
            ("s", "stop server"),
            ("Esc", "close"),
        ]
    );
    assert_eq!(
        access.handle_key(AccessKey::Char('n'), &mut s),
        AccessOutcome::EnableNetwork
    );
    assert_eq!(
        access.handle_key(AccessKey::Char('s'), &mut s),
        AccessOutcome::StopServer
    );
    assert_eq!(
        access.handle_key(AccessKey::Esc, &mut s),
        AccessOutcome::Close
    );
    // State B's keys are not silently live in State A.
    for key in [
        AccessKey::Space,
        AccessKey::Up,
        AccessKey::Down,
        AccessKey::Char('r'),
        AccessKey::Char('x'),
        AccessKey::Char('l'),
    ] {
        assert_eq!(
            access.handle_key(key, &mut s),
            AccessOutcome::Ignored,
            "{key:?} is not in State A's footer"
        );
    }
}

// -- State B: the QR earns its place ---------------------------------------

#[test]
fn the_network_state_publishes_the_selected_address_with_the_code_in_the_qr() {
    let (access, s, _clock) = network();
    let digits = s.bootstrap_code().expect("minted").reveal().to_string();
    let view = access.view(&s, echo_qr);

    assert_eq!(view.mode, Some(AccessMode::Network));
    assert_eq!(view.code.as_deref(), Some(digits.as_str()));
    assert_eq!(view.bound, "0.0.0.0:7420");
    // Sorted by (name, address): bridge100, en0, tailscale0.
    assert_eq!(view.selected_address, Some(0));
    assert_eq!(view.url, "http://192.168.64.1:7420");
    assert_eq!(
        view.qr_rows,
        vec![format!("http://192.168.64.1:7420/#{digits}")],
        "Q1 addition 1: the QR encodes the code, not just the URL"
    );
    assert_eq!(
        view.exposure_line,
        "reachable by anyone on this network who has the code"
    );
}

#[test]
fn the_picker_lists_what_the_enumerator_found_with_its_one_line_descriptions() {
    let (access, s, _clock) = network();
    let rows = access.view(&s, echo_qr).addresses;
    assert_eq!(
        rows,
        vec![
            AddressRow {
                name: "bridge100".to_string(),
                address: "192.168.64.1".to_string(),
                description: Some("vm bridge"),
            },
            AddressRow {
                name: "en0".to_string(),
                address: "192.168.2.14".to_string(),
                description: Some("wifi · reachable by your phone"),
            },
            AddressRow {
                name: "tailscale0".to_string(),
                address: "100.87.14.3".to_string(),
                description: Some("your own tunnel"),
            },
        ]
    );
}

#[test]
fn an_unclassifiable_interface_is_listed_with_no_description_rather_than_a_guess() {
    let (mut s, _clock) = store();
    let access = WebAccess::open(
        &mut s,
        &FakeInterfaceEnumerator::new().with_interface("zt0abc", Ipv4Addr::new(10, 8, 0, 3)),
        wildcard(),
        BindExposure::Routable,
    );
    let rows = access.view(&s, echo_qr).addresses;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].description, None);
}

#[test]
fn moving_the_selection_republishes_the_address_and_clamps_at_both_ends() {
    let (mut access, mut s, _clock) = network();

    assert_eq!(
        access.handle_key(AccessKey::Up, &mut s),
        AccessOutcome::Handled
    );
    assert_eq!(
        access.view(&s, echo_qr).selected_address,
        Some(0),
        "up from the top stays at the top"
    );

    access.handle_key(AccessKey::Down, &mut s);
    let view = access.view(&s, echo_qr);
    assert_eq!(view.selected_address, Some(1));
    assert_eq!(view.url, "http://192.168.2.14:7420");

    for _ in 0..5 {
        access.handle_key(AccessKey::Down, &mut s);
    }
    let view = access.view(&s, echo_qr);
    assert_eq!(view.selected_address, Some(2), "down past the end clamps");
    assert_eq!(view.url, "http://100.87.14.3:7420");
}

#[test]
fn a_host_with_no_routable_interface_says_so_instead_of_publishing_a_wildcard() {
    let (mut s, _clock) = store();
    let access = WebAccess::open(
        &mut s,
        &FakeInterfaceEnumerator::new(),
        wildcard(),
        BindExposure::Routable,
    );
    let view = access.view(&s, echo_qr);
    assert_eq!(view.url, "(no routable address on this host)");
    assert_eq!(view.selected_address, None);
    assert!(view.addresses.is_empty());
    assert!(
        view.qr_rows.is_empty(),
        "there is no address to encode, so there is no QR"
    );
    // The code itself is live and stays on screen: it is a real credential, and
    // a user who reaches this host by a name we cannot enumerate still needs it.
    // What the overlay must not do is invent an address to put beside it.
    assert!(view.code.is_some());
}

#[test]
fn r_hides_the_code_and_the_qr_together_and_toggles_back() {
    let (mut access, mut s, _clock) = network();
    assert!(access.view(&s, echo_qr).code.is_some());

    assert_eq!(
        access.handle_key(AccessKey::Char('r'), &mut s),
        AccessOutcome::Handled
    );
    let view = access.view(&s, echo_qr);
    assert_eq!(view.code, None, "Q1 mitigation 1: r hides the code");
    assert!(view.qr_rows.is_empty(), "and the QR with it");
    assert_eq!(view.qr_width, 0);
    assert!(view.code_hidden);
    assert!(!view.code_expired, "hidden is not expired");
    // The countdown keeps running: the code is hidden, not cancelled.
    assert_eq!(
        view.seconds_remaining,
        Some(BOOTSTRAP_CODE_TTL_MS / 1000),
        "hiding the code must not stop it expiring"
    );

    access.handle_key(AccessKey::Char('r'), &mut s);
    assert!(access.view(&s, echo_qr).code.is_some());
}

#[test]
fn space_brings_a_hidden_code_back_so_the_new_one_is_actually_readable() {
    let (mut access, mut s, _clock) = network();
    access.handle_key(AccessKey::Char('r'), &mut s);
    assert!(access.view(&s, echo_qr).code_hidden);

    access.handle_key(AccessKey::Space, &mut s);
    let view = access.view(&s, echo_qr);
    assert!(!view.code_hidden);
    assert!(view.code.is_some());
}

#[test]
fn the_network_footer_is_the_artboards_and_local_only_keys_are_inert() {
    let (mut access, mut s, _clock) = network();
    assert_eq!(
        access.view(&s, echo_qr).keys,
        vec![
            ("↑↓", "address"),
            ("Space", "new code"),
            ("r", "hide"),
            ("x", "revoke"),
            ("l", "local only"),
            ("Esc", "close"),
        ]
    );
    assert_eq!(
        access.handle_key(AccessKey::Char('l'), &mut s),
        AccessOutcome::BackToLocalOnly
    );
    assert_eq!(
        access.handle_key(AccessKey::Esc, &mut s),
        AccessOutcome::Close
    );
    for key in [
        AccessKey::Enter,
        AccessKey::Char('c'),
        AccessKey::Char('n'),
        AccessKey::Char('s'),
    ] {
        assert_eq!(
            access.handle_key(key, &mut s),
            AccessOutcome::Ignored,
            "{key:?} is not in State B's footer"
        );
    }
}

// -- the door between the two states ---------------------------------------

#[test]
fn a_routable_binding_opens_straight_into_the_network_state() {
    let (mut s, _clock) = store();
    let access = WebAccess::open(
        &mut s,
        &three_interfaces(),
        wildcard(),
        BindExposure::Routable,
    );
    assert_eq!(access.mode(), AccessMode::Network);
    assert_eq!(access.view(&s, echo_qr).addresses.len(), 3);
}

#[test]
fn rebinding_re_enumerates_re_mints_and_reveals() {
    let (mut access, mut s, _clock) = local();
    let before = s.bootstrap_code().expect("minted").reveal().to_string();

    access.rebind(
        &mut s,
        &three_interfaces(),
        wildcard(),
        BindExposure::Routable,
        Some("Now reachable on this network.".to_string()),
    );

    assert_eq!(access.mode(), AccessMode::Network);
    assert_eq!(access.bound(), wildcard());
    let view = access.view(&s, echo_qr);
    assert_eq!(view.addresses.len(), 3);
    assert_eq!(
        view.notice.as_deref(),
        Some("Now reachable on this network.")
    );
    assert!(view.code.is_some());
    let after = s.bootstrap_code().expect("re-minted").reveal().to_string();
    if before != after {
        s.exchange_code("192.168.2.20", &before, None)
            .expect_err("the code minted for loopback does not travel to the new binding");
    }
}

#[test]
fn going_back_to_local_only_puts_the_credential_away_again() {
    let (mut access, mut s, _clock) = network();
    access.rebind(
        &mut s,
        &FakeInterfaceEnumerator::new(),
        loopback(),
        BindExposure::Loopback,
        None,
    );
    let view = access.view(&s, echo_qr);
    assert_eq!(view.mode, Some(AccessMode::LocalOnly));
    assert_eq!(view.code, None);
    assert!(view.qr_rows.is_empty());
    assert!(view.addresses.is_empty());
}

// -- the notice never overclaims -------------------------------------------

#[test]
fn the_rotate_notice_counts_honestly_and_admits_a_failed_persist() {
    assert_eq!(
        rotate_notice(0, None),
        "No browser held access — new code issued."
    );
    assert_eq!(
        rotate_notice(1, None),
        "1 browser revoked — new code issued."
    );
    assert_eq!(
        rotate_notice(3, None),
        "3 browsers revoked — new code issued."
    );
    let failed = rotate_notice(2, Some("disk full"));
    assert!(failed.starts_with("2 browsers revoked — new code issued."));
    assert!(failed.contains("disk full"));
    assert!(failed.contains("may not survive a restart"));
}
