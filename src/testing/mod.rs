//! Test fakes for the service traits (SPECS §26).
//!
//! These are compiled in normal builds (not `#[cfg(test)]`) so that both
//! in-crate unit tests and external integration tests can use them. Every
//! Phase 1+ logic module is unit-tested against these fakes — no real git, no
//! real filesystem mutation outside tempdirs, no real PTY.

use crate::contracts::domain::{MergeOutcome, ProcessState, PtySize, WorktreeInfo};
use crate::contracts::error::{FlightDeckError, Result};
use crate::contracts::traits::{Clock, FileSystem, GitExecutor, PtyBackend, PtySession};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

// ===========================================================================
// FakeFs — in-memory filesystem
// ===========================================================================

/// In-memory [`FileSystem`] for tests.
#[derive(Debug, Default)]
pub struct FakeFs {
    inner: Mutex<FakeFsState>,
}

#[derive(Debug, Default)]
struct FakeFsState {
    files: BTreeMap<PathBuf, String>,
    dirs: HashSet<PathBuf>,
}

impl FakeFs {
    /// Create an empty in-memory filesystem.
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed a file with contents (creating parent dirs).
    pub fn with_file(self, path: impl Into<PathBuf>, contents: impl Into<String>) -> Self {
        let path = path.into();
        {
            let mut st = self.inner.lock().unwrap();
            mark_parents(&mut st.dirs, &path);
            st.files.insert(path, contents.into());
        }
        self
    }

    /// Seed a directory.
    pub fn with_dir(self, path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        {
            let mut st = self.inner.lock().unwrap();
            mark_parents(&mut st.dirs, &path);
            st.dirs.insert(path);
        }
        self
    }

    /// Snapshot of a file's contents, if present.
    pub fn file_contents(&self, path: &Path) -> Option<String> {
        self.inner.lock().unwrap().files.get(path).cloned()
    }

    /// All file paths currently present, sorted.
    pub fn files(&self) -> Vec<PathBuf> {
        self.inner.lock().unwrap().files.keys().cloned().collect()
    }
}

fn mark_parents(dirs: &mut HashSet<PathBuf>, path: &Path) {
    let mut cur = path.parent();
    while let Some(p) = cur {
        if p.as_os_str().is_empty() {
            break;
        }
        dirs.insert(p.to_path_buf());
        cur = p.parent();
    }
}

impl FileSystem for FakeFs {
    fn exists(&self, p: &Path) -> bool {
        let st = self.inner.lock().unwrap();
        st.files.contains_key(p) || st.dirs.contains(p)
    }

    fn create_dir_all(&self, p: &Path) -> Result<()> {
        let mut st = self.inner.lock().unwrap();
        mark_parents(&mut st.dirs, p);
        st.dirs.insert(p.to_path_buf());
        Ok(())
    }

    fn read_to_string(&self, p: &Path) -> Result<String> {
        self.inner
            .lock()
            .unwrap()
            .files
            .get(p)
            .cloned()
            .ok_or_else(|| FlightDeckError::Io(format!("no such file: {}", p.display())))
    }

    fn write(&self, p: &Path, contents: &str) -> Result<()> {
        let mut st = self.inner.lock().unwrap();
        mark_parents(&mut st.dirs, p);
        st.files.insert(p.to_path_buf(), contents.to_string());
        Ok(())
    }

    fn append_line(&self, p: &Path, line: &str) -> Result<()> {
        let mut st = self.inner.lock().unwrap();
        mark_parents(&mut st.dirs, p);
        let entry = st.files.entry(p.to_path_buf()).or_default();
        entry.push_str(line);
        entry.push('\n');
        Ok(())
    }

