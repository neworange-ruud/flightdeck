//! The library half of `flightdeck-desktop`: the parts other modules (and,
//! soon, the terminal element) build on, kept out of the binary so their
//! public API is a real API — tested here, and not dead code while the views
//! that will call it are still being written.
//!
//! The binary (`main.rs`) owns the window and views.

pub mod keys;
pub mod overlays;
// Self-update: install-kind detection, release/asset/checksum rules, the
// once-a-day check and the download-verify-swap (beads bmej.6.5).
pub mod selfupdate;
// The palette lives in the library (the binary re-uses it) because the overlay
// views paint with it and are tested here, without a window.
pub mod theme;
