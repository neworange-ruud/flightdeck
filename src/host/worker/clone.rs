//! Cloning a repository on a worker (`specs/WORKER_PRD.md` §4.4, W16): the
//! Desktop sends a URL, the worker clones into `projects_root/<name>` with
//! progress reported to the window, then opens it.
//!
//! Clone is deliberately not on `GitExecutor`, which is repository-scoped and
//! history-safe by construction: a clone has no repository yet. Git runs by
//! argv, never a shell, with prompts off: failures (authentication, host key,
//! network, name in use) are reported in git's own words.

// stub: filled by E

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::contracts::FileSystem;
use crate::host::worker::projects::Confinement;

/// One progress line of a clone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloneLine {
    /// "Receiving objects", "Resolving deltas", …
    pub phase: String,
    pub percent: Option<u8>,
}

/// Runs a clone. Real: [`RealGitCloner`]; tests: [`FakeCloner`].
pub trait CloneRunner: Send + Sync {
    /// Clone `url` into `dest`, reporting progress; `Err` carries git's own
    /// message.
    fn clone_repo(
        &self,
        url: &str,
        dest: &Path,
        progress: &mut dyn FnMut(CloneLine),
    ) -> std::result::Result<(), String>;
}

/// Parse one line of `git clone --progress` output
/// ("Receiving objects:  45% (450/1000)…").
pub fn parse_progress(_line: &str) -> Option<CloneLine> {
    // stub: filled by E
    None
}

/// The folder name a URL clones into: its last segment without `.git`,
/// limited to `[A-Za-z0-9._-]`, never `.` or `..`.
pub fn derive_name(_url: &str) -> Option<String> {
    // stub: filled by E
    None
}

/// Where a clone of `url` (into `name`, or the derived name) would land
/// under `projects_root`, refusing what cannot or must not be cloned.
/// Creates `projects_root` when missing.
pub fn plan_clone(
    _fs: &dyn FileSystem,
    _conf: &Confinement,
    _url: &str,
    _name: Option<&str>,
    _projects_root: &Path,
) -> std::result::Result<PathBuf, String> {
    // stub: filled by E
    Err("cloning is not implemented in this build yet".to_string())
}

/// `git clone --progress -- <url> <dest>`.
#[derive(Debug, Default, Clone, Copy)]
pub struct RealGitCloner;

impl CloneRunner for RealGitCloner {
    fn clone_repo(
        &self,
        _url: &str,
        _dest: &Path,
        _progress: &mut dyn FnMut(CloneLine),
    ) -> std::result::Result<(), String> {
        // stub: filled by E
        Err("cloning is not implemented in this build yet".to_string())
    }
}

/// Scripted clones, for tests: reports `lines`, then answers `outcome`.
#[derive(Debug, Clone)]
pub struct FakeCloner {
    pub lines: Vec<CloneLine>,
    pub outcome: std::result::Result<(), String>,
}

impl CloneRunner for FakeCloner {
    fn clone_repo(
        &self,
        _url: &str,
        _dest: &Path,
        progress: &mut dyn FnMut(CloneLine),
    ) -> std::result::Result<(), String> {
        for line in &self.lines {
            progress(line.clone());
        }
        self.outcome.clone()
    }
}

/// What a running clone reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CloneEvent {
    Progress {
        id: u64,
        line: CloneLine,
    },
    Done {
        id: u64,
        result: std::result::Result<PathBuf, String>,
    },
}

/// Clones running on their own threads.
#[derive(Debug, Default)]
pub struct CloneJobs {
    // stub: filled by E (the event channel)
    _stub: (),
}

impl CloneJobs {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start cloning `url` into `dest` on a thread, as clone `id`.
    pub fn start(&mut self, _id: u64, _url: String, _dest: PathBuf, _runner: Arc<dyn CloneRunner>) {
        // stub: filled by E
    }

    /// Everything the running clones reported since the last call.
    pub fn poll(&mut self) -> Vec<CloneEvent> {
        // stub: filled by E
        Vec::new()
    }
}
