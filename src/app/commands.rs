//! Application commands and the command dispatcher (T7, SPECS §22).
//!
//! Designed and implemented in Phase 2. Defines the `Command` enum covering
//! every §22 palette action and a `dispatch` reducer that calls the services
//! (SPECS §27 — the app core never executes git/fs/pty itself).
