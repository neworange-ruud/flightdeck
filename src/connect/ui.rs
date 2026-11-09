//! The terminal UI of `flightdeck connect`: the core's [`RemoteSession`]
//! drawn with the local TUI's own widgets (`remote-control-ki05`).
//!
//! [`ConnectUi`] is pure apart from the clock it is handed: it builds the
//! [`Screen`] the renderer draws from the mirror, and turns crossterm input
//! into the same [`HostEvent`]s the local TUI produces, which the session
//! sends or keeps local by the rules FlightDeck Desktop's remote window
//! follows. What is the terminal's alone lives here:
//!
//! * **The header row** names the remote and the link instead of drawing the
//!   logo, so a remote screen is never mistaken for a local one, and carries
//!   the session's one-line notice.
//! * **The palette** is drawn and filtered here, from the rows the host
//!   forwards, minus the Desktop's own rows a terminal cannot perform.
//! * **A worker's shell** is the first tab on the project row; its keys are
//!   all the shell's, and the leave-focus key returns to the project.
//! * **A worker's project picker** is a dialog: Tab cycles scanned
//!   repositories, the home folder browser and a clone URL.
//! * **The link's end** (revoked, taken over, a version mismatch) is a full
//!   screen with Reconnect.

use std::rc::Rc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::commands::{Command, Selector};
use crate::app::modes::InputMode;
use crate::contracts::{PtySize, UiConfig};
use crate::host::{DialogView as HostDialog, HostEvent, OverlayInput, OverlayKey, OverlayView};
use crate::tui::input::{map_key_with_f2, KeyAction};
use crate::tui::layout;
use crate::tui::palette::{FrontEndAction, PaletteAction, PaletteEntry};
use crate::tui::render::{
    self, ChildTarget, Dialog, DialogAccel, DialogButton, DialogListItem, HitTarget, PaletteHit,
    ProjectHit, UiOverlay,
};
use crate::tui::screen::{Screen, SessionPane, TerminalPane};
use crate::view::{HintAction, ProjectStatus, ProjectTabView, TerminalRef};
use crate::web::client::messages::{self, Advice};
use crate::web::client::session::{Dialer, RemoteSession, Tab};
use crate::web::client::views;
use crate::web::client::{LinkEnd, RemoteClient};
use crate::web::protocol::{HostKind, Seat, SessionPhase};

/// The terminal UI's side of the update advice: it updates itself with
/// `flightdeck update`.
pub const ADVICE: Advice = Advice {
    update_self: "Update this FlightDeck: `flightdeck update`, then run `flightdeck connect` \
                  again.",
    update_either: "Update whichever is older: this FlightDeck with `flightdeck update`, or the \
                    worker from its shell with `flightdeck update`, then `flightdeck worker \
                    restart`.",
};

/// What a Desktop-only palette row says when it is chosen anyway.
const DESKTOP_ONLY: &str = "That is a FlightDeck Desktop action; `flightdeck connect` has no \
                            windows of its own.";

/// What Configuration says until the manager is drawn here
/// (`remote-control-ki05.4`).
const NO_CONFIGURATION: &str = "The host's configuration manager is not in `flightdeck \
                                connect` yet. Use FlightDeck Desktop, or edit \
                                ~/.flightdeck/config.toml in the worker shell.";

/// Whether the loop should keep going.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Continue,
    Quit,
}

/// The palette, drawn and filtered here.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Palette {
    filter: String,
    selected: usize,
}

/// Which part of a worker's project picker has the keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PickerPane {
    /// The repositories the scan of the projects root found.
    Scanned,
    /// Folders under `$HOME`, one level at a time.
    Browse,
    /// A URL to clone.
    Clone,
}

/// See the module docs.
pub struct ConnectUi {
    session: RemoteSession,
    ui: UiConfig,
    palette: Option<Palette>,
    /// The project picker's pane and highlighted row, while it is open.
    picker: (PickerPane, usize),
    /// Help or About, drawn from this build's own words.
    local: UiOverlay,
    /// The SSH target saved for this remote by hand ("Open in VS Code").
    ssh_target: Option<String>,
    /// Links chosen for the platform to open, not yet opened.
    open_urls: Vec<String>,
    /// The version skew was said once already on this attach.
    skew_said: bool,
    /// The first snapshot of this attach was looked at: a worker with no
    /// project open starts on its shell, where one is opened or cloned.
    landed: bool,
    quit: bool,
}

impl ConnectUi {
    /// A UI over `client`, called `label` at `address`. `ui` is this
    /// machine's `[ui]` settings: the host's projects' settings are the
    /// host's, but how this terminal looks is this machine's.
    pub fn new(
        client: RemoteClient,
        label: &str,
        address: &str,
        saved_kind: Option<HostKind>,
        ui: UiConfig,
    ) -> ConnectUi {
        let mut session = RemoteSession::new(
            client,
            label.to_string(),
            address.to_string(),
            crate::terminal::session::TerminalProfile::TUI,
        );
        session.set_saved_kind(saved_kind);
        ConnectUi {
            session,
            ui,
            palette: None,
            picker: (PickerPane::Scanned, 0),
            local: UiOverlay::None,
            ssh_target: None,
            open_urls: Vec::new(),
            skew_said: false,
            landed: false,
            quit: false,
        }
    }

