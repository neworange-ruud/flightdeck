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
//! - **Uploads (v8).** Files from this machine into a folder on the worker
//!   ([`UploadPanel`]): the panel is asked over the link, and each granted
//!   file goes out over `PUT /upload/{ticket}` on the client's runtime, one
//!   at a time ([`Transfer`]), its progress and end folded back in on
//!   [`RemoteSession::pump`].
//! - **Downloads (v9).** Files from a folder on the worker into a folder on
//!   this machine ([`DownloadPanel`]): browsed with `include_files`, asked
//!   over the link, each granted file fetched over `GET /download/{ticket}`
//!   one at a time ([`DownloadTransfer`]). Uploads and downloads are the two
//!   tabs of one Transfer Files panel ([`TransferTab`]); the tab does not
//!   change while either is moving bytes.
//!
//! The host's configuration manager ([`RemoteConfigEditor`]) is offered on
//! every host kind that forwards it (design A1): staged here, saved in one
//! command.
//!
//! ## Mission control
//!
//! The main view ([`MainView`]) and Mission control's scope are this
//! front-end's own, like the rest of browsing: kept for the life of the
//! session (across reconnects), never sent, never persisted. Mission control
//! is the local window's ([`crate::view::mission`]), built from the mirror
//! ([`super::mission`]); its selection is this front-end's selected session,
//! so selecting a tile moves nothing on the host. Its moving keys
//! ([`RemoteSession::mission_move`]) are the local window's, and a tile's
//! inline prompt answer ([`HostEvent::AnswerPrompt`]) goes to the host as
//! `answer_prompt`.

use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

use crate::app::commands::{Command, Selector};
use crate::app::keymap::{encode_paste, Action};
use crate::app::modes::InputMode;
use crate::contracts::{Clock, Notification, NotificationSound, Notifier, PtySize, TabId};
use crate::host::{GitStatusView, HostEvent, MessageView, OverlayInput, OverlayKey, OverlayView};
use crate::persistence::workspace::{MainView, MissionScope};
use crate::terminal::session::{Terminal, TerminalKind, TerminalProfile};
use crate::tui::clipboard::{check_pasted_image, PastedImage};
use crate::tui::palette::{FrontEndAction, PaletteAction};
use crate::view::TerminalRef;
use crate::view::{mission_move, MissionView, PromptReply, SessionKey};
use crate::web::client::download::{self as downloads, DownloadPanel};
use crate::web::client::download_http::{DownloadEvent, DownloadTransfer};
use crate::web::client::messages;
use crate::web::client::mission::{self, ActivityWatch};
use crate::web::client::upload::{self as uploads, Resolution};
use crate::web::client::upload_http::{Transfer, TransferEvent};
use crate::web::client::views::{self, DialogDraft, LocalAction, RemoteAction};
use crate::web::client::worker::OpenedProject;
use crate::web::client::{CloneEvent, EditorEffect, ProjectPicker, RemoteConfigEditor};
use crate::web::client::{LinkEnd, LinkState, RemoteClient, RemoteWorkspace};
use crate::web::client::{LocalFile, UploadPanel};
use crate::web::protocol::{
    command as names, AckOutcome, ErrorCode, Geometry, HostKind, ImagePaste, MessageLevel,
    NotificationSoundView, NotificationView, ProjectId, SeatRequest, TerminalId, TerminalRole,
    WorkerShellView, WORKER_SHELL_TERMINAL_ID,
};

#[cfg(test)]
pub(crate) mod tests;

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

/// Why the upload panel does not open on a host that is not a worker.
pub const UPLOAD_NOT_A_WORKER: &str = "Only a FlightDeck worker takes uploads.";

/// Why the upload panel does not open on a worker too old to take files.
pub const UPLOAD_WORKER_TOO_OLD: &str =
    "This worker cannot take uploads yet: update FlightDeck on it, then reconnect.";

/// Why a granted file could not be sent from a client with no HTTP access.
pub const UPLOAD_NO_ACCESS: &str = "This connection cannot send files.";

/// Why the Download tab does not open on a worker too old to give files.
pub const DOWNLOAD_WORKER_TOO_OLD: &str =
    "This worker cannot give downloads yet: update FlightDeck on it, then reconnect.";

/// Why a granted file could not be fetched by a client with no HTTP access.
pub const DOWNLOAD_NO_ACCESS: &str = "This connection cannot fetch files.";

/// Why the Transfer Files panel stays on its tab.
pub const TRANSFER_BUSY: &str = "Wait for the transfer to finish, or cancel it, first.";

/// The Transfer Files panel's two tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferTab {
    /// Files from this machine to the worker ([`UploadPanel`], v8).
    Upload,
    /// Files from the worker to this machine ([`DownloadPanel`], v9).
    Download,
}

