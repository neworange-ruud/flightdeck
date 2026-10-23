//! [`RemoteWorkspace`]: the controlled instance's workspace, as a native client
//! sees it (`specs/DESKTOP_REMOTE_CONTROL_PLAN.md` §2.2).
//!
//! It applies the host's [`Snapshot`] and then every [`Delta`] that follows,
//! and it holds **its own selection** (R2): which project, session and terminal
//! this client is looking at is the client's business, so browsing never moves
//! the host. The host's own selection is recorded only as a marker (R7) — the
//! "host is here" dot in the sidebar.
//!
//! Pure: no socket, no clock, no I/O. Everything here is a function of the
//! frames it was handed, which is what lets it be snapshot-tested.
//!
//! A worker host (v7, `specs/WORKER_PRD.md`) adds a [`WorkerMirror`]: its
//! shell, health, status, connections, and the host messages and
//! notifications it raises — the last two queued here until a front-end
//! takes them, since they are events rather than state.

use std::collections::{HashMap, VecDeque};

use crate::contracts::{AgentTabPosition, TabId};
use crate::web::protocol::{
    AboutDoc, ActivityEvent, CommandView, ConnectionsView, Delta, DialogView, Geometry, Health,
    HelpDoc, HostKind, HostMachine, HostMessage, NotificationView, ProjectId, ProjectView, Seat,
    SeatInfo, Selection, SessionView, Snapshot, StatusBucket, TerminalId, TerminalRole,
    TerminalView, UpdateNotice, ViewerId, WorkerShellView, WorkerSnapshot, WorkerStatusView,
};

/// The most activity entries kept. The host retains its own bounded feed and
/// backfills it on every snapshot; this only bounds what live deltas add.
const ACTIVITY_CAP: usize = 200;

/// The most host messages a [`WorkerMirror`] keeps — the worker's own ring.
pub const MESSAGE_CAP: usize = 100;

/// The most notifications held for a front-end that has not taken them.
const NOTIFICATION_CAP: usize = 100;

/// A worker's own state, as the window or background link sees it (v7).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkerMirror {
    /// `$HOME` on the worker: the project picker's root.
    pub home: String,
    /// The worker shell, once one has run.
    pub shell: Option<WorkerShellView>,
    /// The latest health sample.
    pub health: Option<Health>,
    /// Listen addresses, unread count, projects root.
    pub status: WorkerStatusView,
    /// Who else is connected, relative to this client.
    pub connections: ConnectionsView,
    /// Every host message seen, oldest first, at most [`MESSAGE_CAP`].
    pub messages: VecDeque<HostMessage>,
    /// Notifications not yet taken ([`RemoteWorkspace::take_notifications`]).
    pub notifications: Vec<NotificationView>,
    /// Messages not yet taken ([`RemoteWorkspace::take_new_messages`]).
    new_messages: Vec<HostMessage>,
}

impl WorkerMirror {
    fn from_snapshot(snapshot: WorkerSnapshot, previous: Option<WorkerMirror>) -> WorkerMirror {
        // What was already seen and what a front-end has not taken yet survive
        // a re-snapshot (a reconnect); the replayed unread messages are added
        // to them, once.
        let previous = previous.unwrap_or_default();
        let mut mirror = WorkerMirror {
            home: snapshot.home,
            shell: snapshot.shell,
            health: snapshot.health,
            status: snapshot.status,
            connections: snapshot.connections,
            messages: previous.messages,
            notifications: previous.notifications,
            new_messages: previous.new_messages,
        };
        for message in snapshot.host_messages {
            mirror.push_message(message);
        }
        mirror
    }

    /// Record `message` unless it was seen already. A worker numbers its
    /// messages per run, so the time is part of what makes one the same.
    fn push_message(&mut self, message: HostMessage) -> bool {
        let seen = self
            .messages
            .iter()
            .any(|m| m.id == message.id && m.at_ms == message.at_ms);
        if seen {
            return false;
        }
        self.messages.push_back(message.clone());
        while self.messages.len() > MESSAGE_CAP {
            self.messages.pop_front();
        }
        self.new_messages.push(message);
        let excess = self.new_messages.len().saturating_sub(MESSAGE_CAP);
        self.new_messages.drain(..excess);
        true
    }