    /// Dial with `dialer` when Reconnect is asked for.
    pub fn set_dialer(&mut self, dialer: Dialer) {
        self.session.set_dialer(dialer);
    }

    /// Post the host's notifications through `notifier`.
    pub fn set_notifier(&mut self, notifier: Rc<dyn crate::contracts::Notifier>) {
        self.session.set_notifier(notifier);
    }

    /// The SSH target saved for this remote by hand, for Open in VS Code.
    pub fn set_ssh_target(&mut self, target: Option<String>) {
        self.ssh_target = target;
    }

    pub fn session(&self) -> &RemoteSession {
        &self.session
    }

    pub fn should_quit(&self) -> bool {
        self.quit || self.session.close_requested()
    }

    /// What the program on screen copied, for this machine's clipboard.
    pub fn take_copied(&mut self) -> Option<String> {
        self.session.take_copied()
    }

    /// Links to open on this machine (Open in VS Code), in order.
    pub fn take_open_urls(&mut self) -> Vec<String> {
        std::mem::take(&mut self.open_urls)
    }

    // -- the turn -----------------------------------------------------------

    /// One turn against a screen of `area`: tell a worker the pane size,
    /// apply what the link said. Returns whether anything changed.
    pub fn pump(&mut self, area: Rect) -> bool {
        self.session.set_viewport(self.viewport(area));
        let changed = self.session.pump();
        if !self.landed && self.session.workspace().ready {
            self.landed = true;
            if self.session.is_worker() && self.session.workspace().projects.is_empty() {
                self.session.show_worker_shell();
            }
        }
        if !self.skew_said && self.session.workspace().ready {
            self.skew_said = true;
            if let Some(text) = self.skew_warning() {
                self.session.set_notice(text);
            }
        }
        changed
    }

    /// The terminal pane's size in `area`: what a worker sizes its PTY to
    /// (W5). Other hosts keep their own (D4).
    fn viewport(&self, area: Rect) -> PtySize {
        let mode = self.mode();
        let ml = layout::compute(
            area,
            layout::chrome_for(area, mode),
            crate::tui::mode_style::border_enabled(&self.ui),
            self.ui.agent_tab_side(),
        );
        PtySize {
            rows: ml.terminal.height.max(1),
            cols: ml.terminal.width.max(1),
        }
    }

    /// The version-skew line, when the worker's FlightDeck differs from this
    /// one.
    pub fn skew_warning(&self) -> Option<String> {
        let ws = self.session.workspace();
        if !ws.is_worker() {
            return None;
        }
        messages::skew_warning(&ws.host_version, crate::VERSION, &ADVICE)
    }

    fn mode(&self) -> InputMode {
        match self.session.tab() {
            Tab::WorkerShell => InputMode::Terminal,
            _ => self.session.mode(),
        }
    }

    fn on_shell(&self) -> bool {
        self.session.tab() == Tab::WorkerShell
    }

    // -- what the frame shows -------------------------------------------------

