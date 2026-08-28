//! Unit tests for the pieces of the server that need no socket (D15's first
//! layer). The listener, the routes and the WebSocket are exercised end to end
//! in `tests/web_server.rs` against a real `TcpListener` and a real WS client.
//!
//! What is covered here is the reasoning the integration tests cannot isolate:
//! the cookie's exact attributes, the rule that no proxy header can influence
//! the rate-limit key, and every branch of seat arbitration (D14) including the
//! ones a browser has to try twice to reach.

use super::*;

use std::net::{Ipv4Addr, Ipv6Addr};

/// A fixed host clock reading, for the refusal bodies that pair an instant with
/// the host's own `server_time_ms`.
const NOW_MS: i64 = 1_700_000_000_000;

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn peer(ip: &str, port: u16) -> SocketAddr {
    SocketAddr::new(ip.parse().expect("test address parses"), port)
}

fn viewer(id: &str) -> ViewerId {
    ViewerId::new(id)
}

/// A registry with `count` viewers registered (all observers, as `register`
/// leaves them), plus the channel receivers so nothing is dropped mid-test.
#[allow(clippy::type_complexity)]
fn registry_with(count: usize) -> (SeatRegistry, Vec<mpsc::Receiver<ServerMsg>>) {
    let mut registry = SeatRegistry::new(1_000);
    let mut receivers = Vec::new();
    for index in 0..count {
        let (tx, rx) = mpsc::channel::<ServerMsg>(8);
        registry.register(
            viewer(&format!("v{index}")),
            ViewerIdentity {
                address: IpAddr::V4(Ipv4Addr::new(192, 168, 2, 20 + index as u8)),
                user_agent_label: Some("Chrome on macOS".to_string()),
            },
            2_000 + index as i64,
            tx,
        );
        receivers.push(rx);
    }
    (registry, receivers)
}

// ---------------------------------------------------------------------------
// The cookie (Q4)
// ---------------------------------------------------------------------------

/// The exact attribute set, spelled out, because every one of them is a decision
/// and a silent change to any of them is a security change.
#[test]
fn cookie_is_httponly_lax_and_long_lived_but_never_secure() {
    let header = set_cookie_value("s3cret-token-value");

    assert!(header.starts_with("flightdeck_web=s3cret-token-value;"));
    assert!(
        header.contains("HttpOnly"),
        "Q4 requires HttpOnly: {header}"
    );
    assert!(
        header.contains("SameSite=Lax"),
        "Lax is what lets a QR/link land authenticated while still blocking a \
         cross-site WebSocket handshake: {header}"
    );
    assert!(
        header.contains("Path=/"),
        "the SPA owns every path: {header}"
    );
    assert!(
        header.contains(&format!("Max-Age={COOKIE_MAX_AGE_SECS}")),
        "{header}"
    );
    // The one that would break the feature: this server is plain HTTP on
    // loopback/LAN (D1), so a `Secure` cookie would never be sent back at all.
    assert!(
        !header.contains("Secure"),
        "a Secure cookie is never sent over http:// — it would break auth, not \
         harden it: {header}"
    );
    // `SameSite=None` requires Secure, so it cannot appear either.
    assert!(!header.contains("SameSite=None"), "{header}");
}

#[test]
fn cookie_is_read_out_of_a_crowded_cookie_header() {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::COOKIE,
        "other=1; flightdeck_web=abc123; trailing=2"
            .parse()
            .expect("header parses"),
    );
    assert_eq!(
        cookie_value(&headers, COOKIE_NAME),
        Some("abc123".to_string())
    );
}

#[test]
fn cookie_lookup_tolerates_whitespace_and_reports_absence() {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::COOKIE,
        "  flightdeck_web = spaced  "
            .parse()
            .expect("header parses"),
    );
    assert_eq!(
        cookie_value(&headers, COOKIE_NAME),
        Some("spaced".to_string())
    );

    // A cookie whose *name* merely contains ours must not match.
    let mut decoy = HeaderMap::new();
    decoy.insert(
        header::COOKIE,
        "xflightdeck_web=nope".parse().expect("header parses"),
    );
    assert_eq!(cookie_value(&decoy, COOKIE_NAME), None);

    assert_eq!(cookie_value(&HeaderMap::new(), COOKIE_NAME), None);
}

// ---------------------------------------------------------------------------
// The peer address
// ---------------------------------------------------------------------------

