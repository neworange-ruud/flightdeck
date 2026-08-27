//! FlightDeck Web: the embedded browser control surface (`specs/WEB_INTERFACE.md`).
//!
//! A web server embedded in the desktop binary serves a full-fidelity browser
//! remote control for the *running* instance: the same projects, sessions,
//! statuses and terminals the TUI sees, driving the same code paths.
//!
//! This is deliberately **not** the phone companion ([`crate::remote`]). That is
//! a curated, read-mostly, relay-connected surface with E2E envelopes; this is
//! raw terminals over a single trusted socket on the local network (D1, D12).
//!
//! Module map, each the unit of one backlog task:
//!
//! | Module | Decision | What it owns |
//! | --- | --- | --- |
//! | [`protocol`] | D12, Q3, Q5 | versioned JSON wire types, byte cursors |
//! | [`replay`] | D2, Q2 | per-terminal byte ring buffer + resume |
//! | [`credentials`] | D5, D10, Q4 | bootstrap code → persistent token |
//! | [`interfaces`] | Q1 | network interfaces for the address picker |
//! | [`activity`] | D11 | host-side status-transition event store |
//! | [`assets`] | D9 | the `webui/` SPA baked in with `rust-embed` |
//! | [`server`] | D6 | axum on the shared tokio runtime |
//! | [`stream`] | D2, D8, D14 | PTY bytes out, keystrokes in, takeover |
//! | [`commands`] | D3, D13, D16 | wire name to palette action, one table |

pub mod activity;
pub mod assets;
pub mod commands;
pub mod credentials;
pub mod interfaces;
pub mod protocol;
pub mod replay;
pub mod server;
pub mod stream;
