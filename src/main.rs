//! Thin binary entry point. All logic lives in the `flightdeck` library so it is
//! testable without launching a real terminal (SPECS §26, §27).

fn main() {
    if let Err(e) = flightdeck::run() {
        eprintln!("flightdeck error: {e}");
        std::process::exit(1);
    }
}
