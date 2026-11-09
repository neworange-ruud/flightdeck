//! [`RemoteSession`]: one FlightDeck on another machine, driven by a native
//! front-end (`specs/DESKTOP_REMOTE_CONTROL_PLAN.md` §2.3; `remote-control-ki05`).
//!
//! The local front-ends own an `AppHost`; a remote one owns a
//! [`RemoteClient`] — the core's native client of FlightDeck Web — and this
//! plays the same part for its views: they draw the core's view structs
//! ([`super::views`] builds them from the mirror), and every input they
//! produce is the same [`HostEvent`] a local front-end produces, which
//! [`RemoteSession::apply`] translates into wire commands.
//!
//! It started life as FlightDeck Desktop's remote window model and moved
//! here, whole, so the terminal UI's `flightdeck connect` drives a host with
//! the very same rules. What stays with each front-end is only what is
//! theirs: a run loop, a clipboard, how a terminal is drawn, and the
//! front-end's own palette rows ([`RemoteSession::take_front_end_actions`]).
//!
//! ## What stays here
//!
//! Browsing is this client's own (R2): which project, session and terminal is
//! on screen, the input mode, the palette, help and About. None of it is sent;
//! the host's own selection shows only as a marker in the sidebar (R7).
//!
//! ## What goes to the host
//!
//! Keystrokes, to the terminal on screen by its wire id. Commands, through the
//! host's palette path with an explicit target (protocol v6), so they act on
//! what this front-end shows. Answers to the shared dialog (D13), which the
//! host shows on its own screen too.
//!
//! ## Terminals
//!
//! Every terminal the host streams is mirrored into a core [`Terminal`] over a
//! [`super::StreamPty`], on the front-end's own [`TerminalProfile`], sized to
//! the host's grid (the host owns geometry, D4). So a front-end draws a remote
//! terminal with exactly what it draws a local one. Its emulator never answers
//! queries — the host's already did ([`Terminal::mirror`]).
//!
//! ## A worker host
//!
//! A FlightDeck worker (`specs/WORKER_PRD.md`) has no screen of its own, so
//! the front-end is where its geometry, its projects and its words come from:
//!
//! - **Tabs.** The worker's own shell ([`Tab::WorkerShell`]) is mirrored like
//!   any terminal and typed into by its wire id. Other hosts may instead get a
//!   shell the front-end runs itself over SSH ([`Tab::SshShell`]).
//! - **Geometry (W5).** The pane size the front-end measures
//!   ([`RemoteSession::set_viewport`]) is sent for the terminal on screen
//!   whenever it differs from the last size sent for it; the host resizes that
//!   PTY and its grid comes back at that size. Other hosts keep D4.
//! - **Projects.** Open Project is the project picker ([`ProjectPicker`]):
//!   scan, browse, clone.
//! - **Host messages and notifications** ([`super::messages`]): a message is a
//!   status-bar notice (an error also an overlay); the ones kept while nothing
//!   was attached come back together as one overlay; notifications are posted
//!   on this machine through its [`Notifier`], named after the remote.
//! - **Takeover.** A worker serves one window; another one attaching ends this
//!   link with [`LinkEnd::TakenOver`], and [`RemoteSession::reconnect`] takes it
//!   back.
//!
//! The host's configuration manager ([`RemoteConfigEditor`]) is offered on
//! every host kind that forwards it (design A1): staged here, saved in one
//! command.

use std::collections::HashMap;
use std::rc::Rc;

use crate::app::commands::Selector;
use crate::app::keymap::encode_paste;
use crate::app::modes::InputMode;
use crate::contracts::{Notification, NotificationSound, Notifier, PtySize, TabId};
use crate::host::{GitStatusView, HostEvent, MessageView, OverlayInput, OverlayKey, OverlayView};
use crate::terminal::session::{Terminal, TerminalKind, TerminalProfile};
use crate::tui::clipboard::{check_pasted_image, PastedImage};
use crate::tui::palette::FrontEndAction;
use crate::view::TerminalRef;
use crate::web::client::messages;
use crate::web::client::views::{self, DialogDraft, LocalAction, RemoteAction};
use crate::web::client::worker::OpenedProject;
use crate::web::client::{CloneEvent, EditorEffect, ProjectPicker, RemoteConfigEditor};
use crate::web::client::{LinkEnd, LinkState, RemoteClient, RemoteWorkspace};
use crate::web::protocol::{
    command as names, AckOutcome, ErrorCode, Geometry, HostKind, ImagePaste, MessageLevel,
    NotificationSoundView, NotificationView, ProjectId, SeatRequest, TerminalId, TerminalRole,
    WorkerShellView, WORKER_SHELL_TERMINAL_ID,
};

#[cfg(test)]
mod tests;

/// Parses what a mirrored terminal's stream delivered into its grid; whether
/// the screen may look different now. The core's bounded
/// [`crate::terminal::pump::pump`] unless a front-end needs another.
pub type TerminalPump = fn(&mut Terminal, &mut Vec<u8>) -> bool;

/// [`crate::terminal::pump::pump`] as a [`TerminalPump`].
pub fn default_pump(terminal: &mut Terminal, backlog: &mut Vec<u8>) -> bool {
    crate::terminal::pump::pump(terminal, backlog).changed()
}

/// Dials the host again for Reconnect: a new client on the front-end's own
/// link configuration (a scripted one in tests).
pub type Dialer = Rc<dyn Fn() -> RemoteClient>;

/// Why a worker command could not be sent.
pub const WORKER_NOT_SENT: &str = "Not connected to the worker — nothing was sent.";

/// Why a host command is refused on the worker shell's tab.
pub const WORKER_SHELL_COMMAND_REFUSAL: &str = "Switch to a project tab to run host commands: \
                                                the worker shell is not one of the host's \
                                                sessions.";