    /// The frame's facts, from the mirror.
    pub fn screen(&self) -> Screen<'_> {
        let ws = self.session.workspace();
        let on_shell = self.on_shell();
        let mut agents = views::agent_rows(ws);
        let mut git = views::git_strip(ws);
        let session = if on_shell {
            // The worker's shell stands in for the project: no agent is the
            // one on screen.
            agents.iter_mut().for_each(|row| row.selected = false);
            git.agent = None;
            let terminal = self
                .session
                .on_screen()
                .and_then(|id| self.session.terminal(&id));
            Some(SessionPane {
                creating: None,
                terminals: vec![TerminalPane {
                    target: ChildTarget::Primary,
                    label: "worker shell".to_string(),
                    terminal,
                }],
                active: ChildTarget::Primary,
            })
        } else {
            ws.selected_session().map(|s| SessionPane {
                creating: (s.phase == SessionPhase::Creating)
                    .then(|| s.git.branch.clone().unwrap_or_else(|| s.name.clone())),
                terminals: views::terminal_rows(s)
                    .into_iter()
                    .map(|row| TerminalPane {
                        target: child_target(row.target),
                        terminal: views::terminal_for(s, row.target)
                            .and_then(|t| self.session.terminal(&t.terminal_id)),
                        label: row.label,
                    })
                    .collect(),
                active: ws
                    .selected_terminal()
                    .and_then(|t| views::terminal_ref(s, &t.terminal_id))
                    .map(child_target)
                    .unwrap_or(ChildTarget::Primary),
            })
        };
        Screen {
            ui: &self.ui,
            mode: self.mode(),
            isolated: false,
            update_available: None,
            split_view: false,
            agents,
            git,
            session,
        }
    }

    /// The project tab row: the worker's shell first on a worker, then the
    /// host's projects. Returns the tabs and the one in view.
    pub fn project_tabs(&self) -> (Vec<ProjectTabView>, usize) {
        let ws = self.session.workspace();
        let mut tabs = views::project_tabs(ws);
        let offset = self.shell_offset();
        if offset == 1 {
            tabs.insert(
                0,
                ProjectTabView {
                    name: "worker shell".to_string(),
                    status: ProjectStatus::Idle,
                    agent_count: 0,
                    active: self.on_shell(),
                },
            );
        }
        let active = if self.on_shell() {
            0
        } else {
            ws.selected_project_index().unwrap_or(0) + offset
        };
        (tabs.clone(), active.min(tabs.len().saturating_sub(1)))
    }

    /// 1 when the project row starts with the worker's shell, else 0.
    fn shell_offset(&self) -> usize {
        usize::from(self.session.is_worker())
    }

    /// The header row: who this is driving, the link, and the last notice.
    pub fn header_line(&self) -> Line<'static> {
        let ws = self.session.workspace();
        let kind = if self.session.is_worker() {
            " (worker)"
        } else {
            ""
        };
        let seat = match (ws.seat, ws.input_holder().filter(|s| !s.is_you)) {
            (Some(Seat::Observing), _) => " · observing".to_string(),
            (_, Some(holder)) => format!(" · {} is typing", holder.label),
            _ => String::new(),
        };
        let mut spans = vec![
            Span::styled(
                " ⇄ FLIGHTDECK ",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" {}{kind}", self.session.label()),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" · {}{seat}", self.session.link_label()),
                Style::default().fg(Color::Gray),
            ),
        ];
        let health = ws
            .worker
            .as_ref()
            .and_then(|w| w.health.as_ref())
            .map(messages::health_line)
            .filter(|line| !line.is_empty());
        if let Some(health) = health {
            spans.push(Span::styled(
                format!(" · {health}"),
                Style::default().fg(Color::DarkGray),
            ));
        }
        if let Some(notice) = self.session.notice() {
            spans.push(Span::styled("  │ ", Style::default().fg(Color::DarkGray)));
            spans.push(Span::styled(
                notice.lines().next().unwrap_or_default().to_string(),
                Style::default().fg(Color::Yellow),
            ));
        }
        Line::from(spans)
    }

    /// The overlay to draw on the main screen, as the TUI draws it.
    pub fn overlay(&self) -> UiOverlay {
        if let Some(dialog) = self.picker_dialog() {
            return UiOverlay::Dialog(dialog);
        }
        match self.session.overlay() {
            Some(OverlayView::Message(m)) => UiOverlay::Dialog(Dialog::notification(m.text)),
            Some(OverlayView::Dialog(view)) => UiOverlay::Dialog(dialog(&view)),
            Some(OverlayView::Help(_)) => UiOverlay::Help,
            Some(OverlayView::About(_)) => UiOverlay::About,
            Some(OverlayView::GitStatus(view)) => UiOverlay::GitStatus {
                status: view.status,
                pr_url: view.pr_url,
            },
            Some(OverlayView::Config(_)) => {
                UiOverlay::Dialog(Dialog::notification(NO_CONFIGURATION))
            }
            // Never sent to a remote front-end; the palette is drawn here.
            Some(OverlayView::Palette(_) | OverlayView::WebAccess(_) | OverlayView::Pairing(_)) => {
                UiOverlay::None
            }
            None => self.local.clone(),
        }
    }

    /// The palette rows this terminal offers, filtered by what was typed:
    /// the host's, less the Desktop's own window rows.
    fn palette_rows(&self, filter: &str) -> Vec<&'static PaletteEntry> {
        let needle = filter.to_lowercase();
        views::palette_entries(self.session.workspace())
            .into_iter()
            .filter(|e| match &e.action {
                PaletteAction::FrontEnd(action) => *action == FrontEndAction::OpenInVsCode,
                // Not drawn here yet (`remote-control-ki05.4`).
                PaletteAction::OpenConfig => false,
                _ => true,
            })
            .filter(|e| needle.is_empty() || e.label.to_lowercase().contains(&needle))
            .collect()
    }

    /// Draw the whole frame. `now_ms` drives the spinners.
    pub fn draw(&self, frame: &mut Frame, now_ms: u64) {
        let area = frame.area();
        let ws = self.session.workspace();
        if let Some(end) = self.session.ended() {
            self.draw_ended(frame, area, end);
            return;
        }
        if !ws.ready {
            self.draw_connecting(frame, area);
            return;
        }
        let screen = self.screen();
        let overlay = self.overlay();
        let holder = ws
            .input_holder()
            .filter(|s| !s.is_you)
            .map(|s| s.label.clone());
        let ml = layout::compute(
            area,
            layout::chrome_for(area, screen.mode),
            crate::tui::mode_style::border_enabled(&self.ui),
            self.ui.agent_tab_side(),
        );
        let (tabs, active) = self.project_tabs();
        render::draw_project_tab_bar(
            frame,
            ml.project_tabs,
            &tabs,
            active,
            now_ms,
            self.ui.animations,
        );
        render::draw_screen(frame, &screen, &overlay, holder.as_deref(), now_ms);
        frame.render_widget(Paragraph::new(self.header_line()), ml.header);
        if let Some(palette) = &self.palette {
            let rows = self.palette_rows(&palette.filter);
            render::draw_palette_rows(frame, area, &palette.filter, &rows, palette.selected);
        }
    }

    fn draw_connecting(&self, frame: &mut Frame, area: Rect) {
        let lines = vec![
            Line::from(Span::styled(
                format!(
                    "Connecting to {} ({})…",
                    self.session.label(),
                    self.session.address()
                ),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                self.session.link_label(),
                Style::default().fg(Color::Gray),
            )),
            Line::raw(""),
            Line::from(Span::styled(
                "Ctrl-q quits.",
                Style::default().fg(Color::DarkGray),
            )),
        ];
        draw_panel(frame, area, " flightdeck connect ", lines, Color::Cyan);
    }

    fn draw_ended(&self, frame: &mut Frame, area: Rect, end: &LinkEnd) {
        let ws = self.session.workspace();
        let mut lines = vec![
            Line::from(Span::styled(
                format!("{}: {}", self.session.label(), self.session.link_label()),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::raw(""),
        ];
        let advice = match end {
            LinkEnd::VersionMismatch(_) => Some(messages::mismatch_advice(
                "",
                &ws.host_version,
                crate::VERSION,
                &ADVICE,
            )),
            LinkEnd::Revoked | LinkEnd::Unauthorized => Some(format!(
                "Pair again: run `flightdeck worker pair` (or open browser access) on the host, \
                 then `flightdeck connect --pair {}`.",
                self.session.address()
            )),
            LinkEnd::TakenOver { .. } => {
                Some("Another window took this worker over. Reconnect takes it back.".to_string())
            }
            _ => None,
        };
        if let Some(advice) = advice {
            lines.push(Line::raw(advice));
            lines.push(Line::raw(""));
        }
        lines.push(Line::from(vec![
            Span::styled("r", Style::default().fg(Color::Yellow)),
            Span::raw(": reconnect    "),
            Span::styled("q", Style::default().fg(Color::Yellow)),
            Span::raw(": quit"),
        ]));
        draw_panel(frame, area, " disconnected ", lines, Color::Red);
    }

    // -- the project picker ---------------------------------------------------

    /// The picker's rows in its current pane: label, and the path it stands
    /// for (a repository to open, a folder to browse into, `..` to go up).
    fn picker_rows(&self) -> Vec<(String, Option<String>)> {
        let Some(picker) = self.session.picker() else {
            return Vec::new();
        };
        match self.picker.0 {
            PickerPane::Scanned => match &picker.scanned {
                None => vec![("(scanning…)".to_string(), None)],
                Some(scan) if !scan.root_exists => {
                    vec![(format!("{} does not exist on the worker.", scan.root), None)]
                }
                Some(scan) if scan.repos.is_empty() => {
                    vec![(format!("No repositories in {}.", scan.root), None)]
                }
                Some(scan) => scan
                    .repos
                    .iter()
                    .map(|r| {
                        let open = if r.is_open_project { "  (open)" } else { "" };
                        (format!("{}{open}", r.name), Some(r.path.clone()))
                    })
                    .collect(),
            },
            PickerPane::Browse => {
                let mut rows = Vec::new();
                if let Some(parent) = &picker.parent {
                    rows.push(("..".to_string(), Some(parent.clone())));
                }
                rows.extend(picker.entries.iter().map(|e| {
                    let tag = if e.is_open_project {
                        "  (open)"
                    } else if e.is_git_repo {
                        "  (repository)"
                    } else {
                        "/"
                    };
                    (format!("{}{tag}", e.name), Some(e.path.clone()))
                }));
                if rows.is_empty() {
                    rows.push(("(no folders)".to_string(), None));
                }
                rows
            }
            PickerPane::Clone => Vec::new(),
        }
    }

    /// The picker as a dialog, while it is open.
    fn picker_dialog(&self) -> Option<Dialog> {
        let picker = self.session.picker()?;
        let (pane, selected) = self.picker;
        let heading = match pane {
            PickerPane::Scanned => "Open a project — repositories found".to_string(),
            PickerPane::Browse => format!(
                "Open a project — {}",
                picker.cwd.as_deref().unwrap_or(&picker.home)
            ),
            PickerPane::Clone => "Open a project — clone a repository".to_string(),
        };
        let mut title = vec![heading];
        if let Some(clone) = &picker.clone {
            let percent = clone.percent.map(|p| format!(" {p}%")).unwrap_or_default();
            title.push(format!("Cloning {}: {}{percent}", clone.url, clone.phase));
        }
        if let Some(error) = &picker.error {
            title.push(format!("Error: {error}"));
        } else if let Some(status) = &picker.status {
            title.push(status.clone());
        }
        let list = self
            .picker_rows()
            .into_iter()
            .enumerate()
            .map(|(i, (label, _))| DialogListItem {
                label,
                selected: i == selected,
            })
            .collect();
        let mut buttons = vec![DialogButton::new(DialogAccel::Tab, "Next pane")];
        match pane {
            PickerPane::Clone => buttons.push(DialogButton::new(DialogAccel::Enter, "Clone")),
            PickerPane::Browse => {
                buttons.push(DialogButton::new(DialogAccel::Enter, "Open / enter"));
                buttons.push(DialogButton::new(DialogAccel::Char('.'), "Hidden"));
            }
            PickerPane::Scanned => buttons.push(DialogButton::new(DialogAccel::Enter, "Open")),
        }
        buttons.push(DialogButton::cancelling(DialogAccel::Esc, "Close"));
        Some(Dialog {
            title: title.join("\n"),
            input: (pane == PickerPane::Clone).then(|| picker.clone_url.clone()),
            list,
            buttons,
            accent: Color::Cyan,
            origin: None,
        })
    }

    fn picker_key(&mut self, key: KeyEvent) {
        let (pane, selected) = self.picker;
        let rows = self.picker_rows();
        let last = rows.len().saturating_sub(1);
        match key.code {
            KeyCode::Esc => {
                self.session.close_picker();
            }
            KeyCode::Tab | KeyCode::BackTab => {
                let next = match (pane, key.code) {
                    (PickerPane::Scanned, KeyCode::Tab) => PickerPane::Browse,
                    (PickerPane::Browse, KeyCode::Tab) => PickerPane::Clone,
                    (PickerPane::Clone, KeyCode::Tab) => PickerPane::Scanned,
                    (PickerPane::Scanned, _) => PickerPane::Clone,
                    (PickerPane::Browse, _) => PickerPane::Scanned,
                    (PickerPane::Clone, _) => PickerPane::Browse,
                };
                self.picker = (next, 0);
            }
            KeyCode::Up => self.picker.1 = selected.saturating_sub(1),
            KeyCode::Down => self.picker.1 = (selected + 1).min(last),
            KeyCode::Enter => match pane {
                PickerPane::Clone => {
                    let url = self
                        .session
                        .picker()
                        .map(|p| p.clone_url.clone())
                        .unwrap_or_default();
                    self.session.picker_clone(&url, "");
                }
                PickerPane::Scanned => {
                    if let Some((_, Some(path))) = rows.get(selected) {
                        let path = path.clone();
                        self.session.picker_open(&path);
                    }
                }
                PickerPane::Browse => {
                    let Some((label, Some(path))) = rows.get(selected).cloned() else {
                        return;
                    };
                    let repo = self
                        .session
                        .picker()
                        .and_then(|p| p.entries.iter().find(|e| e.path == path))
                        .is_some_and(|e| e.is_git_repo);
                    if repo {
                        self.session.picker_open(&path);
                    } else {
                        let _ = label;
                        self.session.picker_browse(Some(&path));
                        self.picker.1 = 0;
                    }
                }
            },
            KeyCode::Right if pane == PickerPane::Browse => {
                if let Some((_, Some(path))) = rows.get(selected).cloned() {
                    self.session.picker_browse(Some(&path));
                    self.picker.1 = 0;
                }
            }
            KeyCode::Left if pane == PickerPane::Browse => {
                self.session.picker_up();
                self.picker.1 = 0;
            }
            KeyCode::Backspace if pane == PickerPane::Browse => {
                self.session.picker_up();
                self.picker.1 = 0;
            }
            KeyCode::Char('.') if pane == PickerPane::Browse => {
                self.session.picker_toggle_hidden();
                self.picker.1 = 0;
            }
            KeyCode::Backspace if pane == PickerPane::Clone => {
                if let Some(p) = self.session.picker_mut() {
                    p.clone_url.pop();
                }
            }
            KeyCode::Char(c) if pane == PickerPane::Clone => {
                if let Some(p) = self.session.picker_mut() {
                    p.clone_url.push(c);
                }
            }
            _ => {}
        }
    }

    // -- input ----------------------------------------------------------------

    /// One key press.
    pub fn key(&mut self, key: KeyEvent) -> Flow {
        let ctrl_q =
            key.code == KeyCode::Char('q') && key.modifiers.contains(KeyModifiers::CONTROL);
        if self.session.ended().is_some() {
            match key.code {
                KeyCode::Char('r') => self.reconnect(),
                KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
                _ if ctrl_q => self.quit = true,
                _ => {}
            }
            return self.flow();
        }
        if !self.session.workspace().ready {
            let ctrl_c =
                key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL);
            if ctrl_q || ctrl_c {
                self.quit = true;
            }
            return self.flow();
        }
        if self.palette.is_some() {
            self.palette_key(key);
            return self.flow();
        }
        if self.session.picker().is_some() {
            self.picker_key(key);
            return self.flow();
        }
        if let Some(view) = self.session.overlay() {
            if let Some(input) = overlay_input(&view, key) {
                self.apply(HostEvent::Overlay(input));
            }
            return self.flow();
        }
        if !matches!(self.local, UiOverlay::None) {
            self.local = UiOverlay::None;
            return self.flow();
        }
        let event = match map_key_with_f2(self.mode(), key, self.ui.use_f2_to_leave_terminal_focus)
        {
            KeyAction::Dispatch(cmd) => HostEvent::Command(cmd),
            KeyAction::SwitchProject(selector) => {
                self.switch_project(selector);
                return self.flow();
            }
            KeyAction::Passthrough(bytes) => HostEvent::TerminalInput(bytes),
            // The terminal's own Ctrl-V: the agent runs on the host and reads
            // the host's clipboard, if any.
            KeyAction::Paste => HostEvent::TerminalInput(vec![0x16]),
            KeyAction::OpenPalette => {
                self.open_palette();
                return self.flow();
            }
            KeyAction::OpenHelp => {
                self.local = UiOverlay::Help;
                return self.flow();
            }
            KeyAction::FocusApp if self.on_shell() => {
                self.leave_shell();
                HostEvent::FocusApp
            }
            KeyAction::FocusApp => HostEvent::FocusApp,
            KeyAction::FocusTerminal => HostEvent::FocusTerminal,
            KeyAction::ToggleMissionControl => {
                self.session.set_notice(
                    "Mission control is a view of the FlightDeck desktop app.".to_string(),
                );
                return self.flow();
            }
            KeyAction::Quit => {
                self.quit = true;
                return self.flow();
            }
            KeyAction::None => return self.flow(),
        };
        self.apply(event);
        self.flow()
    }

    /// Text the terminal pasted (bracketed paste).
    pub fn paste(&mut self, text: String) {
        if self.palette.is_some() {
            if let Some(p) = self.palette.as_mut() {
                p.filter.push_str(&text);
                p.selected = 0;
            }
            return;
        }
        if self.session.picker().is_some() {
            if self.picker.0 == PickerPane::Clone {
                if let Some(p) = self.session.picker_mut() {
                    p.clone_url.push_str(text.trim());
                }
            }
            return;
        }
        if let Some(OverlayView::Dialog(view)) = self.session.overlay() {
            if let Some(field) = view.input {
                self.apply(HostEvent::Overlay(OverlayInput::SetText(field + &text)));
            }
            return;
        }
        self.apply(HostEvent::Paste(text));
    }

    /// A mouse gesture over a screen of `area`.
    pub fn mouse(&mut self, me: MouseEvent, area: Rect) {
        if self.session.ended().is_some() || !self.session.workspace().ready {
            return;
        }
        let (col, row) = (me.column, me.row);
        match me.kind {
            MouseEventKind::ScrollUp => {
                if let Some(t) = self.session.active_terminal_mut() {
                    t.scroll_up(3);
                }
            }
            MouseEventKind::ScrollDown => {
                if let Some(t) = self.session.active_terminal_mut() {
                    t.scroll_down(3);
                }
            }
            MouseEventKind::Down(MouseButton::Left) => self.click(area, col, row),
            _ => {}
        }
    }

    fn click(&mut self, area: Rect, col: u16, row: u16) {
        if let Some(palette) = &self.palette {
            let rows = self.palette_rows(&palette.filter);
            match render::palette_rows_hit(area, &rows, palette.selected, col, row) {
                Some(PaletteHit::Entry(i)) => {
                    let action = rows.get(i).map(|e| e.action.clone());
                    self.palette = None;
                    if let Some(action) = action {
                        self.run_palette(action);
                    }
                }
                Some(PaletteHit::Dismiss) => self.palette = None,
                None => {}
            }
            return;
        }
        if let UiOverlay::Dialog(dialog) = self.overlay() {
            if let render::DialogHit::Button(i) = render::dialog_hit(area, &dialog, col, row) {
                let accel = dialog.buttons[i].accel;
                self.key(KeyEvent::new(accel_code(accel), KeyModifiers::NONE));
            }
            return;
        }
        if !matches!(self.overlay(), UiOverlay::None) {
            self.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
            return;
        }
        let screen = self.screen();
        let mode = screen.mode;
        let ml = layout::compute(
            area,
            layout::chrome_for(area, mode),
            crate::tui::mode_style::border_enabled(&self.ui),
            self.ui.agent_tab_side(),
        );
        let (tabs, active) = self.project_tabs();
        let names: Vec<String> = tabs.iter().map(|t| t.name.clone()).collect();
        if let Some(hit) = render::project_tab_hit_test(ml.project_tabs, &names, active, col, row) {
            match hit {
                ProjectHit::Tab(i) => self.switch_project(Selector::Index(i)),
                ProjectHit::NewButton => self.run_palette(PaletteAction::OpenProject),
                ProjectHit::Close(_) => {}
            }
            return;
        }
        if let Some(action) = render::status_bar_hit_screen(area, &screen, None, col, row) {
            let event = match action {
                HintAction::FocusApp => HostEvent::FocusApp,
                HintAction::FocusTerminal => HostEvent::FocusTerminal,
                HintAction::OpenPalette => {
                    self.open_palette();
                    return;
                }
                HintAction::OpenHelp => {
                    self.local = UiOverlay::Help;
                    return;
                }
            };
            self.apply(event);
            return;
        }
        let hit = render::hit_test_screen(area, &screen, col, row);
        drop(screen);
        match hit {
            Some(HitTarget::AgentTab(i)) => {
                self.leave_shell();
                self.session.select_agent(i);
            }
            Some(HitTarget::CloseAgentTab(i)) => {
                self.leave_shell();
                self.session.select_agent(i);
                self.run_palette(PaletteAction::CloseAgentTab);
            }
            Some(HitTarget::Sidebar) => {
                if !self.on_shell() {
                    self.apply(HostEvent::FocusApp);
                }
            }
            Some(HitTarget::Child(target)) => {
                if !self.on_shell() {
                    self.session.focus_terminal(terminal_ref(target));
                }
            }
            Some(HitTarget::CloseChild(_)) => {}
            Some(HitTarget::NewAgentButton) => self.run_palette(PaletteAction::NewAgentTab),
            Some(HitTarget::NewShellButton) => {
                self.run_palette(PaletteAction::Dispatch(Command::OpenShell))
            }
            None => {
                // A click on the terminal focuses it, as locally.
                if rect_contains(ml.terminal, col, row) {
                    self.apply(HostEvent::FocusTerminal);
                }
            }
        }
    }

    fn flow(&self) -> Flow {
        if self.should_quit() {
            Flow::Quit
        } else {
            Flow::Continue
        }
    }

    /// Apply one input through the session, then answer the palette rows it
    /// handed back.
    fn apply(&mut self, event: HostEvent) {
        self.session.apply(event);
        for action in self.session.take_front_end_actions() {
            match action {
                FrontEndAction::OpenInVsCode => {
                    let target = self.ssh_target.clone();
                    if let Some(url) = self.session.open_in_vscode(target.as_deref()) {
                        self.open_urls.push(url);
                    }
                }
                _ => self.session.show_message_overlay(DESKTOP_ONLY.to_string()),
            }
        }
    }

    fn run_palette(&mut self, action: PaletteAction) {
        self.apply(HostEvent::RunPaletteAction(action));
    }

    fn open_palette(&mut self) {
        self.palette = Some(Palette {
            filter: String::new(),
            selected: 0,
        });
    }

    fn palette_key(&mut self, key: KeyEvent) {
        let Some(mut palette) = self.palette.take() else {
            return;
        };
        let rows = self.palette_rows(&palette.filter);
        let split = rows.len().div_ceil(2);
        match key.code {
            KeyCode::Esc => return,
            KeyCode::Enter => {
                if let Some(entry) = rows.get(palette.selected) {
                    let action = entry.action.clone();
                    self.run_palette(action);
                }
                return;
            }
            KeyCode::Up => palette.selected = palette.selected.saturating_sub(1),
            KeyCode::Down => {
                palette.selected = (palette.selected + 1).min(rows.len().saturating_sub(1))
            }
            KeyCode::Right if palette.selected < split => {
                palette.selected = (palette.selected + split).min(rows.len().saturating_sub(1))
            }
            KeyCode::Left if palette.selected >= split => palette.selected -= split,
            KeyCode::Backspace => {
                palette.filter.pop();
                palette.selected = 0;
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                palette.filter.push(c);
                palette.selected = 0;
            }
            _ => {}
        }
        self.palette = Some(palette);
    }

    /// Step the project row — the worker's shell first, on a worker.
    fn switch_project(&mut self, selector: Selector) {
        let (tabs, active) = self.project_tabs();
        let len = tabs.len();
        if len == 0 {
            return;
        }
        let next = match selector {
            Selector::Index(i) if i < len => i,
            Selector::Index(_) => return,
            Selector::Next => (active + 1) % len,
            Selector::Prev => (active + len - 1) % len,
        };
        let offset = self.shell_offset();
        if offset == 1 && next == 0 {
            self.session.show_worker_shell();
        } else {
            self.session.select_project(next - offset);
        }
    }

    /// From the worker's shell back to the project it left.
    fn leave_shell(&mut self) {
        if self.on_shell() {
            let index = self
                .session
                .workspace()
                .selected_project_index()
                .unwrap_or(0);
            self.session.select_project(index);
        }
    }

    fn reconnect(&mut self) {
        self.palette = None;
        self.local = UiOverlay::None;
        self.skew_said = false;
        self.landed = false;
        self.session.reconnect();
    }
}

