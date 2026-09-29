//! The library half of `flightdeck-desktop`: the parts other modules (and,
//! soon, the terminal element) build on, kept out of the binary so their
//! public API is a real API — tested here, and not dead code while the views
//! that will call it are still being written.
//!
//! The binary (`main.rs`) owns the window and views.

pub mod keys;