    fn list_dir(&self, p: &Path) -> Result<Vec<PathBuf>> {
        let st = self.inner.lock().unwrap();
        if !st.dirs.contains(p) {
            return Err(FlightDeckError::Io(format!(
                "no such directory: {}",
                p.display()
            )));
        }
        let mut out: Vec<PathBuf> = Vec::new();
        let collect = |set: &mut Vec<PathBuf>, candidate: &Path| {
            if let Some(parent) = candidate.parent() {
                if parent == p && !set.contains(&candidate.to_path_buf()) {
                    set.push(candidate.to_path_buf());
                }
            }
        };
        for f in st.files.keys() {
            collect(&mut out, f);
        }
        for d in &st.dirs {
            collect(&mut out, d);
        }
        out.sort();
        Ok(out)
    }
}

// ===========================================================================
// FakeGit — scriptable git
// ===========================================================================

/// Scriptable in-memory [`GitExecutor`] for tests.
#[derive(Debug)]
pub struct FakeGit {
    inner: Mutex<FakeGitState>,
}

#[derive(Debug)]
struct FakeGitState {
    root: PathBuf,
    current_branch: String,
    branches: HashSet<String>,
    /// Per-cwd dirty flag; `None` key holds the default.
    dirty: HashMap<PathBuf, bool>,
    default_dirty: bool,
    /// Per-cwd `git status --porcelain` line overrides. When absent, the
    /// porcelain output is synthesized from the dirty flag.
    porcelain: HashMap<PathBuf, Vec<String>>,
    worktrees: Vec<WorktreeInfo>,
    revs: HashMap<String, String>,
    ahead_behind: HashMap<(String, String), (u32, u32)>,
    upstreams: HashMap<String, Option<String>>,
    remotes: HashMap<String, String>,
    merge_outcome: Option<MergeOutcome>,
    // recordings
    created_branches: Vec<(String, String)>,
    added_worktrees: Vec<(PathBuf, String)>,
    removed_worktrees: Vec<PathBuf>,
    pushes: Vec<(String, String, PathBuf)>,
    merges: Vec<(String, PathBuf)>,
}

impl Default for FakeGit {
    fn default() -> Self {
        FakeGit {
            inner: Mutex::new(FakeGitState {
                root: PathBuf::from("/repo"),
                current_branch: "main".to_string(),
                branches: ["main".to_string()].into_iter().collect(),
                dirty: HashMap::new(),
                default_dirty: false,
                porcelain: HashMap::new(),
                worktrees: Vec::new(),
                revs: HashMap::new(),
                ahead_behind: HashMap::new(),
                upstreams: HashMap::new(),
                remotes: HashMap::new(),
                merge_outcome: None,
                created_branches: Vec::new(),
                added_worktrees: Vec::new(),
                removed_worktrees: Vec::new(),
                pushes: Vec::new(),
                merges: Vec::new(),
            }),
        }
    }
}

impl FakeGit {
    /// New fake with repo root `/repo` and a single `main` branch.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the repository root.
    pub fn with_root(self, root: impl Into<PathBuf>) -> Self {
        self.inner.lock().unwrap().root = root.into();
        self
    }

    /// Replace the set of existing branches.
    pub fn with_branches<I, S>(self, branches: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut st = self.inner.lock().unwrap();
        st.branches = branches.into_iter().map(Into::into).collect();
        drop(st);
        self
    }

    /// Set the current branch.
    pub fn with_current_branch(self, branch: impl Into<String>) -> Self {
        self.inner.lock().unwrap().current_branch = branch.into();
        self
    }

    /// Set the default dirty state used when a `cwd` has no specific override.
    pub fn set_dirty(&self, dirty: bool) {
        self.inner.lock().unwrap().default_dirty = dirty;
    }

    /// Set a dirty override for a specific path.
    pub fn set_dirty_at(&self, path: impl Into<PathBuf>, dirty: bool) {
        self.inner.lock().unwrap().dirty.insert(path.into(), dirty);
    }

    /// Set explicit `git status --porcelain` lines for a specific path. This
    /// also implies the worktree is dirty when the lines are non-empty.
    pub fn set_porcelain_at<I, S>(&self, path: impl Into<PathBuf>, lines: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.inner
            .lock()
            .unwrap()
            .porcelain
            .insert(path.into(), lines.into_iter().map(Into::into).collect());
    }