/// The rate-limit key is the TCP peer's IP with no port, which is what
/// `credentials.rs` documents wanting: a NAT'd device reconnecting on a new
/// ephemeral port keeps its budget instead of getting a fresh one.
#[test]
fn rate_limit_address_is_the_peer_ip_without_the_port() {
    assert_eq!(
        rate_limit_address(peer("192.168.2.20", 54_321)),
        "192.168.2.20"
    );
    assert_eq!(rate_limit_address(peer("127.0.0.1", 1)), "127.0.0.1");
    assert_eq!(rate_limit_address(peer("::1", 9999)), "::1");
}

/// No proxy header can influence the rate-limit key. The proof is structural —
/// [`rate_limit_address`] cannot see headers at all — and this test exists to
/// make that a thing a future change has to break on purpose: if someone gives
/// the function a `&HeaderMap`, this test stops compiling.
#[test]
fn proxy_headers_cannot_forge_the_rate_limit_address() {
    let mut forged = HeaderMap::new();
    forged.insert("x-forwarded-for", "127.0.0.1".parse().expect("parses"));
    forged.insert("x-real-ip", "127.0.0.1".parse().expect("parses"));
    forged.insert("forwarded", "for=127.0.0.1".parse().expect("parses"));

    // The headers exist and are simply unreachable from the keying decision.
    let from_socket = rate_limit_address(peer("192.168.2.99", 40_000));
    assert_eq!(from_socket, "192.168.2.99");
    assert!(!forged.is_empty(), "the forged headers were really set");
}

// ---------------------------------------------------------------------------
// Bind exposure (D5)
// ---------------------------------------------------------------------------

#[test]
fn loopback_is_the_default_and_anything_else_is_routable() {
    // The shipped default must be loopback, with no opt-in anywhere.
    assert_eq!(
        bind_exposure(&WebConfig::default().bind),
        BindExposure::Loopback,
        "D5: the default bind is loopback"
    );

    for loopback in ["127.0.0.1", "::1", "[::1]", "localhost", "127.5.5.5"] {
        assert_eq!(
            bind_exposure(loopback),
            BindExposure::Loopback,
            "{loopback} is loopback"
        );
    }
    for routable in ["0.0.0.0", "192.168.2.20", "::", "my-laptop.local", ""] {
        assert_eq!(
            bind_exposure(routable),
            BindExposure::Routable,
            "{routable} must warn"
        );
    }
}

#[test]
fn listen_address_brackets_a_bare_ipv6_literal() {
    assert_eq!(listen_address("127.0.0.1", 8477), "127.0.0.1:8477");
    assert_eq!(listen_address("::1", 8477), "[::1]:8477");
    assert_eq!(listen_address("[::1]", 8477), "[::1]:8477");
    assert_eq!(listen_address(" 127.0.0.1 ", 0), "127.0.0.1:0");
}

// ---------------------------------------------------------------------------
// Seat arbitration (D14)
// ---------------------------------------------------------------------------

#[test]
fn a_registered_viewer_starts_as_an_observer() {
    let (registry, _rx) = registry_with(1);
    assert_eq!(registry.seat_of(&viewer("v0")), Some(Seat::Observing));
    assert!(registry.writers().is_empty());
}

/// D14's revision in one assertion: a writer's seat is a *role*, and asking for
/// it can no longer be refused. What is scarce is the turn, and that lives in
/// [`crate::web::arbiter`] rather than here.
#[test]
fn several_viewers_can_be_writers_at_once() {
    let (mut registry, _rx) = registry_with(3);
    for id in ["v0", "v1", "v2"] {
        assert_eq!(
            registry.request_seat(&viewer(id), SeatRequest::Write),
            Seat::Writing,
            "no writer request is refused any more"
        );
    }
    assert_eq!(registry.writers().len(), 3);
}

/// Takeover has no dedicated frame — the client re-sends
/// `Attach { seat: TakeOver }` — and under the revision it demotes **nobody**.
/// The interrupted writer keeps its seat and gets the turn back the moment the
/// interrupter goes quiet, which is what makes 2f's `Watch read-only` a choice
/// rather than a consolation.
#[test]
fn takeover_demotes_nobody_because_nothing_is_being_taken_from_them() {
    let (mut registry, _rx) = registry_with(2);
    registry.request_seat(&viewer("v0"), SeatRequest::Write);

    assert_eq!(
        registry.request_seat(&viewer("v1"), SeatRequest::TakeOver),
        Seat::Writing
    );
    assert_eq!(
        registry.seat_of(&viewer("v0")),
        Some(Seat::Writing),
        "the interrupted writer keeps its seat"
    );
    assert_eq!(registry.viewers.len(), 2, "nobody was disconnected");
    assert_eq!(registry.writers().len(), 2);
}

