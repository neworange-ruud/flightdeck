//! Cloning a repository on a worker (`specs/WORKER_PRD.md` §4.4, W16): the
//! Desktop sends a URL, the worker clones into `projects_root/<name>` with
//! progress reported to the window, then opens it.
//!
//! Clone is deliberately not on `GitExecutor`, which is repository-scoped and
//! history-safe by construction: a clone has no repository yet. Git runs by
//! argv, never a shell, with prompts off: failures (authentication, host key,
//! network, name in use) are reported in git's own words.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

use crate::contracts::FileSystem;
use crate::host::worker::projects::{lexically_normal, Confinement, ProjectOpError};

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
pub fn parse_progress(line: &str) -> Option<CloneLine> {
    let line = line.trim();
    let line = line.strip_prefix("remote:").map_or(line, str::trim);
    let (phase, rest) = line.split_once(':')?;
    let phase = phase.trim();
    if phase.is_empty() {
        return None;
    }
    let rest = rest.trim_start();
    let digits = rest.chars().take_while(char::is_ascii_digit).count();
    if digits == 0 || !rest[digits..].starts_with('%') {
        return None;
    }
    let percent = rest[..digits].parse::<u32>().ok()?.min(100) as u8;
    Some(CloneLine {
        phase: phase.to_string(),
        percent: Some(percent),
    })
}

/// Whether `name` is usable as a folder name: `[A-Za-z0-9._-]`, not `.`/`..`.
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// The folder name a URL clones into: its last segment without `.git`,
/// limited to `[A-Za-z0-9._-]`, never `.` or `..`.
pub fn derive_name(url: &str) -> Option<String> {
    let url = url.trim().trim_end_matches('/');
    // `https://host/a/b.git` and scp-style `git@host:a/b.git` alike.
    let last = url.rsplit(['/', ':']).next()?;
    let name = last.strip_suffix(".git").unwrap_or(last);
    valid_name(name).then(|| name.to_string())
}

/// Where a clone of `url` (into `name`, or the derived name) would land
/// under `projects_root`, refusing what cannot or must not be cloned.
/// Creates `projects_root` when missing.
pub fn plan_clone(
    fs: &dyn FileSystem,
    conf: &Confinement,
    url: &str,
    name: Option<&str>,
    projects_root: &Path,
) -> std::result::Result<PathBuf, String> {
    let url = url.trim();
    if url.is_empty() {
        return Err("Enter the address of the repository to clone.".to_string());
    }
    // Git would read a leading dash as an option.
    if url.starts_with('-') {
        return Err(format!("{url} is not a repository address."));
    }
    let name = match name.map(str::trim).filter(|n| !n.is_empty()) {
        Some(n) if valid_name(n) => n.to_string(),
        Some(n) => {
            return Err(format!(
            "{n} is not a usable folder name: use letters, digits, dots, dashes and underscores."
        ))
        }
        None => derive_name(url).ok_or_else(|| {
            "Could not work out a folder name from that address: enter one.".to_string()
        })?,
    };
    let root = ensure_projects_root(fs, conf, projects_root)?;
    let dest = root.join(&name);
    if fs.exists(&dest) {
        return Err(format!("{} already exists.", dest.display()));
    }
    Ok(dest)
}

/// The canonical `projects_root`, created when missing, refusing a root that
/// is (or would be created) outside home.
fn ensure_projects_root(
    fs: &dyn FileSystem,
    conf: &Confinement,
    root: &Path,
) -> std::result::Result<PathBuf, String> {
    let outside = || ProjectOpError::RootOutsideHome(root.display().to_string()).to_string();
    let root = lexically_normal(&if root.is_absolute() {
        root.to_path_buf()
    } else {
        conf.home().join(root)
    });
    // Judge the nearest folder that exists before creating anything, so a
    // link on the way cannot make the creation land outside home.
    let mut existing = root.clone();
    while !fs.exists(&existing) {
        if !existing.pop() {
            return Err(outside());
        }
    }
    match fs.canonicalize(&existing) {
        Ok(c) if c.starts_with(conf.home()) => {}
        Ok(_) => return Err(outside()),
        Err(e) => return Err(e.to_string()),
    }
    if existing != root {
        fs.create_dir_all(&root).map_err(|e| e.to_string())?;
    }
    let canonical = fs.canonicalize(&root).map_err(|e| e.to_string())?;
    if !canonical.starts_with(conf.home()) {
        return Err(outside());
    }
    if !fs.is_dir(&canonical) {
        return Err(format!("{} is not a folder.", root.display()));
    }
    Ok(canonical)
}

