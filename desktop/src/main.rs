//! `flightdeck-desktop`: the native GUI front-end for FlightDeck (GPUI).
//!
//! Kept thin on purpose: everything testable lives in modules; this only
//! starts the app. See `desktop/NOTES-M0.md` for how GPUI is consumed.

mod app;
mod theme;
mod views;

fn main() {
    app::run();
}