#[test]
fn observe_never_contends_so_n_observers_cost_nothing() {
    let (mut registry, _rx) = registry_with(4);
    registry.request_seat(&viewer("v0"), SeatRequest::Write);
    for id in ["v1", "v2", "v3"] {
        assert_eq!(
            registry.request_seat(&viewer(id), SeatRequest::Observe),
            Seat::Observing
        );
    }
    assert_eq!(
        registry.writers(),
        vec![viewer("v0")],
        "D14's read-only fan-out is untouched by the revision"
    );
}

#[test]
fn releasing_and_leaving_both_give_up_the_writer_role() {
    let (mut registry, _rx) = registry_with(2);
    registry.request_seat(&viewer("v0"), SeatRequest::Write);
    registry.release(&viewer("v0"));
    assert!(registry.writers().is_empty());

    registry.request_seat(&viewer("v1"), SeatRequest::Write);
    registry.remove(&viewer("v1"));
    assert!(registry.writers().is_empty());
    assert_eq!(registry.seat_of(&viewer("v1")), None);
}

#[test]
fn seat_rows_put_the_desktop_first_and_mark_only_the_recipient() {
    let (mut registry, _rx) = registry_with(2);
    registry.request_seat(&viewer("v0"), SeatRequest::Write);

    let holder = Writer::Viewer(viewer("v0"));
    let rows = registry.seat_rows(Some(&viewer("v1")), Some(&holder));
    assert_eq!(rows.len(), 3, "desktop + two tabs");

    let desktop = &rows[0];
    assert_eq!(desktop.viewer_id, None);
    assert_eq!(desktop.label, DESKTOP_SEAT_LABEL);
    assert_eq!(
        desktop.seat,
        Seat::Writing,
        "the desktop's keyboard is never revoked — it is always a writer"
    );
    assert!(
        !desktop.holds_input,
        "but the *turn* is somebody else's, and the desktop is told so"
    );
    assert!(!desktop.is_you);

    // Exactly one row holds input, and it is the one the arbiter named.
    let holding: Vec<_> = rows.iter().filter(|r| r.holds_input).collect();
    assert_eq!(holding.len(), 1);
    assert_eq!(holding[0].viewer_id, Some(viewer("v0")));

    assert_eq!(rows.iter().filter(|r| r.is_you).count(), 1);
    assert!(rows
        .iter()
        .any(|r| r.is_you && r.viewer_id == Some(viewer("v1"))));

    // A free lock marks nobody. Saying "the desktop has it" would be the
    // asymmetry the model exists to refuse.
    let free = registry.seat_rows(None, None);
    assert!(free.iter().all(|r| !r.holds_input));
}

/// Each viewer's `Delta::Seats` carries *its own* `you`, which is why the
/// fan-out cannot be one shared frame.
#[test]
fn seat_frames_are_personalised_per_viewer() {
    let (mut registry, _rx) = registry_with(2);
    registry.request_seat(&viewer("v0"), SeatRequest::Write);
    registry.request_seat(&viewer("v1"), SeatRequest::Observe);

    let holder = Writer::Viewer(viewer("v0"));
    let frames = registry.seat_frames(NOW_MS, Some(&holder), None);
    assert_eq!(frames.len(), 2);
    for (id, msg) in frames {
        let ServerMsg::Delta(Delta::Seats {
            you,
            seats,
            server_time_ms,
            you_were_preempted,
        }) = msg
        else {
            panic!("a seat change is a Delta::Seats, never a Shutdown");
        };
        // The reference clock for every row's `since_ms`. Without it the
        // browser cannot date the rows, and 2f's `connected` fact silently
        // disappears on this path while surviving on the snapshot path.
        assert_eq!(server_time_ms, NOW_MS);
        let expected = if id == viewer("v0") {
            Seat::Writing
        } else {
            Seat::Observing
        };
        assert_eq!(you, expected, "wrong `you` for {id}");
        assert!(seats
            .iter()
            .any(|r| r.is_you && r.viewer_id.as_ref() == Some(&id)));
        // Every recipient sees the same holder — there is one lock, and the
        // whole point of publishing it is that no two surfaces disagree.
        assert_eq!(
            seats
                .iter()
                .filter(|r| r.holds_input)
                .map(|r| r.viewer_id.clone())
                .collect::<Vec<_>>(),
            vec![Some(viewer("v0"))]
        );
        // Nobody was interrupted here — `v0` claimed a free lock by typing,
        // which is the ordinary movement and the overwhelmingly common one.
        assert!(
            !you_were_preempted,
            "an ordinary hand-off must not put 2f's evicted panel in front of {id}"
        );
    }
}

