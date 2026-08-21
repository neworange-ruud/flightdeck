//! Shared relay-password gate (remote-control-uq7).
//!
//! A relay configured with `FLIGHTDECK_RELAY_PASSWORD` must reject any `hello`
//! that presents no password or the wrong one, and accept the correct one; a
//! relay with **no** password configured must stay open (the historical
//! behavior, so local/dev relays and every other test keep working). The
//! rejection is a plain `error { code: auth_failed }` followed by a close — the
//! relay never leaks whether the password was missing vs merely wrong.

mod support;

use flightdeck_remote_protocol::{RelayErrorCode, RelayFrame};
use support::{hello_probe, spawn_app, spawn_app_with_password};

const PASSWORD: &str = "correct horse battery staple";

/// (a) A configured relay rejects a `hello` presenting the **wrong** password.
#[tokio::test]
async fn wrong_password_is_rejected() {
    let base = spawn_app_with_password(PASSWORD).await;
    let frame = hello_probe(&base, Some("not-the-password")).await;
    assert!(
        matches!(
            frame,
            RelayFrame::Error {
                code: RelayErrorCode::AuthFailed,
                ..
            }
        ),
        "wrong password must be rejected with auth_failed, got {frame:?}"
    );
}

/// (b) A configured relay rejects a `hello` presenting **no** password at all.
/// The rejection is byte-identical to the wrong-password case so nothing leaks
/// about which was the problem.
#[tokio::test]
async fn missing_password_is_rejected() {
    let base = spawn_app_with_password(PASSWORD).await;
    let frame = hello_probe(&base, None).await;
    assert!(
        matches!(
            frame,
            RelayFrame::Error {
                code: RelayErrorCode::AuthFailed,
                ..
            }
        ),
        "missing password must be rejected with auth_failed, got {frame:?}"
    );
}

/// (c) A configured relay accepts the **correct** password and proceeds to the
/// normal handshake (`hello_ok`).
#[tokio::test]
async fn correct_password_is_accepted() {
    let base = spawn_app_with_password(PASSWORD).await;
    let frame = hello_probe(&base, Some(PASSWORD)).await;
    assert!(
        matches!(frame, RelayFrame::HelloOk { .. }),
        "correct password must be accepted with hello_ok, got {frame:?}"
    );
}

/// (d) A relay with **no** password configured stays open: a `hello` with no
/// password is accepted exactly as before the gate existed.
#[tokio::test]
async fn no_password_configured_stays_open() {
    let base = spawn_app().await;
    let frame = hello_probe(&base, None).await;
    assert!(
        matches!(frame, RelayFrame::HelloOk { .. }),
        "unconfigured relay must accept a password-less hello, got {frame:?}"
    );
}

/// A relay with no password configured also ignores a password a client happens
/// to present (an older allowlisted client, or a client mid-rollout), so
/// enabling the client side ahead of the relay never locks anyone out.
#[tokio::test]
async fn no_password_configured_ignores_presented_password() {
    let base = spawn_app().await;
    let frame = hello_probe(&base, Some("anything")).await;
    assert!(
        matches!(frame, RelayFrame::HelloOk { .. }),
        "unconfigured relay must ignore any presented password, got {frame:?}"
    );
}