    /// The worker shell as a [`TerminalView`], so it is fed, cursored and
    /// rendered by exactly the code every other remote terminal is. Its
    /// session is its own id: it belongs to no project.
    pub fn shell_terminal(&self) -> Option<TerminalView> {
        let shell = self.shell.as_ref()?;
        Some(TerminalView {
            terminal_id: shell.terminal_id.clone(),
            session_id: TabId(shell.terminal_id.as_str().to_string()),
            role: TerminalRole::Shell,
            title: shell.title.clone(),
            geometry: shell.geometry,
            byte_len: shell.byte_len,
            replay_from: shell.replay_from,
            alive: shell.alive,
            exit_code: shell.exit_code,
        })
    }
}

/// What applying one frame did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Applied {
    /// Something a view draws changed.
    pub changed: bool,
    /// The delta could not be described exactly (a terminal closed *or*
    /// disappeared — the wire says the same for both), so a fresh snapshot
    /// should be asked for. The browser does the same for every delta it does
    /// not apply itself.
    pub resync: bool,
}

/// The host's workspace plus this client's own view of it.
#[derive(Clone, Debug, Default)]
pub struct RemoteWorkspace {
    /// Whether a snapshot has arrived yet.
    pub ready: bool,
    pub host_version: String,
    pub protocol_version: u16,
    pub viewer_id: Option<ViewerId>,
    pub seat: Option<Seat>,
    pub seats: Vec<SeatInfo>,
    /// The host's clock when the seats were last stated, for `since_ms` ages.
    pub server_time_ms: i64,
    /// The last delta announced this client lost the input lock to another
    /// writer (2f). Cleared by [`RemoteWorkspace::take_preempted`].
    preempted: bool,
    pub projects: Vec<ProjectView>,
    /// The host's own selection — a marker only (R7), never followed.
    pub host_selection: Selection,
    /// The host's selected terminal grid.
    pub geometry: Option<Geometry>,
    pub replay_capacity_bytes: u64,
    pub activity: Vec<ActivityEvent>,
    pub dialog: Option<DialogView>,
    pub commands: Vec<CommandView>,
    pub help: Option<HelpDoc>,
    pub about: Option<AboutDoc>,
    pub update: Option<UpdateNotice>,
    pub sidebar_position: AgentTabPosition,
    pub host_machine: Option<HostMachine>,
    /// What kind of FlightDeck the host is (v7). `None` from a v6 host,
    /// which is a TUI or a Desktop.
    pub host_kind: Option<HostKind>,
    /// The worker's own state. `Some` only on a worker host.
    pub worker: Option<WorkerMirror>,
    selection: LocalSelection,
}

/// This client's own selection (R2).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct LocalSelection {
    project: Option<ProjectId>,
    session: Option<TabId>,
    /// Per project, the session last looked at — so switching back to a
    /// project lands on the agent that was showing, like a local project's
    /// own selected tab.
    sessions: HashMap<ProjectId, TabId>,
    /// Per session, the terminal last looked at — so switching back to a
    /// session lands on the terminal that was showing, like a local tab's own
    /// selected child.
    terminals: HashMap<TabId, TerminalId>,
}

impl RemoteWorkspace {
    /// An empty mirror, waiting for its first snapshot.
    pub fn new() -> RemoteWorkspace {
        RemoteWorkspace::default()
    }

    // -- frames in ----------------------------------------------------------

    /// Replace everything with `snapshot`, keeping this client's selection
    /// where it still names something open. The first snapshot starts the
    /// selection on the host's, which is where the person connecting expects
    /// to land; after that the two are independent.
    pub fn apply_snapshot(&mut self, snapshot: Snapshot) {
        let first = !self.ready;
        self.ready = true;
        self.host_version = snapshot.host_version;
        self.protocol_version = snapshot.protocol_version;
        self.viewer_id = Some(snapshot.viewer_id);
        self.seat = Some(snapshot.seat);
        self.seats = snapshot.seats;
        self.server_time_ms = snapshot.server_time_ms;
        self.projects = snapshot.projects;
        self.host_selection = snapshot.selection;
        self.geometry = Some(snapshot.geometry);
        self.replay_capacity_bytes = snapshot.replay_capacity_bytes;
        self.activity = snapshot.activity;
        self.dialog = snapshot.dialog;
        self.commands = snapshot.commands;
        self.help = snapshot.help;
        self.about = snapshot.about;
        self.update = snapshot.update;
        self.sidebar_position = snapshot.sidebar_position;
        self.host_machine = snapshot.host_machine;
        self.host_kind = snapshot.host_kind;
        let previous = self.worker.take();
        self.worker = snapshot
            .worker
            .map(|worker| WorkerMirror::from_snapshot(worker, previous));
        if first {
            let host = self.host_selection.clone();
            self.selection.project = host.project_id;
            self.selection.session = host.session_id.clone();
            if let (Some(session), Some(terminal)) = (host.session_id, host.terminal_id) {
                self.selection.terminals.insert(session, terminal);
            }
        }
        self.repair_selection();
    }

