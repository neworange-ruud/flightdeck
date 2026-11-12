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
//! The rules below are the core's [`RemoteSession`]
//! (`flightdeck::web::client::session`), which the terminal UI's
//! `flightdeck connect` drives too; [`RemoteModel`] adds only what a GPUI
//! window has — its turn loop and cadence, the clipboard, the text size,
//! Rename Remote, Open in VS Code and the app's own palette rows.
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
//! - **Transfer files (W34 v8, downloads v9).** Transfer files… in the
//!   titlebar opens the Transfer Files panel ([`transfer`]) on the tab it was
//!   last on. Its **Upload** tab (the palette's Upload Files…, files dropped
//!   on the window) sends files from this machine into a folder on the
//!   worker, its Choose… the project picker in a choose-a-folder mode; its
//!   **Download** tab (the palette's Download Files…, on a worker that gives
//!   downloads) fetches files from a folder on the worker into a folder here
//!   ([`download`]).
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

use std::rc::Rc;
use std::time::Instant;

use flightdeck::app::modes::InputMode;
#[cfg(test)]
use flightdeck::contracts::Notifier;
use flightdeck::contracts::PtySize;
use flightdeck::host::{HostEvent, OverlayView};
use flightdeck::persistence::workspace::{MainView, MissionScope};
use flightdeck::terminal::session::Terminal;
use flightdeck::tui::palette::FrontEndAction;
use flightdeck::view::{MissionView, SessionKey, TerminalRef};
use flightdeck::web::client::session::RemoteSession;
pub use flightdeck::web::client::session::TransferTab;
#[cfg(test)]
pub use flightdeck::web::client::session::WORKER_SHELL_COMMAND_REFUSAL;
pub use flightdeck::web::client::session::{Dialer, Tab};
use flightdeck::web::client::store::SavedRemote;
use flightdeck::web::client::upload::Resolution;
use flightdeck::web::client::DownloadPanel;
use flightdeck::web::client::{client_label, LinkConfig};
use flightdeck::web::client::{LinkEnd, ProjectPicker, RemoteClient, RemoteWorkspace, UploadPanel};
use flightdeck::web::protocol::SeatRequest;
use gpui::{App, AppContext, Context, Global, Subscription, Task};

use crate::host::{HOST_ACTIVE_TURN, HOST_IDLE_TURN};
use crate::terminal::cadence::Cadence;

pub mod connect;
pub mod download;
pub mod messages;
pub mod projects;
pub mod shell;
pub mod transfer;
pub mod window;

#[cfg(test)]
mod tests;

/// The saved remote last renamed in this process (Connect to remote…), set as
/// a global so every open window on that address takes the new name at once
/// ([`RemoteModel::follow_renames`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RemoteRenamed {
    pub address: String,
    pub label: String,
}

impl Global for RemoteRenamed {}

/// Tell the open remote windows that the remote at `address` is now `label`.
pub fn announce_rename(address: &str, label: &str, cx: &mut App) {
    cx.set_global(RemoteRenamed {
        address: address.to_string(),
        label: label.to_string(),
    });
}

/// The remote window's entity: the core's [`RemoteSession`] (everything the
/// window shows and sends) plus what only a GPUI window has — its turn loop
/// and cadence, its clipboard, the text size, the saved remotes it renames,
/// and the palette rows only the app performs. See the module docs.
pub struct RemoteModel {
    session: RemoteSession,
    cadence: Cadence,
    /// `[ui] desktop_terminal_font_size` for remote terminals (the global
    /// config's; the host's projects' settings are the host's).
    font_size: u16,
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
    /// Every event dispatched, in order, for the tests.
    #[cfg(test)]
    pub dispatched: Vec<HostEvent>,
    _ticker: Option<Task<()>>,
    /// Takes a new name when the saved remote is renamed.
    _renames: Option<Subscription>,
    /// Rename Remote's name dialog is open; the window draws its field.
    renaming: bool,
    /// Why the name typed there was refused.
    rename_error: Option<String>,
    /// Upload Files was chosen: the platform's file chooser opens once the
    /// update is done ([`RemoteModel::dispatch`]).
    pick_files: bool,
    /// The project picker is open to choose the upload panel's folder.
    choosing_upload_dir: bool,
}

