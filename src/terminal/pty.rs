//! Real [`PtyBackend`]/[`PtySession`] over `portable-pty` (SPECS §17, §25).
//!
//! A background reader thread drains PTY output into a channel; the session
//! exposes a non-blocking read, input write, resize, Ctrl-C, and a force
//! terminate-tree path (SPECS §25).

use crate::contracts::{PtyBackend, PtySession, PtySize, Result};
use std::path::Path;

/// `portable-pty`-backed [`PtyBackend`].
#[derive(Debug, Default)]
pub struct PortablePtyBackend;

impl PtyBackend for PortablePtyBackend {
    fn spawn(
        &self,
        cmd: &str,
        args: &[String],
        cwd: &Path,
        size: PtySize,
    ) -> Result<Box<dyn PtySession>> {
        let _ = (cmd, args, cwd, size);
        todo!("T6: open pty, spawn command, start reader thread, return session")
    }
}
