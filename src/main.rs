//! Thin binary entry point. All logic lives in the `flightdeck` library so it is
//! testable without launching a real terminal (SPECS §26, §27).

fn main() {
    if let Err(e) = flightdeck::run() {
        // Konsole closes stderr with its tab. `eprintln!` panics on that write
        // failure, which used to enter Ratatui's panic hook and abort the
        // process. Reporting an error must never become a second crash.
        use std::io::Write;
        let stderr = std::io::stderr();
        let _ = writeln!(stderr.lock(), "flightdeck error: {e}");
        std::process::exit(1);
    }
}
