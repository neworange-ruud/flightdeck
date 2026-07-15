//! Shared building blocks for the FlightDeck Remote end-to-end harness.
//!
//! This module is deliberately a thin umbrella: each concern (relay, desktop,
//! phone driver, fixture project) gets its own sibling file so later harness
//! pieces can be added without disturbing this one. Today only [`relay`]
//! exists; `desktop` (issue c3m.5) and `phone` (issue c3m.6) are expected to
//! land as `pub mod desktop;` / `pub mod phone;` additions here.
//!
//! Not every helper is exercised by every test binary that includes this
//! module, so submodules are `dead_code`-tolerant individually.

pub mod relay;