/// Why a host command is refused on the SSH shell's tab.
pub const SSH_SHELL_COMMAND_REFUSAL: &str = "Switch to a project tab to run host commands: the \
                                             SSH shell is not one of the host's sessions.";

/// One mirrored terminal.
struct Mirror {
    terminal: Terminal,
    backlog: Vec<u8>,
    generation: u64,
    geometry: Geometry,
}

/// An overlay the front-end draws on its own (never the host's dialog, which
/// is shared and comes from the mirror).
#[derive(Debug, Clone, PartialEq)]
enum Local {
    None,
    Palette {
        filter: String,
        selected: usize,
    },
    Message(String),
    Help,
    About,
    GitStatus(GitStatusView),
    /// The host's configuration manager, staged here.
    Config(Box<RemoteConfigEditor>),
}

/// Which tab of the front-end is on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    /// The selected project: its sidebar, git strip and terminal.
    Project,
    /// A shell the front-end runs itself, `ssh` to the host (FlightDeck
    /// Desktop's, on hosts that are not workers). The host's terminals are
    /// hidden on it, so nothing typed or run here goes to them.
    SshShell,
    /// A worker's own shell, mirrored like the host's terminals.
    WorkerShell,
}

/// See the module docs.
pub struct RemoteSession {
    client: RemoteClient,
    /// What the front-end calls the host: its saved label, else its address.
    label: String,
    address: String,
    terminals: HashMap<TerminalId, Mirror>,
    profile: TerminalProfile,
    pump: TerminalPump,
    mode: InputMode,
    local: Local,
    /// This front-end's draft answer to the host's open dialog.
    draft: Option<DialogDraft>,
    /// A one-line notice for the status bar: the host's word on the last
    /// command, or why a keystroke was refused.
    notice: Option<String>,
    close_requested: bool,
    /// Palette rows only the front-end performs (another window, another
    /// remote, VS Code), chosen but not yet performed.
    front_end: Vec<FrontEndAction>,
    /// The tab on screen.
    tab: Tab,
    /// The host kind saved for this remote by an earlier attach, so a worker
    /// looks like one before this attach answers.
    saved_kind: Option<HostKind>,
    /// Dials again for Reconnect. None when it cannot (tests that do not ask
    /// for it).
    dialer: Option<Dialer>,
    /// The size the front-end last measured for the terminal pane.
    viewport: Option<PtySize>,
    /// The size last sent to a worker for each terminal (W5), so a size is
    /// sent once, not every frame.
    sent_sizes: HashMap<TerminalId, PtySize>,
    /// A worker's project picker, while it is open.
    picker: Option<ProjectPicker>,
    /// A project the worker opened for the picker, to look at once it is in
    /// the workspace.
    pending_open: Option<OpenedProject>,
    /// The seq of the last `open_configuration` (a read or a save), whose ack
    /// is the configuration manager's.
    config_seq: Option<u64>,
    /// The project the configuration manager was asked for.
    config_project: Option<ProjectId>,
    /// The next snapshot's host messages are a replay of the ones kept while
    /// this front-end was away (set while the link is not live).
    awaiting_replay: bool,
    /// Where a worker's notifications are posted: this machine's notifier.
    notifier: Rc<dyn Notifier>,
    /// What the program on screen copied with OSC 52 (Claude Code's own
    /// mouse selection does), for the front-end to put on this machine's
    /// clipboard: the host's emulator cannot, its clipboard is not ours.
    copied: Option<String>,
}

impl RemoteSession {
    /// Wrap a client (connecting, or scripted in tests). Its terminals are
    /// built on `profile`; notifications go to the system notifier until
    /// [`RemoteSession::set_notifier`] says otherwise.
    pub fn new(
        client: RemoteClient,
        label: String,
        address: String,
        profile: TerminalProfile,
    ) -> RemoteSession {
        RemoteSession {
            client,
            label,
            address,
            terminals: HashMap::new(),
            profile,
            pump: default_pump,
            mode: InputMode::Terminal,
            local: Local::None,
            draft: None,
            notice: None,
            close_requested: false,
            front_end: Vec::new(),
            tab: Tab::Project,
            saved_kind: None,
            dialer: None,
            viewport: None,
            sent_sizes: HashMap::new(),
            picker: None,
            pending_open: None,
            config_seq: None,
            config_project: None,
            awaiting_replay: true,
            notifier: Rc::new(crate::notify::SystemNotifier),
            copied: None,
        }
    }

    /// Parse mirrored terminals with `pump` instead of the core's.
    pub fn set_pump(&mut self, pump: TerminalPump) {
        self.pump = pump;
    }

    /// The host kind saved for this remote, so a worker looks like one before
    /// this attach has answered.
    pub fn set_saved_kind(&mut self, kind: Option<HostKind>) {
        self.saved_kind = kind;
    }

    /// Dial with `dialer` when Reconnect is asked for.
    pub fn set_dialer(&mut self, dialer: Dialer) {
        self.dialer = Some(dialer);
    }

    /// Post a worker's notifications through `notifier`.
    pub fn set_notifier(&mut self, notifier: Rc<dyn Notifier>) {
        self.notifier = notifier;
    }

    /// The front-end palette rows chosen since the last call, in order.
    pub fn take_front_end_actions(&mut self) -> Vec<FrontEndAction> {
        std::mem::take(&mut self.front_end)
    }

    /// What the program on screen copied since the last call.
    pub fn take_copied(&mut self) -> Option<String> {
        self.copied.take()
    }

    // -- reading ------------------------------------------------------------

    pub fn workspace(&self) -> &RemoteWorkspace {
        self.client.workspace()
    }