    /// Map a refname to a SHA.
    pub fn set_rev(&self, refname: impl Into<String>, sha: impl Into<String>) {
        self.inner
            .lock()
            .unwrap()
            .revs
            .insert(refname.into(), sha.into());
    }

    /// Set `(ahead, behind)` of `branch` relative to `base`.
    pub fn set_ahead_behind(
        &self,
        base: impl Into<String>,
        branch: impl Into<String>,
        ahead: u32,
        behind: u32,
    ) {
        self.inner
            .lock()
            .unwrap()
            .ahead_behind
            .insert((base.into(), branch.into()), (ahead, behind));
    }

    /// Set the upstream of `branch`.
    pub fn set_upstream(&self, branch: impl Into<String>, upstream: Option<String>) {
        self.inner
            .lock()
            .unwrap()
            .upstreams
            .insert(branch.into(), upstream);
    }

    /// Set the URL of `remote`.
    pub fn set_remote(&self, remote: impl Into<String>, url: impl Into<String>) {
        self.inner
            .lock()
            .unwrap()
            .remotes
            .insert(remote.into(), url.into());
    }

    /// Register an existing worktree (for recovery / attach tests).
    pub fn add_existing_worktree(&self, info: WorktreeInfo) {
        self.inner.lock().unwrap().worktrees.push(info);
    }

    /// Set the outcome returned by [`GitExecutor::merge_no_ff`].
    pub fn set_merge_outcome(&self, outcome: MergeOutcome) {
        self.inner.lock().unwrap().merge_outcome = Some(outcome);
    }

    // --- recordings ---

    /// Branches created via [`GitExecutor::create_branch`], as `(name, from)`.
    pub fn created_branches(&self) -> Vec<(String, String)> {
        self.inner.lock().unwrap().created_branches.clone()
    }

    /// Worktrees added via [`GitExecutor::add_worktree`], as `(path, branch)`.
    pub fn added_worktrees(&self) -> Vec<(PathBuf, String)> {
        self.inner.lock().unwrap().added_worktrees.clone()
    }

    /// Worktrees removed via [`GitExecutor::remove_worktree`].
    pub fn removed_worktrees(&self) -> Vec<PathBuf> {
        self.inner.lock().unwrap().removed_worktrees.clone()
    }

    /// Pushes performed, as `(remote, branch, cwd)`.
    pub fn pushes(&self) -> Vec<(String, String, PathBuf)> {
        self.inner.lock().unwrap().pushes.clone()
    }

    /// Merges performed, as `(branch, cwd)`.
    pub fn merges(&self) -> Vec<(String, PathBuf)> {
        self.inner.lock().unwrap().merges.clone()
    }
}

impl GitExecutor for FakeGit {
    fn repo_root(&self, _cwd: &Path) -> Result<PathBuf> {
        Ok(self.inner.lock().unwrap().root.clone())
    }

    fn current_branch(&self, _cwd: &Path) -> Result<String> {
        Ok(self.inner.lock().unwrap().current_branch.clone())
    }

    fn is_dirty(&self, cwd: &Path) -> Result<bool> {
        let st = self.inner.lock().unwrap();
        Ok(*st.dirty.get(cwd).unwrap_or(&st.default_dirty))
    }

    fn status_porcelain(&self, cwd: &Path) -> Result<Vec<String>> {
        let st = self.inner.lock().unwrap();
        // Explicit override wins; otherwise synthesize from the dirty flag so
        // callers that only set dirtiness still see a non-empty status.
        if let Some(lines) = st.porcelain.get(cwd) {
            return Ok(lines.clone());
        }
        let dirty = *st.dirty.get(cwd).unwrap_or(&st.default_dirty);
        Ok(if dirty {
            vec![" M synthetic-change".to_string()]
        } else {
            Vec::new()
        })
    }

    fn branch_exists(&self, name: &str) -> Result<bool> {
        Ok(self.inner.lock().unwrap().branches.contains(name))
    }

