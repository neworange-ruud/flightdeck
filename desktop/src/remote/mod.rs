//! A remote window's model: one FlightDeck on another machine, driven from
//! here (`specs/DESKTOP_REMOTE_CONTROL_PLAN.md` §2.3, M2).
//!
//! The local window's [`crate::host::HostModel`] owns an `AppHost`; this owns a
//! [`RemoteClient`] — the core's native client of FlightDeck Web — and plays
//! the same part for the views: they draw the core's view structs
//! ([`flightdeck::web::client::views`] builds them from the mirror), and every
//! input they produce is the same [`HostEvent`] a local window produces, which
//! [`RemoteModel::dispatch`] translates into wire commands.
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
//! what this window shows. Answers to the shared dialog (D13), which the host
//! shows on its own screen too.
//!
//! ## Terminals
//!
//! Every terminal the host streams is mirrored into a core [`Terminal`] over a
//! [`flightdeck::web::client::StreamPty`], on the app's own alacritty profile,
//! sized to the host's grid (the host owns geometry, D4). So the existing
//! terminal element draws a remote terminal unchanged; when that grid is
//! larger than this window, the element pans over it rather than clipping it
//! (R17, see `crate::terminal::pan`). Its emulator never
//! answers queries — the host's already did ([`Terminal::mirror`]).
//!
//! ## A worker host
//!
//! A FlightDeck worker (`specs/WORKER_PRD.md`) has no screen of its own, so
//! this window is where its geometry, its projects and its words come from:
//!
//! - **Tabs.** The first tab is the worker's own shell ([`Tab::WorkerShell`],
//!   mirrored like any terminal, typed into by its wire id), not this
//!   machine's SSH shell ([`Tab::SshShell`]), which only other hosts get.
//! - **Geometry (W5).** The pane size the terminal element measures is sent
//!   for the terminal on screen whenever it differs from the last size sent
//!   for it; the host resizes that PTY and its grid comes back at that size.
//!   Other hosts keep D4: their grid is letterboxed or panned here.
//! - **Projects.** Open Project is the project picker ([`projects`]): scan,
//!   browse, clone.
//! - **Host messages and notifications** ([`messages`]): a message is a
//!   status-bar notice (an error also an overlay); the ones kept while no
//!   window was attached come back together as one overlay; notifications
//!   are posted on this machine, named after the window.
//! - **Takeover.** A worker serves one window; another one attaching ends
//!   this link with [`LinkEnd::TakenOver`], and Reconnect takes it back.
//!
//! The host's configuration manager ([`RemoteConfigEditor`]) is offered on
//! every host kind that forwards it (design A1): staged here, drawn with the
//! local manager's card, saved in one command.

use std::collections::HashMap;
use std::rc::Rc;
use std::time::Instant;

use flightdeck::app::commands::Selector;
use flightdeck::app::keymap::encode_paste;
use flightdeck::app::modes::InputMode;
use flightdeck::contracts::{Notification, NotificationSound, Notifier, PtySize, TabId};
use flightdeck::host::{
    GitStatusView, HostEvent, MessageView, OverlayInput, OverlayKey, OverlayView,
};
use flightdeck::terminal::session::{Terminal, TerminalKind};
use flightdeck::tui::clipboard::{check_pasted_image, PastedImage};
use flightdeck::tui::palette::FrontEndAction;
use flightdeck::view::TerminalRef;
use flightdeck::web::client::store::SavedRemote;
use flightdeck::web::client::views::{self, DialogDraft, LocalAction, RemoteAction};
use flightdeck::web::client::worker::OpenedProject;
use flightdeck::web::client::{client_label, LinkConfig};
use flightdeck::web::client::{CloneEvent, EditorEffect, ProjectPicker, RemoteConfigEditor};
use flightdeck::web::client::{LinkEnd, LinkState, RemoteClient, RemoteWorkspace};
use flightdeck::web::protocol::{
    command as names, AckOutcome, ErrorCode, Geometry, HostKind, ImagePaste, MessageLevel,
    NotificationSoundView, NotificationView, ProjectId, SeatRequest, TerminalId, TerminalRole,
    WORKER_SHELL_TERMINAL_ID,
};
use gpui::{App, AppContext, Context, Task};