/// The flag that separates *someone confirmed an override* from *the lock moved
/// the way it moves all day*, which is the difference between a notice and an
/// obstruction.
///
/// The browser cannot make this distinction from the rows: "the lock left me" is
/// true of both, and the lock leaves a writer every time their colleague starts
/// a sentence. Intent exists only at the host, at the moment of the act, so the
/// host carries it — per recipient, because one preemption interrupts exactly
/// one writer and only that writer has anything to be told.
#[test]
fn only_the_interrupted_writers_frame_says_it_was_deliberate() {
    let (mut registry, _rx) = registry_with(3);
    for id in ["v0", "v1", "v2"] {
        registry.request_seat(&viewer(id), SeatRequest::Write);
    }

    // `v2` confirmed `Take over` while `v0` was mid-burst.
    let holder = Writer::Viewer(viewer("v2"));
    let interrupted = Writer::Viewer(viewer("v0"));
    let flagged = |frames: Vec<(ViewerId, ServerMsg)>| -> Vec<ViewerId> {
        frames
            .into_iter()
            .filter(|(_, msg)| {
                matches!(
                    msg,
                    ServerMsg::Delta(Delta::Seats {
                        you_were_preempted: true,
                        ..
                    })
                )
            })
            .map(|(id, _)| id)
            .collect()
    };

    assert_eq!(
        flagged(registry.seat_frames(NOW_MS, Some(&holder), Some(&interrupted))),
        vec![viewer("v0")],
        "the writer that was cut into, and nobody else — least of all the one \
         that pressed the button"
    );
    assert_eq!(
        flagged(registry.seat_frames(NOW_MS, Some(&holder), None)),
        Vec::<ViewerId>::new(),
        "the same roster with the same holder, announced for any other reason, \
         is silent"
    );
    // An interrupted *desktop* reaches no browser panel: 2f gives the person at
    // the machine a transient strip, because their keyboard was never revoked
    // and they have no decision to make.
    assert_eq!(
        flagged(registry.seat_frames(NOW_MS, Some(&holder), Some(&Writer::Desktop))),
        Vec::<ViewerId>::new(),
        "the desktop's interruption is the desktop's business"
    );
}

/// A viewer that stops draining is dropped rather than allowed to grow the
/// host's heap; it comes back through the reconnect path (Q3).
#[test]
fn a_viewer_that_cannot_keep_up_is_dropped_from_the_registry() {
    let mut registry = SeatRegistry::new(0);
    let (tx, _rx) = mpsc::channel::<ServerMsg>(1);
    registry.register(
        viewer("slow"),
        ViewerIdentity {
            address: IpAddr::V6(Ipv6Addr::LOCALHOST),
            user_agent_label: None,
        },
        0,
        tx,
    );

    let frame = ServerMsg::Ack(Ack {
        seq: 1,
        outcome: AckOutcome::Applied,
        detail: None,
    });
    registry.send_to(&viewer("slow"), frame.clone()); // fills the queue
    assert_eq!(registry.viewers.len(), 1);
    registry.send_to(&viewer("slow"), frame); // no room left
    assert!(
        registry.viewers.is_empty(),
        "the queue-full viewer must be dropped, not buffered forever"
    );
}

// ---------------------------------------------------------------------------
// Input cursors (§5.1)
// ---------------------------------------------------------------------------

#[test]
fn input_cursors_survive_a_reconnect_and_never_go_backwards() {
    let (mut registry, _rx) = registry_with(1);
    registry.record_input(&viewer("v0"), 7);
    registry.record_input(&viewer("v0"), 3); // a late frame must not rewind it
    assert_eq!(registry.input_cursor(&viewer("v0")), 7);

    // The reconnect presents its previous id; the cursor moves onto the new one.
    assert_eq!(registry.adopt_cursor(&viewer("v0"), &viewer("v0-again")), 7);
    assert_eq!(registry.input_cursor(&viewer("v0-again")), 7);

    // A viewer nobody has heard of starts at zero, which is what a fresh tab is.
    assert_eq!(registry.input_cursor(&viewer("stranger")), 0);
}