    /// Apply one delta.
    pub fn apply_delta(&mut self, delta: Delta) -> Applied {
        let changed = Applied {
            changed: true,
            resync: false,
        };
        let applied = match delta {
            Delta::ProjectUpsert(project) => {
                match self.project_index(&project.project_id) {
                    Some(i) => self.projects[i] = project,
                    None => self.projects.push(project),
                }
                changed
            }
            Delta::ProjectRemoved { project_id } => {
                self.projects.retain(|p| p.project_id != project_id);
                changed
            }
            Delta::SessionUpsert(session) => {
                let Some(pi) = self.project_index(&session.project_id) else {
                    return Applied {
                        changed: false,
                        resync: true,
                    };
                };
                let sessions = &mut self.projects[pi].sessions;
                match sessions
                    .iter()
                    .position(|s| s.session_id == session.session_id)
                {
                    Some(i) => sessions[i] = session,
                    None => sessions.push(session),
                }
                changed
            }
            Delta::SessionRemoved { session_id } => {
                for project in &mut self.projects {
                    project.sessions.retain(|s| s.session_id != session_id);
                }
                self.selection.sessions.retain(|_, s| s != &session_id);
                self.selection.terminals.remove(&session_id);
                changed
            }
            Delta::Status { session_id, status } => match self.session_mut(&session_id) {
                Some(session) => {
                    session.status = status;
                    changed
                }
                None => resync(),
            },
            Delta::Git { session_id, git } => match self.session_mut(&session_id) {
                Some(session) => {
                    session.git = git;
                    changed
                }
                None => resync(),
            },
            Delta::ProjectDot { project_id, dot } => match self.project_index(&project_id) {
                Some(i) => {
                    self.projects[i].dot = dot;
                    changed
                }
                None => resync(),
            },
            Delta::Selection(selection) => {
                self.host_selection = selection;
                changed
            }
            Delta::TerminalUpsert(terminal) => match self.session_mut(&terminal.session_id) {
                Some(session) => {
                    match session
                        .terminals
                        .iter()
                        .position(|t| t.terminal_id == terminal.terminal_id)
                    {
                        Some(i) => session.terminals[i] = terminal,
                        None => session.terminals.push(terminal),
                    }
                    changed
                }
                None => resync(),
            },
            Delta::TerminalClosed {
                terminal_id,
                exit_code,
            } => {
                if let Some(terminal) = self.terminal_mut(&terminal_id) {
                    terminal.alive = false;
                    terminal.exit_code = exit_code.or(terminal.exit_code);
                }
                // Closed and removed read the same on the wire; a snapshot
                // says which.
                Applied {
                    changed: true,
                    resync: true,
                }
            }
            Delta::Geometry {
                terminal_id,
                geometry,
            } => {
                if let Some(terminal) = self.terminal_mut(&terminal_id) {
                    terminal.geometry = geometry;
                }
                if self.host_selection.terminal_id.as_ref() == Some(&terminal_id) {
                    self.geometry = Some(geometry);
                }
                changed
            }
            Delta::Activity(event) => {
                if !self.activity.iter().any(|e| e.event_id == event.event_id) {
                    self.activity.push(event);
                    let excess = self.activity.len().saturating_sub(ACTIVITY_CAP);
                    self.activity.drain(..excess);
                }
                changed
            }
            Delta::DialogOpened(view) => {
                self.dialog = Some(view);
                changed
            }
            Delta::DialogClosed { dialog_id, .. } => {
                if self.dialog.as_ref().map(|d| &d.dialog_id) == Some(&dialog_id) {
                    self.dialog = None;
                }
                changed
            }
            Delta::Seats {
                you,
                seats,
                server_time_ms,
                you_were_preempted,
            } => {
                self.seat = Some(you);
                self.seats = seats;
                if server_time_ms > 0 {
                    self.server_time_ms = server_time_ms;
                }
                self.preempted |= you_were_preempted;
                changed
            }
            // v7 worker state. A worker always snapshots with its state, so
            // one of these without it means the mirror missed something.
            Delta::Health(health) => match self.worker.as_mut() {
                Some(worker) => {
                    worker.health = Some(health);
                    changed
                }
                None => resync(),
            },
            Delta::WorkerShell(shell) => match self.worker.as_mut() {
                Some(worker) => {
                    worker.shell = Some(shell);
                    changed
                }
                None => resync(),
            },
            Delta::WorkerStatus(status) => match self.worker.as_mut() {
                Some(worker) => {
                    worker.status = status;
                    changed
                }
                None => resync(),
            },
            Delta::Connections(connections) => match self.worker.as_mut() {
                Some(worker) => {
                    worker.connections = connections;
                    changed
                }
                None => resync(),
            },
            Delta::HostMessage(message) => match self.worker.as_mut() {
                Some(worker) => Applied {
                    changed: worker.push_message(message),
                    resync: false,
                },
                None => resync(),
            },
            Delta::Notification(notification) => match self.worker.as_mut() {
                Some(worker) => {
                    worker.notifications.push(notification);
                    let excess = worker.notifications.len().saturating_sub(NOTIFICATION_CAP);
                    worker.notifications.drain(..excess);
                    changed
                }
                None => resync(),
            },
            Delta::Unrecognized => Applied::default(),
        };
        self.repair_selection();
        applied
    }

