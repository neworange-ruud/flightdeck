//! Real [`PtyBackend`]/[`PtySession`] over `portable-pty` (SPECS §17, §25).
//!
//! A background reader thread drains PTY output into a shared buffer; the
//! session exposes a non-blocking read, input write, resize, Ctrl-C, and a
//! force terminate-tree path (SPECS §25).

use crate::contracts::{FlightDeckError, ProcessState, PtyBackend, PtySession, PtySize, Result};
use portable_pty::{Child, ChildKiller, CommandBuilder, MasterPty, PtySize as PortPtySize};
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};

/// `portable-pty`-backed [`PtyBackend`].
#[derive(Debug, Default)]
pub struct PortablePtyBackend;

fn map_size(size: PtySize) -> PortPtySize {
    PortPtySize {
        rows: size.rows,
        cols: size.cols,
        pixel_width: 0,
        pixel_height: 0,
    }
}

impl PtyBackend for PortablePtyBackend {
    fn spawn(
        &self,
        cmd: &str,
        args: &[String],
        cwd: &Path,
        size: PtySize,
    ) -> Result<Box<dyn PtySession>> {
        let pty_system = portable_pty::native_pty_system();
        let pair = pty_system
            .openpty(map_size(size))
            .map_err(|e| FlightDeckError::Io(format!("openpty failed: {e}")))?;

        let mut builder = CommandBuilder::new(cmd);
        builder.args(args);
        builder.cwd(cwd);

        let child = pair
            .slave
            .spawn_command(builder)
            .map_err(|e| FlightDeckError::Io(format!("failed to spawn {cmd}: {e}")))?;

        // The slave handle is no longer needed once the child is spawned; drop it
        // so EOF propagates correctly when the child exits.
        drop(pair.slave);

        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| FlightDeckError::Io(format!("failed to clone pty reader: {e}")))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| FlightDeckError::Io(format!("failed to take pty writer: {e}")))?;

        // Shared, non-blocking output buffer drained by `try_read_output`.
        let buffer: Arc<Mutex<VecDeque<u8>>> = Arc::new(Mutex::new(VecDeque::new()));
        let reader_buffer = Arc::clone(&buffer);

        // Reader thread owns only `Send` data (the boxed reader + the Arc).
        std::thread::spawn(move || {
            let mut reader = reader;
            let mut chunk = [0u8; 8192];
            loop {
                match reader.read(&mut chunk) {
                    Ok(0) => break, // EOF
                    Ok(n) => {
                        if let Ok(mut buf) = reader_buffer.lock() {
                            buf.extend(&chunk[..n]);
                        } else {
                            break;
                        }
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
        });

        let killer = child.clone_killer();

        Ok(Box::new(PortablePtySession {
            master: pair.master,
            writer,
            child: Mutex::new(child),
            killer,
            buffer,
            terminated: Mutex::new(false),
        }))
    }
}

/// A live `portable-pty` session implementing [`PtySession`].
///
/// All fields are `Send`, so the session itself is `Send` as the trait requires.
struct PortablePtySession {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    // `try_wait` requires `&mut`, but `process_state` only has `&self`; the
    // `Mutex` gives us interior mutability so exits can be polled immutably.
    child: Mutex<Box<dyn Child + Send + Sync>>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    buffer: Arc<Mutex<VecDeque<u8>>>,
    terminated: Mutex<bool>,
}

impl PtySession for PortablePtySession {
    fn write_input(&mut self, bytes: &[u8]) -> Result<()> {
        self.writer
            .write_all(bytes)
            .map_err(|e| FlightDeckError::Io(format!("pty write failed: {e}")))?;
        self.writer
            .flush()
            .map_err(|e| FlightDeckError::Io(format!("pty flush failed: {e}")))?;
        Ok(())
    }

    fn resize(&mut self, size: PtySize) -> Result<()> {
        self.master
            .resize(map_size(size))
            .map_err(|e| FlightDeckError::Io(format!("pty resize failed: {e}")))
    }

    fn try_read_output(&mut self) -> Result<Vec<u8>> {
        let mut buf = self
            .buffer
            .lock()
            .map_err(|_| FlightDeckError::Io("pty output buffer poisoned".to_string()))?;
        if buf.is_empty() {
            return Ok(Vec::new());
        }
        Ok(buf.drain(..).collect())
    }

    fn send_ctrl_c(&mut self) -> Result<()> {
        self.write_input(&[0x03])
    }

    fn process_state(&self) -> ProcessState {
        if *self.terminated.lock().expect("terminated flag poisoned") {
            return ProcessState::Stopped;
        }
        let mut child = self.child.lock().expect("child poisoned");
        match child.try_wait() {
            Ok(None) => ProcessState::Running,
            Ok(Some(status)) => {
                if status.success() {
                    ProcessState::Exited(0)
                } else {
                    // Map any non-success (including signal-terminated) to the
                    // process's reported exit code.
                    ProcessState::Exited(status.exit_code() as i32)
                }
            }
            Err(_) => ProcessState::Failed,
        }
    }

    fn terminate_tree(&mut self) -> Result<()> {
        // Best-effort: kill the direct child of the PTY. On macOS/Linux this
        // does NOT recursively reap an arbitrary grandchild tree; children that
        // re-parent (daemonize / double-fork) may survive. For the MVP, killing
        // the pty's direct child (the shell/agent) is acceptable, and dropping
        // the pty master delivers SIGHUP to the foreground group.
        self.killer
            .kill()
            .map_err(|e| FlightDeckError::Io(format!("failed to kill pty child: {e}")))?;
        // Reap so we do not leave a zombie.
        let _ = self.child.lock().expect("child poisoned").try_wait();
        *self.terminated.lock().expect("terminated flag poisoned") = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real-PTY smoke test. Ignored by default (SPECS: real PTY must not run in
    // the standard test run). Spawns a trivial command and verifies the session
    // can be created and read without panicking.
    #[test]
    #[ignore]
    fn real_pty_echo_smoke() {
        let backend = PortablePtyBackend;
        let cwd = std::env::temp_dir();
        let mut session = backend
            .spawn(
                "echo",
                &["flightdeck".to_string()],
                &cwd,
                PtySize::default(),
            )
            .expect("spawn echo");

        // Give the reader thread a moment to drain output.
        std::thread::sleep(std::time::Duration::from_millis(200));
        let out = session.try_read_output().expect("read output");
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("flightdeck"), "got: {text:?}");

        // Terminate is a no-op once the process has exited, but must not error.
        session.terminate_tree().expect("terminate");
    }
}