#[test]
fn remembered_input_cursors_are_bounded() {
    let mut registry = SeatRegistry::new(0);
    for index in 0..(REMEMBERED_INPUT_CURSORS + 10) {
        registry.record_input(&viewer(&format!("v{index}")), index as u64 + 1);
    }
    assert_eq!(registry.input_cursors.len(), REMEMBERED_INPUT_CURSORS);
    // Oldest dropped first.
    assert_eq!(registry.input_cursor(&viewer("v0")), 0);
    assert_eq!(
        registry.input_cursor(&viewer(&format!("v{}", REMEMBERED_INPUT_CURSORS + 9))),
        REMEMBERED_INPUT_CURSORS as u64 + 10
    );
}

// ---------------------------------------------------------------------------
// Refusal shape (artboard 2b)
// ---------------------------------------------------------------------------

#[test]
fn every_auth_failure_maps_to_a_screen_and_a_status() {
    let cases = [
        (AuthFailure::NoCodeOutstanding, "code_entry", 401),
        (AuthFailure::UnknownToken, "code_entry", 401),
        (AuthFailure::WrongCode, "rejected", 401),
        (AuthFailure::CodeExpired, "rejected", 401),
        (AuthFailure::CodeAlreadyUsed, "rejected", 401),
        (
            AuthFailure::TokenRevoked {
                revoked_at_unix_secs: 1_700_000_000,
            },
            "revoked",
            401,
        ),
        (
            AuthFailure::RateLimited {
                retry_after_ms: 60_000,
            },
            "rate_limited",
            429,
        ),
    ];
    for (failure, screen, status) in cases {
        assert_eq!(screen_name(failure.screen()), screen, "{failure:?}");
        assert_eq!(
            refusal_status(failure).as_u16(),
            status,
            "{failure:?} deserves {status}"
        );
    }
}

/// A refusal body must be enough for the SPA to pick a screen and count down,
/// and must contain nothing about the credential itself.
#[test]
fn a_refusal_body_carries_the_countdown_and_no_secret() {
    let body = refusal_body(
        AuthFailure::RateLimited {
            retry_after_ms: 41_500,
        },
        0,
        NOW_MS,
    );
    assert_eq!(body["ok"], serde_json::json!(false));
    assert_eq!(body["screen"], serde_json::json!("rate_limited"));
    assert_eq!(body["retry_after_ms"], serde_json::json!(41_500));
    assert_eq!(body["attempts_remaining"], serde_json::json!(0));

    let plain = refusal_body(AuthFailure::WrongCode, 2, NOW_MS);
    assert_eq!(plain["reason"], serde_json::json!("wrong_code"));
    assert!(
        plain.get("retry_after_ms").is_none(),
        "only the limiter sets a countdown"
    );
    // Nothing that could be a code or a token.
    let rendered = plain.to_string();
    assert!(!rendered.contains("code="), "{rendered}");
}

#[test]
fn retry_after_is_whole_seconds_rounded_up_and_never_zero() {
    let mut response = json_response(StatusCode::TOO_MANY_REQUESTS, serde_json::json!({}));
    attach_retry_after(
        &mut response,
        AuthFailure::RateLimited {
            retry_after_ms: 1_001,
        },
    );
    assert_eq!(
        response
            .headers()
            .get(header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok()),
        Some("2")
    );

    let mut nearly_done = json_response(StatusCode::TOO_MANY_REQUESTS, serde_json::json!({}));
    attach_retry_after(
        &mut nearly_done,
        AuthFailure::RateLimited { retry_after_ms: 1 },
    );
    assert_eq!(
        nearly_done
            .headers()
            .get(header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok()),
        Some("1"),
        "a sub-second lockout must still say wait, not wait zero"
    );

    let mut not_limited = json_response(StatusCode::UNAUTHORIZED, serde_json::json!({}));
    attach_retry_after(&mut not_limited, AuthFailure::WrongCode);
    assert!(not_limited.headers().get(header::RETRY_AFTER).is_none());
}

#[test]
fn credential_responses_are_never_cached() {
    let response = json_response(StatusCode::OK, serde_json::json!({ "ok": true }));
    assert_eq!(
        response
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok()),
        Some("no-store")
    );
}

// ---------------------------------------------------------------------------
// Labels
// ---------------------------------------------------------------------------

