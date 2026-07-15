//! FlightDeck Remote end-to-end harness.
//!
//! c3m.4: minimal harness bootstrap; c3m.7 expands this. This file currently
//! contains only the shared `support` module wiring and one smoke test that
//! exercises the relay launcher on its own. The full Tier A capability suite
//! (desktop + phone driver + every remote capability) lands in issue c3m.7.

#[path = "e2e/support/mod.rs"]
mod support;

use support::relay::RelayHandle;

/// The real relay binary builds, boots, answers `/healthz`, and is killed
/// cleanly on drop (no leaked process, no leaked port).
#[test]
fn relay_boots_and_healthz_ok() {
    let relay = RelayHandle::spawn();

    assert!(relay.port() > 0, "relay should be bound to a real port");
    assert_eq!(
        relay.ws_url(),
        format!("ws://127.0.0.1:{}/ws", relay.port())
    );
    assert_eq!(
        relay.http_base(),
        format!("http://127.0.0.1:{}", relay.port())
    );

    // RelayHandle::spawn already blocked until /healthz answered "ok"; this
    // is an explicit re-check right before drop.
    assert!(relay.healthz_ok(), "relay should still answer /healthz ok");
    drop(relay);
}
