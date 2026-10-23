//! Every file a worker keeps, in one place (SPECS §33).
//!
//! Runtime files live in `~/.flightdeck/worker/` (mode 0700: the control
//! socket in it mints pairing codes, so only the account may reach it); the
//! log lives with FlightDeck's other logs in `~/.flightdeck/logs/`.

use std::path::{Path, PathBuf};

/// The worker's file layout under one home directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerPaths {
    /// The account's home directory: the worker's confinement root and the
    /// directory its processes run in.
    pub home: PathBuf,
    /// `~/.flightdeck/worker`, mode 0700 on unix.
    pub dir: PathBuf,
    /// The control socket (`control.sock`, mode 0600).
    pub control_socket: PathBuf,
    /// The supervisor's pid (`supervisor.pid`). Informational: the lock is
    /// the truth, pids are recycled.
    pub supervisor_pid: PathBuf,
    /// The worker's pid (`worker.pid`), likewise informational.
    pub worker_pid: PathBuf,
    /// Held by a running supervisor (`supervisor.lock`).
    pub supervisor_lock: PathBuf,
    /// Held by a running `worker run` (`run.lock`).
    pub run_lock: PathBuf,
    /// The supervisor's last known state (`supervisor.json`), read by
    /// `worker status` even when nothing is running.
    pub supervisor_state: PathBuf,
    /// Written by `worker restart` so the next worker knows it was restarted
    /// (`restart.marker`).
    pub restart_marker: PathBuf,
    /// The worker's open projects (`workspace.json`), separate from the
    /// TUI's `~/.flightdeck/workspace.json`.
    pub workspace: PathBuf,
    /// Paired Desktops' tokens (`devices.json`), separate from the web
    /// interface's `~/.flightdeck/web.json`.
    pub credentials: PathBuf,
    /// `~/.flightdeck/logs`.
    pub logs_dir: PathBuf,
    /// `~/.flightdeck/logs/worker.log` (rotated by the supervisor).
    pub log_file: PathBuf,
}

impl WorkerPaths {
    /// The layout under `home`.
    pub fn from_home(home: &Path) -> Self {
        let base = home.join(".flightdeck");
        let dir = base.join("worker");
        let logs_dir = base.join("logs");
        WorkerPaths {
            home: home.to_path_buf(),
            control_socket: dir.join("control.sock"),
            supervisor_pid: dir.join("supervisor.pid"),
            worker_pid: dir.join("worker.pid"),
            supervisor_lock: dir.join("supervisor.lock"),
            run_lock: dir.join("run.lock"),
            supervisor_state: dir.join("supervisor.json"),
            restart_marker: dir.join("restart.marker"),
            workspace: dir.join("workspace.json"),
            credentials: dir.join("devices.json"),
            log_file: logs_dir.join("worker.log"),
            logs_dir,
            dir,
        }
    }

    /// The layout under this account's home directory, or `None` when no
    /// home is set (`$HOME`, or `%USERPROFILE%` on Windows).
    pub fn current() -> Option<Self> {
        crate::app::state::user_home().map(|home| Self::from_home(&home))
    }

    /// Create the worker and log directories. On unix the worker directory is
    /// then set to 0700 even if it already existed with a wider mode.
    pub fn ensure_dirs(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        std::fs::create_dir_all(&self.logs_dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.dir, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_under_a_given_home() {
        let home = Path::new("/home/u");
        let p = WorkerPaths::from_home(home);
        let dir = home.join(".flightdeck").join("worker");
        assert_eq!(p.home, home);
        assert_eq!(p.dir, dir);
        assert_eq!(p.control_socket, dir.join("control.sock"));
        assert_eq!(p.supervisor_pid, dir.join("supervisor.pid"));
        assert_eq!(p.worker_pid, dir.join("worker.pid"));
        assert_eq!(p.supervisor_lock, dir.join("supervisor.lock"));
        assert_eq!(p.run_lock, dir.join("run.lock"));
        assert_eq!(p.supervisor_state, dir.join("supervisor.json"));
        assert_eq!(p.restart_marker, dir.join("restart.marker"));
        assert_eq!(p.workspace, dir.join("workspace.json"));
        assert_eq!(p.credentials, dir.join("devices.json"));
        assert_eq!(p.logs_dir, home.join(".flightdeck").join("logs"));
        assert_eq!(p.log_file, p.logs_dir.join("worker.log"));
    }

    #[cfg(unix)]
    #[test]
    fn ensure_dirs_creates_a_private_worker_dir() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let p = WorkerPaths::from_home(home.path());
        std::fs::create_dir_all(&p.dir).unwrap();
        std::fs::set_permissions(&p.dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        p.ensure_dirs().unwrap();
        let mode = std::fs::metadata(&p.dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "a pre-existing wider dir is narrowed");
        assert!(p.logs_dir.is_dir());
    }
}
