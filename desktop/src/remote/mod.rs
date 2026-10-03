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

use std::collections::HashMap;
use std::time::Instant;

use flightdeck::app::commands::Selector;
use flightdeck::app::keymap::encode_paste;
use flightdeck::app::modes::InputMode;
use flightdeck::contracts::{PtySize, TabId};
use flightdeck::host::{
    GitStatusView, HostEvent, MessageView, OverlayInput, OverlayKey, OverlayView,
};
use flightdeck::terminal::session::{Terminal, TerminalKind};
use flightdeck::tui::clipboard::{check_pasted_image, PastedImage};
use flightdeck::tui::palette::FrontEndAction;
use flightdeck::view::TerminalRef;
use flightdeck::web::client::store::SavedRemote;
use flightdeck::web::client::views::{self, DialogDraft, LocalAction, RemoteAction};
use flightdeck::web::client::{user_agent, LinkConfig};
use flightdeck::web::client::{LinkEnd, LinkState, RemoteClient, RemoteWorkspace};
use flightdeck::web::protocol::{
    AckOutcome, ErrorCode, Geometry, ImagePaste, ProjectId, SeatRequest, TerminalId, TerminalRole,
};
use gpui::{App, AppContext, Context, Task};

use crate::host::{HOST_ACTIVE_TURN, HOST_IDLE_TURN};
use crate::terminal::cadence::Cadence;

pub mod connect;
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
    Palette { filter: String, selected: usize },
    Message(String),
    Help,
    About,
    GitStatus(GitStatusView),
}

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
            #[cfg(test)]
            dispatched: Vec::new(),
            _ticker: None,
        }
    }

    /// The front-end palette rows chosen since the last call, in order.
    pub fn take_front_end_actions(&mut self) -> Vec<FrontEndAction> {
        std::mem::take(&mut self.front_end)
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

    /// The terminal on screen: this window's selected terminal.
    pub fn active_terminal(&self) -> Option<&Terminal> {
        let id = &self.workspace().selected_terminal()?.terminal_id;
        self.terminals.get(id).map(|m| &m.terminal)
    }

    /// The terminal on screen, for view-local state (selection, scrollback).
    pub fn active_terminal_mut(&mut self) -> Option<&mut Terminal> {
        let id = self.workspace().selected_terminal()?.terminal_id.clone();
        self.terminals.get_mut(&id).map(|m| &mut m.terminal)
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
        changed |= self.sync_terminals();
        changed
    }

    /// Keep the draft on the host's open dialog: a new dialog starts a new
    /// draft, a closed one drops it.
    fn track_dialog(&mut self) -> bool {
        let open = self.workspace().dialog.clone();
        match (open, &self.draft) {
            (Some(view), Some(draft)) if draft.dialog_id == view.dialog_id => false,
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
        let mut changed = false;
        for ack in self.client.take_results() {
            changed = true;
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
            match error.code {
                ErrorCode::SeatHeld | ErrorCode::ReadOnly => self.notice = Some(error.message),
                _ => self.local = Local::Message(error.message),
            }
        }
        if let Some(view) = self.client.take_git_status() {
            self.local = Local::GitStatus(views::git_status(&view));
            changed = true;
        }
        changed
    }

    /// Mirror every terminal the host streams and parse what arrived.
    fn sync_terminals(&mut self) -> bool {
        let wanted: Vec<(TerminalId, TerminalRole, String, Geometry)> = self
            .workspace()
            .terminals()
            .map(|t| (t.terminal_id.clone(), t.role, t.title.clone(), t.geometry))
            .collect();
        let mut changed = false;
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
            let command = crate::menus::AppCommand::for_front_end(action);
            cx.defer(move |cx| command.perform(cx));
        }
        cx.notify();
    }

    /// [`RemoteModel::dispatch`] without a GPUI context.
    pub fn apply(&mut self, event: HostEvent) {
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
            HostEvent::RunPaletteAction(action) => self.run(views::route_palette(&action)),
            HostEvent::SwitchProject(selector) => self.step_project(selector),
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
            // The host owns geometry (D4); a viewer never resizes a PTY.
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

    /// Keystrokes for the terminal on screen. Typing returns to the live
    /// screen, as it does locally.
    fn type_bytes(&mut self, bytes: Vec<u8>) {
        let Some(id) = self
            .workspace()
            .selected_terminal()
            .map(|t| t.terminal_id.clone())
        else {
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
        let Some(id) = self
            .workspace()
            .selected_terminal()
            .map(|t| t.terminal_id.clone())
        else {
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
        match action {
            RemoteAction::Local(LocalAction::Session(selector)) => self.step_session(selector),
            RemoteAction::Local(LocalAction::Terminal(selector)) => self.step_terminal(selector),
            RemoteAction::Local(LocalAction::Project(selector)) => self.step_project(selector),
            RemoteAction::Local(LocalAction::Help) => self.local = Local::Help,
            RemoteAction::Local(LocalAction::About) => self.local = Local::About,
            RemoteAction::Local(LocalAction::FrontEnd(action)) => self.front_end.push(action),
            RemoteAction::Unavailable(why) => self.local = Local::Message(why.to_string()),
            RemoteAction::Send { name, scope } => {
                let args = views::target(self.workspace(), scope).map(|t| t.args());
                if self.client.command(name, args).is_none() {
                    self.local =
                        Local::Message("Not connected to the host — nothing was sent.".to_string());
                }
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

    fn step_project(&mut self, selector: Selector) {
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
                        self.run(views::route_palette(&action));
                    }
                    return;
                }
                OverlayInput::PaletteRun(action) => {
                    self.local = Local::None;
                    self.run(views::route_palette(&action));
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
        user_agent: user_agent(
            flightdeck::web::server::NATIVE_CLIENT_AGENT,
            env!("CARGO_PKG_VERSION"),
        ),
    };
    let client = RemoteClient::connect(config);
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
        store.save(&saved);
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
            LinkEnd::Stopped => "disconnected".to_string(),
        },
    }
}