#[test]
fn the_chip_label_is_the_observed_address_plus_a_coarse_user_agent() {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::USER_AGENT,
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
         (KHTML, like Gecko) Chrome/131.0 Safari/537.36"
            .parse()
            .expect("header parses"),
    );
    let identity = viewer_identity(peer("192.168.2.20", 51_000), &headers);
    assert_eq!(identity.label(), "192.168.2.20 · Chrome on macOS");
    // The same two facts, still separable, because the chip's label is derived
    // from them rather than the other way round (artboard 2f).
    assert_eq!(identity.address.to_string(), "192.168.2.20");
    assert_eq!(
        identity.user_agent_label.as_deref(),
        Some("Chrome on macOS")
    );

    // No user agent: the address alone, never a guess.
    let bare = viewer_identity(peer("127.0.0.1", 51_000), &HeaderMap::new());
    assert_eq!(bare.label(), "127.0.0.1");
    assert_eq!(bare.user_agent_label, None, "unknown stays unknown");
}

#[test]
fn an_unrecognised_user_agent_adds_nothing_rather_than_guessing() {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::USER_AGENT,
        "curl/8.7.1".parse().expect("header parses"),
    );
    let identity = viewer_identity(peer("10.0.0.4", 1), &headers);
    assert_eq!(identity.label(), "10.0.0.4");
    assert_eq!(identity.user_agent_label, None);
}

/// The browser may refine what it *is*. It may never refine where it *is*.
#[test]
fn a_client_claim_replaces_the_browser_fact_and_never_the_address() {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::USER_AGENT,
        "curl/8.7.1".parse().expect("header parses"),
    );
    let observed = viewer_identity(peer("10.0.0.4", 1), &headers);

    // A claim that is really an address, separator and all — the exact payload
    // that would fool a browser-side split of the merged label.
    let refined = observed.with_claim(Some("9.9.9.9 · Chrome on macOS"));
    assert_eq!(
        refined.address.to_string(),
        "10.0.0.4",
        "the address came off the socket and no frame can move it"
    );
    assert_eq!(
        refined.user_agent_label.as_deref(),
        Some("9.9.9.9 · Chrome on macOS"),
        "the claim is displayed verbatim in its own field, never parsed"
    );

    // A claim made of nothing but control bytes sanitises to empty, and an
    // empty claim must not erase a fact we already had. (`curl/8.7.1` gave us
    // nothing to keep, so `None` here is the header's answer surviving intact.)
    let kept = viewer_identity(peer("10.0.0.4", 1), &headers).with_claim(Some("\u{1b}\u{7}\n"));
    assert_eq!(kept.user_agent_label, None);
    assert_eq!(kept.address.to_string(), "10.0.0.4");

    // The same, over a header we *could* read: a junk claim must not throw away
    // the coarse user agent the host had already worked out.
    let mut chrome = HeaderMap::new();
    chrome.insert(
        header::USER_AGENT,
        "Mozilla/5.0 (Macintosh) Chrome/131.0 Safari/537.36"
            .parse()
            .expect("header parses"),
    );
    let survivor = viewer_identity(peer("10.0.0.4", 1), &chrome).with_claim(Some("\u{1b}\u{7}"));
    assert_eq!(
        survivor.user_agent_label.as_deref(),
        Some("Chrome on macOS"),
        "an unusable claim erases nothing"
    );
}

/// Artboard 2f lists address / browser / connected as three rows, so the seat
/// row hands over three fields. The desktop, which arrived over no socket, has
/// no address to hand over and says so rather than inventing one.
#[test]
fn seat_rows_split_the_address_from_the_browsers_own_claim() {
    let (mut registry, _rx) = registry_with(1);
    registry.request_seat(&viewer("v0"), SeatRequest::Write);
    let rows = registry.seat_rows(Some(&viewer("v0")), None);

    let desktop = &rows[0];
    assert_eq!(desktop.address, None, "never a fabricated `localhost`");
    assert_eq!(desktop.user_agent_label, None);

    let browser = &rows[1];
    assert_eq!(browser.address.as_deref(), Some("192.168.2.20"));
    assert_eq!(browser.user_agent_label.as_deref(), Some("Chrome on macOS"));
    assert_eq!(
        browser.label, "192.168.2.20 · Chrome on macOS",
        "the compact chip keeps its one line"
    );
    assert!(browser.since_ms > 0, "the third fact: connected-since");
}