/// The overlay input a key means for the session's overlay `view`, if any.
fn overlay_input(view: &OverlayView, key: KeyEvent) -> Option<OverlayInput> {
    match view {
        OverlayView::Dialog(dialog) => dialog_key(dialog, key),
        // Read-only panels and messages: Enter or Esc closes them, as any key
        // does in the local TUI.
        _ => Some(match key.code {
            KeyCode::Enter => OverlayInput::Submit,
            _ => OverlayInput::Cancel,
        }),
    }
}

/// A key in the host's shared dialog (D13): typed into its field when it
/// has one, else a button's accelerator.
fn dialog_key(dialog: &HostDialog, key: KeyEvent) -> Option<OverlayInput> {
    Some(match key.code {
        KeyCode::Esc => OverlayInput::Cancel,
        KeyCode::Enter => OverlayInput::Submit,
        KeyCode::Up => OverlayInput::Key(OverlayKey::Up),
        KeyCode::Down => OverlayInput::Key(OverlayKey::Down),
        KeyCode::Tab => OverlayInput::Key(OverlayKey::Tab),
        KeyCode::Backspace => {
            let mut text = dialog.input.clone()?;
            text.pop();
            OverlayInput::SetText(text)
        }
        KeyCode::Char(c) => match &dialog.input {
            Some(text) => OverlayInput::SetText(format!("{text}{c}")),
            None => OverlayInput::Choose(c.to_string()),
        },
        _ => return None,
    })
}