use crate::host::{HOST_ACTIVE_TURN, HOST_IDLE_TURN};
use crate::terminal::cadence::Cadence;

pub mod connect;
pub mod messages;
pub mod projects;
pub mod shell;
pub mod window;

#[cfg(test)]
mod tests;

/// One mirrored terminal.
struct Mirror {
    terminal: Terminal,
    backlog: Vec<u8>,
    generation: u64,
    geometry: Geometry,
}

/// An overlay this window draws on its own (never the host's dialog, which
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

/// Which tab of the window is on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    /// The selected project: its sidebar, git strip and terminal.
    Project,
    /// This machine's `ssh` to the host ([`shell`]); hosts that are not
    /// workers.
    SshShell,
    /// A worker's own shell, mirrored like the host's terminals.
    WorkerShell,
}

/// Dials the host again for Reconnect: a new client on the window's own
/// link configuration (a scripted one in tests).
pub type Dialer = Rc<dyn Fn() -> RemoteClient>;

/// Why a worker command could not be sent.
const WORKER_NOT_SENT: &str = "Not connected to the worker — nothing was sent.";

/// Why a host command is refused on the worker shell's tab.
pub const WORKER_SHELL_COMMAND_REFUSAL: &str = "Switch to a project tab to run host commands: \
                                                the worker shell is not one of the host's \
                                                sessions.";

/// The remote window's entity. See the module docs.
pub struct RemoteModel {
    client: RemoteClient,
    /// What the window calls the host: its saved label, else its address.
    label: String,
    address: String,
    terminals: HashMap<TerminalId, Mirror>,
    mode: InputMode,
    local: Local,
    /// This window's draft answer to the host's open dialog.
    draft: Option<DialogDraft>,
    /// A one-line notice for the status bar: the host's word on the last
    /// command, or why a keystroke was refused.
    notice: Option<String>,
    /// `[ui] desktop_terminal_font_size` for remote terminals (the global
    /// config's; the host's projects' settings are the host's).
    font_size: u16,
    cadence: Cadence,
    close_requested: bool,
    /// Palette rows only the desktop performs (another window, another
    /// remote), chosen but not yet performed — [`RemoteModel::dispatch`] hands
    /// them to the app once its update is done.
    front_end: Vec<FrontEndAction>,
    /// Links for the platform to open (a host folder in VS Code), likewise
    /// opened once the update is done.
    open_urls: Vec<String>,
    /// The saved remotes, read when a per-remote setting is needed (the SSH
    /// target), so a hand edit applies without reconnecting. None in a window
    /// that was not opened from the real file.
    remotes: connect::Store,
    /// The tab on screen. On the SSH shell's ([`shell`]) the host's
    /// terminals are hidden, so nothing typed or run here goes to them; on
    /// the worker shell's, what is typed goes to the worker shell.
    tab: Tab,
    /// The host kind saved for this remote by an earlier attach, so a worker
    /// window looks like one before this attach answers.
    saved_kind: Option<HostKind>,
    /// Dials again for Reconnect. None in a window that cannot (tests that
    /// do not ask for it).
    dialer: Option<Dialer>,
    /// The size the terminal element last measured for the pane.
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
    /// this window was away (set while the link is not live).
    awaiting_replay: bool,
    /// Where a worker's notifications are posted: this machine's notifier.
    notifier: Rc<dyn Notifier>,
    /// What the program on screen copied with OSC 52 (Claude Code's own
    /// mouse selection does), for the turn to put on this machine's
    /// clipboard: the host's emulator cannot, its clipboard is not ours.
    copied: Option<String>,
    /// Every event dispatched, in order, for the tests.
    #[cfg(test)]
    pub dispatched: Vec<HostEvent>,
    _ticker: Option<Task<()>>,
}