/// Why a host command is refused on the worker shell's tab.
pub const WORKER_SHELL_COMMAND_REFUSAL: &str = "Switch to a project tab to run host commands: \
                                                the worker shell is not one of the host's \
                                                sessions.";

/// Why New shell is refused on a worker older than protocol v10 (W35).
pub const WORKER_SHELLS_UNAVAILABLE: &str =
    "This worker has one shell. Update FlightDeck on the worker to open more.";

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
    /// The worker shell the Worker tab shows (W35), and its place in the row
    /// when it was chosen, so closing it lands on a neighbour.
    worker_shell: Option<(TerminalId, usize)>,
    /// Whether the Worker tab shows its shells side by side (Ctrl-B there).
    /// The front-end's own layout: nothing of it reaches the worker but each
    /// pane's size.
    worker_split: bool,
    /// The shells there were when this front-end asked for a new one: the
    /// first id not among them, once it appears, is selected.
    opening_shell: Option<Vec<TerminalId>>,
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
    /// This machine's clock: what Mission control's ages are measured
    /// against and what the activity watch dates changes with.
    clock: Rc<dyn Clock>,
    /// Projects or Mission control.
    main_view: MainView,
    /// What Mission control shows.
    mission_scope: MissionScope,
    /// What this front-end saw happen to each session since the host last
    /// stated its activity.
    watch: ActivityWatch,
    /// Prompts answered from here and not yet withdrawn by the host (at most
    /// [`ANSWERED_KEPT`]), so a tile's buttons go away on the click.
    answered: VecDeque<String>,
    /// The seq of each `answer_prompt` not acked yet, and the prompt it
    /// answered: a refused answer offers the prompt's buttons again.
    answer_seqs: HashMap<u64, String>,
    /// The upload panel, while it is open (v8).
    upload: Option<UploadPanel>,
    /// The file going out now, and its own events: a channel per transfer,
    /// so a cancelled one cannot speak into the next.
    transfer: Option<(Transfer, std::sync::mpsc::Receiver<TransferEvent>)>,
    /// The download tab, once opened (v9).
    download: Option<DownloadPanel>,
    /// The file coming in now, and its own events.
    download_transfer: Option<(DownloadTransfer, std::sync::mpsc::Receiver<DownloadEvent>)>,
    /// The Transfer Files panel's tab on screen; `None` while it is closed.
    transfer_tab: Option<TransferTab>,
    /// The tab the panel was last on, which it opens on again.
    last_transfer_tab: TransferTab,
}