    /// Whether a delta since the last call said another writer took the input
    /// lock from this client.
    pub fn take_preempted(&mut self) -> bool {
        std::mem::take(&mut self.preempted)
    }

    // -- a worker host (v7) -------------------------------------------------

    /// Whether the host is a FlightDeck worker. False for a v6 host.
    pub fn is_worker(&self) -> bool {
        self.host_kind == Some(HostKind::Worker)
    }

    /// The worker's shell, when the host is a worker that has one.
    pub fn worker_shell(&self) -> Option<&WorkerShellView> {
        self.worker.as_ref()?.shell.as_ref()
    }

    /// Notifications the worker raised since the last call, oldest first.
    pub fn take_notifications(&mut self) -> Vec<NotificationView> {
        self.worker
            .as_mut()
            .map(|w| std::mem::take(&mut w.notifications))
            .unwrap_or_default()
    }

    /// Host messages that arrived since the last call — a snapshot's replay of
    /// the unread ones included — oldest first.
    pub fn take_new_messages(&mut self) -> Vec<HostMessage> {
        self.worker
            .as_mut()
            .map(|w| std::mem::take(&mut w.new_messages))
            .unwrap_or_default()
    }

    /// How many sessions, across every project, need the person: waiting for
    /// input or in error.
    pub fn needs_you_count(&self) -> usize {
        self.projects
            .iter()
            .flat_map(|p| p.sessions.iter())
            .filter(|s| matches!(s.status.bucket, StatusBucket::Waiting | StatusBucket::Error))
            .count()
    }

    /// Every terminal a client streams: [`RemoteWorkspace::terminals`] plus a
    /// worker's shell. Owned, because the shell is not a [`TerminalView`] on
    /// the wire.
    pub fn streamed_terminals(&self) -> Vec<TerminalView> {
        let mut out: Vec<TerminalView> = self.terminals().cloned().collect();
        out.extend(self.worker.as_ref().and_then(WorkerMirror::shell_terminal));
        out
    }

    // -- this client's selection (R2) ---------------------------------------

    /// Look at `project`; its remembered (or first) session comes with it.
    pub fn select_project(&mut self, project: &ProjectId) -> bool {
        if self.project_index(project).is_none() {
            return false;
        }
        if self.selection.project.as_ref() != Some(project) {
            self.selection.project = Some(project.clone());
            self.selection.session = self.selection.sessions.get(project).cloned();
        }
        self.repair_selection();
        true
    }

    /// Look at `session` (and its project).
    pub fn select_session(&mut self, session: &TabId) -> bool {
        let Some(project) = self.session(session).map(|s| s.project_id.clone()) else {
            return false;
        };
        self.selection.project = Some(project);
        self.selection.session = Some(session.clone());
        self.repair_selection();
        true
    }

    /// Look at `terminal` (and its session and project).
    pub fn select_terminal(&mut self, terminal: &TerminalId) -> bool {
        let Some(session) = self.terminal(terminal).map(|t| t.session_id.clone()) else {
            return false;
        };
        self.select_session(&session);
        self.selection.terminals.insert(session, terminal.clone());
        true
    }

    /// The project this client is looking at.
    pub fn selected_project(&self) -> Option<&ProjectView> {
        let id = self.selection.project.as_ref()?;
        self.projects.iter().find(|p| &p.project_id == id)
    }

    /// The session this client is looking at.
    pub fn selected_session(&self) -> Option<&SessionView> {
        let id = self.selection.session.as_ref()?;
        self.session(id)
    }

    /// The terminal this client is looking at.
    pub fn selected_terminal(&self) -> Option<&TerminalView> {
        let session = self.selected_session()?;
        let id = self.selection.terminals.get(&session.session_id)?;
        session.terminals.iter().find(|t| &t.terminal_id == id)
    }