    fn create_branch(&self, name: &str, from: &str) -> Result<()> {
        let mut st = self.inner.lock().unwrap();
        st.branches.insert(name.to_string());
        st.created_branches
            .push((name.to_string(), from.to_string()));
        Ok(())
    }

    fn rev_parse(&self, refname: &str) -> Result<String> {
        let st = self.inner.lock().unwrap();
        Ok(st
            .revs
            .get(refname)
            .cloned()
            .unwrap_or_else(|| format!("sha-{refname}")))
    }

    fn add_worktree(&self, path: &Path, branch: &str) -> Result<()> {
        let mut st = self.inner.lock().unwrap();
        st.worktrees.push(WorktreeInfo {
            path: path.to_path_buf(),
            branch: Some(branch.to_string()),
            head: None,
        });
        st.added_worktrees
            .push((path.to_path_buf(), branch.to_string()));
        Ok(())
    }

    fn list_worktrees(&self) -> Result<Vec<WorktreeInfo>> {
        Ok(self.inner.lock().unwrap().worktrees.clone())
    }

    fn remove_worktree(&self, path: &Path, _force: bool) -> Result<()> {
        let mut st = self.inner.lock().unwrap();
        st.worktrees.retain(|w| w.path != path);
        st.removed_worktrees.push(path.to_path_buf());
        Ok(())
    }

    fn ahead_behind(&self, base: &str, branch: &str) -> Result<(u32, u32)> {
        Ok(self
            .inner
            .lock()
            .unwrap()
            .ahead_behind
            .get(&(base.to_string(), branch.to_string()))
            .copied()
            .unwrap_or((0, 0)))
    }

    fn upstream_of(&self, branch: &str) -> Result<Option<String>> {
        Ok(self
            .inner
            .lock()
            .unwrap()
            .upstreams
            .get(branch)
            .cloned()
            .unwrap_or(None))
    }

    fn push(&self, remote: &str, branch: &str, cwd: &Path) -> Result<()> {
        self.inner.lock().unwrap().pushes.push((
            remote.to_string(),
            branch.to_string(),
            cwd.to_path_buf(),
        ));
        Ok(())
    }

    fn remote_url(&self, remote: &str) -> Result<Option<String>> {
        Ok(self.inner.lock().unwrap().remotes.get(remote).cloned())
    }

    fn merge_no_ff(&self, branch: &str, cwd: &Path) -> Result<MergeOutcome> {
        let mut st = self.inner.lock().unwrap();
        st.merges.push((branch.to_string(), cwd.to_path_buf()));
        Ok(st.merge_outcome.clone().unwrap_or(MergeOutcome {
            merged: true,
            conflicted: false,
            message: "merged".to_string(),
        }))
    }
}

// ===========================================================================
// FakePty — scriptable PTY backend
// ===========================================================================

#[derive(Debug)]
struct FakePtyInner {
    output: VecDeque<Vec<u8>>,
    input: Vec<u8>,
    state: ProcessState,
    ctrl_c: u32,
    terminated: bool,
    resizes: Vec<PtySize>,
}

impl Default for FakePtyInner {
    fn default() -> Self {
        FakePtyInner {
            output: VecDeque::new(),
            input: Vec::new(),
            state: ProcessState::Running,
            ctrl_c: 0,
            terminated: false,
            resizes: Vec::new(),
        }
    }
}

/// A handle to a spawned [`FakePtySession`], letting tests drive output and
/// process state and inspect what was written.
#[derive(Debug, Clone)]
pub struct FakePtyHandle(Arc<Mutex<FakePtyInner>>);

impl FakePtyHandle {
    /// Queue output bytes that the next `try_read_output` will drain.
    pub fn push_output(&self, bytes: impl Into<Vec<u8>>) {
        self.0.lock().unwrap().output.push_back(bytes.into());
    }

    /// Override the process state (e.g. simulate exit).
    pub fn set_state(&self, state: ProcessState) {
        self.0.lock().unwrap().state = state;
    }