/// How many answered prompt ids are remembered.
const ANSWERED_KEPT: usize = 64;

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
            worker_shell: None,
            worker_split: false,
            opening_shell: None,
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
            clock: Rc::new(crate::contracts::real::RealClock),
            main_view: MainView::Projects,
            mission_scope: MissionScope::default(),
            watch: ActivityWatch::new(),
            answered: VecDeque::new(),
            answer_seqs: HashMap::new(),
            upload: None,
            transfer: None,
            download: None,
            download_transfer: None,
            transfer_tab: None,
            last_transfer_tab: TransferTab::Upload,
        }
    }

    /// Read time from `clock` instead of the system's.
    pub fn set_clock(&mut self, clock: Rc<dyn Clock>) {
        self.clock = clock;
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

    /// The link itself, mutably: for a front-end's own requests and their
    /// answers (a remote window's browser tunnels).
    pub fn client_mut(&mut self) -> &mut RemoteClient {
        &mut self.client
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

    /// Every worker shell, in the order they were opened (W35).
    pub fn worker_shells(&self) -> &[WorkerShellView] {
        self.workspace().worker_shells()
    }

    /// The worker shell the Worker tab shows: the chosen one, else the
    /// first.
    pub fn worker_shell(&self) -> Option<&WorkerShellView> {
        let shells = self.worker_shells();
        self.worker_shell
            .as_ref()
            .and_then(|(id, _)| shells.iter().find(|s| &s.terminal_id == id))
            .or_else(|| shells.first())
    }

    /// Whether this worker opens and closes shells (protocol v10, W35).
    pub fn opens_worker_shells(&self) -> bool {
        self.workspace().opens_worker_shells()
    }

    /// Show worker shell `id` on the Worker tab.
    pub fn select_worker_shell(&mut self, id: &TerminalId) {
        let Some(at) = self
            .worker_shells()
            .iter()
            .position(|s| &s.terminal_id == id)
        else {
            return;
        };
        self.worker_shell = Some((id.clone(), at));
        self.show_worker_shell();
    }

    /// Ask the worker for another shell; it is shown once it arrives.
    pub fn open_worker_shell(&mut self) {
        if !self.opens_worker_shells() {
            self.notice = Some(WORKER_SHELLS_UNAVAILABLE.to_string());
            return;
        }
        let known = self
            .worker_shells()
            .iter()
            .map(|s| s.terminal_id.clone())
            .collect();
        if self.client.open_worker_shell().is_none() {
            self.local = Local::Message(WORKER_NOT_SENT.to_string());
            return;
        }
        self.opening_shell = Some(known);
        self.show_worker_shell();
    }

    /// Close worker shell `id`, or the one on screen; its neighbour is shown
    /// once the worker confirms.
    pub fn close_worker_shell(&mut self, id: Option<TerminalId>) {
        if !self.opens_worker_shells() {
            self.notice = Some(WORKER_SHELLS_UNAVAILABLE.to_string());
            return;
        }
        let Some(id) = id.or_else(|| self.worker_shell().map(|s| s.terminal_id.clone())) else {
            return;
        };
        if self.client.close_worker_shell(&id).is_none() {
            self.local = Local::Message(WORKER_NOT_SENT.to_string());
        }
    }

    /// Start the shell on screen again after it exited (its Restart).
    pub fn restart_worker_shell(&mut self) {
        let id = self
            .worker_shell()
            .map(|s| s.terminal_id.clone())
            .unwrap_or_else(|| WORKER_SHELL_TERMINAL_ID.into());
        if self.client.restart_worker_shell(&id).is_none() {
            self.local = Local::Message(WORKER_NOT_SENT.to_string());
        }
    }

    /// Whether the Worker tab shows its shells side by side.
    pub fn worker_split(&self) -> bool {
        self.worker_split
    }

    /// Show the worker's shells side by side, or one at a time.
    pub fn set_worker_split(&mut self, split: bool) {
        self.worker_split = split;
    }

    /// Size worker terminal `id`'s PTY to its pane (a split pane that is not
    /// the one on screen; that one follows [`RemoteSession::set_viewport`]).
    /// Sent once per size, as the viewport's is (W5).
    pub fn size_pane(&mut self, id: &TerminalId, size: PtySize) {
        if !self.client.is_worker()
            || !self.terminals.contains_key(id)
            || self.sent_sizes.get(id) == Some(&size)
        {
            return;
        }
        if self.client.resize_terminal(id, size) {
            self.sent_sizes.insert(id.clone(), size);
        }
    }

    /// Step through the worker's shells, as Alt-Left/Right steps through a
    /// session's terminals.
    fn step_worker_shell(&mut self, selector: Selector) {
        let ids: Vec<TerminalId> = self
            .worker_shells()
            .iter()
            .map(|s| s.terminal_id.clone())
            .collect();
        let current = self
            .worker_shell()
            .and_then(|s| ids.iter().position(|id| id == &s.terminal_id));
        if let Some(next) = step(selector, current, ids.len()) {
            self.worker_shell = Some((ids[next].clone(), next));
        }
    }

    /// Keep the Worker tab's choice on a shell that exists: a shell this
    /// front-end opened once it arrives, a neighbour of one that closed.
    fn follow_worker_shells(&mut self) -> bool {
        let ids: Vec<TerminalId> = self
            .worker_shells()
            .iter()
            .map(|s| s.terminal_id.clone())
            .collect();
        if let Some(known) = &self.opening_shell {
            if let Some((at, id)) = ids.iter().enumerate().find(|(_, id)| !known.contains(id)) {
                self.worker_shell = Some((id.clone(), at));
                self.opening_shell = None;
                return true;
            }
        }
        match &self.worker_shell {
            Some((id, _)) if ids.contains(id) => false,
            Some((_, at)) if !ids.is_empty() => {
                let at = (*at).min(ids.len() - 1);
                self.worker_shell = Some((ids[at].clone(), at));
                true
            }
            _ => false,
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

    /// Open the project picker to choose a folder rather than a project (the
    /// upload panel's Choose…): a fresh picker listing `start` — no scan, no
    /// clone. Blank, it lists `$HOME`. A folder the worker refuses is the
    /// picker's error line, and the front-end can list `$HOME` instead.
    pub fn open_folder_picker(&mut self, start: &str) {
        let home = self
            .workspace()
            .worker
            .as_ref()
            .map(|w| w.home.clone())
            .unwrap_or_default();
        let mut picker = ProjectPicker::new(home);
        let start = start.trim();
        let start = (!start.is_empty() && start != "~").then_some(start);
        picker.browse_sent(self.client.browse_dir(start, picker.show_hidden));
        self.picker = Some(picker);
        self.local = Local::None;
    }

    // -- uploads (v8) ----------------------------------------------------------

    /// The upload panel, while it is open.
    pub fn upload(&self) -> Option<&UploadPanel> {
        self.upload.as_ref()
    }

    /// The upload panel, for the front-end's own field edits (the folder).
    pub fn upload_mut(&mut self) -> Option<&mut UploadPanel> {
        self.upload.as_mut()
    }

    /// Open the Transfer Files panel on its Upload tab, aimed at the folder
    /// the person is looking at ([`uploads::default_dir`]). Open already, the
    /// tab's list stays as it is. On a host that cannot take files the
    /// notice says why and nothing opens; while a download is moving bytes
    /// the panel stays on it.
    pub fn open_upload(&mut self) {
        if !self.client.is_worker() {
            self.notice = Some(UPLOAD_NOT_A_WORKER.to_string());
            return;
        }
        if !self.client.supports_upload() {
            self.notice = Some(UPLOAD_WORKER_TOO_OLD.to_string());
            return;
        }
        if self.transfer_tab == Some(TransferTab::Download) && self.transfer_busy() {
            self.notice = Some(TRANSFER_BUSY.to_string());
            return;
        }
        if self.upload.is_none() {
            self.upload = Some(UploadPanel::new(uploads::default_dir(self.workspace())));
        }
        self.show_transfer_tab(TransferTab::Upload);
    }

    fn show_transfer_tab(&mut self, tab: TransferTab) {
        self.transfer_tab = Some(tab);
        self.last_transfer_tab = tab;
    }

    /// The Transfer Files panel's tab on screen; `None` while it is closed.
    pub fn transfer_tab(&self) -> Option<TransferTab> {
        self.transfer_tab
    }

    /// The tab the panel opens on: the one it was last on.
    pub fn last_transfer_tab(&self) -> TransferTab {
        self.last_transfer_tab
    }

    /// Open the Transfer Files panel on `tab` (`None`: the tab it was last
    /// on, Upload at first; Download only where the worker gives files).
    pub fn open_transfer(&mut self, tab: Option<TransferTab>) {
        let tab = tab.unwrap_or(self.last_transfer_tab);
        match tab {
            TransferTab::Download if self.client.supports_download() => self.open_download(),
            _ => self.open_upload(),
        }
    }

    /// Switch the open panel to `tab`. Refused (`false`) while a transfer
    /// is moving bytes or being asked for, and for Download on a worker that
    /// gives none.
    pub fn set_transfer_tab(&mut self, tab: TransferTab) -> bool {
        if self.transfer_tab.is_none() || self.transfer_tab == Some(tab) {
            return false;
        }
        if self.transfer_busy() {
            return false;
        }
        match tab {
            TransferTab::Upload => self.open_upload(),
            TransferTab::Download => self.open_download(),
        }
        self.transfer_tab == Some(tab)
    }

    /// Whether an upload or a download is being asked for or moving bytes.
    pub fn transfer_busy(&self) -> bool {
        let upload = self.upload.as_ref().is_some_and(|p| {
            p.is_uploading() || matches!(p.phase, uploads::UploadPhase::Requested { .. })
        });
        upload || self.download.as_ref().is_some_and(DownloadPanel::is_busy)
    }

    /// Close the Transfer Files panel, both tabs: a file going out or coming
    /// in is stopped (what was written stays written; a partial download is
    /// removed).
    pub fn close_transfer(&mut self) {
        self.transfer = None;
        self.upload = None;
        self.download_transfer = None;
        self.download = None;
        self.transfer_tab = None;
    }

    /// Open the upload panel with `paths` already picked (files dropped on
    /// the window).
    pub fn open_upload_with(&mut self, paths: &[std::path::PathBuf]) {
        self.open_upload();
        if self.transfer_tab == Some(TransferTab::Upload) {
            self.upload_add_paths(paths);
        }
    }

    /// Close the upload panel — the whole Transfer Files panel
    /// ([`RemoteSession::close_transfer`]), stopping a file that is going
    /// out. What was written stays written.
    pub fn close_upload(&mut self) {
        self.close_transfer();
    }

    /// Pick `paths` on this machine: each is looked at here (a file, a
    /// folder, unreadable) and the panel takes what it can.
    pub fn upload_add_paths(&mut self, paths: &[std::path::PathBuf]) {
        let Some(panel) = self.upload.as_mut() else {
            return;
        };
        panel.add(paths.iter().map(|p| uploads::inspect(p)).collect());
    }

    /// Pick files already looked at.
    pub fn upload_add_files(&mut self, files: Vec<LocalFile>) {
        if let Some(panel) = self.upload.as_mut() {
            panel.add(files.into_iter().map(uploads::Picked::File).collect());
        }
    }

    /// Upload: ask the worker for the list, `ask`ing about taken names.
    pub fn start_upload(&mut self) {
        let Some(request) = self.upload.as_mut().and_then(UploadPanel::request) else {
            return;
        };
        self.send_upload_request(request);
    }

    /// The person's answer to taken names: ask again replacing or keeping
    /// both, or go back to the list.
    pub fn resolve_upload(&mut self, resolution: Resolution) {
        let Some(request) = self.upload.as_mut().and_then(|p| p.resolve(resolution)) else {
            return;
        };
        self.send_upload_request(request);
    }

    fn send_upload_request(&mut self, request: uploads::UploadRequest) {
        let seq = self
            .client
            .begin_upload(&request.dir, &request.files, request.on_conflict);
        if let Some(panel) = self.upload.as_mut() {
            panel.requested(seq, request.on_conflict);
        }
    }

    /// Stop: the file going out is cut off (the worker drops it), the rest
    /// are not sent.
    pub fn cancel_upload(&mut self) {
        if let Some(panel) = self.upload.as_mut() {
            panel.cancel();
        }
        self.transfer = None;
    }

    /// Fold in the worker's plan, the transfer's progress and end, and start
    /// the next file. Returns whether the panel changed.
    fn pump_upload(&mut self) -> bool {
        let plan = self.client.take_upload_plan();
        let Some(panel) = self.upload.as_mut() else {
            self.transfer = None;
            return false;
        };
        let mut changed = false;
        if let Some(plan) = plan {
            changed |= panel.on_plan(plan);
        }
        if !self.client.state().is_live() {
            changed |= panel.link_lost();
        }
        let mut ended = false;
        if let Some((_, events)) = &self.transfer {
            while let Ok(event) = events.try_recv() {
                match event {
                    TransferEvent::Progress { index, sent } => {
                        changed |= panel.progress(index, sent);
                    }
                    TransferEvent::Finished { index, result } => {
                        let result = result.map(|reply| reply.path).map_err(|e| e.sentence());
                        changed |= panel.finished(index, result);
                        ended = true;
                    }
                }
            }
        }
        if ended || !panel.is_uploading() {
            self.transfer = None;
        }
        if self.transfer.is_none() {
            if let Some(job) = panel.next_job() {
                changed = true;
                match self.client.http_access() {
                    Some(access) => {
                        let (tx, rx) = std::sync::mpsc::channel();
                        self.transfer = Some((Transfer::start(access.clone(), job, tx), rx));
                    }
                    None => {
                        panel.finished(job.index, Err(UPLOAD_NO_ACCESS.to_string()));
                    }
                }
            }
        }
        changed
    }

    // -- downloads (v9) --------------------------------------------------------

    /// The Download tab, once opened.
    pub fn download(&self) -> Option<&DownloadPanel> {
        self.download.as_ref()
    }

    /// The Download tab, for the front-end's own field edits.
    pub fn download_mut(&mut self) -> Option<&mut DownloadPanel> {
        self.download.as_mut()
    }

    /// Open the Transfer Files panel on its Download tab. The first time it
    /// lists the folder the person is looking at ([`uploads::default_dir`])
    /// and saves into this machine's Downloads folder
    /// ([`downloads::default_destination`]); after that it is as it was
    /// left. On a host that cannot give files the notice says why and
    /// nothing opens; while an upload is moving bytes the panel stays on it.
    pub fn open_download(&mut self) {
        if !self.client.is_worker() {
            self.notice = Some(UPLOAD_NOT_A_WORKER.to_string());
            return;
        }
        if !self.client.supports_download() {
            self.notice = Some(DOWNLOAD_WORKER_TOO_OLD.to_string());
            return;
        }
        if self.transfer_tab == Some(TransferTab::Upload) && self.transfer_busy() {
            self.notice = Some(TRANSFER_BUSY.to_string());
            return;
        }
        if self.download.is_none() {
            let home = self
                .workspace()
                .worker
                .as_ref()
                .map(|w| w.home.clone())
                .unwrap_or_default();
            let start = uploads::default_dir(self.workspace());
            let dest = downloads::default_destination().display().to_string();
            self.download = Some(DownloadPanel::new(start.clone(), home, dest));
            self.download_browse(Some(&start));
        }
        self.show_transfer_tab(TransferTab::Download);
    }

    /// List `path` on the Download tab (`None`: the worker's `$HOME`).
    pub fn download_browse(&mut self, path: Option<&str>) {
        let Some(panel) = self.download.as_mut() else {
            return;
        };
        if !panel.is_browsing() {
            return;
        }
        let path = path.map(str::trim).filter(|p| !p.is_empty() && *p != "~");
        panel.browse_sent(self.client.browse_files(path, panel.show_hidden));
    }

    /// Up one folder from the one the Download tab lists.
    pub fn download_up(&mut self) {
        let parent = self.download.as_ref().and_then(|p| p.parent.clone());
        if let Some(parent) = parent {
            self.download_browse(Some(&parent));
        }
    }

    /// The worker's `$HOME` on the Download tab.
    pub fn download_home(&mut self) {
        self.download_browse(None);
    }

    /// List hidden files and folders too, or not, and list the same folder
    /// again (the ticks still listed stay ticked).
    pub fn download_toggle_hidden(&mut self) {
        let Some(panel) = self.download.as_mut() else {
            return;
        };
        if !panel.is_browsing() {
            return;
        }
        panel.show_hidden = !panel.show_hidden;
        let cwd = panel.cwd.clone();
        self.download_browse(Some(&cwd));
    }

    /// Tick or untick file `name` in the folder on screen.
    pub fn download_toggle(&mut self, name: &str) {
        if let Some(panel) = self.download.as_mut() {
            panel.toggle(name);
        }
    }

    /// Tick every file in the folder, or (all ticked) none.
    pub fn download_select_all(&mut self) {
        if let Some(panel) = self.download.as_mut() {
            panel.select_all();
        }
    }

    /// Save into `dest` (as typed, or chosen with the platform's chooser).
    pub fn set_download_dest(&mut self, dest: &str) {
        if let Some(panel) = self.download.as_mut() {
            if panel.dest != dest {
                panel.set_dest(dest);
            }
        }
    }

    /// Download: names already in the destination are asked about first;
    /// otherwise the worker is asked for the ticked files.
    pub fn start_download(&mut self) {
        let Some(panel) = self.download.as_mut() else {
            return;
        };
        let dest = downloads::resolve_destination(&panel.dest).filter(|d| d.is_dir());
        let folder = dest.clone();
        let exists = |name: &str| {
            folder
                .as_deref()
                .is_some_and(|d| downloads::taken_here(d, name))
        };
        let Some(request) = panel.request(dest, exists) else {
            return;
        };
        self.send_download_request(request);
    }

    /// The person's answer to names already in the destination: ask the
    /// worker, replacing them or keeping both, or go back to browsing.
    pub fn resolve_download(&mut self, resolution: Resolution) {
        let Some(panel) = self.download.as_mut() else {
            return;
        };
        let folder = match &panel.phase {
            downloads::DownloadPhase::Asking { dest, .. } => dest.clone(),
            _ => return,
        };
        let exists = |name: &str| downloads::taken_here(&folder, name);
        let Some(request) = panel.resolve(resolution, exists) else {
            return;
        };
        self.send_download_request(request);
    }

    fn send_download_request(&mut self, request: downloads::DownloadRequest) {
        let seq = self.client.begin_download(&request.dir, &request.names);
        if let Some(panel) = self.download.as_mut() {
            panel.requested(seq);
        }
    }

    /// Stop: the file coming in is dropped (its temporary file removed), the
    /// rest are not fetched. What was saved stays saved.
    pub fn cancel_download(&mut self) {
        if let Some(panel) = self.download.as_mut() {
            panel.cancel();
        }
        self.download_transfer = None;
    }

    /// After Done or Cancelled: browsing again, nothing ticked, the folder
    /// listed afresh.
    pub fn download_more(&mut self) {
        let Some(panel) = self.download.as_mut() else {
            return;
        };
        panel.start_over();
        let cwd = panel.cwd.clone();
        self.download_browse(Some(&cwd));
    }

    /// Fold in the worker's plan, each transfer's progress and end, and
    /// start the next file. Returns whether the tab changed.
    fn pump_download(&mut self) -> bool {
        let plan = self.client.take_download_plan();
        let Some(panel) = self.download.as_mut() else {
            self.download_transfer = None;
            return false;
        };
        let mut changed = false;
        if let Some(plan) = plan {
            changed |= panel.on_plan(plan);
        }
        if !self.client.state().is_live() {
            changed |= panel.link_lost();
        }
        let mut ended = false;
        if let Some((_, events)) = &self.download_transfer {
            while let Ok(event) = events.try_recv() {
                match event {
                    DownloadEvent::Progress {
                        index,
                        received,
                        total,
                    } => {
                        changed |= panel.progress(index, received, total);
                    }
                    DownloadEvent::Finished { index, result } => {
                        let result = result.map(|done| done.path).map_err(|e| e.sentence());
                        changed |= panel.finished(index, result);
                        ended = true;
                    }
                }
            }
        }
        if ended || !panel.is_downloading() {
            self.download_transfer = None;
        }
        // A file that cannot start fails at once, and the next one is tried.
        while self.download_transfer.is_none() {
            let Some(job) = panel.next_job() else {
                break;
            };
            changed = true;
            match self.client.http_access() {
                Some(access) => {
                    let (tx, rx) = std::sync::mpsc::channel();
                    self.download_transfer =
                        Some((DownloadTransfer::start(access.clone(), job, tx), rx));
                }
                None => {
                    panel.finished(job.index, Err(DOWNLOAD_NO_ACCESS.to_string()));
                }
            }
        }
        changed
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
        self.watch.reset();
        self.answered.clear();
        self.answer_seqs.clear();
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
        changed |= self.pump_upload();
        changed |= self.pump_download();
        changed |= self.take_worker_news();
        changed |= self.follow_host_kind();
        changed |= self.select_opened();
        changed |= self.follow_worker_shells();
        changed |= self.sync_terminals();
        self.sync_geometry();
        self.watch
            .observe(self.client.workspace(), self.clock.now_unix_secs());
        changed |= self.forget_withdrawn_prompts();
        changed
    }

    /// Forget the answered prompts the host no longer shows, so the list
    /// stays short and a prompt id the host reuses is offered again.
    fn forget_withdrawn_prompts(&mut self) -> bool {
        if self.answered.is_empty() {
            return false;
        }
        let ws = self.client.workspace();
        let before = self.answered.len();
        self.answered.retain(|id| {
            ws.projects
                .iter()
                .flat_map(|p| p.sessions.iter())
                .any(|s| s.prompt.as_ref().is_some_and(|p| &p.prompt_id == id))
        });
        before != self.answered.len()
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
            if self.upload.as_mut().is_some_and(|p| p.on_ack(&ack)) {
                continue;
            }
            if self.download.as_mut().is_some_and(|p| p.on_ack(&ack)) {
                continue;
            }
            if self.picker.as_mut().is_some_and(|p| p.on_ack(&ack)) {
                continue;
            }
            if self.config_seq.is_some() && self.config_seq == Some(ack.seq) {
                self.config_seq = None;
                self.config_answered(ack.outcome, ack.detail);
                continue;
            }
            if let Some(prompt_id) = self.answer_seqs.remove(&ack.seq) {
                if ack.outcome != AckOutcome::Applied {
                    self.answered.retain(|id| id != &prompt_id);
                }
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
            if self.upload.as_mut().is_some_and(|p| p.on_error(&error)) {
                continue;
            }
            if self.download.as_mut().is_some_and(|p| p.on_error(&error)) {
                continue;
            }
            if self.picker.as_mut().is_some_and(|p| p.on_error(&error)) {
                continue;
            }
            if error.seq.is_some() && error.seq == self.config_seq {
                self.config_seq = None;
                self.config_answered(AckOutcome::Rejected, Some(error.message));
                continue;
            }
            if let Some(prompt_id) = error.seq.and_then(|seq| self.answer_seqs.remove(&seq)) {
                self.answered.retain(|id| id != &prompt_id);
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
        let mut listing = self.client.take_dir_listing();
        // The Download tab's own listing (it browses with files); any other
        // is the picker's.
        if let Some(panel) = self.download.as_mut() {
            if listing
                .as_ref()
                .is_some_and(|l| panel.awaits_listing(l.seq))
            {
                changed |= listing.take().is_some_and(|l| panel.on_listing(l));
            }
        }
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
        // On the Worker tab, split view lays out the worker's shells here:
        // there is no host screen to arrange (W35).
        if self.tab == Tab::WorkerShell {
            let split = matches!(
                &event,
                HostEvent::Command(Command::ToggleSplitView)
                    | HostEvent::RunPaletteAction(PaletteAction::Dispatch(
                        Command::ToggleSplitView
                    ))
            );
            if split {
                self.worker_split = !self.worker_split;
                return;
            }
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
            HostEvent::AnswerPrompt(reply) => self.answer_prompt(&reply),
        }
    }

    // -- Mission control ----------------------------------------------------

    /// Projects or Mission control.
    pub fn main_view(&self) -> MainView {
        self.main_view
    }

    /// Mission control's scope.
    pub fn mission_scope(&self) -> &MissionScope {
        &self.mission_scope
    }

    /// Replace Mission control's scope.
    pub fn set_mission_scope(&mut self, scope: MissionScope) {
        self.mission_scope = scope;
    }

    /// Show `to`, as a local window's view switch does: entering Mission
    /// control lands on the grid (APP mode) with a tile selected when there is
    /// one, so the chords have a target from the first key; leaving it keeps
    /// the selection, so Projects opens on the very session that was selected.
    /// Either shows the host's projects (not a shell's tab).
    pub fn set_main_view(&mut self, to: MainView) {
        // From a shell's tab, the view asked for comes back on screen even
        // when it already is the main view underneath.
        self.tab = Tab::Project;
        if self.main_view == to {
            return;
        }
        self.main_view = to;
        if to == MainView::Mission {
            let view = self.mission_view();
            let on_a_tile = self
                .mission_selection()
                .is_some_and(|k| view.tile(&k).is_some());
            match view.tiles.first() {
                Some(first) if !on_a_tile => {
                    let key = first.key.clone();
                    self.select_mission_session(&key, InputMode::App);
                }
                _ => self.mode = InputMode::App,
            }
        }
    }

    /// The other view (Alt-m).
    pub fn toggle_main_view(&mut self) {
        self.set_main_view(self.main_view.toggled());
    }

    /// Mission control over the host, now.
    pub fn mission_view(&self) -> MissionView {
        let answered: Vec<String> = self.answered.iter().cloned().collect();
        mission::mission_view(
            self.workspace(),
            &self.watch,
            &self.mission_scope,
            self.clock.now_unix_secs(),
            &answered,
        )
    }

    /// The session Mission control has selected: this front-end's own.
    pub fn mission_selection(&self) -> Option<SessionKey> {
        let ws = self.workspace();
        Some(SessionKey {
            project: ws.selected_project_index()?,
            tab_id: ws.selected_session()?.session_id.0.clone(),
        })
    }

    /// Select `key` (this front-end's selection only, R2) and be in `mode`:
    /// APP keeps the grid, TERMINAL opens the focus view when the session has
    /// a terminal to focus.
    pub fn select_mission_session(&mut self, key: &SessionKey, mode: InputMode) {
        let id = TabId(key.tab_id.clone());
        if !self.client.workspace_mut().select_session(&id) {
            return;
        }
        self.tab = Tab::Project;
        match mode {
            InputMode::Terminal if self.active_terminal().is_some() => {
                self.mode = InputMode::Terminal
            }
            InputMode::Terminal => {}
            InputMode::App => self.mode = InputMode::App,
        }
    }

    /// Take over `action` when it means *moving* in Mission control
    /// ([`mission_move`], the local window's rule): returns whether it did.
    /// Every other chord is left to [`RemoteSession::apply`], and acts on the
    /// selected tile like it does on the selected row.
    pub fn mission_move(&mut self, action: &Action) -> bool {
        let view = self.mission_view();
        let current = self
            .mission_selection()
            .and_then(|k| view.tile(&k).map(|(i, _)| i));
        let Some(to) = mission_move(action, self.mode, current, view.tiles.len()) else {
            return false;
        };
        if let Some(tile) = view.tiles.get(to.tile) {
            let key = tile.key.clone();
            self.select_mission_session(&key, to.mode);
        }
        true
    }

    /// The terminal Mission control draws for `key`: the one this front-end
    /// last looked at in that session, else its agent.
    pub fn session_terminal(&self, key: &SessionKey) -> Option<&Terminal> {
        let id = self
            .workspace()
            .session_terminal(&TabId(key.tab_id.clone()))?
            .terminal_id
            .clone();
        self.terminals.get(&id).map(|m| &m.terminal)
    }

    /// Send a tile's inline answer to the host (`answer_prompt`), which checks
    /// it against the prompt it detected and types what a local click would.
    fn answer_prompt(&mut self, reply: &PromptReply) {
        if !mission::answers_prompts(self.workspace()) {
            self.local = Local::Message(
                "The host does not take inline answers. Open the session to answer it.".to_string(),
            );
            return;
        }
        if self.answered.contains(&reply.prompt_id) {
            return;
        }
        match self
            .client
            .command(names::ANSWER_PROMPT, Some(mission::answer_args(reply)))
        {
            Some(seq) => {
                self.answer_seqs.insert(seq, reply.prompt_id.clone());
                self.answered.push_back(reply.prompt_id.clone());
                while self.answered.len() > ANSWERED_KEPT {
                    self.answered.pop_front();
                }
            }
            None => {
                self.local =
                    Local::Message("Not connected to the host — the answer was not sent.".into())
            }
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
        // On the Worker tab, a session's terminal keys work its shells
        // instead (W35): Ctrl-T opens one, Ctrl-W closes the one on screen,
        // Alt-Left/Right step through them.
        if self.tab == Tab::WorkerShell {
            match action {
                RemoteAction::Local(LocalAction::Terminal(selector)) => {
                    return self.step_worker_shell(selector);
                }
                RemoteAction::Send {
                    name: names::NEW_CHILD_TERMINAL,
                    ..
                } => return self.open_worker_shell(),
                RemoteAction::Send {
                    name: names::CLOSE_CHILD_TERMINAL,
                    ..
                } => return self.close_worker_shell(None),
                _ => {}
            }
        }
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
