//! `flightdeck-desktop`: the native GUI front-end for FlightDeck (GPUI).
//!
//! Kept thin on purpose: everything testable lives in modules; this only
//! starts the app. See `desktop/NOTES-M0.md` for how GPUI is consumed and
//! `desktop/NOTES-M2.md` for the shell.

mod app;
mod assets;
mod commands;
mod fonts;
mod host;
mod shell;
mod terminal;
use flightdeck_desktop::theme;
mod views;

#[cfg(test)]
mod shell_tests;

fn main() {
    let mut args = std::env::args().skip(1);
    match args.next() {
        // M0 spike S2: one window with one live terminal (see terminal::spike).
        Some(flag) if flag == "--spike-terminal" => terminal::spike::run_window(args.collect()),
        // The same pipeline headless, printing the grid instead of drawing it.
        Some(flag) if flag == "--dump-grid" => {
            std::process::exit(terminal::spike::dump(args.collect()))
        }
        first => app::run(first.into_iter().chain(args).collect()),
    }
}