    /// All bytes written to the session input so far.
    pub fn input(&self) -> Vec<u8> {
        self.0.lock().unwrap().input.clone()
    }

    /// How many times Ctrl-C was sent.
    pub fn ctrl_c_count(&self) -> u32 {
        self.0.lock().unwrap().ctrl_c
    }

    /// Whether the process tree was force-terminated.
    pub fn terminated(&self) -> bool {
        self.0.lock().unwrap().terminated
    }

    /// Resize requests received.
    pub fn resizes(&self) -> Vec<PtySize> {
        self.0.lock().unwrap().resizes.clone()
    }
}

/// Scriptable [`PtyBackend`]. Each `spawn` consumes a pre-queued session (or
/// creates a default one), unless `fail_next` is set.
#[derive(Debug, Default)]
pub struct FakePty {
    queued: Mutex<VecDeque<Arc<Mutex<FakePtyInner>>>>,
    spawns: Mutex<Vec<(String, Vec<String>, PathBuf)>>,
    fail_next: Mutex<bool>,
}

impl FakePty {
    /// New backend with no queued sessions.
    pub fn new() -> Self {
        Self::default()
    }

    /// Pre-queue a session and return a handle to drive it. Sessions are handed
    /// out by `spawn` in FIFO order.
    pub fn queue_session(&self) -> FakePtyHandle {
        let inner = Arc::new(Mutex::new(FakePtyInner::default()));
        self.queued.lock().unwrap().push_back(inner.clone());
        FakePtyHandle(inner)
    }

    /// Make the next `spawn` fail (SPECS §26 "handles failed process start").
    pub fn fail_next_spawn(&self) {
        *self.fail_next.lock().unwrap() = true;
    }

    /// Record of all spawns, as `(cmd, args, cwd)`.
    pub fn spawns(&self) -> Vec<(String, Vec<String>, PathBuf)> {
        self.spawns.lock().unwrap().clone()
    }
}

impl PtyBackend for FakePty {
    fn spawn(
        &self,
        cmd: &str,
        args: &[String],
        cwd: &Path,
        _size: PtySize,
    ) -> Result<Box<dyn PtySession>> {
        {
            let mut fail = self.fail_next.lock().unwrap();
            if *fail {
                *fail = false;
                return Err(FlightDeckError::Other(format!(
                    "fake spawn failed for {cmd}"
                )));
            }
        }
        self.spawns
            .lock()
            .unwrap()
            .push((cmd.to_string(), args.to_vec(), cwd.to_path_buf()));
        let inner = self
            .queued
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Arc::new(Mutex::new(FakePtyInner::default())));
        Ok(Box::new(FakePtySession(inner)))
    }
}

/// A live fake session backed by a shared [`FakePtyHandle`] inner.
#[derive(Debug)]
pub struct FakePtySession(Arc<Mutex<FakePtyInner>>);

impl PtySession for FakePtySession {
    fn write_input(&mut self, bytes: &[u8]) -> Result<()> {
        self.0.lock().unwrap().input.extend_from_slice(bytes);
        Ok(())
    }

    fn resize(&mut self, size: PtySize) -> Result<()> {
        self.0.lock().unwrap().resizes.push(size);
        Ok(())
    }

    fn try_read_output(&mut self) -> Result<Vec<u8>> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .output
            .pop_front()
            .unwrap_or_default())
    }

    fn send_ctrl_c(&mut self) -> Result<()> {
        self.0.lock().unwrap().ctrl_c += 1;
        Ok(())
    }

    fn process_state(&self) -> ProcessState {
        self.0.lock().unwrap().state
    }

    fn terminate_tree(&mut self) -> Result<()> {
        let mut st = self.0.lock().unwrap();
        st.terminated = true;
        st.state = ProcessState::Stopped;
        Ok(())
    }
}

// ===========================================================================
// FakeClock — fixed time
// ===========================================================================

/// [`Clock`] returning a fixed timestamp and a settable millisecond counter.
#[derive(Debug, Clone)]
pub struct FakeClock {
    now: String,
    millis: Arc<Mutex<u64>>,
}

