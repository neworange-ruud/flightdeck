//! Terminal service (T6): the real PTY backend over `portable-pty` and the
//! session model owning one primary + N child terminals (SPECS §17, §19, §25).

pub mod pty;
pub mod session;
pub mod shell;