impl RemoteModel {
    /// Wrap a client (connecting, or scripted in tests). Call
    /// [`RemoteModel::start_ticking`] to drive it; tests call
    /// [`RemoteModel::turn`] themselves.
    pub fn new(client: RemoteClient, label: String, address: String) -> RemoteModel {
        RemoteModel {
            client,
            label,
            address,
            terminals: HashMap::new(),
            mode: InputMode::Terminal,
            local: Local::None,
            draft: None,
            notice: None,
            font_size: flightdeck::contracts::UiConfig::DEFAULT_DESKTOP_TERMINAL_FONT_SIZE,
            cadence: Cadence::with_rates(HOST_ACTIVE_TURN, HOST_IDLE_TURN),
            close_requested: false,
            front_end: Vec::new(),
            open_urls: Vec::new(),
            remotes: connect::Store {
                fs: std::rc::Rc::new(flightdeck::contracts::real::RealFs),
                path: None,
            },
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
            notifier: Rc::new(flightdeck::notify::SystemNotifier),
            copied: None,
            #[cfg(test)]
            dispatched: Vec::new(),
            _ticker: None,
        }
    }

    /// The front-end palette rows chosen since the last call, in order.
    pub fn take_front_end_actions(&mut self) -> Vec<FrontEndAction> {
        std::mem::take(&mut self.front_end)
    }

    /// The links to open since the last call, in order.
    pub fn take_open_urls(&mut self) -> Vec<String> {
        std::mem::take(&mut self.open_urls)
    }

    /// Read per-remote settings from `remotes`.
    pub fn set_remotes(&mut self, remotes: connect::Store) {
        self.saved_kind = remotes
            .load()
            .get(&self.address)
            .and_then(|saved| saved.host_kind);
        self.remotes = remotes;
    }

    /// Dial with `dialer` when Reconnect is asked for.
    pub fn set_dialer(&mut self, dialer: Dialer) {
        self.dialer = Some(dialer);
    }

    /// Post a worker's notifications through `notifier` instead of the OS's.
    #[cfg(test)]
    pub fn set_notifier(&mut self, notifier: Rc<dyn Notifier>) {
        self.notifier = notifier;
    }