/// `git clone --progress -- <url> <dest>`.
#[derive(Debug, Default, Clone, Copy)]
pub struct RealGitCloner;

/// How many of git's own message lines a failure carries.
const FAILURE_LINES: usize = 5;

impl CloneRunner for RealGitCloner {
    fn clone_repo(
        &self,
        url: &str,
        dest: &Path,
        progress: &mut dyn FnMut(CloneLine),
    ) -> std::result::Result<(), String> {
        let existed = dest.exists();
        let mut cmd = Command::new("git");
        cmd.args(["clone", "--progress", "--"])
            .arg(url)
            .arg(dest)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            // Never ask: there is nobody at a terminal to answer.
            .env("GIT_TERMINAL_PROMPT", "0");
        if std::env::var_os("GIT_SSH_COMMAND").is_none() {
            cmd.env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes");
        }
        let mut child = cmd.spawn().map_err(|e| format!("Could not run git: {e}"))?;
        let mut messages: Vec<String> = Vec::new();
        if let Some(mut stderr) = child.stderr.take() {
            // Git redraws a progress line with `\r`, so split on both.
            let mut pending: Vec<u8> = Vec::new();
            let mut buf = [0u8; 4096];
            let mut handle = |line: &[u8]| {
                let line = String::from_utf8_lossy(line);
                let line = line.trim();
                if line.is_empty() {
                    return;
                }
                match parse_progress(line) {
                    Some(parsed) => progress(parsed),
                    None => messages.push(line.to_string()),
                }
            };
            loop {
                match stderr.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        for &b in &buf[..n] {
                            if b == b'\r' || b == b'\n' {
                                handle(&pending);
                                pending.clear();
                            } else {
                                pending.push(b);
                            }
                        }
                    }
                }
            }
            handle(&pending);
        }
        let status = child
            .wait()
            .map_err(|e| format!("Could not wait for git: {e}"))?;
        if status.success() {
            return Ok(());
        }
        // Leave nothing half-cloned behind (but never a folder that was there).
        if !existed {
            let _ = std::fs::remove_dir_all(dest);
        }
        let own: Vec<&String> = messages
            .iter()
            .filter(|m| !m.starts_with("Cloning into"))
            .collect();
        let tail = &own[own.len().saturating_sub(FAILURE_LINES)..];
        if tail.is_empty() {
            Err(format!("git clone failed ({status})."))
        } else {
            Err(tail
                .iter()
                .map(|m| m.as_str())
                .collect::<Vec<_>>()
                .join("\n"))
        }
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
#[derive(Debug)]
pub struct CloneJobs {
    tx: Sender<CloneEvent>,
    rx: Receiver<CloneEvent>,
}

impl Default for CloneJobs {
    fn default() -> Self {
        let (tx, rx) = channel();
        CloneJobs { tx, rx }
    }
}

impl CloneJobs {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start cloning `url` into `dest` on a thread, as clone `id`.
    pub fn start(&mut self, id: u64, url: String, dest: PathBuf, runner: Arc<dyn CloneRunner>) {
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let outcome = runner.clone_repo(&url, &dest, &mut |line| {
                let _ = tx.send(CloneEvent::Progress { id, line });
            });
            let _ = tx.send(CloneEvent::Done {
                id,
                result: outcome.map(|()| dest),
            });
        });
    }

    /// Everything the running clones reported since the last call.
    pub fn poll(&mut self) -> Vec<CloneEvent> {
        self.rx.try_iter().collect()
    }
}

#[cfg(test)]
mod tests;