impl RemoteModel {
    /// Wrap a client (connecting, or scripted in tests). Call
    /// [`RemoteModel::start_ticking`] to drive it; tests call
    /// [`RemoteModel::turn`] themselves.
    pub fn new(client: RemoteClient, label: String, address: String) -> RemoteModel {
        let mut session =
            RemoteSession::new(client, label, address, crate::terminal::desktop_profile());
        session.set_pump(pump_terminal);
        RemoteModel {
            session,
            cadence: Cadence::with_rates(HOST_ACTIVE_TURN, HOST_IDLE_TURN),
            font_size: flightdeck::contracts::UiConfig::DEFAULT_DESKTOP_TERMINAL_FONT_SIZE,
            front_end: Vec::new(),
            open_urls: Vec::new(),
            remotes: connect::Store {
                fs: std::rc::Rc::new(flightdeck::contracts::real::RealFs),
                path: None,
            },
            #[cfg(test)]
            dispatched: Vec::new(),
            _ticker: None,
            _renames: None,
            renaming: false,
            rename_error: None,
            pick_files: false,
            choosing_upload_dir: false,
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
        let kind = remotes
            .load()
            .get(self.session.address())
            .and_then(|saved| saved.host_kind);
        self.session.set_saved_kind(kind);
        self.remotes = remotes;
    }

    /// Dial with `dialer` when Reconnect is asked for.
    pub fn set_dialer(&mut self, dialer: Dialer) {
        self.session.set_dialer(dialer);
    }

    /// Post a worker's notifications through `notifier` instead of the OS's.
    #[cfg(test)]
    pub fn set_notifier(&mut self, notifier: Rc<dyn Notifier>) {
        self.session.set_notifier(notifier);
    }

    /// Use this text size for the remote terminals.
    pub fn set_font_size(&mut self, size: u16) {
        self.font_size = size;
    }

    // -- reading ------------------------------------------------------------

    pub fn workspace(&self) -> &RemoteWorkspace {
        self.session.workspace()
    }

    pub fn client(&self) -> &RemoteClient {
        self.session.client()
    }

    pub fn label(&self) -> &str {
        self.session.label()
    }

    /// Take `label` as this window's name if the remote renamed is this
    /// window's. Returns whether the name changed.
    pub fn rename(&mut self, address: &str, label: &str) -> bool {
        if address != self.session.address() || label == self.session.label() {
            return false;
        }
        self.session.set_label(label);
        true
    }

    /// Whether Rename Remote's name dialog is open.
    pub fn renaming(&self) -> bool {
        self.renaming
    }

    /// Why the name typed in Rename Remote was refused, while it is open.
    pub fn rename_error(&self) -> Option<&str> {
        self.rename_error.as_deref()
    }

    /// Open Rename Remote's name dialog (the palette row).
    pub fn open_rename(&mut self) {
        self.renaming = true;
        self.rename_error = None;
    }

    pub fn cancel_rename(&mut self, cx: &mut Context<Self>) {
        self.renaming = false;
        self.rename_error = None;
        cx.notify();
    }

    /// Call this remote `typed`: saved in the remotes file, then announced so
    /// every window on it (this one included) and Connect to remote… take
    /// it. A blank name is refused and the dialog stays open. A remote no
    /// longer saved (forgotten while this window was open) is renamed in this
    /// window only.
    pub fn commit_rename(&mut self, typed: &str, cx: &mut Context<Self>) {
        let label = typed.trim();
        if label.is_empty() {
            self.rename_error = Some("Type a name for this remote.".to_string());
            cx.notify();
            return;
        }
        let address = self.session.address().to_string();
        let mut file = self.remotes.load();
        if file.rename(&address, label).is_some() {
            self.remotes.save(&file);
        }
        self.session.set_label(label);
        self.renaming = false;
        self.rename_error = None;
        announce_rename(&address, label, cx);
        cx.notify();
    }

    /// Follow renames of this remote ([`announce_rename`]) for as long as
    /// the model lives.
    pub fn follow_renames(&mut self, cx: &mut Context<Self>) {
        self._renames = Some(cx.observe_global::<RemoteRenamed>(|model, cx| {
            let renamed = cx.global::<RemoteRenamed>().clone();
            if model.rename(&renamed.address, &renamed.label) {
                cx.notify();
            }
        }));
    }

    pub fn address(&self) -> &str {
        self.session.address()
    }

    pub fn mode(&self) -> InputMode {
        self.session.mode()
    }

    pub fn terminal_focused(&self) -> bool {
        self.session.terminal_focused()
    }

    pub fn font_size(&self) -> u16 {
        self.font_size
    }

    pub fn notice(&self) -> Option<&str> {
        self.session.notice()
    }

    /// The tab on screen.
    pub fn tab(&self) -> Tab {
        self.session.tab()
    }

    /// Whether the SSH shell tab is on screen.
    pub fn shell_active(&self) -> bool {
        self.session.tab() == Tab::SshShell
    }

    /// Whether the worker shell's tab is on screen.
    pub fn worker_shell_active(&self) -> bool {
        self.session.tab() == Tab::WorkerShell
    }

    /// Whether the host is a worker — or, before this attach has answered,
    /// was one the last time.
    pub fn is_worker(&self) -> bool {
        self.session.is_worker()
    }

    /// Show the SSH shell tab (its tab's click). The window starts the shell
    /// the first time. A worker has none: it has its own shell.
    pub fn show_shell(&mut self) {
        self.session.show_ssh_shell();
    }

    /// Show the worker shell's tab (its tab's click). Its keys are the
    /// shell's: there is no APP mode on it.
    pub fn show_worker_shell(&mut self) {
        self.session.show_worker_shell();
    }

    /// The worker's shell, as the worker last described it.
    pub fn worker_shell(&self) -> Option<&flightdeck::web::protocol::WorkerShellView> {
        self.session.worker_shell()
    }

    /// Start the worker shell again after it exited (its Restart shell).
    pub fn restart_worker_shell(&mut self) {
        self.session.restart_worker_shell();
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
        self.session.picker()
    }

    /// Open the project picker (Open Project on a worker): scan the projects
    /// root and list `$HOME`. Open already, it is refreshed.
    pub fn open_picker(&mut self) {
        self.choosing_upload_dir = false;
        self.session.open_picker();
    }

    /// Close the project picker. A clone it started carries on; how it ended
    /// is said in the status bar.
    pub fn close_picker(&mut self) {
        self.choosing_upload_dir = false;
        self.session.close_picker();
    }

    /// List `path` in the picker (`None`: `$HOME`).
    pub fn picker_browse(&mut self, path: Option<&str>) {
        self.session.picker_browse(path);
    }

    /// Up one folder from the one the picker lists.
    pub fn picker_up(&mut self) {
        self.session.picker_up();
    }

    /// List hidden folders too, or not, and list the same folder again.
    pub fn picker_toggle_hidden(&mut self) {
        self.session.picker_toggle_hidden();
    }

    /// Open the repository at `path` on the worker as a project.
    pub fn picker_open(&mut self, path: &str) {
        self.session.picker_open(path);
    }

    /// Clone `url` (into `name`, if one is given) and open it.
    pub fn picker_clone(&mut self, url: &str, name: &str) {
        self.session.picker_clone(url, name);
    }

    // -- uploads (W34, v8) ---------------------------------------------------

    /// Whether this worker takes uploads: the entry points exist only then.
    pub fn supports_upload(&self) -> bool {
        self.session.client().supports_upload()
    }

    /// The upload panel, while it is open.
    pub fn upload(&self) -> Option<&UploadPanel> {
        self.session.upload()
    }

    /// Open the upload panel with `paths` picked — or, open already and
    /// still choosing, add them to its list. Folders are left out with the
    /// panel's note.
    pub fn open_upload_with(&mut self, paths: &[std::path::PathBuf]) {
        self.session.open_upload_with(paths);
    }

    /// Ask the platform for files to upload, then open the panel with them
    /// (or add them to it). A cancelled chooser opens nothing.
    pub fn pick_upload_files(&mut self, cx: &mut Context<Self>) {
        if !self.supports_upload() {
            // Says why in the status bar, and opens nothing.
            self.session.open_upload();
            cx.notify();
            return;
        }
        let chosen = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Upload".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = chosen.await else {
                return;
            };
            if paths.is_empty() {
                return;
            }
            this.update(cx, |model, cx| {
                model.open_upload_with(&paths);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Take file `index` out of the list.
    pub fn remove_upload_file(&mut self, index: usize) {
        if let Some(panel) = self.session.upload_mut() {
            panel.remove(index);
        }
    }

    /// Upload into `dir` (typed in the panel's folder field).
    pub fn set_upload_dir(&mut self, dir: &str) {
        if let Some(panel) = self.session.upload_mut() {
            if panel.dir != dir {
                panel.set_dir(dir);
            }
        }
    }

    /// Upload: ask the worker for the list.
    pub fn start_upload(&mut self) {
        self.session.start_upload();
    }

    /// The person's answer to names already taken.
    pub fn resolve_upload(&mut self, resolution: Resolution) {
        self.session.resolve_upload(resolution);
    }

    /// Stop the upload; what was written stays written.
    pub fn cancel_upload(&mut self) {
        self.session.cancel_upload();
    }

    /// After Done or Cancelled: an empty list, the same folder.
    pub fn upload_more(&mut self) {
        if let Some(panel) = self.session.upload_mut() {
            panel.start_over();
        }
    }

    /// Close the Transfer Files panel, both tabs (and a folder choice its
    /// Upload tab had open). A file moving is stopped.
    pub fn close_upload(&mut self) {
        if self.choosing_upload_dir {
            self.close_picker();
        }
        self.session.close_transfer();
    }

    /// The panel's Choose…: the project picker, listing the panel's folder,
    /// in its choose-a-folder mode.
    pub fn choose_upload_dir(&mut self) {
        let Some(dir) = self
            .upload()
            .filter(|p| p.is_choosing())
            .map(|p| p.dir.clone())
        else {
            return;
        };
        self.session.open_folder_picker(&dir);
        self.choosing_upload_dir = true;
    }

    /// Whether the project picker is open to choose the upload's folder.
    pub fn choosing_upload_dir(&self) -> bool {
        self.choosing_upload_dir && self.picker().is_some() && self.upload().is_some()
    }

    /// Upload into `path` (the picker's Choose this folder), and go back to
    /// the panel. The picker's Back ([`RemoteModel::close_picker`]) goes back
    /// without choosing.
    pub fn upload_dir_chosen(&mut self, path: &str) {
        self.set_upload_dir(path);
        self.close_picker();
    }

    // -- the Transfer Files panel and its Download tab (v9) -------------------

    /// Whether this worker gives downloads: the Download tab exists only then.
    pub fn supports_download(&self) -> bool {
        self.session.client().supports_download()
    }

    /// The Transfer Files panel's tab on screen; `None` while it is closed.
    pub fn transfer_tab(&self) -> Option<TransferTab> {
        self.session.transfer_tab()
    }

    /// Open the Transfer Files panel on `tab` (`None`: the one it was last
    /// on) — the titlebar's Transfer files….
    pub fn open_transfer(&mut self, tab: Option<TransferTab>) {
        self.session.open_transfer(tab);
    }

    /// Switch the open panel to `tab`; refused while a transfer is under way.
    pub fn set_transfer_tab(&mut self, tab: TransferTab) -> bool {
        self.session.set_transfer_tab(tab)
    }

    /// Whether an upload or a download is being asked for or moving bytes.
    pub fn transfer_busy(&self) -> bool {
        self.session.transfer_busy()
    }

    /// The Download tab, once opened.
    pub fn download(&self) -> Option<&DownloadPanel> {
        self.session.download()
    }

    /// List `path` on the worker (`None`: its `$HOME`).
    pub fn download_browse(&mut self, path: Option<&str>) {
        self.session.download_browse(path);
    }

    /// Up one folder.
    pub fn download_up(&mut self) {
        self.session.download_up();
    }

    /// The worker's `$HOME`.
    pub fn download_home(&mut self) {
        self.session.download_home();
    }

    /// List hidden files and folders too, or not.
    pub fn download_toggle_hidden(&mut self) {
        self.session.download_toggle_hidden();
    }

    /// Tick or untick file `name`.
    pub fn download_toggle(&mut self, name: &str) {
        self.session.download_toggle(name);
    }

    /// Tick every file, or (all ticked) none.
    pub fn download_select_all(&mut self) {
        self.session.download_select_all();
    }

    /// Save into `dest` (typed in the tab's Save to field).
    pub fn set_download_dest(&mut self, dest: &str) {
        self.session.set_download_dest(dest);
    }

    /// The Save to field's Choose…: the platform's folder chooser, which
    /// sets the destination. A cancelled chooser changes nothing.
    pub fn choose_download_dest(&mut self, cx: &mut Context<Self>) {
        if !self.download().is_some_and(DownloadPanel::is_browsing) {
            return;
        }
        let chosen = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Save here".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = chosen.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            this.update(cx, |model, cx| {
                model.set_download_dest(&path.display().to_string());
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Download: ask about names already here, else ask the worker.
    pub fn start_download(&mut self) {
        self.session.start_download();
    }

    /// The person's answer to names already in the destination.
    pub fn resolve_download(&mut self, resolution: Resolution) {
        self.session.resolve_download(resolution);
    }

    /// Stop the download; what was saved stays saved.
    pub fn cancel_download(&mut self) {
        self.session.cancel_download();
    }

    /// After Done or Cancelled: browse again, nothing ticked.
    pub fn download_more(&mut self) {
        self.session.download_more();
    }

    /// Where the last download landed, to show in the platform's file
    /// manager: the first file saved (selected there), else the folder.
    pub fn downloaded_path(&self) -> Option<std::path::PathBuf> {
        use flightdeck::web::client::download::DownloadPhase;
        match &self.download()?.phase {
            DownloadPhase::Done { saved, dest, .. } => Some(
                saved
                    .first()
                    .map(|(_, path)| path.clone())
                    .unwrap_or_else(|| dest.clone()),
            ),
            _ => None,
        }
    }

    /// Show in folder: the platform's file manager (Finder, Explorer, the
    /// desktop's own) on what the download saved.
    pub fn reveal_download(&self, cx: &mut Context<Self>) {
        if let Some(path) = self.downloaded_path() {
            cx.reveal_path(&path);
        }
    }

    /// The pane size the terminal element measured. On a worker it is sent
    /// for the terminal on screen on the next turn (W5); any other host owns
    /// its geometry (D4) and is told nothing.
    pub fn set_viewport(&mut self, size: PtySize) {
        self.session.set_viewport(size);
    }

    /// Dial the host again with this window's link configuration — on a
    /// worker, taking it back from the window that took it over.
    pub fn reconnect(&mut self) {
        self.session.reconnect();
    }

    /// Where the SSH shell connects: the target saved for this remote by
    /// hand, else the host's own account at the address this window reached
    /// it on — the one "Open in VS Code" uses.
    pub fn ssh_target(&self) -> String {
        flightdeck::host::vscode::ssh_target(
            self.configured_ssh_target().as_deref(),
            self.session.address(),
            self.workspace().host_machine.as_ref(),
        )
    }

    /// The SSH target saved for this remote by hand, read each time so a hand
    /// edit applies without reconnecting.
    fn configured_ssh_target(&self) -> Option<String> {
        self.remotes
            .load()
            .get(self.session.address())
            .and_then(|saved| saved.ssh_target.clone())
    }

    /// Whether the window should close (the table's Quit, or the user's
    /// Disconnect).
    pub fn close_requested(&self) -> bool {
        self.session.close_requested()
    }

    /// The link's state in words, for the title and status bars.
    pub fn link_label(&self) -> String {
        self.session.link_label()
    }

    /// Whether the link ended for good (revoked, host quit, …).
    pub fn ended(&self) -> Option<&LinkEnd> {
        self.session.ended()
    }

    /// The terminal on screen: this window's selected terminal, or the
    /// worker shell on its tab.
    pub fn active_terminal(&self) -> Option<&Terminal> {
        self.session.active_terminal()
    }

    /// The terminal on screen, for view-local state (selection, scrollback).
    pub fn active_terminal_mut(&mut self) -> Option<&mut Terminal> {
        self.session.active_terminal_mut()
    }

    /// The overlay to draw: the host's shared dialog first (D13), else this
    /// window's own.
    pub fn overlay(&self) -> Option<OverlayView> {
        self.session.overlay()
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
        if let Some(text) = self.session.take_copied() {
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
        let changed = self.session.pump();
        // A picker the session closed itself (a reconnect) ends a folder
        // choice with it, so the next picker is the project picker again.
        if self.session.picker().is_none() {
            self.choosing_upload_dir = false;
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
        if std::mem::take(&mut self.pick_files) {
            self.pick_upload_files(cx);
        }
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

    /// [`RemoteModel::dispatch`] without a GPUI context. The palette rows
    /// only this window answers (Open in VS Code, Rename Remote, Upload
    /// Files — whose file chooser `dispatch` then opens — and Download
    /// Files) are answered here; the app's own are kept for
    /// [`RemoteModel::take_front_end_actions`].
    pub fn apply(&mut self, event: HostEvent) {
        self.session.apply(event);
        for action in self.session.take_front_end_actions() {
            match action {
                FrontEndAction::OpenInVsCode => {
                    let configured = self.configured_ssh_target();
                    if let Some(url) = self.session.open_in_vscode(configured.as_deref()) {
                        self.open_urls.push(url);
                    }
                }
                FrontEndAction::RenameRemote => self.open_rename(),
                FrontEndAction::UploadFiles => self.pick_files = true,
                FrontEndAction::DownloadFiles => {
                    self.session.open_transfer(Some(TransferTab::Download))
                }
                action => self.front_end.push(action),
            }
        }
    }

    /// Select agent row `index` and enter APP mode, as a sidebar click does.
    pub fn select_agent(&mut self, index: usize) {
        self.session.select_agent(index);
    }

    /// Focus one of the selected session's terminals, as a terminal-row click.
    pub fn focus_terminal(&mut self, target: TerminalRef) {
        self.session.focus_terminal(target);
    }

    /// Look at project `index` (a project-tab click).
    pub fn select_project(&mut self, index: usize) {
        self.session.select_project(index);
    }

    /// Ask for a seat: observe, write, or take the input lock.
    pub fn request_seat(&mut self, seat: SeatRequest) {
        self.session.request_seat(seat);
    }

    // -- Mission control ----------------------------------------------------

    /// Projects or Mission control (this window's own, for its lifetime).
    pub fn main_view(&self) -> MainView {
        self.session.main_view()
    }

    /// Show `to`: Mission control lands on the grid with a tile selected.
    pub fn set_main_view(&mut self, to: MainView) {
        self.session.set_main_view(to);
    }

    /// Mission control's scope (this window's own, for its lifetime).
    pub fn mission_scope(&self) -> &MissionScope {
        self.session.mission_scope()
    }

    /// Replace Mission control's scope.
    pub fn set_mission_scope(&mut self, scope: MissionScope) {
        self.session.set_mission_scope(scope);
    }

    /// Mission control over the host, now.
    pub fn mission_view(&self) -> MissionView {
        self.session.mission_view()
    }

    /// The session Mission control has selected: this window's own.
    pub fn mission_selection(&self) -> Option<SessionKey> {
        self.session.mission_selection()
    }

    /// Select `key` in this window and be in `mode`.
    pub fn select_mission_session(&mut self, key: &SessionKey, mode: InputMode) {
        self.session.select_mission_session(key, mode);
    }

    /// Take over a chord that means moving in Mission control.
    pub fn mission_move(&mut self, action: &flightdeck::app::keymap::Action) -> bool {
        self.session.mission_move(action)
    }

    /// The terminal a Mission control tile for `key` draws.
    pub fn session_terminal(&self, key: &SessionKey) -> Option<&Terminal> {
        self.session.session_terminal(key)
    }

    /// Close the window (the link closes with it).
    pub fn request_close(&mut self) {
        self.session.request_close();
    }
}

impl Drop for RemoteModel {
    fn drop(&mut self) {
        self._ticker = None;
    }
}

/// The desktop's terminal pump for the mirrored terminals.
fn pump_terminal(terminal: &mut Terminal, backlog: &mut Vec<u8>) -> bool {
    crate::terminal::pump(terminal, backlog).changed()
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
        model.follow_renames(cx);
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
        titlebar.title = Some(window::window_title(&remote.label).into());
    }
    let opened = cx.open_window(options, |window, cx| {
        let view = cx.new(|cx| window::RemoteWindow::new(model, cx));
        cx.new(|cx| gpui_component::Root::new(view, window, cx))
    });
    if let Err(e) = opened {
        eprintln!("flightdeck-desktop: could not open the remote window: {e}");
    }
}