/// The host's dialog as the TUI's dialog box draws it.
fn dialog(view: &HostDialog) -> Dialog {
    let mut title = vec![view.title.clone()];
    title.extend(view.body.iter().cloned());
    Dialog {
        title: title.join("\n"),
        input: view.input.clone(),
        list: view
            .list
            .iter()
            .map(|row| DialogListItem {
                label: row.label.clone(),
                selected: row.selected,
            })
            .collect(),
        buttons: view
            .buttons
            .iter()
            .map(|b| {
                let accel = accel_of(&b.id);
                if b.role == crate::host::ButtonRole::Cancel {
                    DialogButton::cancelling(accel, b.label.clone())
                } else {
                    DialogButton::new(accel, b.label.clone())
                }
            })
            .collect(),
        accent: Color::Cyan,
        origin: view.origin_label.clone(),
    }
}

/// A button id (`y`, `Enter`, `Tab`, `Esc`) as the accelerator the TUI
/// draws.
fn accel_of(id: &str) -> DialogAccel {
    match id {
        "Enter" => DialogAccel::Enter,
        "Esc" => DialogAccel::Esc,
        "Tab" => DialogAccel::Tab,
        other => DialogAccel::Char(other.chars().next().unwrap_or('?')),
    }
}

/// The key an accelerator stands for, for a click on its button.
fn accel_code(accel: DialogAccel) -> KeyCode {
    match accel {
        DialogAccel::Char(c) => KeyCode::Char(c),
        DialogAccel::Enter => KeyCode::Enter,
        DialogAccel::Esc => KeyCode::Esc,
        DialogAccel::Tab => KeyCode::Tab,
    }
}