/// A browser-supplied label reaches the desktop's chip, so it must not be able
/// to carry control bytes into a terminal, or be long enough to break the row.
#[test]
fn a_hostile_label_is_stripped_and_bounded() {
    let hostile = format!("\u{1b}[2Jwiped\u{7}\n{}", "x".repeat(500));
    let cleaned = sanitize_label(&hostile);
    assert!(!cleaned.contains('\u{1b}'), "{cleaned}");
    assert!(!cleaned.contains('\n'), "{cleaned}");
    assert!(!cleaned.contains('\u{7}'), "{cleaned}");
    assert!(cleaned.starts_with("[2Jwiped"));

    let bounded = truncate_chars(&cleaned, MAX_LABEL_CHARS);
    assert_eq!(bounded.chars().count(), MAX_LABEL_CHARS);
}

/// `String::truncate` panics on a byte index inside a codepoint, and the label
/// is attacker-supplied UTF-8, so the bound is by character.
#[test]
fn truncation_never_splits_a_codepoint() {
    let multibyte = "日本語のラベルです".repeat(20);
    let bounded = truncate_chars(&multibyte, 5);
    assert_eq!(bounded, "日本語のラ");
    assert_eq!(truncate_chars("short", 50), "short");
}

// ---------------------------------------------------------------------------
// Frame handling that needs no socket
// ---------------------------------------------------------------------------

#[test]
fn an_unknown_frame_type_parses_as_unrecognized_rather_than_failing() {
    // Forward compatibility: a newer browser's frame must be answerable, not a
    // parse error that drops the socket.
    assert!(matches!(
        parse_client_msg(r#"{"type":"teleport","seq":1}"#),
        Some(ClientMsg::Unrecognized)
    ));
    // Not JSON at all is a different thing, and is reported as such.
    assert!(parse_client_msg("not json").is_none());
    assert!(matches!(
        parse_client_msg(r#"{"type":"resize","viewport":{"cols":100,"rows":30}}"#),
        Some(ClientMsg::Resize(_))
    ));
}

#[test]
fn only_text_frames_carry_protocol_v1() {
    assert_eq!(
        message_text(Message::Text("{}".into())),
        Some("{}".to_string())
    );
    // Terminal bytes are base64 inside the JSON (protocol::TermBytes), so a
    // binary frame is not part of v1 and is ignored rather than misparsed.
    assert_eq!(message_text(Message::Binary(vec![1, 2, 3].into())), None);
    assert_eq!(message_text(Message::Close(None)), None);
}

// ---------------------------------------------------------------------------
// Snapshot assembly
// ---------------------------------------------------------------------------

fn test_shared() -> Arc<Shared> {
    use crate::testing::{FakeClock, FakeFs};

    let clock = Arc::new(FakeClock::default());
    clock.set_millis(1_700_000_000_000);
    let store = CredentialStore::open(
        Arc::new(FakeFs::new()),
        clock.clone(),
        std::path::PathBuf::from("/web.json"),
    );
    let (inbound, _rx) = std::sync::mpsc::channel::<WebInbound>();
    let state = HostState {
        host_version: "9.9.9".to_string(),
        replay_capacity_bytes: 262_144,
        geometry: Geometry {
            cols: 120,
            rows: 34,
        },
        ..HostState::default()
    };
    let (state_tx, state_rx) = watch::channel(Arc::new(state));
    let (_shutdown_tx, shutdown_rx) = watch::channel::<Option<ShutdownNotice>>(None);
    // The shutdown sender is dropped, which every reader treats as
    // "server_stopped" — harmless here, since nothing awaits it.
    Arc::new(Shared {
        credentials: Arc::new(Mutex::new(store)),
        clock,
        inbound: Mutex::new(inbound),
        state: state_tx,
        state_rx,
        registry: Mutex::new(SeatRegistry::new(1_699_999_000_000)),
        input_lock: InputArbiter::shared(),
        announced_holder: Mutex::new(None),
        shutdown: shutdown_rx,
        drain: Arc::new(Drain::default()),
    })
}

#[test]
fn a_snapshot_carries_the_published_state_plus_this_viewers_own_seat() {
    let shared = test_shared();
    let (tx, _rx) = mpsc::channel::<ServerMsg>(8);
    shared.registry().register(
        viewer("v0"),
        ViewerIdentity {
            address: IpAddr::V4(Ipv4Addr::LOCALHOST),
            user_agent_label: None,
        },
        1_700_000_000_000,
        tx,
    );
    shared
        .registry()
        .request_seat(&viewer("v0"), SeatRequest::Write);

    let snapshot = shared.snapshot_for(&viewer("v0"), Seat::Writing, 12);
    assert_eq!(
        snapshot.protocol_version,
        crate::web::protocol::PROTOCOL_VERSION
    );
    assert_eq!(snapshot.host_version, "9.9.9");
    assert_eq!(snapshot.server_time_ms, 1_700_000_000_000);
    assert_eq!(snapshot.viewer_id, viewer("v0"));
    assert_eq!(snapshot.seat, Seat::Writing);
    assert_eq!(snapshot.last_input_seq, 12);
    assert_eq!(snapshot.replay_capacity_bytes, 262_144);
    assert_eq!(
        snapshot.geometry,
        Geometry {
            cols: 120,
            rows: 34
        }
    );
    // The seat rows are the recipient's own view of the chip.
    assert!(snapshot
        .seats
        .iter()
        .any(|r| r.is_you && r.viewer_id == Some(viewer("v0"))));
    assert_eq!(snapshot.seats[0].viewer_id, None, "desktop row first");
}

#[test]
fn publishing_state_changes_what_the_next_snapshot_says() {
    let shared = test_shared();
    shared.state.send_replace(Arc::new(HostState {
        host_version: "1.2.3".to_string(),
        ..HostState::default()
    }));
    let snapshot = shared.snapshot_for(&viewer("nobody"), Seat::Observing, 0);
    assert_eq!(snapshot.host_version, "1.2.3");
}

// ---------------------------------------------------------------------------
// Draining (Q5's ordering guarantee)
// ---------------------------------------------------------------------------

/// The shutdown path waits on this counter, so "all guards dropped" has to mean
/// "every viewer finished writing its goodbye".
#[test]
fn the_drain_counter_only_reports_idle_once_every_guard_is_gone() {
    let runtime = crate::remote::runtime::shared();
    runtime.handle().block_on(async {
        let drain = Arc::new(Drain::default());
        // Idle with no connections: shutdown must not stall on an empty server.
        tokio::time::timeout(Duration::from_secs(1), drain.wait_for_idle())
            .await
            .expect("an idle drain resolves immediately");

        let first = drain.enter();
        let second = drain.enter();
        assert!(
            tokio::time::timeout(Duration::from_millis(50), drain.wait_for_idle())
                .await
                .is_err(),
            "two live connections must hold the listener open"
        );

        drop(first);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), drain.wait_for_idle())
                .await
                .is_err(),
            "one live connection still holds it"
        );

        let waiter = {
            let drain = Arc::clone(&drain);
            tokio::spawn(async move { drain.wait_for_idle().await })
        };
        drop(second);
        tokio::time::timeout(Duration::from_secs(2), waiter)
            .await
            .expect("the last guard dropping wakes the waiter")
            .expect("the waiter task did not panic");
    });
}

