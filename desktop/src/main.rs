//! `flightdeck-desktop`: the native GUI front-end for FlightDeck (GPUI).
//!
//! Kept thin on purpose: everything testable lives in modules; this only
//! starts the app. See `desktop/NOTES-M0.md` for how GPUI is consumed.

mod app;
mod terminal;
use flightdeck_desktop::theme;
mod views;

fn main() {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        // M0 spike S2: one window with one live terminal (see terminal::spike).
        Some("--spike-terminal") => terminal::spike::run_window(args.collect()),
        // The same pipeline headless, printing the grid instead of drawing it.
        Some("--dump-grid") => std::process::exit(terminal::spike::dump(args.collect())),
        _ => app::run(),
    }
}