fn child_target(target: TerminalRef) -> ChildTarget {
    match target {
        TerminalRef::Primary => ChildTarget::Primary,
        TerminalRef::Child(i) => ChildTarget::Child(i),
    }
}

fn terminal_ref(target: ChildTarget) -> TerminalRef {
    match target {
        ChildTarget::Primary => TerminalRef::Primary,
        ChildTarget::Child(i) => TerminalRef::Child(i),
    }
}

fn rect_contains(r: Rect, col: u16, row: u16) -> bool {
    col >= r.x
        && col < r.x.saturating_add(r.width)
        && row >= r.y
        && row < r.y.saturating_add(r.height)
}

/// A centred, bordered panel over a cleared screen.
fn draw_panel(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    lines: Vec<Line<'static>>,
    accent: Color,
) {
    frame.render_widget(Clear, area);
    let rect = layout::centered_overlay(area, 76, (lines.len() as u16).saturating_add(4));
    let block = Block::default()
        .title(title.to_string())
        .borders(Borders::ALL)
        .border_style(Style::default().fg(accent));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    frame.render_widget(
        Paragraph::new(lines)
            .alignment(Alignment::Left)
            .wrap(Wrap { trim: false }),
        inner.inner(ratatui::layout::Margin::new(1, 1)),
    );
}