    /// The selected project's index in [`RemoteWorkspace::projects`].
    pub fn selected_project_index(&self) -> Option<usize> {
        let id = self.selection.project.as_ref()?;
        self.project_index(id)
    }

    /// Whether `session` is the one the *host* is looking at (R7).
    pub fn host_is_viewing(&self, session: &TabId) -> bool {
        self.host_selection.session_id.as_ref() == Some(session)
    }

    // -- lookups ------------------------------------------------------------

    pub fn session(&self, id: &TabId) -> Option<&SessionView> {
        self.projects
            .iter()
            .flat_map(|p| p.sessions.iter())
            .find(|s| &s.session_id == id)
    }

    pub fn terminal(&self, id: &TerminalId) -> Option<&TerminalView> {
        self.projects
            .iter()
            .flat_map(|p| p.sessions.iter())
            .flat_map(|s| s.terminals.iter())
            .find(|t| &t.terminal_id == id)
    }

    /// Every terminal the host streams, in snapshot order.
    pub fn terminals(&self) -> impl Iterator<Item = &TerminalView> {
        self.projects
            .iter()
            .flat_map(|p| p.sessions.iter())
            .flat_map(|s| s.terminals.iter())
    }

    /// The command inventory row named `name`, if this host has one.
    pub fn command(&self, name: &str) -> Option<&CommandView> {
        self.commands.iter().find(|c| c.run.name == name)
    }

    /// This client's own seat row.
    pub fn my_seat(&self) -> Option<&SeatInfo> {
        self.seats.iter().find(|s| s.is_you)
    }

    /// Whoever holds the input lock right now, if anyone.
    pub fn input_holder(&self) -> Option<&SeatInfo> {
        self.seats.iter().find(|s| s.holds_input)
    }

    fn project_index(&self, id: &ProjectId) -> Option<usize> {
        self.projects.iter().position(|p| &p.project_id == id)
    }

    fn session_mut(&mut self, id: &TabId) -> Option<&mut SessionView> {
        self.projects
            .iter_mut()
            .flat_map(|p| p.sessions.iter_mut())
            .find(|s| &s.session_id == id)
    }

    fn terminal_mut(&mut self, id: &TerminalId) -> Option<&mut TerminalView> {
        self.projects
            .iter_mut()
            .flat_map(|p| p.sessions.iter_mut())
            .flat_map(|s| s.terminals.iter_mut())
            .find(|t| &t.terminal_id == id)
    }

    /// Keep the selection naming things that exist: a project that closed
    /// falls back to the first project, a session that closed to its
    /// project's first session, a terminal that closed to its session's
    /// primary.
    fn repair_selection(&mut self) {
        let project_ok = self
            .selection
            .project
            .as_ref()
            .is_some_and(|id| self.project_index(id).is_some());
        if !project_ok {
            self.selection.project = self.projects.first().map(|p| p.project_id.clone());
            self.selection.session = None;
        }
        let Some(pi) = self
            .selection
            .project
            .as_ref()
            .and_then(|id| self.project_index(id))
        else {
            self.selection.session = None;
            return;
        };
        let project = &self.projects[pi];
        let session_ok = self
            .selection
            .session
            .as_ref()
            .is_some_and(|id| project.sessions.iter().any(|s| &s.session_id == id));
        if !session_ok {
            self.selection.session = project.sessions.first().map(|s| s.session_id.clone());
        }
        match &self.selection.session {
            Some(id) => {
                self.selection
                    .sessions
                    .insert(project.project_id.clone(), id.clone());
            }
            None => {
                self.selection.sessions.remove(&project.project_id);
            }
        }
        let Some(session) = self
            .selection
            .session
            .as_ref()
            .and_then(|id| project.sessions.iter().find(|s| &s.session_id == id))
        else {
            return;
        };
        let terminal_ok = self
            .selection
            .terminals
            .get(&session.session_id)
            .is_some_and(|id| session.terminals.iter().any(|t| &t.terminal_id == id));
        if !terminal_ok {
            let fallback = session
                .terminals
                .iter()
                .find(|t| t.role == TerminalRole::Primary)
                .or_else(|| session.terminals.first())
                .map(|t| t.terminal_id.clone());
            match fallback {
                Some(id) => {
                    self.selection
                        .terminals
                        .insert(session.session_id.clone(), id);
                }
                None => {
                    self.selection.terminals.remove(&session.session_id);
                }
            }
        }
    }
}

fn resync() -> Applied {
    Applied {
        changed: false,
        resync: true,
    }
}

#[cfg(test)]
mod tests;