    /// Use this text size for the remote terminals.
    pub fn set_font_size(&mut self, size: u16) {
        self.font_size = size;
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

    pub fn address(&self) -> &str {
        &self.address
    }

    pub fn mode(&self) -> InputMode {
        self.mode
    }

    pub fn terminal_focused(&self) -> bool {
        self.mode == InputMode::Terminal && self.active_terminal().is_some()
    }

    pub fn font_size(&self) -> u16 {
        self.font_size
    }

    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    /// The tab on screen.
    pub fn tab(&self) -> Tab {
        self.tab
    }

    /// Whether the SSH shell tab is on screen.
    pub fn shell_active(&self) -> bool {
        self.tab == Tab::SshShell
    }

    /// Whether the worker shell's tab is on screen.
    pub fn worker_shell_active(&self) -> bool {
        self.tab == Tab::WorkerShell
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

    /// Show the SSH shell tab (its tab's click). The window starts the shell
    /// the first time. A worker has none: it has its own shell.
    pub fn show_shell(&mut self) {
        if self.is_worker() {
            return self.show_worker_shell();
        }
        self.tab = Tab::SshShell;
        self.local = Local::None;
    }

    /// Show the worker shell's tab (its tab's click). Its keys are the
    /// shell's: there is no APP mode on it.
    pub fn show_worker_shell(&mut self) {
        self.tab = Tab::WorkerShell;
        self.mode = InputMode::Terminal;
        self.local = Local::None;
    }

    /// The worker's shell, as the worker last described it.
    pub fn worker_shell(&self) -> Option<&flightdeck::web::protocol::WorkerShellView> {
        self.workspace().worker_shell()
    }

    /// Start the worker shell again after it exited (its Restart shell).
    pub fn restart_worker_shell(&mut self) {
        if self.client.restart_worker_shell().is_none() {
            self.local = Local::Message(WORKER_NOT_SENT.to_string());
        }
    }

    /// The version-skew banner's text, when the worker's FlightDeck is not
    /// the core this app was built with.
    pub fn skew_warning(&self) -> Option<String> {
        let ws = self.workspace();
        if !ws.is_worker() {
            return None;
        }
        messages::skew_warning(&ws.host_version, flightdeck::VERSION)
    }

    /// The project picker, while it is open.
    pub fn picker(&self) -> Option<&ProjectPicker> {
        self.picker.as_ref()
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

    /// The pane size the terminal element measured. On a worker it is sent
    /// for the terminal on screen on the next turn (W5); any other host owns
    /// its geometry (D4) and is told nothing.
    pub fn set_viewport(&mut self, size: PtySize) {
        self.viewport = Some(size);
    }

    /// Dial the host again with this window's link configuration — on a
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

    /// Where the SSH shell connects: the target saved for this remote by
    /// hand, else the host's own account at the address this window reached
    /// it on — the one "Open in VS Code" uses.
    pub fn ssh_target(&self) -> String {
        flightdeck::host::vscode::ssh_target(
            self.configured_ssh_target().as_deref(),
            &self.address,
            self.workspace().host_machine.as_ref(),
        )
    }

    /// The SSH target saved for this remote by hand, read each time so a hand
    /// edit applies without reconnecting.
    fn configured_ssh_target(&self) -> Option<String> {
        self.remotes
            .load()
            .get(&self.address)
            .and_then(|saved| saved.ssh_target.clone())
    }

    /// Whether the window should close (the table's Quit, or the user's
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

    /// The terminal on screen: this window's selected terminal, or the
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

    /// The wire id of the terminal on screen: none on the SSH shell's tab,
    /// the worker shell on its own.
    fn on_screen(&self) -> Option<TerminalId> {
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
    /// window's own.
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

    /// Spawn the turn loop on GPUI's foreground executor.
    pub fn start_ticking(&mut self, cx: &mut Context<Self>) {
        self._ticker = Some(cx.spawn(async move |this, cx| loop {
            let delay = this.update(cx, |model, _| {
                model.cadence.next_delay(Instant::now(), false)
            });
            let Ok(delay) = delay else {
                break;
            };
            cx.background_executor().timer(delay).await;
            if this.update(cx, |model, cx| model.turn(cx)).is_err() {
                break;
            }
        }));
    }

    /// One turn: apply what the link said, mirror the terminals, and redraw
    /// when anything changed. Returns whether it redrew.
    pub fn turn(&mut self, cx: &mut Context<Self>) -> bool {
        let changed = self.pump();
        if let Some(text) = self.copied.take() {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
        }
        if changed {
            self.cadence.output(Instant::now());
            cx.notify();
        }
        changed
    }

    /// [`RemoteModel::turn`] without the redraw, for tests and the loop.
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
    /// window, so two workers' agents are told apart.
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
    /// else the window's notice.
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
    /// this window looks at.
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
                let terminal = Terminal::mirror(
                    kind,
                    title,
                    Box::new(pty),
                    size_of(geometry),
                    crate::terminal::desktop_profile(),
                );
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
            changed |= crate::terminal::pump(&mut mirror.terminal, &mut mirror.backlog).changed();
            if let Some(text) = mirror.terminal.take_clipboard() {
                if on_screen.as_ref() == Some(&id) {
                    self.copied = Some(text);
                }
            }
        }
        changed
    }

    // -- input --------------------------------------------------------------

    /// Handle one input, as a local window would — see the module docs for
    /// what is local and what is sent.
    pub fn dispatch(&mut self, event: HostEvent, cx: &mut Context<Self>) {
        #[cfg(test)]
        self.dispatched.push(event.clone());
        if matches!(
            event,
            HostEvent::TerminalInput(_) | HostEvent::Paste(_) | HostEvent::PasteImage(_)
        ) {
            self.cadence.input(Instant::now());
            if self._ticker.is_some() {
                self.start_ticking(cx);
            }
        }
        self.apply(event);
        for action in self.take_front_end_actions() {
            if let Some(command) = crate::menus::AppCommand::for_front_end(action) {
                cx.defer(move |cx| command.perform(cx));
            }
        }
        for url in self.take_open_urls() {
            cx.open_url(&url);
        }
        cx.notify();
    }

    /// [`RemoteModel::dispatch`] without a GPUI context.
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
            // the terminal element reports ([`RemoteModel::set_viewport`]).
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
    /// shell takes its own keys and pastes ([`window`]).
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

    /// Close the window (the link closes with it).
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
            RemoteAction::Local(LocalAction::FrontEnd(FrontEndAction::OpenInVsCode)) => {
                self.open_in_vscode()
            }
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
                self.local = Local::Message(
                    "Switch to a project tab to run host commands: the SSH shell is not one of \
                     the host's sessions."
                        .to_string(),
                )
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
    /// Code, over SSH to the host ([`flightdeck::host::vscode`]). The SSH
    /// target is the one saved for this remote by hand, else the host's own
    /// account at the address this window reached it on.
    fn open_in_vscode(&mut self) {
        let configured = self.configured_ssh_target();
        match views::vscode_folder(self.workspace(), &self.address, configured.as_deref()) {
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
                self.open_urls.push(open.url);
            }
            None => {
                self.local =
                    Local::Message("Select a project or session to open in VS Code.".to_string())
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

impl Drop for RemoteModel {
    fn drop(&mut self) {
        self._ticker = None;
    }
}

/// Open a remote window on `remote` with `seat`: one remote per window (R4),
/// never mixed with local projects. Its first attach is recorded in the saved
/// remotes (last seen, host version, viewer id).
pub fn open_remote_window(remote: SavedRemote, seat: SeatRequest, cx: &mut App) {
    let config = LinkConfig {
        address: remote.address.clone(),
        token: remote.access_token(),
        seat,
        // A worker shows this machine's name in its device list and in
        // "taken over by …" (design A11).
        user_agent: client_label(
            flightdeck::web::server::NATIVE_CLIENT_AGENT,
            env!("CARGO_PKG_VERSION"),
        ),
        connection: flightdeck::web::protocol::ConnectionKind::Window,
        retry_cap_ms: None,
    };
    let client = RemoteClient::connect(config.clone());
    let label = remote.label.clone();
    let address = remote.address.clone();
    // The host's projects' settings are the host's; text size is this app's.
    let font_size = flightdeck::config::load::global_config_path()
        .and_then(|path| {
            flightdeck::config::load::load_config(&flightdeck::contracts::real::RealFs, &path).ok()
        })
        .map(|config| config.ui.desktop_terminal_font_size)
        .unwrap_or(flightdeck::contracts::UiConfig::DEFAULT_DESKTOP_TERMINAL_FONT_SIZE);
    let model = cx.new(|cx| {
        let mut model = RemoteModel::new(client, label.clone(), address.clone());
        model.set_font_size(font_size);
        model.set_remotes(connect::Store::real());
        model.set_dialer(Rc::new(move || RemoteClient::connect(config.clone())));
        model.start_ticking(cx);
        model
    });
    let mut recorded = false;
    cx.observe(&model, move |model, cx| {
        let ws = model.read(cx).workspace();
        if recorded || !ws.ready {
            return;
        }
        recorded = true;
        let store = connect::Store::real();
        let mut saved = store.load();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        saved.touch(
            &address,
            now,
            ws.viewer_id.as_ref().map(|v| v.to_string()),
            Some(ws.host_version.clone()),
        );
        // What the host is, so the launcher lists a worker under Workers.
        if let Some(kind) = ws.host_kind {
            saved.record_host_kind(&address, kind);
        }
        store.save(&saved);
    })
    .detach();
    // The Workers section's background link stays quiet while this window
    // is open: the window notifies instead.
    let window_address = remote.address.clone();
    flightdeck_desktop::workers::registry::opened(&window_address, cx);
    cx.observe_release(&model, move |_, cx| {
        flightdeck_desktop::workers::registry::closed(&window_address, cx)
    })
    .detach();
    let mut options = crate::app::window_options(cx);
    if let Some(titlebar) = options.titlebar.as_mut() {
        titlebar.title = Some(format!("FlightDeck — {}", remote.label).into());
    }
    let opened = cx.open_window(options, |window, cx| {
        let view = cx.new(|cx| window::RemoteWindow::new(model, cx));
        cx.new(|cx| gpui_component::Root::new(view, window, cx))
    });
    if let Err(e) = opened {
        eprintln!("flightdeck-desktop: could not open the remote window: {e}");
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