    pub fn client(&self) -> &RemoteClient {
        &self.client
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    /// Call the host `label` from now on (a rename).
    pub fn set_label(&mut self, label: &str) {
        self.label = label.to_string();
    }

    pub fn address(&self) -> &str {
        &self.address
    }

    pub fn mode(&self) -> InputMode {
        self.mode
    }

    pub fn terminal_focused(&self) -> bool {
        self.mode == InputMode::Terminal && self.active_terminal().is_some()
    }

    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    /// Put `text` in the status bar.
    pub fn set_notice(&mut self, text: String) {
        self.notice = Some(text);
    }

    /// Show `text` as a message overlay.
    pub fn show_message_overlay(&mut self, text: String) {
        self.local = Local::Message(text);
    }

    /// The tab on screen.
    pub fn tab(&self) -> Tab {
        self.tab
    }

    /// Whether the host is a worker — or, before this attach has answered,
    /// was one the last time.
    pub fn is_worker(&self) -> bool {
        let ws = self.workspace();
        match ws.ready {
            true => ws.is_worker(),
            false => self.saved_kind == Some(HostKind::Worker),
        }
    }

    /// Show the front-end's own SSH shell tab. A worker has none: it has its
    /// own shell, which this shows instead.
    pub fn show_ssh_shell(&mut self) {
        if self.is_worker() {
            return self.show_worker_shell();
        }
        self.tab = Tab::SshShell;
        self.local = Local::None;
    }

    /// Show the worker shell's tab. Its keys are the shell's: there is no APP
    /// mode on it.
    pub fn show_worker_shell(&mut self) {
        self.tab = Tab::WorkerShell;
        self.mode = InputMode::Terminal;
        self.local = Local::None;
    }

    /// The worker's shell, as the worker last described it.
    pub fn worker_shell(&self) -> Option<&WorkerShellView> {
        self.workspace().worker_shell()
    }

    /// Start the worker shell again after it exited (its Restart shell).
    pub fn restart_worker_shell(&mut self) {
        if self.client.restart_worker_shell().is_none() {
            self.local = Local::Message(WORKER_NOT_SENT.to_string());
        }
    }

    /// The project picker, while it is open.
    pub fn picker(&self) -> Option<&ProjectPicker> {
        self.picker.as_ref()
    }

    /// The project picker, for the front-end's own field edits.
    pub fn picker_mut(&mut self) -> Option<&mut ProjectPicker> {
        self.picker.as_mut()
    }

    /// Open the project picker (Open Project on a worker): scan the projects
    /// root and list `$HOME`. Open already, it is refreshed.
    pub fn open_picker(&mut self) {
        let home = self
            .workspace()
            .worker
            .as_ref()
            .map(|w| w.home.clone())
            .unwrap_or_default();
        let mut picker = self
            .picker
            .take()
            .unwrap_or_else(|| ProjectPicker::new(home));
        let cwd = picker.cwd.clone();
        picker.scan_sent(self.client.scan_projects());
        picker.browse_sent(self.client.browse_dir(cwd.as_deref(), picker.show_hidden));
        self.picker = Some(picker);
        self.local = Local::None;
    }

    /// Close the project picker. A clone it started carries on; how it ended
    /// is said in the status bar.
    pub fn close_picker(&mut self) {
        self.picker = None;
    }

    /// List `path` in the picker (`None`: `$HOME`).
    pub fn picker_browse(&mut self, path: Option<&str>) {
        let Some(picker) = self.picker.as_mut() else {
            return;
        };
        let path = path.map(str::trim).filter(|p| !p.is_empty());
        picker.browse_sent(self.client.browse_dir(path, picker.show_hidden));
    }

    /// Up one folder from the one the picker lists.
    pub fn picker_up(&mut self) {
        let parent = self.picker.as_ref().and_then(|p| p.parent.clone());
        if let Some(parent) = parent {
            self.picker_browse(Some(&parent));
        }
    }

    /// List hidden folders too, or not, and list the same folder again.
    pub fn picker_toggle_hidden(&mut self) {
        let Some(picker) = self.picker.as_mut() else {
            return;
        };
        picker.show_hidden = !picker.show_hidden;
        let cwd = picker.cwd.clone();
        self.picker_browse(cwd.as_deref());
    }

    /// Open the repository at `path` on the worker as a project.
    pub fn picker_open(&mut self, path: &str) {
        let Some(picker) = self.picker.as_mut() else {
            return;
        };
        picker.open_sent(self.client.open_project_path(path), path);
    }

    /// Clone `url` (into `name`, if one is given) and open it.
    pub fn picker_clone(&mut self, url: &str, name: &str) {
        let Some(picker) = self.picker.as_mut() else {
            return;
        };
        let url = url.trim();
        if url.is_empty() {
            picker.error = Some("Type the URL of the repository to clone.".to_string());
            return;
        }
        picker.clone_url = url.to_string();
        picker.clone_name = name.trim().to_string();
        let name = (!picker.clone_name.is_empty()).then_some(picker.clone_name.as_str());
        let seq = self.client.clone_project(url, name);
        picker.clone_sent(seq, url);
    }

    /// The pane size the front-end measured. On a worker it is sent for the
    /// terminal on screen on the next pump (W5); any other host owns its
    /// geometry (D4) and is told nothing.
    pub fn set_viewport(&mut self, size: PtySize) {
        self.viewport = Some(size);
    }

    /// Dial the host again with the front-end's link configuration — on a
    /// worker, taking it back from the window that took it over.
    pub fn reconnect(&mut self) {
        let Some(dialer) = self.dialer.clone() else {
            return;
        };
        // The old link has ended; dropping it closes what is left of it.
        self.client = dialer();
        self.terminals.clear();
        self.sent_sizes.clear();
        self.local = Local::None;
        self.draft = None;
        self.notice = None;
        self.picker = None;
        self.pending_open = None;
        self.config_seq = None;
        self.awaiting_replay = true;
    }

    /// Whether the front-end should close (the palette's Quit, or the user's
    /// Disconnect).
    pub fn close_requested(&self) -> bool {
        self.close_requested
    }

    /// The link's state in words, for the title and status bars.
    pub fn link_label(&self) -> String {
        link_label(self.client.state())
    }

    /// Whether the link ended for good (revoked, host quit, …).
    pub fn ended(&self) -> Option<&LinkEnd> {
        match self.client.state() {
            LinkState::Ended(end) => Some(end),
            _ => None,
        }
    }

    /// The terminal on screen: this front-end's selected terminal, or the
    /// worker shell on its tab.
    pub fn active_terminal(&self) -> Option<&Terminal> {
        let id = self.on_screen()?;
        self.terminals.get(&id).map(|m| &m.terminal)
    }

    /// The terminal on screen, for view-local state (selection, scrollback).
    pub fn active_terminal_mut(&mut self) -> Option<&mut Terminal> {
        let id = self.on_screen()?;
        self.terminals.get_mut(&id).map(|m| &mut m.terminal)
    }

    /// The mirrored terminal with wire id `id`, if it is streamed.
    pub fn terminal(&self, id: &TerminalId) -> Option<&Terminal> {
        self.terminals.get(id).map(|m| &m.terminal)
    }

    /// The mirrored terminal with wire id `id`, for view-local state.
    pub fn terminal_mut(&mut self, id: &TerminalId) -> Option<&mut Terminal> {
        self.terminals.get_mut(id).map(|m| &mut m.terminal)
    }

    /// The wire id of the terminal on screen: none on the SSH shell's tab,
    /// the worker shell on its own.
    pub fn on_screen(&self) -> Option<TerminalId> {
        match self.tab {
            Tab::SshShell => None,
            Tab::WorkerShell => Some(
                self.worker_shell()
                    .map(|shell| shell.terminal_id.clone())
                    .unwrap_or_else(|| WORKER_SHELL_TERMINAL_ID.into()),
            ),
            Tab::Project => self
                .workspace()
                .selected_terminal()
                .map(|t| t.terminal_id.clone()),
        }
    }

    /// The overlay to draw: the host's shared dialog first (D13), else this
    /// front-end's own.
    pub fn overlay(&self) -> Option<OverlayView> {
        let ws = self.workspace();
        if let (Some(view), Some(draft)) = (&ws.dialog, &self.draft) {
            return Some(OverlayView::Dialog(views::dialog_overlay(view, draft)));
        }
        match &self.local {
            Local::None => None,
            Local::Palette { filter, selected } => Some(OverlayView::Palette(views::palette_view(
                ws, filter, *selected,
            ))),
            Local::Message(text) => Some(OverlayView::Message(MessageView { text: text.clone() })),
            Local::Help => ws.help.clone().map(OverlayView::Help),
            Local::About => ws.about.clone().map(OverlayView::About),
            Local::GitStatus(view) => Some(OverlayView::GitStatus(view.clone())),
            Local::Config(editor) => Some(OverlayView::Config(editor.overlay())),
        }
    }

    // -- the turn -----------------------------------------------------------

    /// Apply what the link said, mirror the terminals, and send a worker the
    /// pane size. Returns whether anything a front-end draws changed.
    pub fn pump(&mut self) -> bool {
        let mut changed = self.client.pump();
        changed |= self.track_dialog();
        changed |= self.take_answers();
        changed |= self.take_worker_news();
        changed |= self.follow_host_kind();
        changed |= self.select_opened();
        changed |= self.sync_terminals();
        self.sync_geometry();
        changed
    }

    /// A worker's words since the last turn: its host messages (a notice,
    /// an error also an overlay, a replay on attach one overlay of them all)
    /// and its notifications, posted on this machine.
    fn take_worker_news(&mut self) -> bool {
        let live = self.client.state().is_live();
        let attached = live && self.workspace().ready;
        let replay = self.awaiting_replay && attached;
        if attached {
            self.awaiting_replay = false;
        } else if !live {
            self.awaiting_replay = true;
        }
        let mut changed = false;
        let new = self.client.take_new_messages();
        if replay && !new.is_empty() {
            self.show_message(messages::away_text(&new));
            changed = true;
        } else {
            for message in new {
                self.notice = Some(messages::line(&message));
                if message.level == MessageLevel::Error {
                    self.show_message(message.text);
                }
                changed = true;
            }
        }
        for notification in self.client.take_notifications() {
            self.notifier.notify(&self.notification(notification));
        }
        changed
    }

    /// A message overlay, unless something the person is working in (the
    /// palette, the configuration manager) is up: then the notice says it.
    fn show_message(&mut self, text: String) {
        if matches!(self.local, Local::Palette { .. } | Local::Config(_)) {
            self.notice = Some(text.lines().next().unwrap_or_default().to_string());
        } else {
            self.local = Local::Message(text);
        }
    }

    /// A worker's notification as this machine posts it: named after the
    /// remote, so two workers' agents are told apart.
    fn notification(&self, view: NotificationView) -> Notification {
        Notification {
            title: format!("{}: {}", self.label, view.title),
            body: view.body,
            sound: match view.sound {
                NotificationSoundView::None => NotificationSound::None,
                NotificationSoundView::Completion => NotificationSound::Completion,
                NotificationSoundView::InputRequired => NotificationSound::InputRequired,
            },
        }
    }

    /// The tabs follow what the host turned out to be: a worker has no SSH
    /// shell tab, and anything else no worker shell.
    fn follow_host_kind(&mut self) -> bool {
        if !self.workspace().ready {
            return false;
        }
        let worker = self.workspace().is_worker();
        match self.tab {
            Tab::SshShell if worker => self.tab = Tab::Project,
            Tab::WorkerShell if !worker => self.tab = Tab::Project,
            _ => return false,
        }
        true
    }

    /// Look at the project the picker had the worker open, once it is in
    /// the workspace.
    fn select_opened(&mut self) -> bool {
        let Some(opened) = &self.pending_open else {
            return false;
        };
        let found = self
            .workspace()
            .projects
            .iter()
            .find(|p| opened.project_id.as_ref() == Some(&p.project_id) || p.root == opened.path)
            .map(|p| p.project_id.clone());
        let Some(id) = found else {
            return false;
        };
        self.pending_open = None;
        self.client.workspace_mut().select_project(&id);
        self.tab = Tab::Project;
        true
    }

    /// Send a worker the pane size for the terminal on screen, once per size
    /// per terminal (W5). Nothing goes to any other host (D4).
    fn sync_geometry(&mut self) {
        if !self.client.is_worker() {
            return;
        }
        let (Some(size), Some(id)) = (self.viewport, self.on_screen()) else {
            return;
        };
        if !self.terminals.contains_key(&id) || self.sent_sizes.get(&id) == Some(&size) {
            return;
        }
        if self.client.resize_terminal(&id, size) {
            self.sent_sizes.insert(id, size);
        }
    }

    /// Keep the draft on the host's open dialog: a new dialog starts a new
    /// draft, one that changed in place is followed, a closed one drops it.
    fn track_dialog(&mut self) -> bool {
        let open = self.workspace().dialog.clone();
        match (open, &mut self.draft) {
            (Some(view), Some(draft)) if draft.dialog_id == view.dialog_id => draft.follow(&view),
            (Some(view), _) => {
                self.draft = Some(DialogDraft::new(&view));
                true
            }
            (None, Some(_)) => {
                self.draft = None;
                true
            }
            (None, None) => false,
        }
    }

    /// What the host answered: refusals as a message (the TUI shows its own
    /// the same way), outcomes in the status bar, panels as overlays.
    fn take_answers(&mut self) -> bool {
        let mut changed = self.take_picker_answers();
        changed |= self.take_configuration();
        for ack in self.client.take_results() {
            changed = true;
            if self.picker.as_mut().is_some_and(|p| p.on_ack(&ack)) {
                continue;
            }
            if self.config_seq.is_some() && self.config_seq == Some(ack.seq) {
                self.config_seq = None;
                self.config_answered(ack.outcome, ack.detail);
                continue;
            }
            match (ack.outcome, ack.detail) {
                (AckOutcome::Rejected, Some(detail)) => self.local = Local::Message(detail),
                (_, Some(detail)) => self.notice = Some(detail),
                (_, None) => {}
            }
        }
        for refusal in self.client.take_refused_input() {
            self.notice = Some(refusal);
            changed = true;
        }
        for error in self.client.take_errors() {
            changed = true;
            if self.picker.as_mut().is_some_and(|p| p.on_error(&error)) {
                continue;
            }
            if error.seq.is_some() && error.seq == self.config_seq {
                self.config_seq = None;
                self.config_answered(AckOutcome::Rejected, Some(error.message));
                continue;
            }
            match error.code {
                ErrorCode::SeatHeld | ErrorCode::ReadOnly => self.notice = Some(error.message),
                _ => self.local = Local::Message(error.message),
            }
        }
        if let Some(view) = self.client.take_git_status() {
            self.local = Local::GitStatus(views::git_status(&view));
            changed = true;
        }
        changed |= self.close_opened_picker();
        changed
    }

    /// Feed the project picker what the worker answered: a listing, a scan,
    /// a clone's progress and end (its acks are fed with the others).
    fn take_picker_answers(&mut self) -> bool {
        let mut changed = false;
        let listing = self.client.take_dir_listing();
        let scan = self.client.take_project_scan();
        let clones = self.client.take_clone_events();
        if let Some(picker) = self.picker.as_mut() {
            if let Some(listing) = listing {
                changed |= picker.on_listing(listing);
            }
            if let Some(scan) = scan {
                changed |= picker.on_scan(scan);
            }
            for event in clones {
                changed |= picker.on_clone_event(event);
            }
        } else {
            // A clone the closed picker started: its end is a notice. A
            // failure is also the worker's own error message.
            for event in clones {
                if let CloneEvent::Result(result) = event {
                    self.notice = Some(match (result.ok, result.path, result.error) {
                        (true, Some(path), _) => format!("Cloned {} into {path}.", result.url),
                        (true, None, _) => format!("Cloned {}.", result.url),
                        (false, _, error) => error.unwrap_or_else(|| "The clone failed.".into()),
                    });
                    changed = true;
                }
            }
        }
        changed
    }

    /// When the worker opened a project for the picker (an open's ack, a
    /// clone's result), close the picker and go to that project.
    fn close_opened_picker(&mut self) -> bool {
        let Some(opened) = self.picker.as_mut().and_then(|p| p.opened.take()) else {
            return false;
        };
        let status = self.picker.as_ref().and_then(|p| p.status.clone());
        self.notice = Some(status.unwrap_or_else(|| format!("Opened {}.", opened.path)));
        self.picker = None;
        self.pending_open = Some(opened);
        true
    }

    /// The configuration manager the host sent: a first answer opens it, a
    /// later one (after a save) is its new truth.
    fn take_configuration(&mut self) -> bool {
        let Some(view) = self.client.take_configuration() else {
            return false;
        };
        match &mut self.local {
            Local::Config(editor) => editor.on_view(view),
            _ => {
                let editor = RemoteConfigEditor::new(view, self.config_project.clone());
                self.local = Local::Config(Box::new(editor));
            }
        }
        true
    }

    /// The host's word on an `open_configuration`: a refusal is the
    /// manager's status line (or a message when it is not open), anything
    /// else the front-end's notice.
    fn config_answered(&mut self, outcome: AckOutcome, detail: Option<String>) {
        if outcome != AckOutcome::Rejected {
            if detail.is_some() {
                self.notice = detail;
            }
        } else if let Local::Config(editor) = &mut self.local {
            editor.on_refused(detail.unwrap_or_else(|| "The host refused the change.".to_string()));
        } else {
            self.local =
                Local::Message(detail.unwrap_or_else(|| {
                    "The host did not open its configuration manager.".to_string()
                }));
        }
    }

    /// Ask the host for its configuration manager, resolved for the project
    /// this front-end looks at.
    fn open_configuration(&mut self) {
        let project = self
            .workspace()
            .selected_project()
            .map(|p| p.project_id.clone());
        match self.client.open_configuration(project.as_ref()) {
            Some(seq) => {
                self.config_seq = Some(seq);
                self.config_project = project;
            }
            None => {
                self.local =
                    Local::Message("Not connected to the host — nothing was sent.".to_string())
            }
        }
    }

    /// Mirror every terminal the host streams and parse what arrived.
    fn sync_terminals(&mut self) -> bool {
        // A worker's shell is streamed like any terminal.
        let wanted: Vec<(TerminalId, TerminalRole, String, Geometry)> = self
            .workspace()
            .streamed_terminals()
            .into_iter()
            .map(|t| (t.terminal_id, t.role, t.title, t.geometry))
            .collect();
        let mut changed = false;
        // Only the terminal on screen may copy: the user selected in it. The
        // SSH shell tab hides them all.
        let on_screen = self.on_screen();
        self.terminals
            .retain(|id, _| wanted.iter().any(|(w, ..)| w == id));
        for (id, role, title, geometry) in wanted {
            let generation = self.client.terminals().generation(&id);
            let stale = self
                .terminals
                .get(&id)
                .is_none_or(|m| m.generation != generation);
            if stale {
                let kind = match role {
                    TerminalRole::Primary => TerminalKind::Primary,
                    TerminalRole::Agent => TerminalKind::Agent,
                    TerminalRole::Shell => TerminalKind::Child,
                };
                let pty = self.client.pty(&id);
                let terminal =
                    Terminal::mirror(kind, title, Box::new(pty), size_of(geometry), self.profile);
                self.terminals.insert(
                    id.clone(),
                    Mirror {
                        terminal,
                        backlog: Vec::new(),
                        generation,
                        geometry,
                    },
                );
                changed = true;
            }
            let mirror = self.terminals.get_mut(&id).expect("just ensured");
            if mirror.geometry != geometry {
                mirror.geometry = geometry;
                let _ = mirror.terminal.resize(size_of(geometry));
                changed = true;
            }
            changed |= (self.pump)(&mut mirror.terminal, &mut mirror.backlog);
            if let Some(text) = mirror.terminal.take_clipboard() {
                if on_screen.as_ref() == Some(&id) {
                    self.copied = Some(text);
                }
            }
        }
        changed
    }

    // -- input --------------------------------------------------------------

    /// Handle one input, as a local front-end would — see the module docs for
    /// what is local and what is sent. Palette rows only the front-end can
    /// perform are queued for [`RemoteSession::take_front_end_actions`].
    pub fn apply(&mut self, event: HostEvent) {
        if self.tab == Tab::SshShell && Self::hidden_from_shell(&event) {
            return;
        }
        match event {
            HostEvent::TerminalInput(bytes) => self.type_bytes(bytes),
            HostEvent::Paste(text) => {
                let bracketed = self.active_terminal().is_some_and(|t| t.bracketed_paste());
                self.type_bytes(encode_paste(&text, bracketed));
            }
            HostEvent::PasteImage(image) => self.paste_image(image),
            HostEvent::Command(command) => {
                let route = views::route_command(&command).unwrap_or(RemoteAction::Unavailable(
                    "The host does not offer this command to a remote window.",
                ));
                self.run(route);
            }
            HostEvent::RunPaletteAction(action) => {
                let route = views::route_palette_for(self.workspace(), &action);
                self.run(route)
            }
            HostEvent::SwitchProject(selector) => self.step_project(selector),
            // The worker shell has no APP mode: its keys are all the shell's.
            HostEvent::FocusApp if self.tab == Tab::WorkerShell => {}
            HostEvent::FocusApp => self.mode = InputMode::App,
            HostEvent::FocusTerminal => {
                if self.active_terminal().is_some() {
                    self.mode = InputMode::Terminal;
                }
            }
            HostEvent::OpenPalette => {
                self.local = Local::Palette {
                    filter: String::new(),
                    selected: 0,
                }
            }
            HostEvent::OpenHelp => self.local = Local::Help,
            HostEvent::Overlay(input) => self.overlay_input(input),
            HostEvent::Quit => self.close_requested = true,
            // The host owns geometry (D4); a worker's is the pane's, which
            // the front-end reports ([`RemoteSession::set_viewport`]).
            HostEvent::Resize(_) => {}
            HostEvent::OpenProject(_) => {
                self.local = Local::Message(
                    "Projects open on the host: use Open Project from this window's palette."
                        .to_string(),
                )
            }
            // Mission control's inline prompt answers are a local window's.
            HostEvent::AnswerPrompt(_) => {}
        }
    }

    /// Input meant for the host's terminal on screen, which the SSH shell
    /// hides: dropped rather than sent to an agent nobody is looking at. The
    /// shell takes its own keys and pastes.
    fn hidden_from_shell(event: &HostEvent) -> bool {
        matches!(
            event,
            HostEvent::TerminalInput(_)
                | HostEvent::Paste(_)
                | HostEvent::PasteImage(_)
                | HostEvent::FocusApp
                | HostEvent::FocusTerminal
        )
    }

    /// Keystrokes for the terminal on screen. Typing returns to the live
    /// screen, as it does locally.
    fn type_bytes(&mut self, bytes: Vec<u8>) {
        let Some(id) = self.on_screen() else {
            return;
        };
        if let Some(mirror) = self.terminals.get_mut(&id) {
            mirror.terminal.clear_selection();
            mirror.terminal.scroll_to_bottom();
        }
        self.client.outbound().input(id, bytes);
    }

    /// Send a pasted image to the host, which saves it and types its path:
    /// the agent runs there and cannot read this machine's clipboard. One the
    /// host would refuse is refused here, before it costs a frame — a frame
    /// past the server's size limit would drop the link.
    fn paste_image(&mut self, image: PastedImage) {
        if self.mode != InputMode::Terminal {
            return;
        }
        let Some(id) = self.on_screen() else {
            return;
        };
        if let Err(reason) = check_pasted_image(&image) {
            self.local = Local::Message(reason);
            return;
        }
        if let Some(mirror) = self.terminals.get_mut(&id) {
            mirror.terminal.clear_selection();
            mirror.terminal.scroll_to_bottom();
        }
        let image = ImagePaste {
            format: image.format,
            data: image.bytes,
        };
        self.client.outbound().input_image(id, image);
    }

    /// Select agent row `index` and enter APP mode, as a sidebar click does.
    pub fn select_agent(&mut self, index: usize) {
        let id = self
            .workspace()
            .selected_project()
            .and_then(|p| p.sessions.get(index))
            .map(|s| s.session_id.clone());
        if let Some(id) = id {
            self.client.workspace_mut().select_session(&id);
        }
        self.mode = InputMode::App;
    }

    /// Focus one of the selected session's terminals, as a terminal-row click.
    pub fn focus_terminal(&mut self, target: TerminalRef) {
        let id = self
            .workspace()
            .selected_session()
            .and_then(|s| views::terminal_for(s, target))
            .map(|t| t.terminal_id.clone());
        if let Some(id) = id {
            self.client.workspace_mut().select_terminal(&id);
            self.mode = InputMode::Terminal;
        }
    }

    /// Look at project `index` (a project-tab click).
    pub fn select_project(&mut self, index: usize) {
        let id: Option<ProjectId> = self
            .workspace()
            .projects
            .get(index)
            .map(|p| p.project_id.clone());
        if let Some(id) = id {
            self.tab = Tab::Project;
            self.client.workspace_mut().select_project(&id);
        }
    }

    /// Ask for a seat: observe, write, or take the input lock.
    pub fn request_seat(&mut self, seat: SeatRequest) {
        self.client.request_seat(seat);
    }

    /// Close the front-end (the link closes with it).
    pub fn request_close(&mut self) {
        self.close_requested = true;
    }

    fn run(&mut self, action: RemoteAction) {
        // Stepping through sessions, terminals or projects from a shell tab
        // first goes back to the project.
        if let RemoteAction::Local(
            LocalAction::Session(_) | LocalAction::Terminal(_) | LocalAction::Project(_),
        ) = action
        {
            if self.leave_shell_tab() {
                return;
            }
        }
        match action {
            RemoteAction::Local(LocalAction::Session(selector)) => self.step_session(selector),
            RemoteAction::Local(LocalAction::Terminal(selector)) => self.step_terminal(selector),
            RemoteAction::Local(LocalAction::Project(selector)) => self.step_project(selector),
            RemoteAction::Local(LocalAction::Help) => self.local = Local::Help,
            RemoteAction::Local(LocalAction::About) => self.local = Local::About,
            RemoteAction::Local(LocalAction::FrontEnd(action)) => self.front_end.push(action),
            RemoteAction::Local(LocalAction::Configuration) => self.open_configuration(),
            RemoteAction::Local(LocalAction::ProjectPicker) => self.open_picker(),
            RemoteAction::Unavailable(why) => self.local = Local::Message(why.to_string()),
            // A worker's Open Project is its picker, however it is asked for.
            RemoteAction::Send {
                name: names::OPEN_PROJECT,
                ..
            } if self.client.is_worker() => self.open_picker(),
            RemoteAction::Send { .. } if self.tab == Tab::WorkerShell => {
                self.local = Local::Message(WORKER_SHELL_COMMAND_REFUSAL.to_string())
            }
            RemoteAction::Send { .. } if self.tab == Tab::SshShell => {
                self.local = Local::Message(SSH_SHELL_COMMAND_REFUSAL.to_string())
            }
            RemoteAction::Send { name, scope } => {
                let args = views::target(self.workspace(), scope).map(|t| t.args());
                if self.client.command(name, args).is_none() {
                    self.local =
                        Local::Message("Not connected to the host — nothing was sent.".to_string());
                }
            }
        }
    }

    /// Open the session on screen — else the project — in this machine's VS
    /// Code, over SSH to the host ([`crate::host::vscode`]). The SSH target is
    /// `configured` (one saved for this remote by hand), else the host's own
    /// account at the address this front-end reached it on. Returns the link
    /// for the platform to open.
    pub fn open_in_vscode(&mut self, configured: Option<&str>) -> Option<String> {
        match views::vscode_folder(self.workspace(), &self.address, configured) {
            Some(open) => {
                let fallback = if open.root_fallback {
                    " — the host's FlightDeck is too old to name the session's folder, so this \
                     is the project root"
                } else {
                    ""
                };
                self.notice = Some(format!(
                    "Opening {} on {} in VS Code (Remote - SSH){fallback}",
                    open.path, open.ssh_target
                ));
                Some(open.url)
            }
            None => {
                self.local =
                    Local::Message("Select a project or session to open in VS Code.".to_string());
                None
            }
        }
    }

    fn step_session(&mut self, selector: Selector) {
        let Some(project) = self.workspace().selected_project() else {
            return;
        };
        let ids: Vec<TabId> = project
            .sessions
            .iter()
            .map(|s| s.session_id.clone())
            .collect();
        let current = self
            .workspace()
            .selected_session()
            .and_then(|s| ids.iter().position(|id| id == &s.session_id));
        if let Some(next) = step(selector, current, ids.len()) {
            self.client.workspace_mut().select_session(&ids[next]);
        }
    }

    fn step_terminal(&mut self, selector: Selector) {
        let Some(session) = self.workspace().selected_session() else {
            return;
        };
        let ids: Vec<TerminalId> = session
            .terminals
            .iter()
            .map(|t| t.terminal_id.clone())
            .collect();
        // `Index(i)` is child `i`, the primary being the ring's first slot.
        let selector = match selector {
            Selector::Index(i) => Selector::Index(i + 1),
            other => other,
        };
        let current = self
            .workspace()
            .selected_terminal()
            .and_then(|t| ids.iter().position(|id| id == &t.terminal_id));
        if let Some(next) = step(selector, current, ids.len()) {
            self.client.workspace_mut().select_terminal(&ids[next]);
        }
    }

    /// Back to the project tab from a shell's. Whether it was on one.
    fn leave_shell_tab(&mut self) -> bool {
        std::mem::replace(&mut self.tab, Tab::Project) != Tab::Project
    }

    fn step_project(&mut self, selector: Selector) {
        // From a shell tab, back to the project it left.
        if self.leave_shell_tab() {
            return;
        }
        let ids: Vec<ProjectId> = self
            .workspace()
            .projects
            .iter()
            .map(|p| p.project_id.clone())
            .collect();
        let current = self.workspace().selected_project_index();
        if let Some(next) = step(selector, current, ids.len()) {
            self.client.workspace_mut().select_project(&ids[next]);
        }
    }

    fn overlay_input(&mut self, input: OverlayInput) {
        // The host's dialog takes the keys while it is up.
        let open = self.workspace().dialog.clone();
        if let (Some(view), Some(draft)) = (open, self.draft.as_mut()) {
            if let Some(reply) = views::dialog_input(&view, draft, &input) {
                if self.client.command(reply.name, Some(reply.args)).is_none() {
                    self.local = Local::Message(
                        "Not connected to the host — the answer was not sent.".to_string(),
                    );
                }
            }
            return;
        }
        if let Local::Config(editor) = &mut self.local {
            match editor.input(input) {
                EditorEffect::None => {}
                EditorEffect::Close => self.local = Local::None,
                EditorEffect::Send(request) => match self.client.save_configuration(&request) {
                    Some(seq) => self.config_seq = Some(seq),
                    None => editor.on_refused("Not connected to the host — nothing was saved."),
                },
            }
            return;
        }
        if let Local::Palette { filter, selected } = &self.local {
            let (mut filter, mut selected) = (filter.clone(), *selected);
            match input {
                OverlayInput::PaletteFilter(text) | OverlayInput::SetText(text) => {
                    filter = text;
                    selected = 0;
                }
                OverlayInput::SelectRow(row) => selected = row,
                OverlayInput::Key(OverlayKey::Down) => selected += 1,
                OverlayInput::Key(OverlayKey::Up) => selected = selected.saturating_sub(1),
                OverlayInput::Submit => {
                    let view = views::palette_view(self.workspace(), &filter, selected);
                    let action = view.entries.get(view.selected).map(|r| r.action.clone());
                    self.local = Local::None;
                    if let Some(action) = action {
                        self.run(views::route_palette_for(self.workspace(), &action));
                    }
                    return;
                }
                OverlayInput::PaletteRun(action) => {
                    self.local = Local::None;
                    self.run(views::route_palette_for(self.workspace(), &action));
                    return;
                }
                OverlayInput::Cancel => {
                    self.local = Local::None;
                    return;
                }
                _ => {}
            }
            let len = views::palette_view(self.workspace(), &filter, 0)
                .entries
                .len();
            self.local = Local::Palette {
                filter,
                selected: selected.min(len.saturating_sub(1)),
            };
            return;
        }
        // A notice, help, About or the git panel: any decisive key closes it.
        if self.local != Local::None
            && matches!(
                input,
                OverlayInput::Submit | OverlayInput::Cancel | OverlayInput::Choose(_)
            )
        {
            self.local = Local::None;
        }
    }
}

/// A host grid as a PTY size.
fn size_of(geometry: Geometry) -> PtySize {
    PtySize {
        rows: geometry.rows,
        cols: geometry.cols,
    }
}

/// Resolve a [`Selector`] over `len` items from `current`, wrapping.
fn step(selector: Selector, current: Option<usize>, len: usize) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let cur = current.unwrap_or(0);
    match selector {
        Selector::Index(i) => (i < len).then_some(i),
        Selector::Next => Some((cur + 1) % len),
        Selector::Prev => Some((cur + len - 1) % len),
    }
}

/// The link's state in words.
pub fn link_label(state: &LinkState) -> String {
    match state {
        LinkState::Connecting => "connecting…".to_string(),
        LinkState::Live {
            latency_ms: Some(ms),
        } => format!("live · {ms} ms"),
        LinkState::Live { latency_ms: None } => "live".to_string(),
        LinkState::Reconnecting {
            attempt,
            retry_in_ms,
        } => format!(
            "reconnecting · attempt {attempt} in {}",
            if *retry_in_ms < 1000 {
                format!("{retry_in_ms} ms")
            } else {
                format!("{}s", retry_in_ms / 1000)
            }
        ),
        LinkState::Ended(end) => match end {
            LinkEnd::Revoked => "access withdrawn by the host".to_string(),
            LinkEnd::Unauthorized => "not paired — the host does not know this app".to_string(),
            LinkEnd::HostQuit => "the host quit FlightDeck".to_string(),
            LinkEnd::ServerStopped => "the host stopped its web interface".to_string(),
            LinkEnd::VersionMismatch(why) => why.clone(),
            LinkEnd::TakenOver { by } => format!("taken over by {by}"),
            LinkEnd::Stopped => "disconnected".to_string(),
        },
    }
}
