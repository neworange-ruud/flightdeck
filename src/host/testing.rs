//! Building an [`AppHost`] over the fakes in [`crate::testing`], with no
//! repository, terminal or network behind it.
//!
//! Compiled for this crate's own tests and, behind the off-by-default
//! `testing` cargo feature, for another front-end's tests: the GPUI desktop app
//! enables the feature in its dev-dependencies only, so its views can be
//! driven against a real host whose workspace holds known projects. Nothing
//! here reaches a shipped binary.
//!
//! The host borrows its services (`'a`), so a caller keeps its fakes alive for
//! the host's whole life — the same shape the front-ends have with the real
//! services.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use super::AppHost;
use crate::app::state::AppState;
use crate::contracts::{Config, Notifier, TabState};
use crate::git::status::WorktreeStatus;
use crate::persistence::project_state::default_state;
use crate::testing::{FakeClock, FakeFs, FakeGit};
use crate::{Env, Project, WebSurface, Workspace};

/// One project of a test workspace.
pub struct TestProject {
    /// Display name (the TUI derives it from the folder name).
    pub name: String,
    /// The repository root the project reports.
    pub root: PathBuf,
    /// Its application state.
    pub state: AppState,
    /// The git executor behind it; keep a clone to configure what the host's
    /// dispatches will read.
    pub git: Arc<FakeGit>,
}

impl TestProject {
    /// A project named `name` at `/<name>` on base branch `main`, holding one
    /// recovered Agent Tab per entry of `tabs` (see [`state_with_tabs`]).
    pub fn new(name: &str, tabs: &[&str]) -> TestProject {
        TestProject {
            name: name.to_string(),
            root: PathBuf::from(format!("/{name}")),
            state: state_with_tabs(name, tabs),
            git: Arc::new(FakeGit::new().with_branches(["main"])),
        }
    }
}

/// An [`AppState`] for the repository at `/<project>` with one Agent Tab per
/// name, in order, the first selected — as a project reopened from its
/// `state.json` looks before its agents are resumed: every tab materialized
/// (its git actions available) but with no process running yet. Tab ids are
/// `t0`, `t1`, …; branches `flightdeck/<name>`.
pub fn state_with_tabs(project: &str, tabs: &[&str]) -> AppState {
    let mut ps = default_state("main");
    for (i, name) in tabs.iter().enumerate() {
        ps.tabs.push(TabState {
            id: format!("t{i}"),
            name: name.to_string(),
            slug: name.to_string(),
            agent: "opencode".to_string(),
            branch: format!("flightdeck/{name}"),
            worktree_path_relative: format!(".flightdeck/worktrees/{name}"),
            base_branch: "main".to_string(),
            base_commit_sha: "sha".to_string(),
            created_at: "t".to_string(),
            attached_existing_branch: false,
            recovered: false,
            last_known_status: "unknown".to_string(),
            manual_status: None,
            containerized: false,
            container_image: None,
            runs_on_base: false,
            resume_args: Vec::new(),
            activity: Default::default(),
        });
    }
    let root = format!("/{project}");
    AppState::new(
        Config::default(),
        ps,
        &root,
        format!("{root}/.flightdeck/state.json"),
    )
}

/// A host over `projects`, `active` on screen, with no background service
/// started: no web server, no relay, no update check. Its first turn skips the
/// tick-0 git refresh (which would answer from a worker thread on a later
/// turn), so a test's turns are deterministic.
///
/// Panics when `projects` is empty or `active` is out of range, which is a
/// test bug: a workspace always has an active project.
pub fn host<'a>(
    env: Env<'a>,
    notifier: &'a dyn Notifier,
    projects: Vec<TestProject>,
    active: usize,
) -> AppHost<'a> {
    build(env, notifier, projects, active, false)
}

/// An `--isolated` host (SPECS §32): exactly one project, marked isolated the
/// way `open_project` marks it, and refusing what an isolated run refuses.
pub fn isolated_host<'a>(
    env: Env<'a>,
    notifier: &'a dyn Notifier,
    mut project: TestProject,
) -> AppHost<'a> {
    project.state.set_isolated(None);
    build(env, notifier, vec![project], 0, true)
}

fn build<'a>(
    env: Env<'a>,
    notifier: &'a dyn Notifier,
    projects: Vec<TestProject>,
    active: usize,
    isolated: bool,
) -> AppHost<'a> {
    assert!(active < projects.len(), "the active project must exist");
    let workspace = Workspace {
        projects: projects
            .into_iter()
            .map(|p| project(&p.name, p.root, p.state, p.git))
            .collect(),
        active,
    };
    let mut host = AppHost::from_parts(env, notifier, workspace, web_surface(), isolated);
    host.tick = 1;
    host
}

/// [`host`], plus a workspace file at `ws_path` on `env.fs`, treated the way
/// [`AppHost::open`] treats the real one: its view state
/// ([`AppHost::workspace_ui`]) is loaded now, and [`AppHost::persist`] writes
/// it back. Build a second host over the same fakes to see what a restart
/// keeps. (The file's project list is not reopened: `projects` is the
/// workspace.)
pub fn host_with_workspace_file<'a>(
    env: Env<'a>,
    notifier: &'a dyn Notifier,
    projects: Vec<TestProject>,
    active: usize,
    ws_path: PathBuf,
) -> AppHost<'a> {
    let fs = env.fs;
    let mut host = host(env, notifier, projects, active);
    if let Ok(saved) = crate::persistence::workspace::load_workspace(fs, &ws_path) {
        host.workspace_ui = saved.ui;
    }
    host.ws_path = Some(ws_path);
    host
}

/// Record `status` as project `project`'s latest collected git status for tab
/// `tab_id`, as the periodic refresh would.
pub fn set_git_status(host: &mut AppHost, project: usize, tab_id: &str, status: WorktreeStatus) {
    host.workspace.projects[project]
        .cache
        .insert(tab_id.to_string(), status);
}

/// A [`Project`] with fresh worker channels, as `open_project` builds one.
pub(crate) fn project(name: &str, root: PathBuf, state: AppState, git: Arc<FakeGit>) -> Project {
    let (create_tx, create_rx) = std::sync::mpsc::channel();
    let (status_tx, status_rx) = std::sync::mpsc::channel();
    Project {
        name: name.to_string(),
        git: crate::git::repo::ProjectGit::new(root, git),
        state,
        cache: HashMap::new(),
        create_tx,
        create_rx,
        status_tx,
        status_rx,
        status_in_flight: false,
        git_lock: Arc::new(Mutex::new(())),
    }
}

/// A web surface with no server and no real credential file behind it.
pub(crate) fn web_surface() -> WebSurface {
    let store = crate::web::credentials::CredentialStore::open(
        Arc::new(FakeFs::new()),
        Arc::new(FakeClock::default()),
        PathBuf::from("/web.json"),
    );
    let (inbound_tx, inbound_rx) = std::sync::mpsc::channel();
    let (count_tx, count_rx) = std::sync::mpsc::channel();
    WebSurface {
        credentials: Arc::new(Mutex::new(store)),
        streams: crate::web::stream::TerminalStreams::new(1024),
        activity: crate::web::activity::ActivityStore::new(),
        pending_finishes: crate::web::activity::PendingFinishes::new(),
        inbound_tx,
        inbound_rx,
        count_tx,
        count_rx,
        handle: None,
        published: crate::web::server::HostState::default(),
    }
}