impl Default for FakeClock {
    fn default() -> Self {
        FakeClock {
            now: "2026-01-01T00:00:00Z".to_string(),
            millis: Arc::new(Mutex::new(0)),
        }
    }
}

impl FakeClock {
    /// New clock with a custom fixed timestamp.
    pub fn new(now: impl Into<String>) -> Self {
        FakeClock {
            now: now.into(),
            millis: Arc::new(Mutex::new(0)),
        }
    }

    /// Set the value returned by [`Clock::now_millis`].
    pub fn set_millis(&self, millis: u64) {
        *self.millis.lock().unwrap() = millis;
    }

    /// Advance the millisecond counter by `delta`.
    pub fn advance_millis(&self, delta: u64) {
        *self.millis.lock().unwrap() += delta;
    }
}

impl Clock for FakeClock {
    fn now_iso8601(&self) -> String {
        self.now.clone()
    }

    fn now_millis(&self) -> u64 {
        *self.millis.lock().unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fakefs_write_read_and_append() {
        let fs = FakeFs::new();
        let p = Path::new("/repo/.gitignore");
        fs.write(p, "a\n").unwrap();
        assert_eq!(fs.read_to_string(p).unwrap(), "a\n");
        fs.append_line(p, "b").unwrap();
        assert_eq!(fs.read_to_string(p).unwrap(), "a\nb\n");
        assert!(fs.exists(Path::new("/repo")));
    }

    #[test]
    fn fakefs_list_dir() {
        let fs = FakeFs::new()
            .with_file("/repo/a.txt", "x")
            .with_dir("/repo/sub");
        let mut entries = fs.list_dir(Path::new("/repo")).unwrap();
        entries.sort();
        assert_eq!(
            entries,
            vec![PathBuf::from("/repo/a.txt"), PathBuf::from("/repo/sub")]
        );
    }

    #[test]
    fn fakegit_branches_and_dirty() {
        let git = FakeGit::new().with_branches(["main", "flightdeck/x"]);
        assert!(git.branch_exists("flightdeck/x").unwrap());
        assert!(!git.branch_exists("nope").unwrap());
        git.set_dirty(true);
        assert!(git.is_dirty(Path::new("/repo")).unwrap());
        git.set_dirty_at("/repo/clean", false);
        assert!(!git.is_dirty(Path::new("/repo/clean")).unwrap());
    }

    #[test]
    fn fakegit_records_mutations() {
        let git = FakeGit::new();
        git.create_branch("flightdeck/y", "main").unwrap();
        assert_eq!(
            git.created_branches(),
            vec![("flightdeck/y".to_string(), "main".to_string())]
        );
        git.push("origin", "flightdeck/y", Path::new("/wt"))
            .unwrap();
        assert_eq!(git.pushes().len(), 1);
    }

    #[test]
    fn fakepty_spawn_and_drive() {
        let pty = FakePty::new();
        let handle = pty.queue_session();
        let mut session = pty
            .spawn("opencode", &[], Path::new("/wt"), PtySize::default())
            .unwrap();
        handle.push_output(b"Proceed?".to_vec());
        assert_eq!(session.try_read_output().unwrap(), b"Proceed?");
        session.write_input(b"y").unwrap();
        assert_eq!(handle.input(), b"y");
        session.send_ctrl_c().unwrap();
        assert_eq!(handle.ctrl_c_count(), 1);
        handle.set_state(ProcessState::Exited(0));
        assert_eq!(session.process_state(), ProcessState::Exited(0));
    }

    #[test]
    fn fakepty_failed_spawn() {
        let pty = FakePty::new();
        pty.fail_next_spawn();
        assert!(pty
            .spawn("missing", &[], Path::new("/wt"), PtySize::default())
            .is_err());
        // subsequent spawn succeeds
        assert!(pty
            .spawn("ok", &[], Path::new("/wt"), PtySize::default())
            .is_ok());
    }
}