/// A dropped shutdown sender is itself a shutdown signal, so a handle that goes
/// out of scope never leaves the listener bound.
#[test]
fn a_dropped_shutdown_sender_reads_as_server_stopped() {
    let runtime = crate::remote::runtime::shared();
    runtime.handle().block_on(async {
        let (tx, mut rx) = watch::channel::<Option<ShutdownNotice>>(None);
        drop(tx);
        let notice = tokio::time::timeout(Duration::from_secs(1), await_shutdown(&mut rx))
            .await
            .expect("a closed channel resolves");
        assert_eq!(notice.reason, ShutdownReason::ServerStopped);
        assert_eq!(notice.initiator, None);
    });
}

#[test]
fn an_explicit_notice_is_delivered_verbatim() {
    let runtime = crate::remote::runtime::shared();
    runtime.handle().block_on(async {
        let (tx, mut rx) = watch::channel::<Option<ShutdownNotice>>(None);
        let sent = ShutdownNotice::host_quit(Some(viewer("v0")));
        tx.send(Some(sent.clone())).expect("a receiver is live");
        let notice = tokio::time::timeout(Duration::from_secs(1), await_shutdown(&mut rx))
            .await
            .expect("a pending notice resolves");
        assert_eq!(notice, sent);
        assert_eq!(notice.reason, ShutdownReason::HostQuit);
    });
}

/// Q5's two screens from one frame: only the tab that asked sees
/// `self_initiated`.
#[test]
fn self_initiated_is_true_only_for_the_viewer_that_asked() {
    let notice = ShutdownNotice::host_quit(Some(viewer("asker")));
    assert_eq!(notice.initiator.as_ref(), Some(&viewer("asker")));
    assert!(notice.initiator.as_ref() != Some(&viewer("bystander")));
    assert_eq!(ShutdownNotice::server_stopped().initiator, None);
}
