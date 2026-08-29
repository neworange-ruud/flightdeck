//! Drawing functions for the FlightDeck TUI (T8, SPECS §20, §21, §23, §24).
//!
//! All render functions are pure: they consume state and write into a
//! [`ratatui::Frame`]; they never call git/fs/pty directly.
//!
//! ## Git status cache
//!
//! The sidebar and git status panel need data (dirty flag, ahead/behind, base
//! drift) that is not cached in [`AppState`]. The wiring layer (T9) is
//! responsible for populating a [`GitStatusCache`] (a `HashMap<String,
//! WorktreeStatus>` keyed by tab id) periodically and passing it into
//! [`draw`]. If a tab id is absent from the cache, those indicators render as
//! "?" or blank — this module never panics on a missing entry.
//!
//! T9 integration notes:
//! - Call [`draw`] inside `Terminal::draw(|frame| ...)` once per event-loop
//!   tick with a freshly-computed layout via [`crate::tui::layout::compute`].
//! - Populate [`GitStatusCache`] by calling `collect_status` for each tab in a
//!   background task and updating the cache on completion.
//! - Pass [`UiOverlays`] to control which (if any) overlay is visible.

use std::collections::HashMap;

use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph};
use ratatui::Frame;

use crate::app::modes::InputMode;
use crate::app::state::{AppState, TabPhase};
use crate::git::status::WorktreeStatus;
use crate::terminal::session::TerminalKind;
use crate::tui::config_manager::{ConfigManager, Origin};
use crate::tui::layout;
use crate::tui::mode_style;
use crate::tui::palette::{CommandPalette, PaletteEntry};
use crate::tui::selection::Selection;
use crate::web::access::{AccessMode, WebAccessView};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Git status cache keyed by tab id (SPECS §20, §21).
///
/// Populated by T9; absent entries render as unknown. Never causes a panic.
pub type GitStatusCache = HashMap<String, WorktreeStatus>;

/// Which overlay (if any) is currently shown on top of the main layout.
#[derive(Debug, Clone, Default)]
pub enum UiOverlay {
    /// No overlay — normal main view.
    #[default]
    None,
    /// Command palette with the current [`CommandPalette`] state.
    Palette(CommandPalette),
    /// Help / keybindings overlay.
    Help,
    /// About dialog: version, description, and authorship / credits.
    About,
    /// Git status panel for the active tab, optionally with a PR URL.
    GitStatus {
        /// The git status data (typically from [`GitStatusCache`]).
        status: WorktreeStatus,
        /// A PR compare URL, if available (SPECS §14, §21).
        pr_url: Option<String>,
    },
    /// A centered modal dialog: a confirmation/notification with clickable
    /// buttons (each also bound to a keyboard accelerator).
    Dialog(Dialog),
    /// The configuration manager: curated toggles for the global/project config
    /// (SPECS §8).
    Config(ConfigManager),
    /// The desktop pairing surface (Settings → Remote): the QR + 4-digit code
    /// and pairing status (spec §5.2).
    Remote(RemotePairing),
    /// The browser access surface (`specs/WEB_INTERFACE.md` D5, Q1; design
    /// `2a`): how a browser gets in, in whichever of the two states the current
    /// binding puts it.
    WebAccess(WebAccessView),
}

/// Render-ready snapshot of a pairing attempt for [`UiOverlay::Remote`]. Rebuilt
/// each tick from the event loop's `PairingSession` so the countdown and status
/// stay live without the renderer touching any pairing logic.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RemotePairing {
    /// The one-line status ("Waiting for phone…", "Paired ✓", an error).
    pub status_line: String,
    /// The 4-digit (or relay-minted) code to type on the phone, if displaying.
    pub code: Option<String>,
    /// QR half-block art rows (black-on-white), empty when not displaying.
    pub qr_rows: Vec<String>,
    /// Width of the QR art in terminal cells (each row's char count).
    pub qr_width: usize,
    /// Seconds until the code expires, if displaying.
    pub seconds_remaining: Option<i64>,
    /// Pairing completed (show the success accent).
    pub done: bool,
    /// Pairing failed (show the error accent).
    pub failed: bool,
}

// ---------------------------------------------------------------------------
// Modal dialog model (confirmations & notifications)
// ---------------------------------------------------------------------------

/// The keyboard accelerator bound to a dialog button. Clicking the button
/// synthesizes the matching key, so mouse and keyboard drive the exact same
/// prompt-handling code path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogAccel {
    /// A character key, e.g. `y`, `n`, `1`.
    Char(char),
    /// The Enter key (confirm text entry).
    Enter,
    /// The Esc key (cancel/dismiss).
    Esc,
    /// The Tab key (cycle a form option, e.g. the target mode in the New Agent
    /// form — chosen because it never collides with text typed into an input).
    Tab,
}

impl DialogAccel {
    /// The label shown in brackets on the button, e.g. `y`, `Enter`, `Esc`.
    fn key_label(self) -> String {
        match self {
            DialogAccel::Char(c) => c.to_string(),
            DialogAccel::Enter => "Enter".to_string(),
            DialogAccel::Esc => "Esc".to_string(),
            DialogAccel::Tab => "Tab".to_string(),
        }
    }
}

/// One button in a [`Dialog`]: a label plus the accelerator that triggers it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialogButton {
    pub label: String,
    pub accel: DialogAccel,
}

impl DialogButton {
    pub fn new(accel: DialogAccel, label: impl Into<String>) -> DialogButton {
        DialogButton {
            accel,
            label: label.into(),
        }
    }

    /// Whether this button **dismisses** the dialog rather than deciding it.
    ///
    /// The dialogs do not agree on a cancel key — `n` in the close
    /// confirmations, `c` in the push confirmation, `Esc` in the forms — but
    /// they all agree on the *label*, because `prompt_dialog` writes it. One
    /// rule, read by `dialog_decision` (which key cancelled) and by
    /// `web_dialog_view` (which button a browser should cancel with), so the
    /// two cannot drift.
    pub fn cancels(&self) -> bool {
        self.label == "Cancel"
    }

    /// The rendered cell text, e.g. `" [y] Close "`.
    fn cell(&self) -> String {
        format!(" [{}] {} ", self.accel.key_label(), self.label)
    }

    /// The cell width in columns.
    fn width(&self) -> u16 {
        self.cell().chars().count() as u16
    }
}

/// One row of a [`Dialog`]'s optional list (e.g. the folder browser's
/// subdirectories). Rendered between the title and the input/buttons.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialogListItem {
    pub label: String,
    /// Highlighted as the current selection.
    pub selected: bool,
}

/// A centered modal dialog. Used for every confirmation, selection, text-entry
/// prompt, and notification, so they read clearly instead of as a cramped
/// bottom line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dialog {
    /// The question / message (wrapped across lines when rendered).
    pub title: String,
    /// `Some(buffer)` for a text-entry prompt: renders an input field.
    pub input: Option<String>,
    /// An optional scrollable list rendered between the title and the buttons
    /// (used by the project-folder browser). Empty for ordinary dialogs.
    pub list: Vec<DialogListItem>,
    /// The action buttons, in display order.
    pub buttons: Vec<DialogButton>,
    /// Border / accent colour (confirmations vs notifications).
    pub accent: Color,
    /// **D13's origin label**, e.g. `opened from browser · 192.168.2.20`.
    ///
    /// `None` for a dialog the person at this keyboard opened — the normal case,
    /// where an origin line would be noise. `Some` is the whole reason D13 is
    /// acceptable: a dialog is app state, so a browser opening one puts a modal
    /// on the desktop that *this* user did not ask for, and this line is the only
    /// thing that explains it. `specs/WEB_INTERFACE.md` D13 calls it load-bearing,
    /// not decoration, which is why it is a field on the render model rather than
    /// something a caller remembers to prepend to the title.
    pub origin: Option<String>,
}

impl Dialog {
    /// A confirmation/selection dialog with the given buttons.
    pub fn confirm(title: impl Into<String>, buttons: Vec<DialogButton>) -> Dialog {
        Dialog {
            title: title.into(),
            input: None,
            list: Vec::new(),
            buttons,
            accent: Color::Cyan,
            origin: None,
        }
    }

    /// A text-entry dialog with an input field and the given buttons.
    pub fn input(title: impl Into<String>, buffer: String, buttons: Vec<DialogButton>) -> Dialog {
        Dialog {
            title: title.into(),
            input: Some(buffer),
            list: Vec::new(),
            buttons,
            accent: Color::Cyan,
            origin: None,
        }
    }

    /// A browser dialog: a title, a text-entry field (a typed path), a
    /// scrollable list of choices (e.g. subdirectories), and action buttons.
    /// Used by the project-folder picker.
    pub fn browser(
        title: impl Into<String>,
        typed: String,
        list: Vec<DialogListItem>,
        buttons: Vec<DialogButton>,
    ) -> Dialog {
        Dialog {
            title: title.into(),
            input: Some(typed),
            list,
            buttons,
            accent: Color::Cyan,
            origin: None,
        }
    }

    /// A plain notification: a message with a single "OK" button. It is also
    /// dismissed by any key or a click (SPECS §22).
    pub fn notification(msg: impl Into<String>) -> Dialog {
        Dialog {
            title: msg.into(),
            input: None,
            list: Vec::new(),
            buttons: vec![DialogButton::new(DialogAccel::Enter, "OK")],
            accent: Color::Blue,
            origin: None,
        }
    }

    /// The same dialog, tagged with where the request to open it came from
    /// (D13). Builder-shaped because every prompt builds its dialog from the
    /// prompt alone and only the *event loop* knows the origin.
    pub fn from_origin(mut self, origin: impl Into<String>) -> Dialog {
        self.origin = Some(origin.into());
        self
    }
}

/// Where a click landed relative to an open [`Dialog`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogHit {
    /// On the button at this index.
    Button(usize),
    /// Inside the dialog box, but not on a button.
    Inside,
    /// Outside the dialog box entirely.
    Outside,
}

// ---------------------------------------------------------------------------
// Mouse hit-testing (clickable tabs)
// ---------------------------------------------------------------------------

/// Rows the sidebar header ("Agents") occupies before the first tab.
const SIDEBAR_HEADER_ROWS: u16 = 1;
/// Rows each agent tab occupies in the sidebar: divider + name + agent + git.
const SIDEBAR_ROWS_PER_TAB: u16 = 4;
/// The close control glyph shown inside tabs / on sidebar rows. A crisp "✕"
/// reads better than a bracketed `[x]` text link.
const CLOSE_GLYPH: &str = "✕";
/// Right-side tab-bar buttons, in left-to-right display order.
const NEW_AGENT_LABEL: &str = "+ agent";
const NEW_SHELL_LABEL: &str = "+ shell";
/// The right-aligned "open another project" button on the project tab row.
const NEW_PROJECT_LABEL: &str = "+ project";
/// Dark navy used for the active project tab and its row-level action.
const PROJECT_TAB_ACTIVE_BG: Color = Color::Rgb(16, 38, 68);

/// One project's summary for the project tab row (SPECS: multi-project). Carries
/// only what the row needs to render, so the pure renderer never touches the
/// runtime `Workspace`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectTabInfo {
    /// Display name (the project's folder name).
    pub name: String,
    /// An agent in this project needs attention / is waiting / failed.
    pub attention: bool,
    /// An agent in this project is actively working.
    pub busy: bool,
}

/// What a click on the project tab row resolved to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectHit {
    /// A project tab (by index) — switch to it.
    Tab(usize),
    /// The `✕` close control on a project tab (by index).
    Close(usize),
    /// The right-aligned "+ project" button — open another project.
    NewButton,
}

/// Which child-terminal tab a click landed on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildTarget {
    /// The primary agent terminal.
    Primary,
    /// The child shell terminal at this index.
    Child(usize),
}

/// What a mouse click resolved to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitTarget {
    /// An Agent Tab in the sidebar (by index).
    AgentTab(usize),
    /// The `✕` close control on a sidebar Agent Tab's name row (by index).
    CloseAgentTab(usize),
    /// The sidebar chrome itself (header/heading/empty space below the tabs) —
    /// anywhere in the left panel that is not an Agent Tab row. A click here
    /// still focuses the app (APP mode) without changing the selected tab, so
    /// clicking the sidebar works even with zero or one agents (SPECS §23).
    Sidebar,
    /// A child-terminal tab in the main pane.
    Child(ChildTarget),
    /// The `✕` close control inside a child-terminal tab. `Primary` closes the
    /// whole Agent Tab; `Child(i)` closes shell `i`.
    CloseChild(ChildTarget),
    /// The "+ agent" button at the right of the child tab bar (new Agent Tab).
    NewAgentButton,
    /// The "+ shell" button at the right of the child tab bar (new child shell).
    NewShellButton,
}

/// Resolve a click at `(col, row)` (terminal coordinates) against the layout for
/// `area`, returning the agent tab or child-terminal tab it lands on, if any.
pub fn hit_test(area: Rect, state: &AppState, col: u16, row: u16) -> Option<HitTarget> {
    let chrome = layout::chrome_for(area, state.mode());
    let side = state.config.ui.agent_tab_side();
    let ml = layout::compute(
        area,
        chrome,
        crate::tui::mode_style::border_enabled(&state.config.ui),
        side,
    );
    if rect_contains(ml.sidebar, col, row) {
        // A click on the `✕` on a tab's name row closes it; elsewhere on a tab
        // row selects it; anywhere else in the sidebar (logo header, "Agents"
        // heading, or the empty space below the last tab) resolves to the
        // sidebar chrome so the click still focuses the app — even with no
        // agents or just one (SPECS §23).
        return Some(
            sidebar_hit(ml.sidebar, state.tabs.len(), chrome, side, col, row)
                .unwrap_or(HitTarget::Sidebar),
        );
    }
    if state.split_view {
        // In split view a click on a column's header row switches to that
        // terminal. Clicks in the column *body* are not switch targets here —
        // they begin a text selection (handled by the mouse wiring, which still
        // focuses the column). This mirrors normal mode, where the tab bar
        // switches and the viewport selects.
        let region = layout::split_region(&ml);
        if rect_contains(region, col, row) {
            let entries = child_tab_entries(state);
            let cols = layout::split_columns(region, entries.len());
            for ((target, _label), c) in entries.iter().zip(cols.iter()) {
                if rect_contains(c.header, col, row) {
                    return Some(HitTarget::Child(*target));
                }
            }
        }
        return None;
    }
    if rect_contains(ml.child_tabs, col, row) {
        // The right-side buttons are drawn on top of the tab strip, so they win
        // hit-testing where they overlap a long tab strip: check them first.
        for (target, start, w) in child_tab_buttons(ml.child_tabs, state) {
            if col >= start && col < start.saturating_add(w) {
                return Some(target);
            }
        }
        for seg in child_tab_positions(ml.child_tabs, state) {
            if col >= seg.start && col < seg.start.saturating_add(seg.width) {
                // A click on the tab's `✕` closes it; elsewhere selects it.
                if col == seg.close_col {
                    return Some(HitTarget::CloseChild(seg.target));
                }
                return Some(HitTarget::Child(seg.target));
            }
        }
    }
    None
}

/// Whether `(col, row)` is inside `r`.
fn rect_contains(r: Rect, col: u16, row: u16) -> bool {
    col >= r.x
        && col < r.x.saturating_add(r.width)
        && row >= r.y
        && row < r.y.saturating_add(r.height)
}

/// Map a click in the sidebar `area` to an agent tab hit: the `✕` close control
/// (on the name row, at the far right) or the tab row itself. Returns `None` for
/// clicks that are not on a tab (header/heading/empty space below the tabs).
/// In the collapsed strip each agent occupies a single row and there is no close control.
fn sidebar_hit(
    area: Rect,
    tab_count: usize,
    chrome: layout::Chrome,
    side: crate::contracts::AgentTabPosition,
    col: u16,
    row: u16,
) -> Option<HitTarget> {
    let inner = Block::default().borders(sidebar_seam(side)).inner(area);
    if col < inner.x || col >= inner.x.saturating_add(inner.width) {
        return None;
    }
    // The collapsed strip is one row per agent with no heading above it.
    let (header_rows, rows_per_tab) = match chrome {
        layout::Chrome::Full => (SIDEBAR_HEADER_ROWS, SIDEBAR_ROWS_PER_TAB),
        layout::Chrome::Collapsed => (0, 1),
    };
    let first = inner.y.saturating_add(header_rows);
    if row < first {
        return None;
    }
    let rel = row - first;
    let idx = (rel / rows_per_tab) as usize;
    if idx >= tab_count {
        return None;
    }
    // Within a full tab block the rows are: divider(0), name(1), agent(2),
    // git(3). The `✕` lives on the name row at the sidebar's outer end — the
    // far right at `left`, the far left at `right`, mirrored with the row it
    // is drawn in (`sidebar_name_line`). Give it a forgiving 3-column target so
    // it stays easy to click. The collapsed strip has no close control — use
    // APP mode to close an agent.
    if chrome == layout::Chrome::Full && rel % rows_per_tab == 1 {
        let hit = match side {
            crate::contracts::AgentTabPosition::Left => {
                col >= inner.x.saturating_add(inner.width).saturating_sub(3)
            }
            crate::contracts::AgentTabPosition::Right => col < inner.x.saturating_add(3),
        };
        if hit {
            return Some(HitTarget::CloseAgentTab(idx));
        }
    }
    Some(HitTarget::AgentTab(idx))
}

/// The child-terminal tab entries for the selected tab: the primary "agent" tab
/// plus one per child shell. Shared by rendering and hit-testing so positions
/// always agree.
fn child_tab_entries(state: &AppState) -> Vec<(ChildTarget, String)> {
    // The primary agent terminal is "agent"; additional agents count up from 2,
    // shells count up from 1, each numbered in creation order (SPECS §19).
    let mut v = vec![(ChildTarget::Primary, "agent".to_string())];
    if let Some(tab) = state.selected() {
        let mut agent_n = 2;
        let mut shell_n = 1;
        for i in 0..tab.session.child_count() {
            let is_agent = tab.session.child(i).map(|t| t.kind) == Some(TerminalKind::Agent);
            let label = if is_agent {
                let l = format!("agent {agent_n}");
                agent_n += 1;
                l
            } else {
                let l = format!("shell {shell_n}");
                shell_n += 1;
                l
            };
            v.push((ChildTarget::Child(i), label));
        }
    }
    v
}

/// The screen geometry of one child-terminal tab segment.
struct ChildTabSeg {
    target: ChildTarget,
    /// First column of the segment.
    start: u16,
    /// Total width of the segment, including the `✕` close control.
    width: u16,
    /// Column of the `✕` close control within the segment.
    close_col: u16,
}

/// The display label of a child-terminal `target` (e.g. "agent 2", "shell 1"),
/// as shown on the tab bar. Used so close confirmations name the same thing the
/// user clicked. Returns `None` if the target is not present.
pub fn child_tab_label(state: &AppState, target: ChildTarget) -> Option<String> {
    child_tab_entries(state)
        .into_iter()
        .find(|(t, _)| *t == target)
        .map(|(_, label)| label)
}

/// Compute the geometry of each child-terminal tab segment, matching exactly how
/// [`draw_child_tab_bar`] lays them out. Each segment renders as
/// `" {label} ✕ "`: a leading space, the label, a space, the close glyph, and a
/// trailing space, so its width is `label.len() + 4` and the `✕` sits at
/// `start + label.len() + 2`.
fn child_tab_positions(area: Rect, state: &AppState) -> Vec<ChildTabSeg> {
    let mut out = Vec::new();
    let mut x = area.x;
    for (i, (target, label)) in child_tab_entries(state).into_iter().enumerate() {
        if i > 0 {
            x = x.saturating_add(3); // " | " separator
        }
        let label_len = label.chars().count() as u16;
        let w = label_len + 4; // " label ✕ "
        out.push(ChildTabSeg {
            target,
            start: x,
            width: w,
            close_col: x.saturating_add(label_len).saturating_add(2),
        });
        x = x.saturating_add(w);
    }
    out
}

/// The tab-bar buttons (`+ agent`, `+ shell`) as `(target, start_col, width)`,
/// right-aligned in `area`. `+ shell` is only offered when a tab is selected (a
/// child shell needs a host tab). Shared by rendering and hit-testing so the
/// clickable regions always match what is drawn.
fn child_tab_buttons(area: Rect, state: &AppState) -> Vec<(HitTarget, u16, u16)> {
    // Each button renders as " label " (one space of padding each side).
    let agent_w = NEW_AGENT_LABEL.chars().count() as u16 + 2;
    let shell_w = NEW_SHELL_LABEL.chars().count() as u16 + 2;
    let has_tab = state.selected().is_some();

    let right = area.x.saturating_add(area.width);
    let mut out = Vec::new();
    if has_tab {
        // Lay out right-to-left: shell is flush right, agent sits to its left
        // separated by a single space.
        let shell_start = right.saturating_sub(shell_w);
        let agent_start = shell_start.saturating_sub(1).saturating_sub(agent_w);
        out.push((HitTarget::NewAgentButton, agent_start, agent_w));
        out.push((HitTarget::NewShellButton, shell_start, shell_w));
    } else {
        let agent_start = right.saturating_sub(agent_w);
        out.push((HitTarget::NewAgentButton, agent_start, agent_w));
    }
    out
}

/// A full-width horizontal divider line (used between sidebar tabs).
fn divider_line(width: usize) -> Line<'static> {
    Line::from(Span::styled(
        "─".repeat(width),
        Style::default().fg(Color::DarkGray),
    ))
}

// ---------------------------------------------------------------------------
// Top-level entry point
// ---------------------------------------------------------------------------

/// Draw the complete FlightDeck UI into `frame`.
///
/// Called once per tick by T9 inside `Terminal::draw(|frame| draw(frame, ...))`.
pub fn draw(
    frame: &mut Frame,
    state: &AppState,
    cache: &GitStatusCache,
    overlay: &UiOverlay,
    input_holder: Option<&str>,
    now_ms: u64,
) {
    let area = frame.area();
    let chrome = layout::chrome_for(area, state.mode());
    let ml = layout::compute(
        area,
        chrome,
        crate::tui::mode_style::border_enabled(&state.config.ui),
        state.config.ui.agent_tab_side(),
    );

    draw_header(frame, ml.header);
    let divider = Paragraph::new(divider_line(ml.divider.width as usize));
    frame.render_widget(divider, ml.divider);
    draw_sidebar(frame, state, cache, ml.sidebar, chrome, now_ms);
    if state.split_view {
        // Split view reclaims the tab-bar row and lays the selected tab's
        // terminals out side by side in equal-width columns.
        draw_split_view(frame, state, layout::split_region(&ml), now_ms);
    } else {
        draw_child_tab_bar(frame, state, ml.child_tabs);
        draw_terminal_viewport(frame, state, ml.terminal, now_ms);
    }

    // Live-pane border (SPECS §23): frame ONLY the pane receiving keys. The
    // frame rects are present only when `mode_border != off`; geometry is fixed
    // by layout::compute. The non-focused pane's frame is not drawn at all —
    // previously it was rendered dark gray, which read as visual clutter.
    let mode = state.mode();
    if mode == InputMode::App {
        if let Some(frame_rect) = ml.sidebar_frame {
            let block =
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(mode_style::pane_border_style(
                        &state.config.ui,
                        mode,
                        mode_style::Pane::Sidebar,
                    ));
            frame.render_widget(block, frame_rect);
        }
    }
    if mode == InputMode::Terminal {
        if let Some(frame_rect) = ml.terminal_frame {
            let block =
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(mode_style::pane_border_style(
                        &state.config.ui,
                        mode,
                        mode_style::Pane::Terminal,
                    ));
            frame.render_widget(block, frame_rect);
        }
    }

    // Collapsed chrome gives the git info bar's rows to the terminal; git
    // details stay reachable through the git status overlay.
    if chrome == layout::Chrome::Full {
        let info_divider = Paragraph::new(divider_line(ml.info_divider.width as usize));
        frame.render_widget(info_divider, ml.info_divider);
        draw_info_bar(frame, state, cache, ml.info_bar);
    }
    let status_divider = Paragraph::new(divider_line(ml.status_divider.width as usize));
    frame.render_widget(status_divider, ml.status_divider);
    if chrome == layout::Chrome::Collapsed {
        frame.render_widget(
            Paragraph::new(compact_status_bar_text(
                state,
                input_holder,
                ml.status_bar.width,
            )),
            ml.status_bar,
        );
    } else {
        draw_status_bar(frame, state, input_holder, ml.status_bar);
    }

    // Draw overlay on top if active.
    match overlay {
        UiOverlay::None => {}
        UiOverlay::Dialog(dialog) => draw_dialog(frame, dialog, area),
        UiOverlay::Palette(palette) => draw_palette_overlay(frame, palette, area),
        UiOverlay::Help => draw_help_overlay(
            frame,
            area,
            state.config.ui.use_f2_to_leave_terminal_focus,
            state.isolated,
        ),
        UiOverlay::About => draw_about_overlay(frame, area),
        UiOverlay::GitStatus { status, pr_url } => {
            draw_git_status_overlay(frame, status, pr_url.as_deref(), area);
        }
        UiOverlay::Config(manager) => draw_config_overlay(frame, manager, area),
        UiOverlay::Remote(pairing) => draw_remote_overlay(frame, pairing, area),
        UiOverlay::WebAccess(access) => draw_web_access_overlay(frame, access, area),
    }
}

// ---------------------------------------------------------------------------
// Branded header (logo)
// ---------------------------------------------------------------------------

/// The ░▒▓ gradient ramps that flank the wordmark, read *toward* the centered
/// brand: solid blocks on the outside fade down to clear next to the text. The
/// remaining width on each side is filled with solid `█` so the title bar spans
/// the whole window (e.g. `█████▓▓▓▒▒▒░░░ · F L I G H T D E C K · ░░░▒▒▒▓▓▓█████`).
const RAMP_IN: &str = "▓▓▓▒▒▒░░░";
const RAMP_OUT: &str = "░░░▒▒▒▓▓▓";
/// The brand wordmark, spaced (wide) and tight (narrow) variants.
const BRAND_WIDE: &str = " · F L I G H T D E C K · ";
const BRAND_NARROW: &str = " F·L·I·G·H·T·D·E·C·K ";

/// Draw the full-width branded header: the wordmark centered with the block
/// gradient filling the row edge to edge.
pub fn draw_header(frame: &mut Frame, area: Rect) {
    let line = header_line(area.width as usize);
    let para = Paragraph::new(line).alignment(Alignment::Center);
    frame.render_widget(para, area);
}

/// Build the full-width logo [`Line`] for a given width: the wordmark (wide when
/// it fits, tight when it does not) framed by the ░▒▓ ramps and padded with solid
/// `█` blocks out to both edges so the bar always fills the window. Falls back to
/// a plain truncated brand when even the tight framed form is too wide for the
/// row. Exported for testing.
pub fn header_line(width: usize) -> Line<'static> {
    let block_style = Style::default().fg(Color::Cyan);
    let brand_style = Style::default()
        .fg(Color::White)
        .add_modifier(Modifier::BOLD);

    let ramp = RAMP_IN.chars().count() + RAMP_OUT.chars().count();

    // Pick the widest wordmark whose framed form (brand + both ramps) fits.
    let brand = if width >= BRAND_WIDE.chars().count() + ramp {
        BRAND_WIDE
    } else if width >= BRAND_NARROW.chars().count() + ramp {
        BRAND_NARROW
    } else {
        // Too narrow for the framed logo: show the brand alone, truncated to fit.
        let truncated: String = "FLIGHTDECK".chars().take(width).collect();
        return Line::from(Span::styled(truncated, brand_style));
    };

    // Fill the leftover columns with solid blocks, split across both sides so the
    // wordmark stays centered (any odd column goes to the right side).
    let fill = width - (brand.chars().count() + ramp);
    let left_blocks = fill / 2;
    let right_blocks = fill - left_blocks;

    Line::from(vec![
        Span::styled(format!("{}{RAMP_IN}", "█".repeat(left_blocks)), block_style),
        Span::styled(brand, brand_style),
        Span::styled(
            format!("{RAMP_OUT}{}", "█".repeat(right_blocks)),
            block_style,
        ),
    ])
}

// ---------------------------------------------------------------------------
// Project tab row (SPECS: multi-project)
// ---------------------------------------------------------------------------

/// The screen geometry of one project tab segment on the project tab row.
struct ProjectTabSeg {
    index: usize,
    /// First column of the segment.
    start: u16,
    /// Total width, including the `✕` close control.
    width: u16,
    /// Column of the `✕` close control within the segment.
    close_col: u16,
}

/// The display label for a project tab (a leading one-cell status indicator +
/// the name). The indicator width stays fixed while its glyph animates, so
/// hit-testing and rendering agree.
fn project_tab_label(name: &str) -> String {
    format!("● {name}")
}

/// Compute the geometry of each project tab segment, matching exactly how
/// [`draw_project_tab_bar`] lays them out. Each renders as `" {label} ✕ "`, so
/// its width is `label.len() + 4` and the `✕` sits at `start + label.len() + 2`.
/// Mirrors [`child_tab_positions`] so mouse hit-testing and drawing never drift.
fn project_tab_positions(area: Rect, names: &[String]) -> Vec<ProjectTabSeg> {
    let mut out = Vec::new();
    let mut x = area.x;
    for (i, name) in names.iter().enumerate() {
        if i > 0 {
            x = x.saturating_add(3); // " | " separator
        }
        let label_len = project_tab_label(name).chars().count() as u16;
        let w = label_len + 4; // " label ✕ "
        out.push(ProjectTabSeg {
            index: i,
            start: x,
            width: w,
            close_col: x.saturating_add(label_len).saturating_add(2),
        });
        x = x.saturating_add(w);
    }
    out
}

/// The "+ project" button as `(start_col, width)`, right-aligned in `area`.
fn project_new_button(area: Rect) -> (u16, u16) {
    let w = NEW_PROJECT_LABEL.chars().count() as u16 + 2; // " + project "
    let start = area.x.saturating_add(area.width).saturating_sub(w);
    (start, w)
}

/// Resolve a click at `(col, row)` on the project tab row `area` to a
/// [`ProjectHit`]. `names` are the project display names in tab order. The
/// right-aligned "+ project" button is checked first so it wins where it
/// overlaps a long tab strip.
pub fn project_tab_hit_test(
    area: Rect,
    names: &[String],
    col: u16,
    row: u16,
) -> Option<ProjectHit> {
    if !rect_contains(area, col, row) {
        return None;
    }
    let (btn_start, btn_w) = project_new_button(area);
    if col >= btn_start && col < btn_start.saturating_add(btn_w) {
        return Some(ProjectHit::NewButton);
    }
    for seg in project_tab_positions(area, names) {
        if col >= seg.start && col < seg.start.saturating_add(seg.width) {
            if col == seg.close_col {
                return Some(ProjectHit::Close(seg.index));
            }
            return Some(ProjectHit::Tab(seg.index));
        }
    }
    None
}

/// Draw the full-width project tab row: `● name ✕ | ● name ✕ …` on the left with
/// a right-aligned `+ project` button. The active project is highlighted; the
/// status indicator is red when a project needs attention, an animated red
/// spinner when busy, and a green dot when idle.
pub fn draw_project_tab_bar(
    frame: &mut Frame,
    area: Rect,
    projects: &[ProjectTabInfo],
    active: usize,
    now_ms: u64,
) {
    // A zero-height (or zero-width) area means the row is collapsed. Bail out
    // before building any sub-rect: `project_new_button` derives a fixed
    // height of 1 for its rect regardless of `area`, so without this guard it
    // would paint onto whatever row follows the collapsed project tab row.
    if area.height == 0 || area.width == 0 {
        return;
    }
    let mut spans: Vec<Span> = Vec::new();
    for (i, p) in projects.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" | ", Style::default().fg(Color::DarkGray)));
        }
        let is_active = i == active;
        let tab_style = if is_active {
            Style::default()
                .fg(PROJECT_TAB_ACTIVE_BG)
                .bg(Color::White)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        // Attention keeps visual priority when one agent needs input while
        // another is working in the same project.
        let (indicator, indicator_color) = if p.attention {
            ('●', Color::Red)
        } else if p.busy {
            (spinner_frame(now_ms), Color::Red)
        } else {
            ('●', Color::Green)
        };
        spans.push(Span::styled(" ", tab_style));
        spans.push(Span::styled(
            indicator.to_string(),
            tab_style.fg(indicator_color),
        ));
        spans.push(Span::styled(format!(" {} ", p.name), tab_style));
        spans.push(Span::styled(CLOSE_GLYPH, tab_style.fg(Color::Red)));
        spans.push(Span::styled(" ", tab_style));
    }
    let para = Paragraph::new(Line::from(spans)).style(Style::default().bg(Color::Reset));
    frame.render_widget(para, area);

    // Right-aligned "+ project" button, drawn on top of the tab strip.
    let (start, width) = project_new_button(area);
    if start >= area.x {
        let style = Style::default()
            .fg(Color::White)
            .bg(PROJECT_TAB_ACTIVE_BG)
            .add_modifier(Modifier::BOLD);
        let rect = Rect::new(start, area.y, width.min(area.width), 1);
        let btn = Paragraph::new(Line::from(Span::styled(
            format!(" {NEW_PROJECT_LABEL} "),
            style,
        )));
        frame.render_widget(btn, rect);
    }
}

// ---------------------------------------------------------------------------
// Sidebar (SPECS §20, §24)
// ---------------------------------------------------------------------------

/// Draw the left Agent Tabs sidebar.
pub fn draw_sidebar(
    frame: &mut Frame,
    state: &AppState,
    cache: &GitStatusCache,
    area: Rect,
    chrome: layout::Chrome,
    now_ms: u64,
) {
    if chrome == layout::Chrome::Collapsed {
        draw_sidebar_collapsed(frame, state, area, now_ms);
        return;
    }

    // When the live-pane border feature is on, the focused pane's frame
    // already supplies the separating vertical line, so the sidebar's own
    // seam divider is suppressed here — otherwise two adjacent vertical
    // lines would be drawn (SPECS §23).
    let side = state.config.ui.agent_tab_side();
    let block = if mode_style::border_enabled(&state.config.ui) {
        Block::default()
    } else {
        Block::default().borders(sidebar_seam(side))
    };
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let width = inner.width as usize;
    let mut lines: Vec<Line> = Vec::new();

    // Header row (SIDEBAR_HEADER_ROWS): centered "Agents" title.
    lines.push(
        Line::from(Span::styled(
            "Agents",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ))
        .alignment(Alignment::Center),
    );

    if state.tabs.is_empty() {
        lines.push(divider_line(width));
        lines.push(Line::from(Span::styled(
            "No tabs. Ctrl-n to create.",
            Style::default().fg(Color::DarkGray),
        )));
        frame.render_widget(Paragraph::new(lines), inner);
        return;
    }

    // Each tab block is SIDEBAR_ROWS_PER_TAB rows: divider, name, agent, git —
    // a divider above every tab including the first (SPECS §20).
    for (i, tab) in state.tabs.iter().enumerate() {
        let selected = state.selected_tab == Some(i);
        let ds = tab.display_status(now_ms);
        let git = cache.get(&tab.meta.id);

        // Divider (top of the tab block).
        lines.push(divider_line(width));

        // Name (with selection marker).
        let name_style = if selected {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };
        let marker = if selected { "▸ " } else { "  " };

        // A tab whose worktree is still being materialized on a background
        // worker shows an animated spinner instead of a process/status line, so
        // the user always sees that something is happening (SPECS §16/§17).
        if tab.phase == TabPhase::Creating {
            let spin = Style::default().fg(Color::Red);
            lines.push(sidebar_name_line(
                width,
                side,
                marker,
                name_style,
                Span::styled(format!("{} ", spinner_frame(now_ms)), spin),
                &tab.meta.name,
            ));
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled("creating worktree…", spin),
            ]));
            // Keep the block height uniform (divider/name/status/git rows).
            lines.push(Line::from(Span::raw("")));
            continue;
        }
        // A colour-coded status indicator on the name line: idle is a green
        // dot, active work is a red spinner, and input-required/errors are
        // red. Manual override takes colour priority but never hides the
        // lifecycle.
        let indicator_color = if matches!(
            ds.interpreted,
            crate::contracts::InterpretedStatus::Starting
                | crate::contracts::InterpretedStatus::Running
                | crate::contracts::InterpretedStatus::Working
        ) {
            Color::Red
        } else {
            ds.manual
                .map(|_| Color::Cyan)
                .unwrap_or_else(|| status_label_color(ds.interpreted).1)
        };
        let indicator = status_indicator(ds.interpreted, now_ms);
        lines.push(sidebar_name_line(
            width,
            side,
            marker,
            name_style,
            Span::styled(
                format!("{indicator} "),
                Style::default().fg(indicator_color),
            ),
            &tab.meta.name,
        ));

        // Agent name + simplified status, e.g. "Claude Code [in progress]".
        // A manual override (cyan) takes visual priority; otherwise the
        // interpreted status collapses to idle / in progress / waiting / error.
        let agent_name = state
            .registry
            .get(&tab.meta.agent)
            .map(|a| a.display_name.clone())
            .unwrap_or_else(|| tab.meta.agent.clone());
        let (status_label, status_color) = match ds.manual {
            Some(manual) => (manual.as_str().to_string(), Color::Cyan),
            None => {
                let (label, color) = status_label_color(ds.interpreted);
                (label.to_string(), color)
            }
        };
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(agent_name, Style::default().fg(Color::Gray)),
            Span::raw(" "),
            Span::styled(
                format!("[{status_label}]"),
                Style::default().fg(status_color),
            ),
        ]));

        // Git indicators (dirty, ahead/behind, base drift, recovered/existing).
        lines.push(build_git_indicator_line(tab, git));
    }

    frame.render_widget(Paragraph::new(lines), inner);
}

/// Draw the collapsed agent strip: one indicator glyph per agent, no heading
/// and no close control, for windows too small to afford the full sidebar.
fn draw_sidebar_collapsed(frame: &mut Frame, state: &AppState, area: Rect, now_ms: u64) {
    let block = Block::default().borders(sidebar_seam(state.config.ui.agent_tab_side()));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let lines: Vec<Line> = state
        .tabs
        .iter()
        .enumerate()
        .map(|(i, tab)| {
            Line::from(collapsed_agent_span(
                tab,
                state.selected_tab == Some(i),
                now_ms,
            ))
        })
        .collect();

    frame.render_widget(Paragraph::new(lines), inner);
}

/// The single-cell indicator for one agent in the collapsed strip: the
/// selection arrow, a spinner while it works, or its status dot. Same glyphs
/// and colours the full sidebar draws on each agent's name line, with the text
/// removed.
fn collapsed_agent_span(
    tab: &crate::app::state::RuntimeTab,
    selected: bool,
    now_ms: u64,
) -> Span<'static> {
    use crate::contracts::InterpretedStatus::{Running, Starting, Working};

    if selected {
        return Span::styled(
            "▸",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        );
    }
    let spinner = || {
        Span::styled(
            spinner_frame(now_ms).to_string(),
            Style::default().fg(Color::Red),
        )
    };
    if tab.phase == TabPhase::Creating {
        return spinner();
    }
    let ds = tab.display_status(now_ms);
    if matches!(ds.interpreted, Starting | Running | Working) {
        return spinner();
    }
    let color = ds
        .manual
        .map(|_| Color::Cyan)
        .unwrap_or_else(|| status_label_color(ds.interpreted).1);
    Span::styled("●", Style::default().fg(color))
}

/// Build a sidebar tab name line: `<marker><lead><name>` on the left with a
/// right-aligned `✕` close control filling to `width`. The name is truncated
/// with an ellipsis if it would collide with the close control (SPECS §20/§23).
fn sidebar_name_line(
    width: usize,
    side: crate::contracts::AgentTabPosition,
    marker: &'static str,
    name_style: Style,
    lead: Span<'static>,
    name: &str,
) -> Line<'static> {
    let marker_w = marker.chars().count();
    let lead_w = lead.content.chars().count();
    // Reserve two columns at the sidebar's outer end for a padding space and
    // the glyph.
    let name_budget = width.saturating_sub(marker_w + lead_w + 2);
    let shown = truncate_ellipsis(name, name_budget);
    let used = marker_w + lead_w + shown.chars().count();
    // Pad so the glyph lands in the outermost inner column.
    let pad = width.saturating_sub(used).saturating_sub(1);
    let close = Span::styled(CLOSE_GLYPH, Style::default().fg(Color::Red));
    let name_spans = [
        Span::styled(marker, name_style),
        lead,
        Span::styled(shown, name_style),
    ];
    // The `✕` column mirrors with the sidebar: it stays on the end furthest
    // from the terminal, so it never sits against the seam the two panes share.
    // The name itself does not mirror — it is text, and text keeps its reading
    // order on both settings.
    match side {
        crate::contracts::AgentTabPosition::Left => Line::from(
            name_spans
                .into_iter()
                .chain([Span::raw(" ".repeat(pad)), close])
                .collect::<Vec<_>>(),
        ),
        crate::contracts::AgentTabPosition::Right => Line::from(
            [close, Span::raw(" ".to_string())]
                .into_iter()
                .chain(name_spans)
                .chain([Span::raw(" ".repeat(pad.saturating_sub(1)))])
                .collect::<Vec<_>>(),
        ),
    }
}

/// The one-cell divider the sidebar draws on the seam it shares with the main
/// pane: its right edge at `left`, its left edge at `right`. One function so
/// drawing and hit-testing can never disagree about which column it eats.
fn sidebar_seam(side: crate::contracts::AgentTabPosition) -> Borders {
    match side {
        crate::contracts::AgentTabPosition::Left => Borders::RIGHT,
        crate::contracts::AgentTabPosition::Right => Borders::LEFT,
    }
}

/// Truncate `s` to at most `max` display columns, appending `…` when clipped.
fn truncate_ellipsis(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        return s.to_string();
    }
    match max {
        0 => String::new(),
        1 => "…".to_string(),
        _ => {
            let taken: String = s.chars().take(max - 1).collect();
            format!("{taken}…")
        }
    }
}

/// A braille spinner frame chosen from the wall clock (≈12.5 fps), used to
/// animate in-progress work (e.g. a tab whose worktree is being created).
pub fn spinner_frame(now_ms: u64) -> char {
    const FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    FRAMES[((now_ms / 80) % FRAMES.len() as u64) as usize]
}

/// Use the smooth one-cell braille spinner for active lifecycle states and the
/// stable centered dot for every settled state.
fn status_indicator(status: crate::contracts::InterpretedStatus, now_ms: u64) -> char {
    use crate::contracts::InterpretedStatus::*;
    match status {
        Starting | Running | Working => spinner_frame(now_ms),
        _ => '●',
    }
}

/// Collapse an interpreted status to a glanceable sidebar label + colour.
fn status_label_color(status: crate::contracts::InterpretedStatus) -> (&'static str, Color) {
    use crate::contracts::InterpretedStatus::*;
    match status {
        Starting | Running | Working => ("in progress", Color::Cyan),
        WaitingForInput | NeedsAttention => ("waiting", Color::Red),
        Failed | SessionLost => ("error", Color::Red),
        Idle | Completed | Stopped | Recovered | Unknown => ("idle", Color::Green),
    }
}

/// Build a single line of git indicators for a sidebar tab row.
fn build_git_indicator_line(
    tab: &crate::app::state::RuntimeTab,
    git: Option<&WorktreeStatus>,
) -> Line<'static> {
    let mut spans = vec![Span::raw("  ")];

    // Recovered / attached markers.
    if tab.meta.recovered {
        spans.push(Span::styled(
            "[recovered]",
            Style::default().fg(Color::Magenta),
        ));
        spans.push(Span::raw(" "));
    }
    if tab.meta.attached_existing_branch {
        spans.push(Span::styled("[existing]", Style::default().fg(Color::Cyan)));
        spans.push(Span::raw(" "));
    }

    match git {
        None => {
            spans.push(Span::styled("git: ?", Style::default().fg(Color::DarkGray)));
        }
        Some(ws) => {
            // Dirty indicator.
            if ws.dirty {
                spans.push(Span::styled("~dirty", Style::default().fg(Color::Yellow)));
                spans.push(Span::raw(" "));
            }
            // Ahead/behind vs upstream.
            if ws.upstream.is_some() {
                if ws.ahead > 0 || ws.behind > 0 {
                    let ab = format!("+{} -{}", ws.ahead, ws.behind);
                    spans.push(Span::styled(ab, Style::default().fg(Color::Cyan)));
                    spans.push(Span::raw(" "));
                }
            } else {
                spans.push(Span::styled(
                    "no-upstream",
                    Style::default().fg(Color::DarkGray),
                ));
                spans.push(Span::raw(" "));
            }
            // Target-base movement.
            if ws.base_drift > 0 {
                let moved = format!("target+{}", ws.base_drift);
                spans.push(Span::styled(moved, Style::default().fg(Color::Magenta)));
            }
        }
    }

    Line::from(spans)
}

// ---------------------------------------------------------------------------
// Child terminal tab bar (SPECS §19, §20)
// ---------------------------------------------------------------------------

/// Draw the horizontal child terminal tab bar inside the main pane (SPECS §19).
///
/// Layout: `agent ✕ | shell 1 ✕ | …` on the left, with `+ agent` / `+ shell`
/// buttons right-aligned. Each tab carries a `✕` close control; the buttons are
/// styled distinctly from the tabs so they read as actions, not tabs.
pub fn draw_child_tab_bar(frame: &mut Frame, state: &AppState, area: Rect) {
    match state.selected() {
        None => {
            let empty =
                Paragraph::new(" No tab selected ").style(Style::default().fg(Color::DarkGray));
            frame.render_widget(empty, area);
        }
        Some(tab) => {
            // Build "agent ✕ | shell 1 ✕ …" from the shared segmentation so the
            // rendered positions line up with mouse hit-testing (SPECS §19).
            let active = tab.session.selected_child(); // None = primary
            let mut spans: Vec<Span> = Vec::new();
            for (i, (target, label)) in child_tab_entries(state).into_iter().enumerate() {
                if i > 0 {
                    spans.push(Span::styled(" | ", Style::default().fg(Color::DarkGray)));
                }
                let is_active = match target {
                    ChildTarget::Primary => active.is_none(),
                    ChildTarget::Child(c) => active == Some(c),
                };
                let style = if is_active {
                    let bg = if matches!(target, ChildTarget::Primary) {
                        Color::Yellow
                    } else {
                        Color::Cyan
                    };
                    Style::default()
                        .fg(Color::Black)
                        .bg(bg)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Gray)
                };
                // " {label} ✕ ": the close glyph keeps the tab's background but
                // is tinted red so it reads as a distinct control.
                spans.push(Span::styled(format!(" {label} "), style));
                spans.push(Span::styled(CLOSE_GLYPH, style.fg(Color::Red)));
                spans.push(Span::styled(" ", style));
            }
            let para = Paragraph::new(Line::from(spans)).style(Style::default().bg(Color::Reset));
            frame.render_widget(para, area);
        }
    }

    // Right-aligned action buttons, drawn on top of the tab strip.
    for (target, start, width) in child_tab_buttons(area, state) {
        if start < area.x {
            continue; // no room
        }
        let (label, style) = match target {
            HitTarget::NewAgentButton => (
                NEW_AGENT_LABEL,
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            HitTarget::NewShellButton => (
                NEW_SHELL_LABEL,
                Style::default()
                    .fg(Color::White)
                    .bg(Color::Blue)
                    .add_modifier(Modifier::BOLD),
            ),
            _ => continue,
        };
        let rect = Rect::new(start, area.y, width.min(area.width), 1);
        let btn = Paragraph::new(Line::from(Span::styled(format!(" {label} "), style)));
        frame.render_widget(btn, rect);
    }
}

// ---------------------------------------------------------------------------
// Terminal viewport (SPECS §20)
// ---------------------------------------------------------------------------

/// Whether the terminal viewport should render dimmed: only when it is not the
/// focused pane (i.e. APP mode) and the user has left dimming enabled (SPECS §23).
fn dim_terminal(focused: bool, ui: &crate::contracts::UiConfig) -> bool {
    !focused && ui.dim_terminal_in_app_mode
}

/// Draw the active terminal viewport (SPECS §20): the VT100 screen of the
/// selected tab's active terminal (primary agent, or the selected child shell),
/// rendered cell-by-cell from its parser.
pub fn draw_terminal_viewport(frame: &mut Frame, state: &AppState, area: Rect, now_ms: u64) {
    let Some(tab) = state.selected() else {
        let p = Paragraph::new(
            "\n  FlightDeck — no Agent Session Tab selected.\n  Press Ctrl-n to create one.",
        )
        .style(Style::default().fg(Color::DarkGray));
        frame.render_widget(p, area);
        return;
    };

    // While the worktree is being created on a background worker there is no
    // session yet: show an animated progress message so the UI never looks
    // frozen (SPECS §16/§17).
    if tab.phase == TabPhase::Creating {
        let msg = Paragraph::new(Line::from(vec![
            Span::styled(
                format!("{} ", spinner_frame(now_ms)),
                Style::default().fg(Color::Cyan),
            ),
            Span::styled(
                format!("Creating worktree for {}…", tab.meta.branch),
                Style::default().fg(Color::Cyan),
            ),
        ]))
        .alignment(Alignment::Center);
        let inner = Rect {
            y: area.y + area.height / 2,
            height: 1,
            ..area
        };
        frame.render_widget(msg, inner);
        return;
    }

    let Some(term) = tab.session.active() else {
        let p =
            Paragraph::new("  (terminal starting…)").style(Style::default().fg(Color::DarkGray));
        frame.render_widget(p, area);
        return;
    };

    let focused = state.mode() == InputMode::Terminal;
    let dim = dim_terminal(focused, &state.config.ui);
    render_screen(frame, area, term.screen(), focused, term.selection(), dim);
}

/// Background colour used to highlight selected terminal cells (SPECS §20).
const SELECTION_BG: Color = Color::Rgb(58, 90, 138);

/// Render a VT100 [`vt100::Screen`] into `area`, cell-by-cell. When `focused`,
/// the terminal cursor is positioned to match the screen's cursor. Cells inside
/// `selection` are drawn with the selection highlight.
fn render_screen(
    frame: &mut Frame,
    area: Rect,
    screen: &vt100::Screen,
    focused: bool,
    selection: Option<&Selection>,
    dim: bool,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let (rows, cols) = screen.size();
    let offset = screen.scrollback();
    {
        let buf = frame.buffer_mut();
        let max_r = area.height.min(rows);
        let max_c = area.width.min(cols);
        for r in 0..max_r {
            // Columns selected on this visible row, if any.
            let sel_cols = selection.and_then(|s| s.row_selection(r, rows, cols, offset));
            for c in 0..max_c {
                let Some(cell) = screen.cell(r, c) else {
                    continue;
                };
                let target = &mut buf[(area.x + c, area.y + r)];
                let contents = cell.contents();
                if contents.is_empty() {
                    target.set_symbol(" ");
                } else {
                    target.set_symbol(contents);
                }
                let mut style = Style::default()
                    .fg(vt_color(cell.fgcolor()))
                    .bg(vt_color(cell.bgcolor()));
                if cell.bold() {
                    style = style.add_modifier(Modifier::BOLD);
                }
                if cell.italic() {
                    style = style.add_modifier(Modifier::ITALIC);
                }
                if cell.underline() {
                    style = style.add_modifier(Modifier::UNDERLINED);
                }
                if cell.inverse() {
                    style = style.add_modifier(Modifier::REVERSED);
                }
                // Gray out dimmed (unfocused) terminal text: force a muted gray
                // foreground and drop bold, so inactive terminal content reads
                // clearly as "asleep". Applied BEFORE the selection override
                // below so a selected cell's highlight always wins.
                if dim {
                    style = style
                        .fg(Color::DarkGray)
                        .remove_modifier(Modifier::BOLD)
                        .add_modifier(Modifier::DIM);
                }
                // Selection highlight overrides the cell background and drops any
                // inverse so the highlight reads consistently.
                if sel_cols.map(|(a, b)| c >= a && c <= b).unwrap_or(false) {
                    style = style
                        .bg(SELECTION_BG)
                        .fg(Color::White)
                        .remove_modifier(Modifier::REVERSED);
                }
                target.set_style(style);
            }
        }
    }
    if focused && offset == 0 && !screen.hide_cursor() {
        let (cr, cc) = screen.cursor_position();
        if cr < area.height && cc < area.width {
            frame.set_cursor_position((area.x + cc, area.y + cr));
        }
    }
}

/// Convert a [`vt100::Color`] to a ratatui [`Color`].
fn vt_color(c: vt100::Color) -> Color {
    match c {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(i) => Color::Indexed(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

// ---------------------------------------------------------------------------
// Split view (terminals side by side)
// ---------------------------------------------------------------------------

/// Draw the selected tab's terminals (primary agent + child shells) side by side
/// in equal-width columns, each topped by its label, with a vertical separator
/// between columns. Replaces the horizontal tab bar + single viewport when split
/// view is enabled. Column geometry comes from [`layout::split_columns`] so it
/// matches the per-terminal PTY sizing the wiring layer applies.
pub fn draw_split_view(frame: &mut Frame, state: &AppState, region: Rect, now_ms: u64) {
    let Some(tab) = state.selected() else {
        let p = Paragraph::new(
            "\n  FlightDeck — no Agent Session Tab selected.\n  Press Ctrl-n to create one.",
        )
        .style(Style::default().fg(Color::DarkGray));
        frame.render_widget(p, region);
        return;
    };

    // While the worktree is materializing there is no session yet (SPECS §16/§17).
    if tab.phase == TabPhase::Creating {
        let msg = Paragraph::new(Line::from(vec![
            Span::styled(
                format!("{} ", spinner_frame(now_ms)),
                Style::default().fg(Color::Cyan),
            ),
            Span::styled(
                format!("Creating worktree for {}…", tab.meta.branch),
                Style::default().fg(Color::Cyan),
            ),
        ]))
        .alignment(Alignment::Center);
        let inner = Rect {
            y: region.y + region.height / 2,
            height: 1,
            ..region
        };
        frame.render_widget(msg, inner);
        return;
    }

    let entries = child_tab_entries(state);
    let cols = layout::split_columns(region, entries.len());
    let active = tab.session.selected_child(); // None = primary
    let focused = state.mode() == InputMode::Terminal;
    let dim = dim_terminal(focused, &state.config.ui);

    for (i, ((target, label), col)) in entries.iter().zip(cols.iter()).enumerate() {
        let is_active = match target {
            ChildTarget::Primary => active.is_none(),
            ChildTarget::Child(c) => active == Some(*c),
        };

        // Column header: the terminal label, highlighted when active (matching
        // the tab-bar colours: agent = yellow, shell = cyan).
        let header_style = if is_active {
            let bg = if matches!(target, ChildTarget::Primary) {
                Color::Yellow
            } else {
                Color::Cyan
            };
            Style::default()
                .fg(Color::Black)
                .bg(bg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        let header = Paragraph::new(Line::from(Span::styled(format!(" {label} "), header_style)))
            .alignment(Alignment::Center);
        frame.render_widget(header, col.header);

        // Column body: the terminal's VT100 screen. Only the active column shows
        // the cursor, and only while a terminal is focused.
        let term = match target {
            ChildTarget::Primary => tab.session.primary(),
            ChildTarget::Child(c) => tab.session.child(*c),
        };
        match term {
            Some(term) => render_screen(
                frame,
                col.viewport,
                term.screen(),
                focused && is_active,
                term.selection(),
                dim,
            ),
            None => {
                let p = Paragraph::new("  (starting…)").style(Style::default().fg(Color::DarkGray));
                frame.render_widget(p, col.viewport);
            }
        }

        // Vertical separator in the gutter to the right of every column but the
        // last, spanning the full region height.
        if i + 1 < cols.len() {
            let sep_x = col.col.right();
            let sep = Paragraph::new(vec![Line::from("│"); region.height as usize])
                .style(Style::default().fg(Color::DarkGray));
            frame.render_widget(
                sep,
                Rect {
                    x: sep_x,
                    y: region.y,
                    width: 1,
                    height: region.height,
                },
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Git info bar (SPECS §21)
// ---------------------------------------------------------------------------

/// Draw the one-line git info bar for the selected tab (SPECS §21).
///
/// Shown for whichever child terminal is active (agent or shell) — it reflects
/// the tab's worktree, not the focused process. Content: branch name, the
/// add/modify/delete file counts, ahead/behind vs upstream, base drift, and the
/// base branch. Data comes from the [`GitStatusCache`]; a missing entry renders
/// as `git: ?` and never panics.
pub fn draw_info_bar(frame: &mut Frame, state: &AppState, cache: &GitStatusCache, area: Rect) {
    let line = info_bar_line(state, cache);
    let para = Paragraph::new(line).style(Style::default().bg(Color::Reset));
    frame.render_widget(para, area);
}

/// A dim ` │ ` segment separator for the info bar.
fn info_sep() -> Span<'static> {
    Span::styled(" │ ", Style::default().fg(Color::DarkGray))
}

fn shorten_branch(branch: &str, max_chars: usize) -> String {
    let chars: Vec<char> = branch.chars().collect();
    if chars.len() <= max_chars {
        return branch.to_string();
    }
    if max_chars <= 3 {
        return ".".repeat(max_chars);
    }
    let left = (max_chars - 3) / 2;
    let right = max_chars - 3 - left;
    format!(
        "{}...{}",
        chars[..left].iter().collect::<String>(),
        chars[chars.len() - right..].iter().collect::<String>()
    )
}

/// Build the git info bar [`Line`] for the selected tab. Exported for testing.
pub fn info_bar_line(state: &AppState, cache: &GitStatusCache) -> Line<'static> {
    let configured_default = state
        .invalid_base_branch
        .as_deref()
        .unwrap_or(&state.base_branch);
    let configured_default = shorten_branch(configured_default, 18);
    let invalid_default = state.invalid_base_branch.is_some();
    let Some(tab) = state.selected() else {
        return Line::from(vec![
            Span::styled(" Default base: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                if invalid_default {
                    format!("{configured_default} (not local)")
                } else {
                    configured_default
                },
                if invalid_default {
                    Style::default().fg(Color::Red)
                } else {
                    Style::default().fg(Color::White)
                },
            ),
            info_sep(),
            Span::styled(
                "No Agent Session Tab selected",
                Style::default().fg(Color::DarkGray),
            ),
        ]);
    };
    let git = cache.get(&tab.meta.id);

    let mut spans = vec![Span::styled(
        if invalid_default {
            format!(" default: {configured_default} (not local)")
        } else {
            format!(" default: {configured_default}")
        },
        if invalid_default {
            Style::default().fg(Color::Red)
        } else if tab.meta.base_branch == state.base_branch {
            Style::default().fg(Color::DarkGray)
        } else {
            Style::default().fg(Color::Yellow)
        },
    )];
    spans.push(info_sep());
    spans.push(Span::styled(
        format!("target: {}", shorten_branch(&tab.meta.base_branch, 18)),
        Style::default().fg(Color::DarkGray),
    ));
    if let Some(ws) = git {
        if ws.base_drift > 0 {
            spans.push(info_sep());
            spans.push(Span::styled(
                format!("target advanced +{}", ws.base_drift),
                Style::default().fg(Color::Magenta),
            ));
        }
    }

    // Branch (prefer the freshly-collected name; fall back to stored meta).
    let branch = git
        .map(|w| w.branch.clone())
        .unwrap_or_else(|| tab.meta.branch.clone());
    spans.push(Span::styled(" ⎇ ", Style::default().fg(Color::Blue)));
    spans.push(Span::styled(
        branch,
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    ));

    match git {
        None => {
            spans.push(info_sep());
            spans.push(Span::styled("git: ?", Style::default().fg(Color::DarkGray)));
        }
        Some(ws) => {
            // Change counts: +added ~modified -deleted (N files), or "clean".
            spans.push(info_sep());
            let ch = ws.changes;
            if ch.is_empty() {
                spans.push(Span::styled("clean", Style::default().fg(Color::Green)));
            } else {
                spans.push(Span::styled(
                    format!("+{}", ch.added),
                    Style::default().fg(Color::Green),
                ));
                spans.push(Span::raw(" "));
                spans.push(Span::styled(
                    format!("~{}", ch.modified),
                    Style::default().fg(Color::Yellow),
                ));
                spans.push(Span::raw(" "));
                spans.push(Span::styled(
                    format!("-{}", ch.deleted),
                    Style::default().fg(Color::Red),
                ));
                spans.push(Span::raw(" "));
                spans.push(Span::styled(
                    format!("({} files)", ch.total()),
                    Style::default().fg(Color::DarkGray),
                ));
            }

            // Ahead/behind vs upstream.
            spans.push(info_sep());
            if ws.upstream.is_some() {
                spans.push(Span::styled(
                    format!("↑{} ↓{}", ws.ahead, ws.behind),
                    Style::default().fg(Color::Cyan),
                ));
            } else {
                spans.push(Span::styled(
                    "no upstream",
                    Style::default().fg(Color::DarkGray),
                ));
            }
        }
    }

    Line::from(spans)
}

// ---------------------------------------------------------------------------
// Status bar (SPECS §23)
// ---------------------------------------------------------------------------

/// The global help keys, as one label. Both status-bar modes render this
/// constant, and so does the help screen's own Global row
/// ([`crate::tui::help::help_doc`]) — one constant, three renderings, so the
/// bar and the panel cannot claim different keys. A test still asserts the
/// panel contains it, because the panel is now built elsewhere.
pub const HELP_KEYS: &str = "F1 / Alt-h";

/// Draw the mode status bar (SPECS §23).
///
/// Terminal mode: `MODE: TERMINAL | <leave-focus>: app mode | Ctrl-g: palette | F1 / Alt-h: help`
/// App mode:      `MODE: APP | Enter: focus terminal | Ctrl-g: palette | F1 / Alt-h: help`
///
/// The bar is a no-wrap Paragraph in the main pane (`x = SIDEBAR_WIDTH`), so it
/// truncates on a narrow terminal and the trailing help hint is lost first.
/// "palette"/"app mode" are abbreviated to keep the widest bar inside ~102
/// columns — adding the help hint at no cost to the width it already needed.
///
/// Both help keys are listed, and identically on every OS: unlike the
/// leave-focus key they are the same binding everywhere, so a platform-varying
/// label would imply a difference that does not exist.
pub fn draw_status_bar(
    frame: &mut Frame,
    state: &AppState,
    input_holder: Option<&str>,
    area: Rect,
) {
    let text = status_bar_text(
        state.mode(),
        &state.config.ui,
        state.update_available.as_deref(),
        state.isolated,
        input_holder,
    );
    let para = Paragraph::new(text).style(Style::default().bg(Color::Reset));
    frame.render_widget(para, area);
}

/// Compact terminal-mode status used when the git info row is reclaimed. Safety
/// and mode indicators come first, followed by bounded base context; optional
/// shortcut hints are the first content allowed to clip.
fn compact_status_bar_text(
    state: &AppState,
    input_holder: Option<&str>,
    width: u16,
) -> Line<'static> {
    let branch_limit = match width {
        0..=49 => 4,
        50..=79 => 8,
        _ => 16,
    };
    let configured_default = state
        .invalid_base_branch
        .as_deref()
        .unwrap_or(&state.base_branch);
    let configured_default = shorten_branch(configured_default, branch_limit);
    let mut spans = Vec::new();
    if state.isolated {
        spans.push(Span::styled(
            " ISOLATED",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(info_sep());
    }
    spans.push(Span::styled(
        if width < 80 { "TERM" } else { "MODE: TERMINAL" },
        Style::default()
            .fg(Color::Black)
            .bg(crate::tui::mode_style::chip_color(
                &state.config.ui,
                InputMode::Terminal,
            ))
            .add_modifier(Modifier::BOLD),
    ));
    if let Some(holder) = input_holder {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            format!("INPUT: {holder}"),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ));
    }
    if let Some(version) = &state.update_available {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            if width < 80 {
                "UPDATE".to_string()
            } else {
                format!("v{version} update")
            },
            Style::default().fg(Color::Black).bg(Color::Yellow),
        ));
    }
    spans.push(info_sep());
    spans.push(Span::styled(
        if state.invalid_base_branch.is_some() {
            format!("default: !{configured_default}")
        } else {
            format!("default: {configured_default}")
        },
        if state.invalid_base_branch.is_some() {
            Style::default().fg(Color::Red)
        } else {
            Style::default().fg(Color::White)
        },
    ));
    if width >= 50 {
        if let Some(tab) = state.selected() {
            spans.push(info_sep());
            spans.push(Span::styled(
                format!(
                    "target: {}",
                    shorten_branch(&tab.meta.base_branch, branch_limit)
                ),
                Style::default().fg(Color::DarkGray),
            ));
        }
    }
    if width >= 100 {
        spans.push(Span::raw(" | "));
        spans.push(Span::styled(
            crate::tui::platform::leave_focus_key(state.config.ui.use_f2_to_leave_terminal_focus),
            Style::default().fg(Color::Yellow),
        ));
        spans.push(Span::raw(": app | "));
        spans.push(Span::styled("Ctrl-g", Style::default().fg(Color::Yellow)));
        spans.push(Span::raw(": palette"));
    }
    Line::from(spans)
}

/// Build the status bar [`Line`] for the given mode (SPECS §23), with an
/// optional trailing update hint when a newer release is available (SPECS §30).
///
/// Exported for snapshot testing.
pub fn status_bar_text(
    mode: InputMode,
    ui: &crate::contracts::UiConfig,
    update_available: Option<&str>,
    isolated: bool,
    input_holder: Option<&str>,
) -> Line<'static> {
    let chip_bg = crate::tui::mode_style::chip_color(ui, mode);
    let use_f2 = ui.use_f2_to_leave_terminal_focus;
    let mut spans = match mode {
        InputMode::Terminal => vec![
            Span::raw(" "),
            Span::styled(
                "MODE: TERMINAL",
                Style::default()
                    .fg(Color::Black)
                    .bg(chip_bg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" | "),
            Span::styled(
                crate::tui::platform::leave_focus_key(use_f2),
                Style::default().fg(Color::Yellow),
            ),
            Span::raw(": app mode | "),
            Span::styled("Ctrl-g", Style::default().fg(Color::Yellow)),
            Span::raw(": palette | "),
            Span::styled(HELP_KEYS, Style::default().fg(Color::Yellow)),
            Span::raw(": help"),
        ],
        InputMode::App => vec![
            Span::raw(" "),
            Span::styled(
                "MODE: APP",
                Style::default()
                    .fg(Color::Black)
                    .bg(chip_bg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" | "),
            Span::styled("Enter", Style::default().fg(Color::Yellow)),
            Span::raw(": focus terminal | "),
            Span::styled("Ctrl-g", Style::default().fg(Color::Yellow)),
            Span::raw(": palette | "),
            Span::styled(HELP_KEYS, Style::default().fg(Color::Yellow)),
            Span::raw(": help"),
        ],
    };

    // The input lock (`specs/WEB_INTERFACE.md` D14 as revised). Drawn only when
    // a browser is seated as a writer and somebody holds the turn, because with
    // one writer there is no contest to report.
    //
    // **This is not decoration and it is not a warning.** It is the only reason
    // a desktop user has for why the keys they just pressed did not appear:
    // the model refuses a keystroke typed into another writer's live burst
    // rather than interleaving it, and §5.1 does not allow that to happen
    // silently. `Take Input Lock` in the palette is the way past it.
    if let Some(holder) = input_holder {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            format!("INPUT: {holder}"),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ));
    }

    // Isolated run (SPECS §32): nothing persists and several actions are gone,
    // so say so permanently rather than once at launch.
    if isolated {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            "ISOLATED",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ));
    }

    // Update notice (SPECS §30): a non-intrusive hint, never a modal. It points
    // at `flightdeck update`, which itself routes Homebrew installs to
    // `brew update && brew upgrade`, so a single message is correct for every
    // install method.
    if let Some(version) = update_available {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            format!("● v{version} available — run `flightdeck update`"),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }

    Line::from(spans)
}

// ---------------------------------------------------------------------------
// Git status panel (SPECS §21)
// ---------------------------------------------------------------------------

/// Draw the git status panel as a centered overlay (SPECS §21).
///
/// Shows: branch, base branch, drift, dirty/clean, ahead/behind vs upstream,
/// whether upstream exists, worktree path, and optionally a PR compare URL.
/// No file diff view (SPECS §21 "No file diff view in MVP").
pub fn draw_git_status_overlay(
    frame: &mut Frame,
    status: &WorktreeStatus,
    pr_url: Option<&str>,
    area: Rect,
) {
    let overlay_area = layout::centered_overlay(area, 70, 18);
    frame.render_widget(Clear, overlay_area);

    let mut lines: Vec<Line> = Vec::new();

    lines.push(Line::from(vec![
        Span::styled("Branch:     ", Style::default().fg(Color::Gray)),
        Span::styled(status.branch.clone(), Style::default().fg(Color::Yellow)),
    ]));
    lines.push(Line::from(vec![
        Span::styled("Target base:", Style::default().fg(Color::Gray)),
        Span::raw(" "),
        Span::styled(
            status.base_branch.clone(),
            Style::default().fg(Color::White),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled("Target moved:", Style::default().fg(Color::Gray)),
        Span::raw(" "),
        Span::styled(
            if status.base_drift == 0 {
                "none".to_string()
            } else {
                format!("{} commits since this tab last synced", status.base_drift)
            },
            if status.base_drift == 0 {
                Style::default().fg(Color::Green)
            } else {
                Style::default().fg(Color::Magenta)
            },
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled("Dirty:      ", Style::default().fg(Color::Gray)),
        Span::styled(
            if status.dirty { "yes" } else { "clean" },
            if status.dirty {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default().fg(Color::Green)
            },
        ),
    ]));

    let upstream_label = status.upstream.as_deref().unwrap_or("none (not pushed)");
    lines.push(Line::from(vec![
        Span::styled("Upstream:   ", Style::default().fg(Color::Gray)),
        Span::styled(
            upstream_label.to_string(),
            if status.upstream.is_some() {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default().fg(Color::DarkGray)
            },
        ),
    ]));

    if status.upstream.is_some() {
        lines.push(Line::from(vec![
            Span::styled("Ahead/behind:", Style::default().fg(Color::Gray)),
            Span::raw(" "),
            Span::styled(
                format!("↑{} ↓{}", status.ahead, status.behind),
                Style::default().fg(Color::Cyan),
            ),
        ]));
    }

    lines.push(Line::from(vec![
        Span::styled("Worktree:   ", Style::default().fg(Color::Gray)),
        Span::styled(
            status.worktree_path.to_string_lossy().to_string(),
            Style::default().fg(Color::White),
        ),
    ]));

    if let Some(url) = pr_url {
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![
            Span::styled("PR URL:     ", Style::default().fg(Color::Gray)),
            Span::styled(url.to_string(), Style::default().fg(Color::Green)),
        ]));
    }

    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        "  Esc / q to close",
        Style::default().fg(Color::DarkGray),
    )));

    let block = Block::default()
        .title(" Git Status ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow));

    let para = Paragraph::new(lines).block(block);
    frame.render_widget(para, overlay_area);
}

// ---------------------------------------------------------------------------
// Command palette overlay (SPECS §22)
// ---------------------------------------------------------------------------

/// Draw the command palette as a centered overlay (SPECS §22).
pub fn draw_palette_overlay(frame: &mut Frame, palette: &CommandPalette, area: Rect) {
    let overlay_area = layout::centered_overlay(area, 90, 32);
    frame.render_widget(Clear, overlay_area);

    let block = Block::default()
        .title(" Command Palette  (Esc to close) ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let inner = block.inner(overlay_area);
    frame.render_widget(block, overlay_area);

    // Split inner: one row for filter input, rest for filtered list.
    let [filter_area, list_area] = ratatui::layout::Layout::vertical([
        ratatui::layout::Constraint::Length(1),
        ratatui::layout::Constraint::Fill(1),
    ])
    .areas(inner);

    // Filter input line.
    let filter_line = Line::from(vec![
        Span::styled("> ", Style::default().fg(Color::Cyan)),
        Span::raw(palette.filter().to_string()),
        Span::styled("_", Style::default().fg(Color::Cyan)), // cursor
    ]);
    frame.render_widget(Paragraph::new(filter_line), filter_area);

    // Filtered list.
    let filtered = palette.filtered();
    let selected_idx = palette.selected_index();

    if filtered.is_empty() {
        frame.render_widget(
            Paragraph::new("  (no matches)").style(Style::default().fg(Color::DarkGray)),
            list_area,
        );
        return;
    }

    // Split the filtered entries across two columns. The left column takes the
    // first half; the right column the remainder. Each column renders its own
    // group headers so groups read correctly even when split at the boundary.
    let split = filtered.len().div_ceil(2);
    let [left_area, right_area] = ratatui::layout::Layout::horizontal([
        ratatui::layout::Constraint::Percentage(50),
        ratatui::layout::Constraint::Percentage(50),
    ])
    .areas(list_area);

    // Build the `ListItem`s for one column from a slice of the filtered
    // entries. `base` is the flat index of the first entry so selection
    // highlighting stays aligned with `selected_index`.
    let build_column = |entries: &[&PaletteEntry], base: usize| -> Vec<ListItem<'static>> {
        let mut last_group: Option<&str> = None;
        let mut items: Vec<ListItem> = Vec::new();
        for (offset, entry) in entries.iter().enumerate() {
            let i = base + offset;
            if last_group != Some(entry.group) {
                // Blank line above each group header (except the first) for breathing room.
                if last_group.is_some() {
                    items.push(ListItem::new(Line::raw("")));
                }
                last_group = Some(entry.group);
                items.push(ListItem::new(Line::from(Span::styled(
                    format!("  {}", entry.group),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ))));
            }

            items.push(if i == selected_idx {
                ListItem::new(Line::from(Span::styled(
                    format!("  {} ", entry.label),
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )))
            } else {
                ListItem::new(Line::from(Span::styled(
                    format!("  {} ", entry.label),
                    Style::default().fg(Color::White),
                )))
            });
        }
        items
    };

    frame.render_widget(List::new(build_column(&filtered[..split], 0)), left_area);
    frame.render_widget(
        List::new(build_column(&filtered[split..], split)),
        right_area,
    );
}

// ---------------------------------------------------------------------------
// Help overlay (SPECS §23)
// ---------------------------------------------------------------------------

/// Draw the help / keybindings overlay (SPECS §23).
///
/// The words are not here: [`crate::tui::help::help_doc`] owns them, and the
/// browser's help overlay is drawn from the very same value
/// (`specs/WEB_INTERFACE.md` §6.5 R16). This function is the ratatui half of
/// that one source — it decides indentation, colour and where the hints sit,
/// and nothing else.
pub fn draw_help_overlay(frame: &mut Frame, area: Rect, use_f2: bool, isolated: bool) {
    let overlay_area = layout::centered_overlay(area, 64, 40);
    frame.render_widget(Clear, overlay_area);

    let doc = crate::tui::help::help_doc(use_f2, isolated);

    let mut help_text: Vec<Line> = Vec::new();

    // SPECS §32: an isolated run's note leads, not trails. The overlay is a
    // fixed 64x40 box with no scroll or pagination (a known, separate defect:
    // the base shortcut list alone already clips its own tail there), so
    // leading with the note guarantees it survives that clip regardless of
    // terminal height. `help_doc` puts it first for both surfaces; this loop
    // only draws what it was given.
    for note in &doc.notes {
        help_text.push(Line::from(Span::styled(
            note.title.clone(),
            Style::default().fg(Color::Magenta),
        )));
        for line in &note.lines {
            help_text.push(Line::raw(format!("  {line}")));
        }
    }

    help_text.push(Line::from(Span::styled(
        doc.title.clone(),
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    )));

    for section in &doc.sections {
        help_text.push(Line::raw(""));
        help_text.push(Line::from(Span::styled(
            section.title.clone(),
            Style::default().fg(Color::Yellow),
        )));
        for row in &section.rows {
            help_text.push(shortcut_line(
                format!("  {}", row.keys),
                row.description.clone(),
            ));
        }
    }

    // The hints live on the bottom border, not in the shortcut list: the list is
    // taller than the overlay on any ordinary terminal, so a trailing content
    // line is truncated away exactly when a user needs it.
    let block = Block::default()
        .title(" Help / Keybindings ")
        .title_bottom(Span::styled(
            " Press the help key again: open on GitHub · Esc / q: close ",
            Style::default().fg(Color::DarkGray),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow));

    let para = Paragraph::new(help_text).block(block);
    frame.render_widget(para, overlay_area);
}

/// The overlay's minimum content width, so a codes-only surface still reads as a
/// dialog rather than a sliver.
const PAIRING_MIN_CONTENT_W: u16 = 44;
/// Left + right border plus one column of padding on each side.
const PAIRING_BORDERED_CHROME_W: u16 = 4;
/// Top + bottom border rows.
const PAIRING_BORDER_H: u16 = 2;

/// How the pairing overlay decided to lay itself out for a given terminal size:
/// which frame it drew and which optional rows survived the height budget.
///
/// Split out from the drawing so the fit logic — the part that decides whether a
/// phone can scan anything at all — is unit-testable at exact terminal sizes.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PairingLayout {
    /// Draw the titled border (false = borderless, which buys back two rows).
    bordered: bool,
    /// Render the QR art.
    show_qr: bool,
    /// Render the "terminal too small" note (only when a QR exists but no room).
    note: Vec<String>,
    /// Content width the text is wrapped to.
    content_w: u16,
    /// Wrapped status-line rows.
    status: Vec<String>,
    /// Whether a claim code row is rendered (never dropped when present).
    has_code: bool,
    /// Optional rows that fit, in drop order.
    show_countdown: bool,
    show_gap_after_art: bool,
    show_esc: bool,
    show_gap_before_status: bool,
}

impl PairingLayout {
    /// Total content rows this layout renders (excluding the border).
    fn content_h(&self, qr_h: u16) -> u16 {
        (if self.show_qr { qr_h } else { 0 })
            + self.note.len() as u16
            + u16::from(self.show_gap_after_art)
            + u16::from(self.has_code)
            + u16::from(self.show_countdown)
            + u16::from(self.show_gap_before_status)
            + self.status.len() as u16
            + u16::from(self.show_esc)
    }
}

/// The line both QR-bearing overlays show when the terminal cannot hold the art:
/// the size it would need, the size there is, and what to do instead.
///
/// Shared by the phone pairing overlay ([`pairing_layout`]) and the browser
/// access overlay ([`web_access_layout`]) so the two degrade with the same
/// words. A bare "too small" leaves the user guessing which dimension to grow,
/// which is the whole reason the numbers are in it.
fn qr_too_small_note(needs_w: u16, needs_h: u16, area: Rect, instead: &str) -> String {
    format!(
        "Terminal too small for the QR (needs {}x{}, have {}x{}) — {instead}.",
        needs_w, needs_h, area.width, area.height
    )
}

/// Decide the pairing overlay's layout for `area`.
///
/// The QR is what a phone actually scans, so it is fitted **first** and the
/// chrome around it is what gives way: the countdown, the spacers and the "Esc
/// to close" hint are dropped in that order, and if the box's own border is what
/// tips the QR off screen the border goes too. The previous fixed 10-row chrome
/// budget meant a real 57x29 `fdr1:` QR needed a 61x39 terminal — so a
/// default-size Windows Terminal (~120x30) only ever showed the fallback note,
/// never a scannable code.
fn pairing_layout(pairing: &RemotePairing, area: Rect) -> PairingLayout {
    let qr_w = pairing.qr_width as u16;
    let qr_h = pairing.qr_rows.len() as u16;
    let has_qr = !pairing.qr_rows.is_empty();
    let has_code = pairing.code.is_some();

    // The only row that never gives way beside the art: the claim code, so the
    // manual path always stays available. The status line *is* droppable while a
    // QR is on screen — "Scan the QR or type the code" says nothing the visible
    // QR and code do not, and giving it up is what lets a 29-row QR plus its
    // code land in a 30-row terminal. With no QR the status is the only
    // explanation there is, so it becomes required (see `budget` below).
    let qr_content_w = qr_w.max(PAIRING_MIN_CONTENT_W);
    let art_required_h = u16::from(has_code);
    let qr_bordered = has_qr
        && qr_content_w + PAIRING_BORDERED_CHROME_W <= area.width
        && qr_h + PAIRING_BORDER_H + art_required_h <= area.height;
    let qr_borderless = has_qr
        && !qr_bordered
        && qr_content_w <= area.width
        && qr_h + art_required_h <= area.height;

    let show_qr = qr_bordered || qr_borderless;
    let bordered = !qr_borderless;
    let content_w = if show_qr {
        qr_content_w
    } else {
        PAIRING_MIN_CONTENT_W
    }
    .min(area.width.saturating_sub(if bordered {
        PAIRING_BORDERED_CHROME_W
    } else {
        0
    }))
    .max(1);

    let status_lines = wrap_message(&pairing.status_line, content_w as usize);
    // Name the smallest terminal that would show the QR (the borderless fit) —
    // a bare "too small" leaves the user guessing which dimension to grow.
    let note_text = qr_too_small_note(
        qr_content_w,
        qr_h + art_required_h,
        area,
        "enter the code below",
    );

    let mut budget = area
        .height
        .saturating_sub(if bordered { PAIRING_BORDER_H } else { 0 })
        .saturating_sub(if show_qr { qr_h } else { 0 })
        .saturating_sub(u16::from(has_code))
        // With no art, the status is required rather than budgeted.
        .saturating_sub(if show_qr {
            0
        } else {
            status_lines.len() as u16
        });
    let mut take = |n: u16| -> bool {
        if n > 0 && budget >= n {
            budget -= n;
            true
        } else {
            false
        }
    };

    let note = if has_qr && !show_qr {
        let lines = wrap_message(&note_text, content_w as usize);
        if take(lines.len() as u16) {
            lines
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };
    // Highest-value optional row first: with the QR up, the status only makes it
    // in when there is height to spare.
    let status = if show_qr && !take(status_lines.len() as u16) {
        Vec::new()
    } else {
        status_lines
    };
    let show_countdown = pairing.seconds_remaining.is_some() && take(1);
    let show_gap_after_art = (show_qr || !note.is_empty()) && take(1);
    let show_esc = take(1);
    let show_gap_before_status = take(1);

    PairingLayout {
        bordered,
        show_qr,
        note,
        content_w,
        status,
        has_code,
        show_countdown,
        show_gap_after_art,
        show_esc,
        show_gap_before_status,
    }
}

/// Draw the desktop pairing overlay (Settings → Remote, spec §5.2): the QR code
/// (rendered as black-on-white half-block cells so a phone camera can scan it),
/// the 4-digit code, an expiry countdown, and the pairing status. The layout
/// adapts to the terminal ([`pairing_layout`]); when even a borderless QR cannot
/// fit it honestly shows the code plus the size the QR would need.
pub fn draw_remote_overlay(frame: &mut Frame, pairing: &RemotePairing, area: Rect) {
    let qr_h = pairing.qr_rows.len() as u16;
    let l = pairing_layout(pairing, area);

    let box_w = (l.content_w
        + if l.bordered {
            PAIRING_BORDERED_CHROME_W
        } else {
            0
        })
    .min(area.width);
    let box_h =
        (l.content_h(qr_h) + if l.bordered { PAIRING_BORDER_H } else { 0 }).min(area.height);
    let overlay = layout::centered_overlay(area, box_w, box_h);
    frame.render_widget(Clear, overlay);

    let accent = if pairing.failed {
        Color::Red
    } else if pairing.done {
        Color::Green
    } else {
        Color::Cyan
    };
    let inner = if l.bordered {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(accent))
            .title(" Pair Phone ");
        let inner = block.inner(overlay);
        frame.render_widget(block, overlay);
        inner
    } else {
        overlay
    };

    let mut lines: Vec<Line> = Vec::new();
    if l.show_qr {
        // Each row: black modules (foreground) on a white background.
        let style = Style::default().fg(Color::Black).bg(Color::White);
        for row in &pairing.qr_rows {
            lines.push(Line::from(Span::styled(row.clone(), style)));
        }
    }
    for note in &l.note {
        lines.push(Line::from(Span::styled(
            note.clone(),
            Style::default().fg(Color::Yellow),
        )));
    }
    if l.show_gap_after_art {
        lines.push(Line::raw(""));
    }
    if let Some(code) = &pairing.code {
        lines.push(Line::from(Span::styled(
            format!("Code  {code}"),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )));
    }
    if let (true, Some(secs)) = (l.show_countdown, pairing.seconds_remaining) {
        lines.push(Line::from(Span::styled(
            format!("expires in {secs}s"),
            Style::default().fg(Color::DarkGray),
        )));
    }
    if l.show_gap_before_status {
        lines.push(Line::raw(""));
    }
    for row in &l.status {
        lines.push(Line::from(Span::styled(
            row.clone(),
            Style::default().fg(accent),
        )));
    }
    if l.show_esc {
        lines.push(Line::from(Span::styled(
            "Esc to close",
            Style::default().fg(Color::DarkGray),
        )));
    }

    let para = Paragraph::new(lines).alignment(Alignment::Center);
    frame.render_widget(para, inner);
}

// ---------------------------------------------------------------------------
// The browser access overlay (D5, Q1, Q7; design `2a`)
// ---------------------------------------------------------------------------

/// The access overlay's minimum content width.
///
/// 76 plus the 4 columns of chrome is exactly 80, the classic terminal floor:
/// this is the widest the overlay can ask for and still be drawn whole on the
/// narrowest terminal anybody actually uses. It is much wider than the pairing
/// overlay's 44 because this surface carries sentences — the address picker's
/// descriptions, D5's warning, a six-key legend — rather than a code and a
/// status line.
const WEB_ACCESS_MIN_CONTENT_W: u16 = 76;
/// Left + right border plus one column of padding on each side.
const WEB_ACCESS_CHROME_W: u16 = 4;
/// Top + bottom border rows.
const WEB_ACCESS_BORDER_H: u16 = 2;

/// How readily a row gives way when the terminal is short. [`REQUIRED`] rows
/// never do; higher tiers are dropped first, and a whole tier goes before the
/// next one is touched.
type Tier = u8;

/// A row that is the surface: dropping it would make the overlay lie by
/// omission (the status line, the code, the selected address, the key legend).
const REQUIRED: Tier = 0;
/// Explanatory prose: D5's warning body, the network door's consequence. High
/// value, but the headline above each still says what it is.
const TIER_PROSE: Tier = 1;
/// Rows whose information is repeated in the key legend at the foot.
const TIER_ECHOED: Tier = 2;
/// Blank spacers. The first thing to go and the last thing anyone misses.
const TIER_SPACER: Tier = 3;

/// Drop rows tier by tier, from the bottom of each tier upward, until the list
/// fits `height`.
///
/// Bottom-upward within a tier because these overlays put the most contextual
/// material last (the browsers-holding-access line, the second warning
/// paragraph): when two rows are equally droppable, the later one is the one
/// the reader has already been prepared for by everything above it.
///
/// Split out from the drawing so the fit is unit-testable at exact terminal
/// sizes, exactly as [`pairing_layout`] is.
fn fit_rows(mut rows: Vec<(Tier, Line<'static>)>, height: u16) -> Vec<Line<'static>> {
    let height = height as usize;
    let mut tier = Tier::MAX;
    while rows.len() > height && tier > REQUIRED {
        // Highest tier still present, so a lower tier is never touched while a
        // higher one still has rows to give.
        tier = match rows.iter().map(|(t, _)| *t).filter(|t| *t > REQUIRED).max() {
            Some(t) => t,
            None => break,
        };
        while rows.len() > height {
            match rows.iter().rposition(|(t, _)| *t == tier) {
                Some(idx) => {
                    rows.remove(idx);
                }
                None => break,
            }
        }
    }
    // Everything left is required and still does not fit: the terminal is
    // smaller than the smallest honest form of this overlay, so the tail is
    // clipped rather than something load-bearing being silently reordered.
    rows.truncate(height);
    rows.into_iter().map(|(_, line)| line).collect()
}

/// Pad `s` to `width` cells with the remainder split either side, so a QR or a
/// code centres inside a left-aligned paragraph. Wider-than-`width` input is
/// returned untouched — clipping is the caller's business, not the padder's.
fn center_pad(s: &str, width: u16) -> String {
    let len = s.chars().count();
    let width = width as usize;
    if len >= width {
        return s.to_string();
    }
    let left = (width - len) / 2;
    format!("{}{}", " ".repeat(left), s)
}

/// A `key` chip followed by its label, the way every FlightDeck overlay draws
/// one (design turn 1: "every button shows its key").
fn key_chip(key: &str, label: &str) -> Vec<Span<'static>> {
    vec![
        Span::styled(
            format!(" {key} "),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(label.to_string(), Style::default().fg(Color::Gray)),
    ]
}

/// Draw the browser access overlay (`specs/WEB_INTERFACE.md` D5, D10, Q1, Q7;
/// design `2a`) in whichever of its two states the current binding puts it.
///
/// **State A (loopback, the default) never draws a credential.** There is no
/// code and no QR to hide, because on this machine neither buys anything: the
/// QR would encode an address that resolves nowhere else, and the code would be
/// a shoulder-surfing hazard bought for nothing. The credential still exists —
/// `Enter` and `c` spend one — it simply travels in a URL fragment instead of
/// across the room. **State B** is where the QR earns its place, and it is
/// drawn with the same black-on-white half-block art the phone pairing overlay
/// uses, subject to the same honest degradation: when the terminal cannot hold
/// it, the art gives way to a note naming the size it would need
/// ([`qr_too_small_note`]) and the code stays, because the code is the path
/// that always works.
pub fn draw_web_access_overlay(frame: &mut Frame, view: &WebAccessView, area: Rect) {
    let Some(mode) = view.mode else {
        return;
    };
    let l = web_access_layout(view, area);

    let box_w = (l.content_w + WEB_ACCESS_CHROME_W).min(area.width);
    let box_h = (l.rows_h + WEB_ACCESS_BORDER_H).min(area.height);
    let overlay = layout::centered_overlay(area, box_w, box_h);
    frame.render_widget(Clear, overlay);

    let title = match mode {
        AccessMode::LocalOnly => " Web Interface ",
        AccessMode::Network => " Web Interface — network access ",
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(title);
    let inner = block.inner(overlay);
    frame.render_widget(block, overlay);

    let rows = web_access_rows(view, mode, &l);
    let lines = fit_rows(rows, inner.height);
    frame.render_widget(Paragraph::new(lines), inner);
}

/// What the access overlay decided about its own size: the content width, the
/// QR's fate, and how tall the surviving rows are.
#[derive(Debug, Clone, PartialEq, Eq)]
struct WebAccessLayout {
    /// Content width the prose is wrapped to and the QR is centred in.
    content_w: u16,
    /// Render the QR art.
    show_qr: bool,
    /// The "terminal too small" note, when a QR exists but has no room.
    note: Vec<String>,
    /// Height of the rows that will actually be drawn.
    rows_h: u16,
}

/// Decide the access overlay's layout for `area`.
///
/// The QR is fitted **first and against the required rows only**: it appears
/// when the art plus everything that can never give way still fits. That is the
/// same priority the pairing overlay gives it — a QR nobody can scan is worth
/// nothing — but the required set here is larger, because this overlay also
/// carries the address it is publishing and the warning about what that means,
/// and neither may be silently dropped to make room for art.
fn web_access_layout(view: &WebAccessView, area: Rect) -> WebAccessLayout {
    let qr_w = view.qr_width as u16;
    let qr_h = view.qr_rows.len() as u16;
    let has_qr = !view.qr_rows.is_empty();

    let content_w = qr_w
        .max(WEB_ACCESS_MIN_CONTENT_W)
        .min(area.width.saturating_sub(WEB_ACCESS_CHROME_W))
        .max(1);
    let inner_h = area.height.saturating_sub(WEB_ACCESS_BORDER_H);

    // Probe with no QR and no note: what the required rows alone cost.
    let probe = WebAccessLayout {
        content_w,
        show_qr: false,
        note: Vec::new(),
        rows_h: 0,
    };
    let mode = view.mode.unwrap_or(AccessMode::LocalOnly);
    let required_h = web_access_rows(view, mode, &probe)
        .iter()
        .filter(|(tier, _)| *tier == REQUIRED)
        .count() as u16;

    let show_qr =
        has_qr && qr_w + WEB_ACCESS_CHROME_W <= area.width && qr_h + required_h <= inner_h;
    let note = if has_qr && !show_qr {
        wrap_message(
            &qr_too_small_note(
                qr_w + WEB_ACCESS_CHROME_W,
                qr_h + required_h + WEB_ACCESS_BORDER_H,
                area,
                "type the code instead",
            ),
            content_w as usize,
        )
    } else {
        Vec::new()
    };

    let mut decided = WebAccessLayout {
        content_w,
        show_qr,
        note,
        rows_h: 0,
    };
    let rows = web_access_rows(view, mode, &decided);
    decided.rows_h = (fit_rows(rows, inner_h).len() as u16).max(1);
    decided
}

/// Build the overlay's rows, each tagged with how readily it gives way.
///
/// One function for both states so the two can never drift into different
/// chrome: the state only decides *which* rows exist, never how they are drawn.
fn web_access_rows(
    view: &WebAccessView,
    mode: AccessMode,
    l: &WebAccessLayout,
) -> Vec<(Tier, Line<'static>)> {
    let w = l.content_w;
    let mut rows: Vec<(Tier, Line<'static>)> = Vec::new();
    let dim = Style::default().fg(Color::DarkGray);
    let body = Style::default().fg(Color::Gray);
    let bright = Style::default()
        .fg(Color::White)
        .add_modifier(Modifier::BOLD);

    let spacer = |rows: &mut Vec<(Tier, Line<'static>)>| rows.push((TIER_SPACER, Line::raw("")));

    match mode {
        AccessMode::LocalOnly => {
            rows.push((REQUIRED, serving_line(view, mode)));
            for line in wrap_message(&view.exposure_line, w as usize) {
                rows.push((REQUIRED, Line::from(Span::styled(line, dim))));
            }
            spacer(&mut rows);
            let mut action = key_chip("Enter", "Open in browser");
            action.push(Span::raw("   "));
            action.extend(key_chip("c", "Copy URL"));
            rows.push((REQUIRED, Line::from(action)));
            rows.push((
                TIER_ECHOED,
                Line::from(Span::styled(
                    "  launches the host's default browser · host only".to_string(),
                    dim,
                )),
            ));
            spacer(&mut rows);
            rows.push((
                REQUIRED,
                Line::from(vec![
                    Span::styled("url  ".to_string(), dim),
                    Span::styled(view.url.clone(), Style::default().fg(Color::White)),
                ]),
            ));
            // Never "already authenticated": the URL as drawn carries no
            // credential, and a second browser opening it would be asked for a
            // code. What `c` puts on the clipboard is a different string, and
            // this says so rather than letting the row imply otherwise.
            rows.push((
                TIER_PROSE,
                Line::from(Span::styled(
                    "     c copies it with a one-time code attached".to_string(),
                    dim,
                )),
            ));
            spacer(&mut rows);
            let mut door = vec![Span::styled(
                "▸".to_string(),
                Style::default().fg(Color::Cyan),
            )];
            door.extend(key_chip(
                "n",
                "Allow other devices on this network to connect",
            ));
            rows.push((REQUIRED, Line::from(door)));
            for line in wrap_message(
                "Rebinds to 0.0.0.0, then asks which address to publish and issues a scannable \
                 code. Reaching FlightDeck from outside this network is your own tunnel — this \
                 switch does not do it.",
                w.saturating_sub(4).max(1) as usize,
            ) {
                rows.push((
                    TIER_PROSE,
                    Line::from(Span::styled(format!("    {line}"), dim)),
                ));
            }
        }
        AccessMode::Network => {
            if l.show_qr {
                let art = Style::default().fg(Color::Black).bg(Color::White);
                for row in &view.qr_rows {
                    rows.push((REQUIRED, Line::from(Span::styled(center_pad(row, w), art))));
                }
            }
            for note in &l.note {
                rows.push((
                    REQUIRED,
                    Line::from(Span::styled(
                        note.clone(),
                        Style::default().fg(Color::Yellow),
                    )),
                ));
            }
            match (&view.code, view.code_hidden, view.code_expired) {
                (Some(code), _, _) => {
                    // The terminal's answer to artboard 2a's 30px letterspaced
                    // numerals: spaced digits, centred, in the brightest tier.
                    let spaced: String = code
                        .chars()
                        .flat_map(|c| [c, ' '])
                        .collect::<String>()
                        .trim_end()
                        .to_string();
                    rows.push((
                        REQUIRED,
                        Line::from(Span::styled(center_pad(&spaced, w), bright)),
                    ));
                    if let Some(secs) = view.seconds_remaining {
                        rows.push((
                            REQUIRED,
                            Line::from(Span::styled(
                                center_pad(&format!("expires in {secs}s"), w),
                                Style::default().fg(Color::Yellow),
                            )),
                        ));
                    }
                }
                (None, true, _) => rows.push((
                    REQUIRED,
                    Line::from(Span::styled(
                        center_pad("code and QR hidden — r to show", w),
                        dim,
                    )),
                )),
                (None, false, true) => rows.push((
                    REQUIRED,
                    Line::from(Span::styled(
                        center_pad("code expired — Space for a new one", w),
                        Style::default().fg(Color::Yellow),
                    )),
                )),
                (None, false, false) => {}
            }
            spacer(&mut rows);
            rows.push((REQUIRED, serving_line(view, mode)));
            for line in wrap_message(&view.exposure_line, w as usize) {
                rows.push((TIER_PROSE, Line::from(Span::styled(line, dim))));
            }
            spacer(&mut rows);
            rows.push((
                TIER_ECHOED,
                Line::from(Span::styled("PUBLISH WHICH ADDRESS".to_string(), dim)),
            ));
            for (idx, addr) in view.addresses.iter().enumerate() {
                let selected = view.selected_address == Some(idx);
                // The published address is required; the alternatives are the
                // picker, and a short terminal shows the choice it made.
                let tier = if selected { REQUIRED } else { TIER_ECHOED };
                let name_style = if selected {
                    Style::default().fg(Color::Cyan)
                } else {
                    dim
                };
                let addr_style = if selected { bright } else { body };
                let mut spans = vec![
                    Span::styled(
                        if selected { "▸ " } else { "  " }.to_string(),
                        Style::default().fg(Color::Cyan),
                    ),
                    Span::styled(format!("‹{}› ", addr.name), name_style),
                    Span::styled(format!("{} ", addr.address), addr_style),
                ];
                if let Some(description) = addr.description {
                    spans.push(Span::styled(format!("· {description}"), dim));
                }
                rows.push((tier, Line::from(spans)));
            }
            if view.addresses.is_empty() {
                rows.push((
                    REQUIRED,
                    Line::from(Span::styled(
                        "  no routable interface found on this host".to_string(),
                        Style::default().fg(Color::Yellow),
                    )),
                ));
            }
            spacer(&mut rows);
            rows.push((
                REQUIRED,
                Line::from(Span::styled(
                    "WHAT YOU ARE ALLOWING".to_string(),
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                )),
            ));
            for line in wrap_message(
                "Anyone on this network who has the code can read your repositories, type into \
                 your agents, and push branches.",
                w as usize,
            ) {
                rows.push((TIER_PROSE, Line::from(Span::styled(line, body))));
            }
            for line in wrap_message(
                "The code lasts 120s and buys a cookie in one browser. Revoke it and that \
                 browser is locked out on its next request.",
                w as usize,
            ) {
                rows.push((TIER_PROSE, Line::from(Span::styled(line, dim))));
            }
            spacer(&mut rows);
            let mut actions = key_chip("l", "Back to local only");
            actions.push(Span::raw("   "));
            actions.extend(key_chip("x", "Revoke browser access"));
            rows.push((TIER_ECHOED, Line::from(actions)));
            rows.push((TIER_ECHOED, browsers_line(&view.browsers)));
            for browser in &view.browsers {
                rows.push((TIER_ECHOED, browser_row_line(browser)));
            }
        }
    }

    if let Some(notice) = &view.notice {
        spacer(&mut rows);
        for line in wrap_message(notice, w as usize) {
            rows.push((
                TIER_PROSE,
                Line::from(Span::styled(line, Style::default().fg(Color::Cyan))),
            ));
        }
    }
    spacer(&mut rows);
    rows.push((REQUIRED, key_legend(&view.keys)));
    rows
}

/// The `● serving │ <addr>` header, in the host's words.
///
/// The exposure clause the artboard puts on the same line is a row of its own
/// here: the artboard's overlay is 780 CSS pixels wide and this one has to be
/// legible in 80 columns, where `● serving │ 127.0.0.1:7420 │ loopback only —
/// nothing off this machine can reach it` would be clipped mid-sentence. A
/// clause that stops halfway is worse than a clause on the next line.
fn serving_line(view: &WebAccessView, mode: AccessMode) -> Line<'static> {
    let (dot, dot_style, label) = match mode {
        AccessMode::LocalOnly => ("● ", Style::default().fg(Color::Green), "serving"),
        // Amber, the one token turn 2 reserved for a state that must never be
        // mistaken for the calm one.
        AccessMode::Network => (
            "● ",
            Style::default().fg(Color::Yellow),
            "serving on this network",
        ),
    };
    Line::from(vec![
        Span::styled(dot.to_string(), dot_style),
        Span::styled(
            label.to_string(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" │ ".to_string(), Style::default().fg(Color::DarkGray)),
        Span::styled(view.bound.clone(), Style::default().fg(Color::Gray)),
    ])
}

/// How many browsers hold access, counted rather than rounded to "some" — and
/// which digits withdraw one of them (`remote-control-gk94`, §6.5 R25).
///
/// The `1-n` hint lives here rather than in the footer legend because it is
/// only true of the rows underneath it: those rows are an echoed tier that a
/// short terminal drops, and the range names exactly the digits that are bound,
/// so a tenth browser — listed, revocable by `x`, but past the last digit —
/// cannot be implied to have a key it does not have.
fn browsers_line(browsers: &[crate::web::access::BrowserRow]) -> Line<'static> {
    let keyed = browsers.iter().filter(|b| b.key.is_some()).count();
    let (dot_style, text) = match browsers.len() {
        0 => (
            Style::default().fg(Color::DarkGray),
            "no browser holds access".to_string(),
        ),
        1 => (
            Style::default().fg(Color::Green),
            "1 browser holds access — 1 revokes it".to_string(),
        ),
        n => (
            Style::default().fg(Color::Green),
            format!("{n} browsers hold access — 1-{keyed} revokes one"),
        ),
    };
    Line::from(vec![
        Span::styled("● ".to_string(), dot_style),
        Span::styled(text, Style::default().fg(Color::DarkGray)),
    ])
}

/// One holder of access, as artboard 2a State B draws it: the digit that
/// revokes it, then the facts that tell it from an intruder
/// (`remote-control-gk94`, `specs/WEB_INTERFACE.md` §6.5 R25).
///
/// The pieces are joined with the same ` · ` the rest of the overlay uses and
/// each is drawn only when the host actually has it — a record from before the
/// address was stored is drawn short, never with a placeholder standing in for
/// something nobody observed.
fn browser_row_line(browser: &crate::web::access::BrowserRow) -> Line<'static> {
    let dim = Style::default().fg(Color::DarkGray);
    let mut spans = vec![Span::styled(
        match browser.key {
            // Two spaces where the digit would be, so an unkeyed tenth row
            // still lines up under the ones above it.
            None => "    ".to_string(),
            Some(key) => format!("  {key} "),
        },
        Style::default().fg(Color::Yellow),
    )];
    let mut facts: Vec<String> = Vec::new();
    if let Some(address) = &browser.address {
        facts.push(address.clone());
    }
    if let Some(label) = &browser.browser {
        facts.push(label.clone());
    }
    facts.push(crate::web::access::age_label(browser.granted_secs_ago));
    spans.push(Span::styled(facts.join(" · "), dim));
    Line::from(spans)
}

/// The footer legend: every key the current state binds, and nothing it does
/// not.
fn key_legend(keys: &[(&'static str, &'static str)]) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    for (idx, (key, label)) in keys.iter().enumerate() {
        if idx > 0 {
            spans.push(Span::styled(
                " · ".to_string(),
                Style::default().fg(Color::DarkGray),
            ));
        }
        spans.push(Span::styled(
            (*key).to_string(),
            Style::default().fg(Color::Yellow),
        ));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            (*label).to_string(),
            Style::default().fg(Color::DarkGray),
        ));
    }
    Line::from(spans)
}

/// Draw the configuration manager overlay (SPECS §8): a scope selector, the
/// file being edited, the curated toggles/choices, and the key legend.
pub fn draw_config_overlay(frame: &mut Frame, manager: &ConfigManager, area: Rect) {
    use crate::tui::config_manager::ConfigScope;

    let scope = manager.scope();
    let accent = Color::Cyan;

    // Scope selector line: the active scope is highlighted; the project scope
    // names the project so it is always clear what is being edited.
    let scope_style = |on: bool| {
        if on {
            Style::default()
                .fg(Color::Black)
                .bg(accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        }
    };
    let project_label = format!(" Project ({}) ", manager.project_name());

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled("Scope:  ", Style::default().fg(Color::White)),
        Span::styled(" Global ", scope_style(scope == ConfigScope::Global)),
        Span::raw("  "),
        Span::styled(project_label, scope_style(scope == ConfigScope::Project)),
    ]));

    // The file being edited (so the target is unambiguous).
    let path_str = manager
        .current_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "(no home dir — global config unavailable)".to_string());
    lines.push(Line::from(vec![
        Span::styled("Editing: ", Style::default().fg(Color::DarkGray)),
        Span::styled(path_str, Style::default().fg(Color::Gray)),
    ]));
    lines.push(Line::raw(""));

    // Curated rows.
    for row in manager.rows() {
        let marker = if row.selected { "▸ " } else { "  " };
        let name_style = if row.selected {
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        let origin_style = match row.origin {
            Origin::SetHere => Style::default().fg(Color::Green),
            Origin::Global => Style::default().fg(Color::Blue),
            Origin::Default => Style::default().fg(Color::DarkGray),
        };
        if row.is_text {
            // A free-text field (e.g. the relay URL): the value can be long, so
            // render it after the label rather than in the fixed control column.
            // When editing, append a block cursor and highlight the value.
            let value_style = if row.editing {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Cyan)
            };
            let value = if row.editing {
                format!("{}█", row.value)
            } else {
                row.value.clone()
            };
            lines.push(Line::from(vec![
                Span::styled(marker, Style::default().fg(accent)),
                Span::styled(format!("{:<22}", row.label), name_style),
                Span::styled(value, value_style),
                Span::raw(" "),
                Span::styled(format!("({})", row.origin.label()), origin_style),
            ]));
        } else {
            let control = if row.is_bool {
                if row.bool_value {
                    "[x]".to_string()
                } else {
                    "[ ]".to_string()
                }
            } else {
                format!("‹{}›", row.value)
            };
            lines.push(Line::from(vec![
                Span::styled(marker, Style::default().fg(accent)),
                Span::styled(format!("{control:<8} "), Style::default().fg(Color::Cyan)),
                Span::styled(format!("{:<22}", row.label), name_style),
                Span::styled(format!("({})", row.origin.label()), origin_style),
            ]));
        }
    }

    // A standing note that the default relay is private, so users understand why
    // enabling Remote against it won't connect (mirrors the config-file comment).
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        "Note: the default relay (relay.flightdeckai.app) is restricted and not",
        Style::default().fg(Color::Yellow),
    )));
    lines.push(Line::from(Span::styled(
        "publicly accessible. Point Relay URL at your own relay to use Remote",
        Style::default().fg(Color::Yellow),
    )));
    lines.push(Line::from(Span::styled(
        "(self-hosting is unsupported). See https://flightdeckai.app/remote",
        Style::default().fg(Color::Yellow),
    )));

    lines.push(Line::raw(""));
    if let Some(status) = manager.status() {
        lines.push(Line::from(Span::styled(
            status.to_string(),
            Style::default().fg(Color::Green),
        )));
    } else if manager.dirty() {
        lines.push(Line::from(Span::styled(
            "Unsaved changes",
            Style::default().fg(Color::Yellow),
        )));
    } else {
        lines.push(Line::raw(""));
    }

    lines.push(Line::raw(""));
    if manager.is_editing() {
        lines.push(Line::from(Span::styled(
            "Type to edit   Enter save value   Esc cancel   Backspace delete",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            "↑↓ move   Space toggle / edit   Tab switch scope   c clear override",
            Style::default().fg(Color::DarkGray),
        )));
        lines.push(Line::from(Span::styled(
            "s save   e edit file in $EDITOR   Esc close",
            Style::default().fg(Color::DarkGray),
        )));
    }

    // Fit the box to its content instead of stretching it to a fixed height.
    // centered_overlay still clamps it when the terminal is shorter.
    let content_height = u16::try_from(lines.len()).unwrap_or(u16::MAX);
    let overlay_area = layout::centered_overlay(area, 66, content_height.saturating_add(2));
    frame.render_widget(Clear, overlay_area);

    let block = Block::default()
        .title(" Configuration ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(accent));
    let para = Paragraph::new(lines).block(block);
    frame.render_widget(para, overlay_area);
}

/// Draw the About dialog: version, one-line description, and authorship credits.
pub fn draw_about_overlay(frame: &mut Frame, area: Rect) {
    let accent = Color::Cyan;
    let doc = crate::tui::help::about_doc();
    let mut lines: Vec<Line> = vec![
        Line::from(Span::styled(
            format!("{}  v{}", doc.name, doc.version),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )),
        Line::raw(""),
        Line::from(Span::styled(
            doc.tagline.clone(),
            Style::default().fg(Color::Gray),
        )),
        Line::raw(""),
    ];
    for credit in &doc.credits {
        lines.push(Line::from(vec![
            Span::styled(
                format!("{} ", credit.role),
                Style::default().fg(Color::Gray),
            ),
            Span::styled(
                credit.name.clone(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        doc.url.clone(),
        Style::default().fg(accent),
    )));
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        "Esc / q to close",
        Style::default().fg(Color::DarkGray),
    )));

    let content_height = u16::try_from(lines.len()).unwrap_or(u16::MAX);
    let overlay_area = layout::centered_overlay(area, 62, content_height.saturating_add(2));
    frame.render_widget(Clear, overlay_area);

    let block = Block::default()
        .title(" About FlightDeck ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(accent));
    let para = Paragraph::new(lines)
        .alignment(Alignment::Center)
        .block(block);
    frame.render_widget(para, overlay_area);
}

/// Build a shortcut description line for the help overlay.
fn shortcut_line(keys: String, desc: String) -> Line<'static> {
    Line::from(vec![
        Span::styled(keys, Style::default().fg(Color::Cyan)),
        Span::raw("  "),
        Span::styled(desc, Style::default().fg(Color::Gray)),
    ])
}

// ---------------------------------------------------------------------------
// Modal dialog overlay (confirmations & notifications)
// ---------------------------------------------------------------------------

/// The computed geometry of a [`Dialog`], shared by rendering and hit-testing so
/// the clickable button regions always match what is drawn.
struct DialogLayout {
    /// The full dialog box (border included).
    rect: Rect,
    /// The content region inside the border and one column of padding.
    inner: Rect,
    /// Title text, pre-wrapped to the content width.
    title_lines: Vec<String>,
    /// D13's origin label, pre-wrapped. Empty when the dialog has no origin.
    origin_lines: Vec<String>,
    /// Screen rect of each button, aligned with `Dialog::buttons`.
    button_rects: Vec<Rect>,
}

/// Maximum number of list rows rendered in a dialog before it windows around
/// the selected item (so a folder with hundreds of subdirs stays compact).
const MAX_DIALOG_LIST_ROWS: usize = 10;

/// The list rows actually shown, windowed around the selected item when the
/// full list exceeds [`MAX_DIALOG_LIST_ROWS`]. Shared by layout and drawing so
/// the two never disagree on height.
fn windowed_list(dialog: &Dialog) -> Vec<DialogListItem> {
    let n = dialog.list.len();
    if n <= MAX_DIALOG_LIST_ROWS {
        return dialog.list.clone();
    }
    let sel = dialog.list.iter().position(|i| i.selected).unwrap_or(0);
    let half = MAX_DIALOG_LIST_ROWS / 2;
    let start = sel.saturating_sub(half).min(n - MAX_DIALOG_LIST_ROWS);
    dialog.list[start..start + MAX_DIALOG_LIST_ROWS].to_vec()
}

/// Compute the centered geometry for `dialog` within `area`.
fn layout_dialog(area: Rect, dialog: &Dialog) -> DialogLayout {
    const GAP: u16 = 1;
    // Cap the content width so the box stays comfortably inside the screen.
    let cap_w = area.width.saturating_sub(6).clamp(16, 72);

    // Base width on the longest title line, the input field, and the buttons.
    let title_probe = wrap_message(&dialog.title, cap_w as usize);
    let mut content_w = title_probe
        .iter()
        .map(|l| l.chars().count() as u16)
        .max()
        .unwrap_or(0);
    // D13's origin line widens the box like any other content: truncating the
    // one sentence that explains why this modal appeared would defeat it.
    if let Some(origin) = &dialog.origin {
        for line in wrap_message(origin, cap_w as usize) {
            content_w = content_w.max(line.chars().count() as u16);
        }
    }
    if let Some(inp) = &dialog.input {
        // "> " prefix + text + cursor.
        content_w = content_w.max(inp.chars().count() as u16 + 4).max(24);
    }
    let vlist = windowed_list(dialog);
    for it in &vlist {
        // "▸ " marker + label.
        content_w = content_w.max(it.label.chars().count() as u16 + 2);
    }
    let widest_btn = dialog.buttons.iter().map(|b| b.width()).max().unwrap_or(0);
    content_w = content_w.max(widest_btn);
    // Prefer fitting all buttons on one row when they reasonably fit.
    let one_row: u16 = dialog
        .buttons
        .iter()
        .map(|b| b.width())
        .sum::<u16>()
        .saturating_add(GAP * dialog.buttons.len().saturating_sub(1) as u16);
    content_w = content_w.max(one_row.min(cap_w)).clamp(1, cap_w);

    let title_lines = wrap_message(&dialog.title, content_w as usize);
    let origin_lines = match &dialog.origin {
        Some(origin) => wrap_message(origin, content_w as usize),
        None => Vec::new(),
    };

    // Pack buttons greedily into rows within the content width.
    let mut rows: Vec<Vec<usize>> = Vec::new();
    let mut cur: Vec<usize> = Vec::new();
    let mut cur_w = 0u16;
    for (i, b) in dialog.buttons.iter().enumerate() {
        let bw = b.width();
        let projected = if cur.is_empty() { bw } else { cur_w + GAP + bw };
        if !cur.is_empty() && projected > content_w {
            rows.push(std::mem::take(&mut cur));
            cur_w = 0;
        }
        cur_w = if cur.is_empty() { bw } else { cur_w + GAP + bw };
        cur.push(i);
    }
    if !cur.is_empty() {
        rows.push(cur);
    }

    // Inner height: title + (origin) + (blank + list) + (blank + input) +
    // (blank + buttons).
    let mut inner_h = title_lines.len() as u16;
    if !origin_lines.is_empty() {
        inner_h += origin_lines.len() as u16;
    }
    if !vlist.is_empty() {
        inner_h += 1 + vlist.len() as u16;
    }
    if dialog.input.is_some() {
        inner_h += 2;
    }
    if !rows.is_empty() {
        inner_h += 1 + rows.len() as u16;
    }

    // Box = content + 1 col padding + 1 col border on each side; + border/pad rows.
    let box_w = content_w + 4;
    let box_h = inner_h + 4;
    let rect = layout::centered_overlay(area, box_w, box_h);
    let inner = Rect::new(
        rect.x + 2,
        rect.y + 2,
        rect.width.saturating_sub(4),
        rect.height.saturating_sub(4),
    );

    // Button rects: below the title, origin, list, and input, each row centered.
    let mut y = inner.y + title_lines.len() as u16 + origin_lines.len() as u16;
    if !vlist.is_empty() {
        y += 1 + vlist.len() as u16;
    }
    if dialog.input.is_some() {
        y += 2;
    }
    if !rows.is_empty() {
        y += 1; // blank separator row
    }
    let mut button_rects = vec![Rect::new(0, 0, 0, 0); dialog.buttons.len()];
    for row in &rows {
        let row_w: u16 = row.iter().map(|&i| dialog.buttons[i].width()).sum::<u16>()
            + GAP * row.len().saturating_sub(1) as u16;
        let mut x = inner.x + inner.width.saturating_sub(row_w) / 2;
        for &i in row {
            let bw = dialog.buttons[i].width();
            button_rects[i] = Rect::new(x, y, bw, 1);
            x += bw + GAP;
        }
        y += 1;
    }

    DialogLayout {
        rect,
        inner,
        title_lines,
        origin_lines,
        button_rects,
    }
}

/// Resolve a click at `(col, row)` against an open `dialog`.
pub fn dialog_hit(area: Rect, dialog: &Dialog, col: u16, row: u16) -> DialogHit {
    let dl = layout_dialog(area, dialog);
    for (i, r) in dl.button_rects.iter().enumerate() {
        if rect_contains(*r, col, row) {
            return DialogHit::Button(i);
        }
    }
    if rect_contains(dl.rect, col, row) {
        DialogHit::Inside
    } else {
        DialogHit::Outside
    }
}

/// Draw a centered modal dialog (confirmation / notification) over the UI.
pub fn draw_dialog(frame: &mut Frame, dialog: &Dialog, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let dl = layout_dialog(area, dialog);
    frame.render_widget(Clear, dl.rect);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(dialog.accent))
        .title(Span::styled(
            " FlightDeck ",
            Style::default()
                .fg(dialog.accent)
                .add_modifier(Modifier::BOLD),
        ));
    frame.render_widget(block, dl.rect);

    let mut y = dl.inner.y;
    // Title lines.
    for line in &dl.title_lines {
        let rect = Rect::new(dl.inner.x, y, dl.inner.width, 1);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                line.clone(),
                Style::default().fg(Color::White),
            ))),
            rect,
        );
        y += 1;
    }
    // D13's origin label, directly under the title and before everything the
    // user might act on: they should know *why* this modal is here before they
    // read what it is asking. Magenta is the "another actor acted" hue this
    // codebase already uses for a remote surface having done something.
    for line in &dl.origin_lines {
        let rect = Rect::new(dl.inner.x, y, dl.inner.width, 1);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                line.clone(),
                Style::default().fg(Color::Magenta),
            ))),
            rect,
        );
        y += 1;
    }
    // Scrollable list (e.g. the folder browser), if any.
    let vlist = windowed_list(dialog);
    if !vlist.is_empty() {
        y += 1; // blank separator
        for it in &vlist {
            let rect = Rect::new(dl.inner.x, y, dl.inner.width, 1);
            let style = if it.selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            let marker = if it.selected { "▸ " } else { "  " };
            let budget = dl.inner.width.saturating_sub(2) as usize;
            let label = truncate_ellipsis(&it.label, budget);
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(format!("{marker}{label}"), style))),
                rect,
            );
            y += 1;
        }
    }
    // Input field, if any.
    if let Some(buffer) = &dialog.input {
        y += 1; // blank separator
        let rect = Rect::new(dl.inner.x, y, dl.inner.width, 1);
        let line = Line::from(vec![
            Span::styled("> ", Style::default().fg(dialog.accent)),
            Span::styled(buffer.clone(), Style::default().fg(Color::White)),
            Span::styled("_", Style::default().fg(dialog.accent)),
        ]);
        frame.render_widget(Paragraph::new(line), rect);
    }
    // Buttons.
    for (i, button) in dialog.buttons.iter().enumerate() {
        let rect = dl.button_rects[i];
        if rect.width == 0 {
            continue;
        }
        let base = Style::default().bg(Color::DarkGray).fg(Color::White);
        let line = Line::from(vec![
            Span::styled(" [", base),
            Span::styled(
                button.accel.key_label(),
                base.fg(Color::Yellow).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("] {} ", button.label), base),
        ]);
        frame.render_widget(Paragraph::new(line).style(base), rect);
    }
}

/// Greedily word-wrap `msg` to lines of at most `width` display columns. Words
/// longer than `width` are hard-split so a single long token never overflows.
fn wrap_message(msg: &str, width: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    for word in msg.split_whitespace() {
        let word_len = word.chars().count();
        if word_len > width {
            // Flush the current line, then hard-split the oversized word.
            if !cur.is_empty() {
                lines.push(std::mem::take(&mut cur));
            }
            let mut chunk = String::new();
            for ch in word.chars() {
                if chunk.chars().count() == width {
                    lines.push(std::mem::take(&mut chunk));
                }
                chunk.push(ch);
            }
            cur = chunk;
            continue;
        }
        let cur_len = cur.chars().count();
        let needed = if cur.is_empty() {
            word_len
        } else {
            cur_len + 1 + word_len
        };
        if needed > width {
            lines.push(std::mem::take(&mut cur));
            cur.push_str(word);
        } else {
            if !cur.is_empty() {
                cur.push(' ');
            }
            cur.push_str(word);
        }
    }
    if !cur.is_empty() || lines.is_empty() {
        lines.push(cur);
    }
    lines
}

// ---------------------------------------------------------------------------
// Tests (SPECS §26)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::path::PathBuf;

    use crate::app::modes::InputMode;
    use crate::contracts::Config;
    use crate::persistence::project_state::default_state;

    fn test_terminal(width: u16, height: u16) -> Terminal<TestBackend> {
        let backend = TestBackend::new(width, height);
        Terminal::new(backend).unwrap()
    }

    fn empty_state() -> AppState {
        AppState::new(
            Config::default(),
            default_state("main"),
            "/repo",
            "/repo/state.json",
        )
    }

    fn empty_cache() -> GitStatusCache {
        GitStatusCache::new()
    }

    fn state_with_tabs(n: usize) -> AppState {
        let mut ps = default_state("main");
        for i in 0..n {
            ps.tabs.push(crate::contracts::TabState {
                id: format!("t{i}"),
                name: format!("tab{i}"),
                slug: format!("tab{i}"),
                agent: "opencode".to_string(),
                branch: format!("flightdeck/tab{i}"),
                worktree_path_relative: format!(".flightdeck/worktrees/tab{i}"),
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
            });
        }
        AppState::new(Config::default(), ps, "/repo", "/repo/state.json")
    }

    // --- Mouse hit-testing (clickable tabs) ------------------------------

    #[test]
    fn hit_test_maps_sidebar_rows_to_agent_tabs() {
        let state = state_with_tabs(2);
        let area = Rect::new(0, 0, 80, 24);
        // Rows 0-2 are the logo header, project tab row, and divider; row 3 is
        // the sidebar's "Agents" heading. Tab 0 occupies rows 4..=7, tab 1 8..=11.
        assert_eq!(hit_test(area, &state, 2, 4), Some(HitTarget::AgentTab(0)));
        assert_eq!(hit_test(area, &state, 2, 7), Some(HitTarget::AgentTab(0)));
        assert_eq!(hit_test(area, &state, 2, 8), Some(HitTarget::AgentTab(1)));
        // The header band sits above the sidebar and selects nothing.
        assert_eq!(hit_test(area, &state, 2, 0), None);
        // The sidebar heading (and any non-tab sidebar row) resolves to the
        // sidebar chrome, so the click still focuses the app (SPECS §23).
        assert_eq!(hit_test(area, &state, 2, 3), Some(HitTarget::Sidebar));
    }

    #[test]
    fn hit_test_empty_sidebar_resolves_to_chrome() {
        // With no agents, a click anywhere in the sidebar (heading or the empty
        // space below it) still resolves to the sidebar chrome so APP mode is
        // reachable by clicking the left panel (SPECS §23).
        let state = state_with_tabs(0);
        let area = Rect::new(0, 0, 80, 24);
        assert_eq!(hit_test(area, &state, 2, 3), Some(HitTarget::Sidebar));
        assert_eq!(hit_test(area, &state, 2, 6), Some(HitTarget::Sidebar));
    }

    // --- Collapsed chrome (small windows in terminal mode) -----------------

    /// Read the glyph in the first column of `row` from a rendered buffer.
    fn strip_glyph(buffer: &ratatui::buffer::Buffer, row: u16) -> String {
        buffer[(0, row)].symbol().to_string()
    }

    /// Read a full row of a rendered buffer as a string.
    fn buffer_row(buffer: &ratatui::buffer::Buffer, row: u16) -> String {
        (0..buffer.area.width)
            .map(|x| buffer[(x, row)].symbol().to_string())
            .collect()
    }

    #[test]
    fn collapsed_sidebar_draws_one_glyph_per_agent() {
        let mut state = state_with_tabs(3);
        state.selected_tab = Some(1);
        // Tab 2 is mid-creation, so it must show a spinner rather than a dot.
        state.tabs[2].phase = TabPhase::Creating;

        let mut term = test_terminal(layout::COLLAPSED_SIDEBAR_WIDTH, 6);
        term.draw(|frame| {
            draw_sidebar(
                frame,
                &state,
                &empty_cache(),
                Rect::new(0, 0, layout::COLLAPSED_SIDEBAR_WIDTH, 6),
                layout::Chrome::Collapsed,
                0,
            )
        })
        .unwrap();
        let buffer = term.backend().buffer().clone();

        // Row 0 is tab 0 (idle → dot), row 1 the selection arrow, row 2 a spinner.
        assert_eq!(strip_glyph(&buffer, 0), "●");
        assert_eq!(strip_glyph(&buffer, 1), "▸");
        assert_eq!(
            strip_glyph(&buffer, 2),
            spinner_frame(0).to_string(),
            "a tab being created shows a spinner, not a status dot"
        );
        // No heading: there is no room for the word "Agents".
        let all: String = (0..6_u16)
            .flat_map(|y| (0..layout::COLLAPSED_SIDEBAR_WIDTH).map(move |x| (x, y)))
            .map(|(x, y)| buffer[(x, y)].symbol().to_string())
            .collect();
        assert!(!all.contains("Agents"));
    }

    #[test]
    fn collapsed_sidebar_hit_maps_one_row_per_agent_and_never_closes() {
        let area = Rect::new(0, 0, layout::COLLAPSED_SIDEBAR_WIDTH, 6);
        // One row per tab, starting at the sidebar's first row — no heading offset.
        assert_eq!(
            sidebar_hit(
                area,
                3,
                layout::Chrome::Collapsed,
                crate::contracts::AgentTabPosition::Left,
                0,
                0
            ),
            Some(HitTarget::AgentTab(0))
        );
        assert_eq!(
            sidebar_hit(
                area,
                3,
                layout::Chrome::Collapsed,
                crate::contracts::AgentTabPosition::Left,
                0,
                2
            ),
            Some(HitTarget::AgentTab(2))
        );
        // Past the last agent resolves to nothing (the caller falls back to chrome).
        assert_eq!(
            sidebar_hit(
                area,
                3,
                layout::Chrome::Collapsed,
                crate::contracts::AgentTabPosition::Left,
                0,
                3
            ),
            None
        );
        // The rightmost inner column selects; it is never a close control.
        assert_eq!(
            sidebar_hit(
                area,
                3,
                layout::Chrome::Collapsed,
                crate::contracts::AgentTabPosition::Left,
                1,
                1
            ),
            Some(HitTarget::AgentTab(1))
        );
    }

    /// A window below both thresholds, so terminal mode collapses the chrome.
    const SMALL: (u16, u16) = (100, 24);

    #[test]
    fn small_window_in_terminal_mode_hides_the_info_bar_and_narrows_the_sidebar() {
        let (w, h) = SMALL;
        let mut state = state_with_tabs(2);
        state.selected_tab = Some(0);
        state.focus_terminal();

        let mut term = test_terminal(w, h);
        term.draw(|frame| draw(frame, &state, &empty_cache(), &UiOverlay::None, None, 0))
            .unwrap();
        let buffer = term.backend().buffer().clone();
        let all: String = (0..h)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .map(|(x, y)| buffer[(x, y)].symbol().to_string())
            .collect();

        // The sidebar heading and the git info bar are both gone. "⎇" is the
        // info bar's branch marker and appears nowhere else in the main view.
        assert!(!all.contains("Agents"), "collapsed sidebar has no heading");
        assert!(!all.contains('⎇'), "collapsed layout has no git info bar");
        // The compact status bar retains base context and mode cues when the
        // dedicated info row is reclaimed.
        let status = buffer_row(&buffer, h - 1);
        assert!(status.contains("default: main"), "status: {status:?}");
        assert!(status.contains("target: main"), "status: {status:?}");
        assert!(status.contains("MODE: TERMINAL"), "status: {status:?}");
    }

    #[test]
    fn same_small_window_in_app_mode_keeps_full_chrome() {
        let (w, h) = SMALL;
        let mut state = state_with_tabs(2);
        state.selected_tab = Some(0);
        state.focus_app();

        let mut term = test_terminal(w, h);
        term.draw(|frame| draw(frame, &state, &empty_cache(), &UiOverlay::None, None, 0))
            .unwrap();
        let buffer = term.backend().buffer().clone();
        let all: String = (0..h)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .map(|(x, y)| buffer[(x, y)].symbol().to_string())
            .collect();

        assert!(all.contains("Agents"), "app mode keeps the full sidebar");
        assert!(all.contains('⎇'), "app mode keeps the git info bar");
    }

    /// Reproduces the event loop's draw order (src/lib.rs, around line 1253):
    /// `draw_project_tab_bar` is called first with the layout's
    /// `project_tabs` rect, then `draw` is called with the full area — `draw`
    /// recomputes the same chrome and layout internally and paints the
    /// divider row on top of whatever `draw_project_tab_bar` left behind.
    fn draw_composition(
        term: &mut Terminal<TestBackend>,
        state: &AppState,
        projects: &[ProjectTabInfo],
    ) {
        term.draw(|frame| {
            let area = frame.area();
            let chrome = layout::chrome_for(area, state.mode());
            let ml = layout::compute(
                area,
                chrome,
                mode_style::border_enabled(&state.config.ui),
                state.config.ui.agent_tab_side(),
            );
            draw_project_tab_bar(frame, ml.project_tabs, projects, 0, 0);
            draw(frame, state, &empty_cache(), &UiOverlay::None, None, 0);
        })
        .unwrap();
    }

    #[test]
    fn collapsed_composition_leaves_the_divider_row_clean() {
        let (w, h) = SMALL;
        let mut state = state_with_tabs(2);
        state.focus_terminal();
        assert_eq!(
            layout::chrome_for(Rect::new(0, 0, w, h), state.mode()),
            layout::Chrome::Collapsed
        );

        let projects = vec![ProjectTabInfo {
            name: "alpha".to_string(),
            attention: false,
            busy: false,
        }];
        let mut term = test_terminal(w, h);
        draw_composition(&mut term, &state, &projects);
        let buffer = term.backend().buffer().clone();

        // The divider sits at row 1 when collapsed: header row 0, divider row
        // 1, no project tab row in between. No cell in it may carry the
        // "+ project" button's background — that background surviving means
        // the button's fixed-height rect painted through onto the divider
        // underneath it.
        for x in 0..w {
            assert_ne!(
                buffer[(x, 1)].bg,
                PROJECT_TAB_ACTIVE_BG,
                "divider row cell ({x}, 1) carries the project-tab button's background"
            );
        }
    }

    #[test]
    fn full_composition_leaves_the_divider_row_clean() {
        let (w, h) = SMALL;
        let mut state = state_with_tabs(2);
        state.focus_app();
        assert_eq!(
            layout::chrome_for(Rect::new(0, 0, w, h), state.mode()),
            layout::Chrome::Full
        );

        let projects = vec![ProjectTabInfo {
            name: "alpha".to_string(),
            attention: false,
            busy: false,
        }];
        let mut term = test_terminal(w, h);
        draw_composition(&mut term, &state, &projects);
        let buffer = term.backend().buffer().clone();

        // The project tab row (row 1, below the header) renders normally...
        let tab_row: String = (0..w)
            .map(|x| buffer[(x, 1)].symbol().to_string())
            .collect();
        assert!(tab_row.contains("alpha"), "project tab row: {tab_row:?}");
        // ...and the divider row below it (row 2) is still clean.
        for x in 0..w {
            assert_ne!(
                buffer[(x, 2)].bg,
                PROJECT_TAB_ACTIVE_BG,
                "divider row cell ({x}, 2) carries the project-tab button's background"
            );
        }
    }

    #[test]
    fn hit_test_uses_the_collapsed_strip_in_a_small_terminal_mode_window() {
        let (w, h) = SMALL;
        let mut state = state_with_tabs(2);
        state.focus_terminal();
        let area = Rect::new(0, 0, w, h);

        // Body starts at row 2 (header row 0, divider row 1) — no project row.
        assert_eq!(hit_test(area, &state, 0, 2), Some(HitTarget::AgentTab(0)));
        assert_eq!(hit_test(area, &state, 0, 3), Some(HitTarget::AgentTab(1)));
        // The strip's border column is sidebar chrome, not an agent.
        assert_eq!(
            hit_test(area, &state, layout::COLLAPSED_SIDEBAR_WIDTH - 1, 2),
            Some(HitTarget::Sidebar)
        );
        // Just past the strip is the main pane, not the sidebar.
        assert_ne!(
            hit_test(area, &state, layout::COLLAPSED_SIDEBAR_WIDTH, 2),
            Some(HitTarget::Sidebar)
        );

        // App mode restores the full sidebar geometry on the same window.
        state.focus_app();
        assert_eq!(hit_test(area, &state, 0, 2), None);
    }

    #[test]
    fn terminal_viewport_renders_parsed_pty_output() {
        // Regression: the active terminal's PTY output must actually render
        // (previously a placeholder was shown). Spawn a primary, feed it bytes,
        // and assert the text lands in the viewport region of the buffer.
        use crate::contracts::PtySize;
        use crate::testing::FakePty;
        use std::path::Path;

        let pty = FakePty::new();
        pty.queue_session();
        let mut state = state_with_tabs(1);
        state.tabs[0]
            .session
            .spawn_primary(&pty, "agent", &[], Path::new("/wt"), PtySize::default())
            .unwrap();
        state.tabs[0]
            .session
            .primary_mut()
            .unwrap()
            .process_output(b"HELLO_FLIGHTDECK");

        let mut term = test_terminal(80, 24);
        term.draw(|frame| draw(frame, &state, &empty_cache(), &UiOverlay::None, None, 0))
            .unwrap();

        let buffer = term.backend().buffer().clone();
        let all_text: String = (0..24_u16)
            .flat_map(|y| (0..80_u16).map(move |x| (x, y)))
            .map(|(x, y)| buffer[(x, y)].symbol().to_string())
            .collect();
        assert!(
            all_text.contains("HELLO_FLIGHTDECK"),
            "terminal viewport must render parsed PTY output"
        );
    }

    #[test]
    fn hit_test_maps_child_tab_bar_to_primary() {
        let state = state_with_tabs(1);
        let area = Rect::new(0, 0, 80, 24);
        // Child tab bar is the first body row (row 3, below logo + project tabs
        // + divider); the "agent" segment starts at the sidebar width (28).
        assert_eq!(
            hit_test(area, &state, 30, 3),
            Some(HitTarget::Child(ChildTarget::Primary))
        );
    }

    #[test]
    fn child_tab_entries_label_agents_and_shells() {
        use crate::contracts::PtySize;
        use crate::testing::FakePty;
        use std::path::Path;

        let pty = FakePty::new();
        pty.queue_session();
        pty.queue_session();
        let mut state = state_with_tabs(1);
        let session = &mut state.tabs[0].session;
        session
            .spawn_agent_child(&pty, "claude", &[], Path::new("/wt"), PtySize::default())
            .unwrap();
        session
            .spawn_child(&pty, "zsh", &[], Path::new("/wt"), PtySize::default())
            .unwrap();

        let labels: Vec<String> = child_tab_entries(&state)
            .into_iter()
            .map(|(_, l)| l)
            .collect();
        // Primary "agent", the extra agent numbered from 2, the shell from 1.
        assert_eq!(labels, vec!["agent", "agent 2", "shell 1"]);
    }

    #[test]
    fn hit_test_maps_child_tab_close_glyph() {
        let state = state_with_tabs(1);
        let area = Rect::new(0, 0, 80, 24);
        // " agent ✕ " starts at col 28; the ✕ sits at 28 + len("agent") + 2 = 35.
        assert_eq!(
            hit_test(area, &state, 35, 3),
            Some(HitTarget::CloseChild(ChildTarget::Primary))
        );
        // A column just left of the glyph still selects the tab, not close it.
        assert_eq!(
            hit_test(area, &state, 30, 3),
            Some(HitTarget::Child(ChildTarget::Primary))
        );
    }

    #[test]
    fn hit_test_maps_tab_bar_buttons() {
        let state = state_with_tabs(1);
        let area = Rect::new(0, 0, 80, 24);
        // With a tab selected both buttons show, right-aligned: "+ shell" flush
        // right (cols 71..=79), "+ agent" to its left (cols 61..=69).
        assert_eq!(
            hit_test(area, &state, 72, 3),
            Some(HitTarget::NewShellButton)
        );
        assert_eq!(
            hit_test(area, &state, 62, 3),
            Some(HitTarget::NewAgentButton)
        );
    }

    #[test]
    fn hit_test_maps_sidebar_close_glyph() {
        let state = state_with_tabs(2);
        let area = Rect::new(0, 0, 80, 24);
        // Tab 0's name row is row 5; the ✕ occupies the far-right inner columns.
        assert_eq!(
            hit_test(area, &state, 26, 5),
            Some(HitTarget::CloseAgentTab(0))
        );
        // A click on the left of the same row selects the tab instead.
        assert_eq!(hit_test(area, &state, 2, 5), Some(HitTarget::AgentTab(0)));
    }

    // --- Project tab row (multi-project) ---------------------------------

    #[test]
    fn project_tab_hit_test_maps_tabs_close_and_new_button() {
        // Project row sits at y = 1 (row 0 header, row 1 project tabs).
        let area = Rect::new(0, 1, 80, 1);
        let names = vec!["alpha".to_string(), "beta".to_string()];
        // "● alpha" is 7 cols; segment " label ✕ " spans cols 0..11, ✕ at col 9.
        assert_eq!(
            project_tab_hit_test(area, &names, 2, 1),
            Some(ProjectHit::Tab(0))
        );
        assert_eq!(
            project_tab_hit_test(area, &names, 9, 1),
            Some(ProjectHit::Close(0))
        );
        // The "+ project" button is flush right.
        assert_eq!(
            project_tab_hit_test(area, &names, 79, 1),
            Some(ProjectHit::NewButton)
        );
        // A row outside the project tab row resolves to nothing.
        assert_eq!(project_tab_hit_test(area, &names, 2, 0), None);
    }

    #[test]
    fn draw_project_tab_bar_renders_names_and_button() {
        let mut term = test_terminal(80, 3);
        let projects = vec![
            ProjectTabInfo {
                name: "alpha".to_string(),
                attention: false,
                busy: true,
            },
            ProjectTabInfo {
                name: "beta".to_string(),
                attention: true,
                busy: false,
            },
        ];
        term.draw(|frame| draw_project_tab_bar(frame, Rect::new(0, 0, 80, 1), &projects, 0, 0))
            .unwrap();
        let buffer = term.backend().buffer().clone();
        let row: String = (0..80)
            .map(|x| buffer[(x, 0)].symbol().to_string())
            .collect();
        assert!(row.contains("alpha"), "first project name: {row:?}");
        assert!(row.contains("beta"), "second project name: {row:?}");
        assert!(row.contains("+ project"), "new-project button: {row:?}");
    }

    #[test]
    fn project_tab_uses_spinner_when_busy_and_green_dot_when_idle() {
        let mut term = test_terminal(80, 1);
        let projects = vec![
            ProjectTabInfo {
                name: "alpha".to_string(),
                attention: false,
                busy: true,
            },
            ProjectTabInfo {
                name: "beta".to_string(),
                attention: false,
                busy: false,
            },
        ];

        term.draw(|frame| draw_project_tab_bar(frame, Rect::new(0, 0, 80, 1), &projects, 0, 0))
            .unwrap();
        let buffer = term.backend().buffer();

        // First indicator is at x=1. The second tab begins after the first
        // segment and separator, placing its indicator at x=15.
        assert_eq!(buffer[(1, 0)].symbol(), "⠋");
        assert_eq!(buffer[(1, 0)].fg, Color::Red);
        assert_eq!(buffer[(2, 0)].bg, Color::White);
        assert_eq!(buffer[(2, 0)].fg, PROJECT_TAB_ACTIVE_BG);
        assert_eq!(buffer[(15, 0)].symbol(), "●");
        assert_eq!(buffer[(15, 0)].fg, Color::Green);
    }

    #[test]
    fn wrap_message_splits_long_text_across_lines() {
        let msg = "Rebase flightdeck/foo onto main then remove the worktree";
        let lines = wrap_message(msg, 20);
        assert!(lines.len() > 1, "long message should wrap");
        assert!(lines.iter().all(|l| l.chars().count() <= 20));
        // Round-trips the words in order.
        assert_eq!(lines.join(" "), msg);
    }

    #[test]
    fn wrap_message_hard_splits_oversized_word() {
        let lines = wrap_message("supercalifragilistic", 5);
        assert!(lines.iter().all(|l| l.chars().count() <= 5));
        assert_eq!(lines.concat(), "supercalifragilistic");
    }

    #[test]
    fn split_view_renders_both_terminals_side_by_side() {
        // In split view the primary agent and a child shell render at the same
        // time, each in its own column.
        use crate::contracts::PtySize;
        use crate::testing::FakePty;
        use std::path::Path;

        let pty = FakePty::new();
        pty.queue_session();
        pty.queue_session();
        let mut state = state_with_tabs(1);
        state.split_view = true;
        let session = &mut state.tabs[0].session;
        session
            .spawn_primary(&pty, "agent", &[], Path::new("/wt"), PtySize::default())
            .unwrap();
        session
            .spawn_child(&pty, "zsh", &[], Path::new("/wt"), PtySize::default())
            .unwrap();
        session.primary_mut().unwrap().process_output(b"AGENT_PANE");
        session.child_mut(0).unwrap().process_output(b"SHELL_PANE");

        let mut term = test_terminal(120, 30);
        term.draw(|frame| draw(frame, &state, &empty_cache(), &UiOverlay::None, None, 0))
            .unwrap();

        let buffer = term.backend().buffer().clone();
        let all_text: String = (0..30_u16)
            .flat_map(|y| (0..120_u16).map(move |x| (x, y)))
            .map(|(x, y)| buffer[(x, y)].symbol().to_string())
            .collect();
        assert!(all_text.contains("AGENT_PANE"), "agent column must render");
        assert!(all_text.contains("SHELL_PANE"), "shell column must render");
    }

    #[test]
    fn hit_test_in_split_view_selects_column() {
        use crate::contracts::PtySize;
        use crate::testing::FakePty;
        use std::path::Path;

        let pty = FakePty::new();
        pty.queue_session();
        pty.queue_session();
        let mut state = state_with_tabs(1);
        state.split_view = true;
        let session = &mut state.tabs[0].session;
        session
            .spawn_primary(&pty, "agent", &[], Path::new("/wt"), PtySize::default())
            .unwrap();
        session
            .spawn_child(&pty, "zsh", &[], Path::new("/wt"), PtySize::default())
            .unwrap();

        let area = Rect::new(0, 0, 120, 30);
        // Two columns over the main pane (x ≥ sidebar width 28). A click on a
        // column's header row switches to that terminal: the left header lands
        // on the agent (primary) column, the right header on the shell column.
        let region = layout::split_region(&layout::compute(
            area,
            layout::Chrome::Full,
            false,
            crate::contracts::AgentTabPosition::Left,
        ));
        let cols = layout::split_columns(region, 2);
        let left = cols[0].col.x + cols[0].col.width / 2;
        let right = cols[1].col.x + cols[1].col.width / 2;
        let header_row = cols[0].header.y;
        assert_eq!(
            hit_test(area, &state, left, header_row),
            Some(HitTarget::Child(ChildTarget::Primary))
        );
        assert_eq!(
            hit_test(area, &state, right, header_row),
            Some(HitTarget::Child(ChildTarget::Child(0)))
        );
        // A click in a column *body* is not a switch target — it begins a text
        // selection instead (handled by the mouse wiring).
        let body_row = cols[0].viewport.y + 1;
        assert_eq!(hit_test(area, &state, left, body_row), None);
        assert_eq!(hit_test(area, &state, right, body_row), None);
    }

    // --- Git info bar (SPECS §21) ----------------------------------------

    fn flatten(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    // --- Branded header (logo) -------------------------------------------

    #[test]
    fn header_uses_wide_logo_when_space_allows() {
        let flat = flatten(&header_line(200));
        assert!(flat.contains("F L I G H T D E C K"), "wide brand: {flat:?}");
        assert!(flat.contains("██████"), "block flourish: {flat:?}");
    }

    #[test]
    fn header_fills_the_full_window_width() {
        // The title bar must span the entire width edge to edge, with blocks
        // running out to both ends and the ░▒▓ ramps framing the wordmark.
        for width in [50usize, 80, 120, 201] {
            let line = header_line(width);
            let flat = flatten(&line);
            assert_eq!(
                flat.chars().count(),
                width,
                "title bar must be exactly {width} cols: {flat:?}"
            );
            assert!(flat.starts_with('█'), "fills to the left edge: {flat:?}");
            assert!(flat.ends_with('█'), "fills to the right edge: {flat:?}");
            assert!(flat.contains("▓▓▓▒▒▒░░░"), "left ramp present: {flat:?}");
        }
    }

    #[test]
    fn header_shrinks_to_narrow_logo_when_tight() {
        // 40 cols fits the narrow logo (brand + ramps) but not the wide one.
        let flat = flatten(&header_line(40));
        assert!(
            flat.contains("F·L·I·G·H·T·D·E·C·K"),
            "narrow brand: {flat:?}"
        );
        assert!(
            !flat.contains("F L I G H T"),
            "must not be the wide brand: {flat:?}"
        );
        assert!(flat.contains("▓▓▓▒▒▒░░░"), "block ramp: {flat:?}");
    }

    #[test]
    fn header_falls_back_to_truncated_brand_when_very_narrow() {
        let flat = flatten(&header_line(8));
        assert_eq!(flat, "FLIGHTDE", "8-col fallback: {flat:?}");
    }

    #[test]
    fn header_and_divider_render_on_top_rows() {
        let state = state_with_tabs(1);
        let mut term = test_terminal(120, 24);
        term.draw(|frame| draw(frame, &state, &empty_cache(), &UiOverlay::None, None, 0))
            .unwrap();
        let buffer = term.backend().buffer().clone();
        let row0: String = (0..120)
            .map(|x| buffer[(x, 0)].symbol().to_string())
            .collect();
        // Row 1 is the project tab row (drawn by the wiring layer, not `draw`);
        // row 2 is the divider.
        let row2: String = (0..120)
            .map(|x| buffer[(x, 2)].symbol().to_string())
            .collect();
        // The logo (block flourish + brand) sits on the very first row.
        assert!(row0.contains("██████"), "logo row: {row0:?}");
        assert!(row0.contains("F L I G H T"), "brand on logo row: {row0:?}");
        // The divider fills the third row (below the header + project tab row).
        assert!(
            row2.chars().filter(|&c| c == '─').count() > 100,
            "divider row should be a full-width rule: {row2:?}"
        );
    }

    #[test]
    fn info_bar_without_selection_says_no_tab() {
        let state = empty_state();
        let flat = flatten(&info_bar_line(&state, &empty_cache()));
        assert!(flat.contains("Default base: main"), "got: {flat:?}");
        assert!(
            flat.contains("No Agent Session Tab selected"),
            "got: {flat:?}"
        );
    }

    #[test]
    fn info_bar_without_cache_shows_branch_and_unknown_git() {
        let state = state_with_tabs(1);
        let flat = flatten(&info_bar_line(&state, &empty_cache()));
        assert!(flat.contains("flightdeck/tab0"), "branch missing: {flat:?}");
        assert!(flat.contains("git: ?"), "unknown marker missing: {flat:?}");
        assert!(flat.contains("target: main"), "target missing: {flat:?}");
    }

    #[test]
    fn info_bar_distinguishes_tab_target_from_project_default() {
        let mut state = state_with_tabs(1);
        state.base_branch = "develop".to_string();
        let flat = flatten(&info_bar_line(&state, &empty_cache()));
        assert!(flat.contains("target: main"), "target missing: {flat:?}");
        assert!(
            flat.contains("default: develop"),
            "project default missing: {flat:?}"
        );
    }

    #[test]
    fn info_bar_marks_an_invalid_project_default() {
        let mut state = state_with_tabs(1);
        state.invalid_base_branch = Some("missing".to_string());
        let flat = flatten(&info_bar_line(&state, &empty_cache()));
        assert!(
            flat.contains("default: missing (not local)"),
            "got: {flat:?}"
        );
    }

    #[test]
    fn invalid_default_is_visible_without_a_selected_tab() {
        let mut state = empty_state();
        state.invalid_base_branch = Some("missing".to_string());
        let flat = flatten(&info_bar_line(&state, &empty_cache()));
        assert!(
            flat.contains("Default base: missing (not local)"),
            "got: {flat:?}"
        );
    }

    #[test]
    fn collapsed_status_keeps_default_and_target_visible_first() {
        let mut state = state_with_tabs(1);
        state.base_branch = "develop".to_string();
        let flat = flatten(&compact_status_bar_text(&state, None, 100));
        assert!(flat.contains("default: develop"), "got: {flat:?}");
        assert!(flat.contains("target: main"), "got: {flat:?}");
    }

    #[test]
    fn collapsed_status_retains_isolated_and_update_indicators() {
        let mut state = state_with_tabs(1);
        state.isolated = true;
        state.update_available = Some("2.0.0".to_string());
        let flat = flatten(&compact_status_bar_text(&state, None, 100));
        assert!(flat.contains("ISOLATED"), "got: {flat:?}");
        assert!(flat.contains("v2.0.0 update"), "got: {flat:?}");
    }

    #[test]
    fn very_narrow_collapsed_status_renders_safety_mode_update_and_default() {
        let mut state = state_with_tabs(1);
        state.focus_terminal();
        state.isolated = true;
        state.update_available = Some("2.0.0".to_string());
        let mut term = test_terminal(43, 24); // 40-column main pane.
        term.draw(|frame| draw(frame, &state, &empty_cache(), &UiOverlay::None, 0))
            .unwrap();
        let row = buffer_row(term.backend().buffer(), 23);
        assert!(row.contains("ISOLATED"), "row: {row:?}");
        assert!(row.contains("TERM"), "row: {row:?}");
        assert!(row.contains("UPDATE"), "row: {row:?}");
        assert!(row.contains("default: main"), "row: {row:?}");
    }

    #[test]
    fn long_base_names_are_shortened_before_required_context_is_clipped() {
        let mut state = state_with_tabs(1);
        state.base_branch = "feature/a-very-long-project-default-branch".to_string();
        state.tabs[0].meta.base_branch = "release/a-very-long-pinned-target-branch".to_string();
        let flat = flatten(&compact_status_bar_text(&state, None, 80));
        assert!(flat.contains("default: featur...-branch"), "got: {flat:?}");
        assert!(flat.contains("target: releas...-branch"), "got: {flat:?}");
        assert!(flat.contains("MODE: TERMINAL"), "got: {flat:?}");
    }

    #[test]
    fn info_bar_shows_branch_and_change_counts() {
        let state = state_with_tabs(1);
        let mut cache = empty_cache();
        cache.insert(
            "t0".to_string(),
            WorktreeStatus {
                branch: "flightdeck/tab0".to_string(),
                base_branch: "main".to_string(),
                dirty: true,
                changes: crate::git::status::WorktreeChanges {
                    added: 1,
                    modified: 2,
                    deleted: 3,
                },
                ahead: 4,
                behind: 5,
                upstream: Some("origin/flightdeck/tab0".to_string()),
                base_drift: 6,
                worktree_path: PathBuf::from("/repo/.flightdeck/worktrees/tab0"),
            },
        );
        let flat = flatten(&info_bar_line(&state, &cache));
        assert!(flat.contains("flightdeck/tab0"), "branch: {flat:?}");
        assert!(flat.contains("+1"), "added: {flat:?}");
        assert!(flat.contains("~2"), "modified: {flat:?}");
        assert!(flat.contains("-3"), "deleted: {flat:?}");
        assert!(flat.contains("(6 files)"), "total: {flat:?}");
        assert!(flat.contains("↑4 ↓5"), "ahead/behind: {flat:?}");
        assert!(flat.contains("target advanced +6"), "movement: {flat:?}");
        assert!(flat.contains("target: main"), "target branch: {flat:?}");
    }

    #[test]
    fn info_bar_clean_worktree_says_clean() {
        let state = state_with_tabs(1);
        let mut cache = empty_cache();
        cache.insert(
            "t0".to_string(),
            WorktreeStatus {
                branch: "flightdeck/tab0".to_string(),
                base_branch: "main".to_string(),
                dirty: false,
                changes: crate::git::status::WorktreeChanges::default(),
                ahead: 0,
                behind: 0,
                upstream: None,
                base_drift: 0,
                worktree_path: PathBuf::from("/repo/.flightdeck/worktrees/tab0"),
            },
        );
        let flat = flatten(&info_bar_line(&state, &cache));
        assert!(flat.contains("clean"), "clean marker: {flat:?}");
        assert!(flat.contains("no upstream"), "upstream marker: {flat:?}");
    }

    #[test]
    fn info_bar_renders_above_status_bar_in_buffer() {
        // The info bar occupies the row just above the bottom status bar.
        let state = state_with_tabs(1);
        let mut cache = empty_cache();
        cache.insert(
            "t0".to_string(),
            WorktreeStatus {
                branch: "flightdeck/tab0".to_string(),
                base_branch: "main".to_string(),
                dirty: true,
                changes: crate::git::status::WorktreeChanges {
                    added: 2,
                    modified: 0,
                    deleted: 0,
                },
                ahead: 0,
                behind: 0,
                upstream: None,
                base_drift: 0,
                worktree_path: PathBuf::from("/repo/.flightdeck/worktrees/tab0"),
            },
        );
        let mut term = test_terminal(80, 24);
        term.draw(|frame| draw(frame, &state, &cache, &UiOverlay::None, None, 0))
            .unwrap();
        let buffer = term.backend().buffer().clone();
        // Layout bottom rows: info_bar (y = 21), status_divider (y = 22),
        // status_bar (y = 23).
        let info_row: String = (0..80)
            .map(|x| buffer[(x, 21)].symbol().to_string())
            .collect();
        assert!(
            info_row.contains("flightdeck/tab0"),
            "info bar row should show the branch, got: {info_row:?}"
        );
        // The divider row sits directly above the status bar.
        let divider_row: String = (0..80)
            .map(|x| buffer[(x, 22)].symbol().to_string())
            .collect();
        assert!(
            divider_row.contains('─'),
            "divider row should be drawn above status bar, got: {divider_row:?}"
        );
    }

    // --- Status bar text (SPECS §23) -------------------------------------

    #[test]
    fn status_bar_terminal_mode_text() {
        let ui = crate::contracts::UiConfig::default();
        let line = status_bar_text(InputMode::Terminal, &ui, None, false, None);
        let flat: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(flat.contains("MODE: TERMINAL"), "must show mode name");
        assert!(
            flat.contains(crate::tui::platform::leave_focus_key(false)),
            "must mention the platform-default leave-focus key"
        );
        assert!(flat.contains("app mode"), "must say app mode");
        assert!(flat.contains("Ctrl-g"), "must mention Ctrl-g");
        assert!(flat.contains("palette"), "must mention palette");
        // Help is global, so terminal focus must advertise it too — this is the
        // mode where a user reaches for help and cannot type '?'.
        assert!(flat.contains(HELP_KEYS), "must mention both help keys");
        assert!(flat.contains("help"), "must say help");
    }

    #[test]
    fn status_bar_help_hint_does_not_vary_by_platform() {
        // F1 and Alt-h are bound on every OS; only their ergonomics differ. A
        // platform-varying label (as leave_focus_key does) would imply a
        // difference in the binding that does not exist.
        for mode in [InputMode::Terminal, InputMode::App] {
            let ui = crate::contracts::UiConfig::default();
            let line = status_bar_text(mode, &ui, None, false, None);
            let flat: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            assert!(flat.contains("F1 / Alt-h"), "{mode:?} must show both keys");
        }
    }

    #[test]
    fn status_bar_help_hint_fits_the_documented_minimum_width() {
        // The bar is a Paragraph with no wrap, so anything past the pane width
        // is silently cut — and the help hint sits last, so it is the first
        // thing lost. The bar lives in the main pane, not the full width, so
        // the budget is (terminal width - SIDEBAR_WIDTH). 102 is the width the
        // widest bar (Terminal mode) needs; this fails if the bar grows.
        let mut term = test_terminal(102, 10);
        let state = empty_state();
        let cache = empty_cache();
        term.draw(|frame| {
            draw(frame, &state, &cache, &UiOverlay::None, None, 0);
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        let text: String = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(
            text.contains(HELP_KEYS),
            "the help hint must fit a 102-column terminal, got: {text}"
        );
    }

    #[test]
    fn status_bar_shows_f2_when_enabled() {
        let ui = crate::contracts::UiConfig {
            use_f2_to_leave_terminal_focus: true,
            ..crate::contracts::UiConfig::default()
        };
        let line = status_bar_text(InputMode::Terminal, &ui, None, false, None);
        let flat: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(flat.contains("F2"));
    }

    #[test]
    fn status_bar_app_mode_text() {
        let ui = crate::contracts::UiConfig::default();
        let line = status_bar_text(InputMode::App, &ui, None, false, None);
        let flat: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(flat.contains("MODE: APP"), "must show mode name");
        assert!(flat.contains("Enter"), "must mention Enter");
        assert!(flat.contains("focus terminal"), "must say focus terminal");
        assert!(flat.contains("Ctrl-g"), "must mention Ctrl-g");
        assert!(flat.contains("palette"), "must mention palette");
        assert!(flat.contains(HELP_KEYS), "must mention both help keys");
        assert!(flat.contains("help"), "must mention help");
        assert!(
            !flat.contains('?'),
            "'?' is no longer a help key and must not be advertised as one"
        );
    }

    #[test]
    fn status_bar_shows_update_hint_when_available() {
        let ui = crate::contracts::UiConfig::default();
        let line = status_bar_text(InputMode::App, &ui, Some("1.0.3"), false, None);
        let flat: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            flat.contains("v1.0.3 available"),
            "must show the new version"
        );
        assert!(
            flat.contains("flightdeck update"),
            "must point at the update command"
        );
        // Absent the notice, the bar is unchanged.
        let none = status_bar_text(InputMode::App, &ui, None, false, None);
        let none_flat: String = none.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(!none_flat.contains("available"), "no hint when up to date");
    }

    /// `specs/WEB_INTERFACE.md` D14 as revised: the desktop is one of the
    /// writers, so it needs the same answer to "who can type" the browser gets.
    ///
    /// This chip is the desktop's whole trace for a refused keystroke. Without
    /// it, typing into another writer's live burst would look exactly like a
    /// broken keyboard.
    #[test]
    fn status_bar_names_whoever_holds_the_input_lock() {
        let ui = crate::contracts::UiConfig::default();
        let held = status_bar_text(
            InputMode::Terminal,
            &ui,
            None,
            false,
            Some("192.168.2.20 · Chrome on macOS"),
        );
        let flat: String = held.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            flat.contains("INPUT: 192.168.2.20 · Chrome on macOS"),
            "the holder must be named, not merely reported as `busy`: {flat}"
        );

        // Nothing to contend with — the web interface is stopped, or no browser
        // is seated as a writer — and the bar says nothing rather than
        // reporting a contest that cannot happen.
        let free = status_bar_text(InputMode::Terminal, &ui, None, false, None);
        let free_flat: String = free.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            !free_flat.contains("INPUT:"),
            "one writer means no chip: {free_flat}"
        );
    }

    #[test]
    fn status_bar_shows_the_isolated_badge() {
        let ui = crate::contracts::UiConfig::default();
        let line = status_bar_text(InputMode::App, &ui, None, true, None);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            text.contains("ISOLATED"),
            "an isolated run must be unmistakable: {text}"
        );
    }

    #[test]
    fn status_bar_has_no_badge_in_a_normal_run() {
        let ui = crate::contracts::UiConfig::default();
        let line = status_bar_text(InputMode::App, &ui, None, false, None);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(!text.contains("ISOLATED"), "no badge normally: {text}");
    }

    #[test]
    fn status_bar_shows_both_the_badge_and_the_update_hint() {
        // The two trailing spans must coexist, not overwrite each other.
        let ui = crate::contracts::UiConfig::default();
        let line = status_bar_text(InputMode::App, &ui, Some("9.9.9"), true, None);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            text.contains("ISOLATED") && text.contains("9.9.9"),
            "{text}"
        );
    }

    #[test]
    fn status_bar_chip_uses_configured_color() {
        let ui = crate::contracts::UiConfig {
            terminal_mode_color: "magenta".to_string(),
            ..crate::contracts::UiConfig::default()
        };
        let line = status_bar_text(InputMode::Terminal, &ui, None, false, None);
        let chip = line
            .spans
            .iter()
            .find(|s| s.content.contains("MODE: TERMINAL"))
            .expect("chip span present");
        assert_eq!(chip.style.bg, Some(ratatui::style::Color::Magenta));
    }

    // --- Render smoke tests (TestBackend) ---------------------------------

    #[test]
    fn draw_does_not_panic_with_no_tabs() {
        let mut term = test_terminal(80, 24);
        let state = empty_state();
        let cache = empty_cache();
        term.draw(|frame| {
            draw(frame, &state, &cache, &UiOverlay::None, None, 0);
        })
        .unwrap();
    }

    #[test]
    fn draw_renders_live_pane_border_when_enabled() {
        // Default mode is APP, so the sidebar frame is the live one; we only
        // need *a* border glyph to prove the frame is drawn when the setting
        // is on. A selected tab with an active (spawned) terminal ensures the
        // terminal frame is also present, mirroring a real session.
        use crate::contracts::PtySize;
        use crate::testing::FakePty;
        use std::path::Path;

        let pty = FakePty::new();
        pty.queue_session();
        let mut state = state_with_tabs(1);
        state.tabs[0]
            .session
            .spawn_primary(&pty, "agent", &[], Path::new("/wt"), PtySize::default())
            .unwrap();
        state.config.ui.mode_border = "normal".to_string();
        let mut term = test_terminal(120, 40);
        let cache = GitStatusCache::new();
        term.draw(|f| draw(f, &state, &cache, &UiOverlay::None, None, 0))
            .unwrap();
        let buf = term.backend().buffer().clone();
        let text: String = buf.content().iter().map(|c| c.symbol()).collect();
        // Corner glyphs are unique to `Block::borders(ALL)` — unlike '─'/'│',
        // nothing else in `draw` emits them (dividers are plain horizontal
        // rules), so this only passes once the frame block is actually drawn.
        assert!(
            text.contains('┐') || text.contains('┌') || text.contains('└') || text.contains('┘'),
            "expected a border corner glyph when mode_border = normal"
        );
    }

    #[test]
    fn draw_renders_only_focused_pane_border_in_terminal_mode() {
        // Change 1: only the live pane gets a frame. In TERMINAL mode the
        // terminal frame is drawn (a corner glyph appears inside its rect);
        // the sidebar frame must NOT be drawn at all (no corner glyph inside
        // its rect), since the inactive pane no longer gets a DarkGray frame.
        use crate::contracts::PtySize;
        use crate::testing::FakePty;
        use std::path::Path;

        let pty = FakePty::new();
        pty.queue_session();
        let mut state = state_with_tabs(1);
        state.tabs[0]
            .session
            .spawn_primary(&pty, "agent", &[], Path::new("/wt"), PtySize::default())
            .unwrap();
        state.config.ui.mode_border = "normal".to_string();
        state.focus_terminal();

        let area = Rect::new(0, 0, 120, 40);
        let ml = layout::compute(
            area,
            layout::Chrome::Full,
            mode_style::border_enabled(&state.config.ui),
            state.config.ui.agent_tab_side(),
        );
        let sidebar_frame = ml.sidebar_frame.expect("sidebar frame reserved");
        let terminal_frame = ml.terminal_frame.expect("terminal frame reserved");

        let mut term = test_terminal(120, 40);
        let cache = GitStatusCache::new();
        term.draw(|f| draw(f, &state, &cache, &UiOverlay::None, None, 0))
            .unwrap();
        let buf = term.backend().buffer().clone();

        let is_corner = |r: Rect| -> bool {
            let mut found = false;
            for y in r.y..r.y.saturating_add(r.height) {
                for x in r.x..r.x.saturating_add(r.width) {
                    let sym = buf[(x, y)].symbol();
                    if matches!(sym, "┐" | "┌" | "└" | "┘") {
                        found = true;
                    }
                }
            }
            found
        };

        assert!(
            is_corner(terminal_frame),
            "expected a border corner glyph in the terminal frame when live in TERMINAL mode"
        );
        assert!(
            !is_corner(sidebar_frame),
            "sidebar frame must not be drawn when it is not the live pane"
        );
    }

    #[test]
    fn render_screen_dim_grays_out_non_selected_cells() {
        // Change 3: dimming must strongly gray out inactive terminal text
        // (fg forced to DarkGray), not just apply a subtle DIM modifier, and
        // must not corrupt the selection highlight for selected cells.
        let mut parser = vt100::Parser::new(4, 10, 0);
        parser.process(b"HELLO");
        let screen = parser.screen().clone();

        let backend = TestBackend::new(10, 4);
        let mut term = Terminal::new(backend).unwrap();
        let area = Rect::new(0, 0, 10, 4);
        term.draw(|f| {
            render_screen(f, area, &screen, false, None, true);
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        let cell = &buf[(0, 0)];
        assert_eq!(
            cell.style().fg,
            Some(Color::DarkGray),
            "dimmed non-selected cell should be forced to DarkGray fg"
        );
    }

    #[test]
    fn render_screen_dim_preserves_selection_highlight() {
        // The selection override must win over the dim gray-out: a selected
        // cell keeps White fg / SELECTION_BG bg even when `dim` is true.
        use crate::tui::selection::{Point, Selection};

        let mut parser = vt100::Parser::new(4, 10, 0);
        parser.process(b"HELLO");
        let screen = parser.screen().clone();

        // Screen row 0 (top of a 4-row screen at offset 0) is rows-from-bottom
        // 3; select columns 0..=4 on that row so cell (0, 0) is covered.
        let selection = Selection {
            anchor: Point {
                rows_from_bottom: 3,
                col: 0,
            },
            head: Point {
                rows_from_bottom: 3,
                col: 4,
            },
        };

        let backend = TestBackend::new(10, 4);
        let mut term = Terminal::new(backend).unwrap();
        let area = Rect::new(0, 0, 10, 4);
        term.draw(|f| {
            render_screen(f, area, &screen, false, Some(&selection), true);
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        let cell = &buf[(0, 0)];
        assert_eq!(
            cell.style().fg,
            Some(Color::White),
            "selected cell must keep white fg even while dimmed"
        );
        assert_eq!(
            cell.style().bg,
            Some(SELECTION_BG),
            "selected cell must keep the selection background even while dimmed"
        );
    }

    #[test]
    fn draw_does_not_panic_with_dialog_overlay() {
        let mut term = test_terminal(80, 24);
        let state = empty_state();
        let cache = empty_cache();
        term.draw(|frame| {
            draw(
                frame,
                &state,
                &cache,
                &UiOverlay::Dialog(Dialog::notification("Test message")),
                None,
                0,
            );
        })
        .unwrap();
    }

    #[test]
    fn draw_dialog_renders_title_and_buttons() {
        let mut term = test_terminal(80, 24);
        let dialog = Dialog::confirm(
            "Abandon this worktree?",
            vec![
                DialogButton::new(DialogAccel::Char('y'), "Abandon"),
                DialogButton::new(DialogAccel::Char('n'), "Cancel"),
            ],
        );
        term.draw(|frame| draw_dialog(frame, &dialog, frame.area()))
            .unwrap();
        let buffer = term.backend().buffer().clone();
        let text: String = (0..24_u16)
            .flat_map(|y| (0..80_u16).map(move |x| (x, y)))
            .map(|(x, y)| buffer[(x, y)].symbol().to_string())
            .collect();
        assert!(text.contains("Abandon this worktree?"), "title must render");
        assert!(
            text.contains("[y]") && text.contains("Abandon"),
            "buttons must render"
        );
        assert!(
            text.contains("[n]") && text.contains("Cancel"),
            "cancel must render"
        );
    }

    /// **D13's load-bearing line.** A dialog a browser opened appears on the
    /// desktop whether or not the person at this keyboard asked for it, and the
    /// origin line is the only thing that explains it. It renders above the
    /// buttons, so it is read before anything is decided.
    #[test]
    fn draw_dialog_renders_the_browser_origin_above_the_buttons() {
        let mut term = test_terminal(80, 24);
        let dialog = Dialog::confirm(
            "Set status override",
            vec![
                DialogButton::new(DialogAccel::Char('d'), "Done"),
                DialogButton::new(DialogAccel::Esc, "Cancel"),
            ],
        )
        .from_origin("opened from browser · 192.168.2.20");

        term.draw(|frame| draw_dialog(frame, &dialog, frame.area()))
            .unwrap();
        let buffer = term.backend().buffer().clone();
        let rows: Vec<String> = (0..24_u16)
            .map(|y| {
                (0..80_u16)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect()
            })
            .collect();
        let text = rows.join("\n");
        assert!(
            text.contains("opened from browser · 192.168.2.20"),
            "the origin label must render verbatim:\n{text}"
        );
        let origin_row = rows
            .iter()
            .position(|row| row.contains("opened from browser"))
            .expect("the origin row exists");
        let title_row = rows
            .iter()
            .position(|row| row.contains("Set status override"))
            .expect("the title row exists");
        let button_row = rows
            .iter()
            .position(|row| row.contains("[d]"))
            .expect("the button row exists");
        assert!(
            title_row < origin_row && origin_row < button_row,
            "the origin must sit under the title and above the buttons: \
             title={title_row} origin={origin_row} buttons={button_row}"
        );
    }

    /// A dialog the desktop opened for itself renders no origin line: the person
    /// reading it is the person who asked, and D13 is explicit that the label is
    /// not decoration.
    #[test]
    fn draw_dialog_renders_no_origin_for_a_desktop_dialog() {
        let mut term = test_terminal(80, 24);
        let dialog = Dialog::confirm(
            "Set status override",
            vec![DialogButton::new(DialogAccel::Char('d'), "Done")],
        );
        assert!(dialog.origin.is_none());
        term.draw(|frame| draw_dialog(frame, &dialog, frame.area()))
            .unwrap();
        let buffer = term.backend().buffer().clone();
        let text: String = (0..24_u16)
            .flat_map(|y| (0..80_u16).map(move |x| (x, y)))
            .map(|(x, y)| buffer[(x, y)].symbol().to_string())
            .collect();
        assert!(!text.contains("opened from"), "no origin line: {text}");
    }

    /// The origin line is content, so it grows the box and the buttons move down
    /// with it — the hit-test and the drawing read the same layout, and a click
    /// that landed on a button before the line was added must still land on it.
    #[test]
    fn an_origin_line_grows_the_box_and_keeps_the_buttons_clickable() {
        let area = Rect::new(0, 0, 80, 24);
        let plain = Dialog::confirm(
            "Close shell 2?",
            vec![
                DialogButton::new(DialogAccel::Char('y'), "Close"),
                DialogButton::new(DialogAccel::Char('n'), "Cancel"),
            ],
        );
        let tagged = plain
            .clone()
            .from_origin("opened from browser · 192.168.2.20 · Chrome on macOS");

        let plain_layout = layout_dialog(area, &plain);
        let tagged_layout = layout_dialog(area, &tagged);
        assert!(
            tagged_layout.rect.height > plain_layout.rect.height,
            "the origin line needs a row of its own"
        );
        assert!(
            tagged_layout.rect.width > plain_layout.rect.width,
            "the box widens to fit the sentence rather than truncating it"
        );

        let r = tagged_layout.button_rects[0];
        assert_eq!(
            dialog_hit(area, &tagged, r.x + r.width / 2, r.y),
            DialogHit::Button(0)
        );
    }

    #[test]
    fn dialog_hit_maps_click_to_button() {
        let area = Rect::new(0, 0, 80, 24);
        let dialog = Dialog::confirm(
            "Abandon this worktree?",
            vec![
                DialogButton::new(DialogAccel::Char('y'), "Abandon"),
                DialogButton::new(DialogAccel::Char('n'), "Cancel"),
            ],
        );
        // Hit-test the center of the first button's rect.
        let dl = layout_dialog(area, &dialog);
        let r = dl.button_rects[0];
        let (cx, cy) = (r.x + r.width / 2, r.y);
        assert_eq!(dialog_hit(area, &dialog, cx, cy), DialogHit::Button(0));
        // A click far outside the box resolves to Outside.
        assert_eq!(dialog_hit(area, &dialog, 0, 0), DialogHit::Outside);
    }

    #[test]
    fn draw_does_not_panic_with_help_overlay() {
        let mut term = test_terminal(80, 30);
        let state = empty_state();
        let cache = empty_cache();
        term.draw(|frame| {
            draw(frame, &state, &cache, &UiOverlay::Help, None, 0);
        })
        .unwrap();
    }

    fn help_overlay_buffer_text(area_h: u16, isolated: bool) -> String {
        // Realistic terminal heights: the 64x40 overlay box is clamped to
        // the terminal by `centered_overlay`, and the Paragraph has no
        // scroll, so a note near the bottom of a long list can be present in
        // the data yet invisible on an ordinary terminal. Testing at heights
        // nobody actually runs (e.g. 60 rows) would hide that.
        let mut term = test_terminal(80, area_h);
        let area = Rect::new(0, 0, 80, area_h);
        term.draw(|frame| {
            draw_help_overlay(frame, area, false, isolated);
        })
        .unwrap();
        let buffer = term.backend().buffer().clone();
        (0..area_h)
            .map(|y| {
                (0..80)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn help_overlay_shows_the_isolated_note_when_isolated() {
        // The note leads the overlay in an isolated run (SPECS §32), so it
        // must be visible even on a short, ordinary terminal.
        for area_h in [24, 40] {
            let text = help_overlay_buffer_text(area_h, true);
            assert!(
                text.contains("Isolated run"),
                "isolated help note must be shown at height {area_h}: {text}"
            );
        }
    }

    #[test]
    fn help_overlay_has_no_isolated_note_normally() {
        for area_h in [24, 40] {
            let text = help_overlay_buffer_text(area_h, false);
            assert!(
                !text.contains("Isolated run"),
                "no isolated note in a normal run at height {area_h}: {text}"
            );
        }
    }

    #[test]
    fn help_overlay_advertises_the_f1_repository_gesture() {
        // The gesture is invisible unless the panel says so — nobody presses a
        // key twice on a hunch. The hints live on the block's bottom border, so
        // they survive even when the shortcut list is taller than the overlay.
        let mut term = test_terminal(120, 50);
        term.draw(|frame| {
            draw_help_overlay(frame, frame.area(), false, false);
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        let text: String = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(
            text.contains(HELP_KEYS),
            "the shortcut list must name every help key, and must stay in step \
             with the status bar's HELP_KEYS, got: {text}"
        );
        assert!(
            text.contains("Press the help key again"),
            "must advertise the second-press gesture, got: {text}"
        );
        assert!(
            text.contains("GitHub"),
            "must say where the second press leads, got: {text}"
        );
        assert!(
            text.contains("Esc"),
            "must keep the close hint, got: {text}"
        );
    }

    #[test]
    fn help_overlay_hints_stay_visible_on_a_short_terminal() {
        // The shortcut list overflows a small overlay; border titles must not.
        let mut term = test_terminal(80, 24);
        term.draw(|frame| {
            draw_help_overlay(frame, frame.area(), false, false);
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        let text: String = buf.content().iter().map(|c| c.symbol()).collect();
        // Assert on the border hint's own wording, not on "F1" — that also
        // appears in the shortcut list, which would let this pass even if the
        // hint were truncated away.
        assert!(
            text.contains("Press the help key again"),
            "border hint must survive truncation, got: {text}"
        );
        assert!(
            text.contains("GitHub"),
            "border hint must survive truncation, got: {text}"
        );
        assert!(
            text.contains("Esc / q: close"),
            "border hint must survive truncation, got: {text}"
        );
    }

    #[test]
    fn draw_does_not_panic_with_palette_overlay() {
        let mut term = test_terminal(80, 24);
        let state = empty_state();
        let cache = empty_cache();
        let palette = CommandPalette::new();
        term.draw(|frame| {
            draw(frame, &state, &cache, &UiOverlay::Palette(palette), None, 0);
        })
        .unwrap();
    }

    #[test]
    fn draw_does_not_panic_with_config_overlay() {
        let mut term = test_terminal(80, 24);
        let state = empty_state();
        let cache = empty_cache();
        let manager = ConfigManager::new(
            "demo-project",
            Some(PathBuf::from("/home/u/.flightdeck/config.toml")),
            PathBuf::from("/repo/.flightdeck/config.toml"),
            toml::Table::new(),
            toml::Table::new(),
            vec!["opencode".to_string(), "claude".to_string()],
        );
        term.draw(|frame| {
            draw(frame, &state, &cache, &UiOverlay::Config(manager), None, 0);
        })
        .unwrap();

        let buffer = term.backend().buffer();
        // Fifteen settings (incl. the two remote fields) plus headers, the relay
        // restriction note, legend, and borders now exceed a 24-row terminal, so
        // centered_overlay clamps the box to the full height. Its top-left corner
        // is still at column 7 (width 66 centered in 80).
        assert_eq!(buffer[(7, 0)].symbol(), "┌");
        assert_eq!(buffer[(7, 23)].symbol(), "└");
    }

    #[test]
    fn config_overlay_shows_relay_restriction_note() {
        let mut term = test_terminal(100, 40);
        let state = empty_state();
        let cache = empty_cache();
        let manager = ConfigManager::new(
            "demo-project",
            Some(PathBuf::from("/home/u/.flightdeck/config.toml")),
            PathBuf::from("/repo/.flightdeck/config.toml"),
            toml::Table::new(),
            toml::Table::new(),
            vec!["opencode".to_string()],
        );
        term.draw(|frame| {
            draw(frame, &state, &cache, &UiOverlay::Config(manager), None, 0);
        })
        .unwrap();
        let buffer = term.backend().buffer();
        let text: String = (0..40_u16)
            .flat_map(|y| (0..100_u16).map(move |x| (x, y)))
            .map(|(x, y)| buffer[(x, y)].symbol().to_string())
            .collect();
        assert!(text.contains("Relay URL"), "relay field must render");
        assert!(
            text.contains("restricted"),
            "the relay restriction note must render"
        );
    }

    #[test]
    fn draw_does_not_panic_with_about_overlay() {
        let mut term = test_terminal(80, 24);
        let state = empty_state();
        let cache = empty_cache();
        term.draw(|frame| {
            draw(frame, &state, &cache, &UiOverlay::About, None, 0);
        })
        .unwrap();
        let buffer = term.backend().buffer();
        let text: String = (0..24_u16)
            .flat_map(|y| (0..80_u16).map(move |x| (x, y)))
            .map(|(x, y)| buffer[(x, y)].symbol().to_string())
            .collect();
        assert!(text.contains("Ruud van Falier"), "author must render");
        assert!(
            text.contains("Sander Langhorst"),
            "collaborator must render"
        );
    }

    #[test]
    fn draw_does_not_panic_with_git_status_overlay() {
        let mut term = test_terminal(80, 24);
        let state = empty_state();
        let cache = empty_cache();
        let ws = WorktreeStatus {
            branch: "flightdeck/test".to_string(),
            base_branch: "main".to_string(),
            dirty: true,
            changes: crate::git::status::WorktreeChanges {
                added: 1,
                modified: 2,
                deleted: 0,
            },
            ahead: 3,
            behind: 1,
            upstream: Some("origin/flightdeck/test".to_string()),
            base_drift: 2,
            worktree_path: PathBuf::from("/repo/.flightdeck/worktrees/test"),
        };
        term.draw(|frame| {
            draw(
                frame,
                &state,
                &cache,
                &UiOverlay::GitStatus {
                    status: ws,
                    pr_url: Some("https://github.com/owner/repo/compare/main...test".to_string()),
                },
                None,
                0,
            );
        })
        .unwrap();
    }

    #[test]
    fn status_bar_appears_at_bottom_of_buffer() {
        let mut term = test_terminal(80, 24);
        let state = empty_state();
        let cache = empty_cache();
        term.draw(|frame| {
            draw(frame, &state, &cache, &UiOverlay::None, None, 0);
        })
        .unwrap();

        // Bottom row (y=23) should contain status bar text.
        let buffer = term.backend().buffer().clone();
        let bottom_row: String = (0..80)
            .map(|x| buffer[(x, 23)].symbol().to_string())
            .collect();

        // Status bar must be on the bottom row.
        assert!(
            bottom_row.contains("MODE:") || bottom_row.contains("APP"),
            "bottom row should contain status bar, got: {bottom_row:?}"
        );
    }

    #[test]
    fn git_status_overlay_shows_branch() {
        let mut term = test_terminal(80, 24);
        let state = empty_state();
        let cache = empty_cache();
        let ws = WorktreeStatus {
            branch: "flightdeck/mybranch".to_string(),
            base_branch: "main".to_string(),
            dirty: false,
            changes: crate::git::status::WorktreeChanges::default(),
            ahead: 0,
            behind: 0,
            upstream: None,
            base_drift: 0,
            worktree_path: PathBuf::from("/repo/.flightdeck/worktrees/mybranch"),
        };
        term.draw(|frame| {
            draw(
                frame,
                &state,
                &cache,
                &UiOverlay::GitStatus {
                    status: ws,
                    pr_url: None,
                },
                None,
                0,
            );
        })
        .unwrap();

        // The buffer should contain the branch name somewhere.
        let buffer = term.backend().buffer().clone();
        let all_text: String = (0..24_u16)
            .flat_map(|y| (0..80_u16).map(move |x| (x, y)))
            .map(|(x, y)| buffer[(x, y)].symbol().to_string())
            .collect();

        assert!(
            all_text.contains("flightdeck/mybranch"),
            "git status overlay must show branch name, got: ...truncated..."
        );
    }

    #[test]
    fn sidebar_shows_no_tabs_hint() {
        let mut term = test_terminal(80, 24);
        let state = empty_state();
        let cache = empty_cache();
        term.draw(|frame| {
            draw(frame, &state, &cache, &UiOverlay::None, None, 0);
        })
        .unwrap();

        let buffer = term.backend().buffer().clone();
        let all_text: String = (0..24_u16)
            .flat_map(|y| (0..28_u16).map(move |x| (x, y)))
            .map(|(x, y)| buffer[(x, y)].symbol().to_string())
            .collect();

        assert!(
            all_text.contains("No tabs"),
            "sidebar should show 'No tabs' hint when empty, got: {all_text:?}"
        );
    }

    /// `remote-control-ecsv`, `specs/WEB_INTERFACE.md` §6.5 R24.
    ///
    /// The front door on purpose: the only thing this test does differently
    /// between the two runs is set the TOML key a user sets, and everything it
    /// asserts is read back out of the drawn buffer. Nothing here calls
    /// `compute` or hands the renderer a side — the point of the test is that
    /// the setting *reaches* the layout, which is precisely what it did not do
    /// before.
    #[test]
    fn agent_tab_position_right_mirrors_the_body_row_in_the_drawn_buffer() {
        const W: u16 = 120;
        const H: u16 = 40;

        fn draw_buffer(state: &AppState) -> ratatui::buffer::Buffer {
            let mut term = test_terminal(W, H);
            let cache = empty_cache();
            term.draw(|frame| draw(frame, state, &cache, &UiOverlay::None, None, 0))
                .unwrap();
            term.backend().buffer().clone()
        }

        /// The `(x, y)` of the first cell of `needle`, scanning row by row.
        fn find(buffer: &ratatui::buffer::Buffer, needle: &str) -> (u16, u16) {
            let chars: Vec<String> = needle.chars().map(|c| c.to_string()).collect();
            for y in 0..H {
                for x in 0..=W.saturating_sub(chars.len() as u16) {
                    if chars
                        .iter()
                        .enumerate()
                        .all(|(i, c)| buffer[(x + i as u16, y)].symbol() == c)
                    {
                        return (x, y);
                    }
                }
            }
            panic!("{needle:?} is not on screen");
        }

        let mut state = state_with_tabs(2);
        state.selected_tab = Some(0);

        let left = draw_buffer(&state);
        state.config.ui.agent_tab_position = "right".to_string();
        let right = draw_buffer(&state);

        // The sidebar's heading — the column itself.
        let (left_heading, heading_row) = find(&left, "Agents");
        let (right_heading, _) = find(&right, "Agents");
        assert!(
            left_heading < layout::SIDEBAR_WIDTH,
            "default: the sidebar is the first column"
        );
        assert!(
            right_heading >= W - layout::SIDEBAR_WIDTH,
            "right: the sidebar is the last column, got x={right_heading}"
        );

        // The `✕` column, which 1h names explicitly: at the sidebar's outer
        // end on both settings, so it never sits on the seam. Searched on the
        // first agent's name row, because the child tab bar draws a `✕` of its
        // own on the same screen row.
        let name_row = heading_row + 2;
        let close_x = |buffer: &ratatui::buffer::Buffer| -> u16 {
            (0..W)
                .find(|&x| buffer[(x, name_row)].symbol() == CLOSE_GLYPH)
                .expect("the agent row draws a close control")
        };
        let left_close = close_x(&left);
        let right_close = close_x(&right);
        assert_eq!(
            left_close,
            layout::SIDEBAR_WIDTH - 2,
            "default: the last inner column, inside the sidebar's right divider"
        );
        assert_eq!(
            right_close,
            W - layout::SIDEBAR_WIDTH + 1,
            "right: the first inner column, inside the sidebar's left divider"
        );

        // The seam between the two panes moves with them, and the top band
        // does not move at all.
        assert_eq!(
            left[(layout::SIDEBAR_WIDTH - 1, heading_row)].symbol(),
            "\u{2502}",
            "default: the divider is the sidebar's right edge"
        );
        assert_eq!(
            right[(W - layout::SIDEBAR_WIDTH, heading_row)].symbol(),
            "\u{2502}",
            "right: the divider is the sidebar's left edge"
        );
        for y in 0..layout::HEADER_HEIGHT + layout::PROJECT_TAB_BAR_HEIGHT + layout::DIVIDER_HEIGHT
        {
            assert_eq!(
                buffer_row(&left, y),
                buffer_row(&right, y),
                "the full-width top band does not move (row {y})"
            );
        }
    }

    /// The click path, which is the other half of the same fact: a `✕` that is
    /// drawn on the left and hit-tested on the right would close nothing.
    #[test]
    fn agent_tab_position_right_moves_the_hit_targets_with_the_sidebar() {
        let area = Rect::new(0, 0, 120, 40);
        let mut state = state_with_tabs(2);
        state.selected_tab = Some(0);

        // The first agent's name row, in both layouts.
        let name_row = layout::HEADER_HEIGHT
            + layout::PROJECT_TAB_BAR_HEIGHT
            + layout::DIVIDER_HEIGHT
            + SIDEBAR_HEADER_ROWS
            + 1;

        assert_eq!(
            hit_test(area, &state, 4, name_row),
            Some(HitTarget::AgentTab(0))
        );
        assert_eq!(
            hit_test(area, &state, layout::SIDEBAR_WIDTH - 2, name_row),
            Some(HitTarget::CloseAgentTab(0))
        );
        // The same columns are the terminal once the sidebar has moved.
        state.config.ui.agent_tab_position = "right".to_string();
        assert_eq!(hit_test(area, &state, 4, name_row), None);
        let sidebar_x = area.width - layout::SIDEBAR_WIDTH;
        assert_eq!(
            hit_test(area, &state, sidebar_x + 6, name_row),
            Some(HitTarget::AgentTab(0))
        );
        assert_eq!(
            hit_test(area, &state, sidebar_x + 1, name_row),
            Some(HitTarget::CloseAgentTab(0))
        );
    }

    // --- §24: simplified sidebar status ------------------------------------

    #[test]
    fn status_label_color_preserves_waiting_attention() {
        use crate::contracts::InterpretedStatus::*;
        use ratatui::style::Color;

        // In progress (cyan).
        for s in [Starting, Running, Working] {
            assert_eq!(status_label_color(s), ("in progress", Color::Cyan));
        }
        // Error (red).
        for s in [Failed, SessionLost] {
            assert_eq!(status_label_color(s), ("error", Color::Red));
        }
        // Waiting for input needs the same red visual priority as project tabs.
        for s in [WaitingForInput, NeedsAttention] {
            assert_eq!(status_label_color(s), ("waiting", Color::Red));
        }
        // All other settled statuses read as idle (green).
        for s in [Idle, Completed, Stopped, Recovered, Unknown] {
            assert_eq!(status_label_color(s), ("idle", Color::Green));
        }
    }

    #[test]
    fn active_status_indicator_uses_smooth_braille_spinner() {
        use crate::contracts::InterpretedStatus::*;

        let frames: String = (0..10)
            .map(|frame| status_indicator(Working, frame * 80))
            .collect();
        assert_eq!(frames, "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏");
        assert_eq!(status_indicator(Starting, 80), '⠙');
        assert_eq!(status_indicator(Running, 160), '⠹');
        assert_eq!(status_indicator(Idle, 160), '●');
        assert_eq!(status_indicator(Failed, 160), '●');
    }

    #[test]
    fn sidebar_shows_bracketed_status_without_proc_prefix() {
        let state = state_with_tabs(1);
        let mut term = test_terminal(80, 24);
        term.draw(|f| draw(f, &state, &empty_cache(), &UiOverlay::None, None, 0))
            .unwrap();

        let buffer = term.backend().buffer().clone();
        let all_text: String = (0..24_u16)
            .flat_map(|y| (0..80_u16).map(move |x| (x, y)))
            .map(|(x, y)| buffer[(x, y)].symbol().to_string())
            .collect();

        // New format: "<agent> [<status>]"; a fresh (not-started) tab reads idle.
        assert!(
            all_text.contains("[idle]"),
            "sidebar should show bracketed status, got: {all_text:?}"
        );
        // The "proc:" prefix is gone.
        assert!(
            !all_text.contains("proc:"),
            "sidebar must not show the 'proc:' prefix, got: {all_text:?}"
        );
    }

    #[test]
    fn terminal_dims_in_app_mode_when_enabled() {
        // Calls `dim_terminal` directly to pin the production policy: dim only
        // when NOT focused (i.e. APP mode) and the setting is on.
        let mut ui = crate::contracts::UiConfig {
            dim_terminal_in_app_mode: true,
            ..Default::default()
        };

        // Terminal mode (focused) never dims.
        assert!(!super::dim_terminal(true, &ui));
        // App mode (unfocused) + setting on → dim.
        assert!(super::dim_terminal(false, &ui));
        // App mode + setting off → no dim.
        ui.dim_terminal_in_app_mode = false;
        assert!(!super::dim_terminal(false, &ui));
    }

    // --- Access overlay (D5, Q1; design `2a`, both states) -----------------

    /// The base of a State B view: a QR the size `qr_art` really produces for a
    /// `http://192.168.2.14:7420/#8412` payload, the code beside it, the three
    /// interfaces artboard 2a draws.
    fn network_access_view() -> WebAccessView {
        WebAccessView {
            mode: Some(AccessMode::Network),
            bound: "0.0.0.0:7420".to_string(),
            exposure_line: "reachable by anyone on this network who has the code".to_string(),
            url: "http://192.168.2.14:7420".to_string(),
            code: Some("8412".to_string()),
            code_hidden: false,
            code_expired: false,
            qr_rows: vec!["#".repeat(33); 17],
            qr_width: 33,
            seconds_remaining: Some(97),
            addresses: vec![
                crate::web::access::AddressRow {
                    name: "en0".to_string(),
                    address: "192.168.2.14".to_string(),
                    description: Some("wifi · reachable by your phone"),
                },
                crate::web::access::AddressRow {
                    name: "bridge100".to_string(),
                    address: "192.168.64.1".to_string(),
                    description: Some("vm bridge"),
                },
                crate::web::access::AddressRow {
                    name: "tailscale0".to_string(),
                    address: "100.87.14.3".to_string(),
                    description: Some("your own tunnel"),
                },
            ],
            selected_address: Some(0),
            browsers: vec![crate::web::access::BrowserRow {
                key: Some('1'),
                address: Some("192.168.2.20".to_string()),
                browser: Some("Safari on iOS".to_string()),
                granted_secs_ago: 14 * 60,
            }],
            notice: None,
            keys: vec![
                ("↑↓", "address"),
                ("Space", "new code"),
                ("r", "hide"),
                ("x", "revoke"),
                ("l", "local only"),
                ("Esc", "close"),
            ],
        }
    }

    fn local_access_view() -> WebAccessView {
        WebAccessView {
            mode: Some(AccessMode::LocalOnly),
            bound: "127.0.0.1:7420".to_string(),
            exposure_line: "loopback only — nothing off this machine can reach it".to_string(),
            url: "http://127.0.0.1:7420".to_string(),
            code: None,
            code_hidden: false,
            code_expired: false,
            qr_rows: Vec::new(),
            qr_width: 0,
            seconds_remaining: None,
            addresses: Vec::new(),
            selected_address: None,
            browsers: Vec::new(),
            notice: None,
            keys: vec![
                ("Enter", "open"),
                ("c", "copy"),
                ("n", "network access"),
                ("s", "stop server"),
                ("Esc", "close"),
            ],
        }
    }

    /// Every cell of a drawn frame, as one string.
    fn painted(width: u16, height: u16, view: &WebAccessView) -> String {
        let mut term = test_terminal(width, height);
        term.draw(|f| {
            let area = f.area();
            draw_web_access_overlay(f, view, area);
        })
        .unwrap();
        let buffer = term.backend().buffer().clone();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn state_a_draws_the_open_in_browser_door_and_never_a_code() {
        let text = painted(120, 30, &local_access_view());
        assert!(text.contains("Web Interface"), "{text}");
        assert!(text.contains("serving"), "{text}");
        assert!(
            text.contains("loopback only"),
            "the exposure line is the host's, verbatim: {text}"
        );
        assert!(text.contains("Open in browser"), "{text}");
        assert!(text.contains("Copy URL"), "{text}");
        assert!(text.contains("http://127.0.0.1:7420"), "{text}");
        assert!(
            text.contains("Allow other devices on this network to connect"),
            "the door to State B states its consequence: {text}"
        );
        assert!(text.contains("this switch does not do it"), "{text}");
        // The whole point of State A.
        assert!(
            !text.contains("expires in"),
            "State A shows no countdown: {text}"
        );
        assert!(
            !text.contains("WHAT YOU ARE ALLOWING"),
            "nothing is being allowed off this machine: {text}"
        );
    }

    #[test]
    fn state_a_never_claims_the_bare_url_is_already_authenticated() {
        // The URL row draws a credential-free URL; only `c` attaches a code. A
        // row reading "already authenticated" would be a claim the host never
        // made about the string next to it.
        let text = painted(120, 30, &local_access_view());
        assert!(!text.contains("already authenticated"), "{text}");
        assert!(text.contains("one-time code attached"), "{text}");
    }

    #[test]
    fn state_b_draws_the_qr_the_code_the_picker_and_the_warning() {
        let text = painted(120, 44, &network_access_view());
        assert!(
            text.contains("network access"),
            "the title names it: {text}"
        );
        assert!(text.contains("#".repeat(33).as_str()), "the QR art: {text}");
        assert!(
            text.contains("8 4 1 2"),
            "the code, spaced out as artboard 2a's large type: {text}"
        );
        assert!(text.contains("expires in 97s"), "{text}");
        assert!(text.contains("PUBLISH WHICH ADDRESS"), "{text}");
        assert!(text.contains("192.168.2.14"), "{text}");
        assert!(
            text.contains("wifi · reachable by your phone"),
            "interfaces.rs's one-line descriptions: {text}"
        );
        assert!(
            text.contains("‹bridge100›") && text.contains("vm bridge"),
            "{text}"
        );
        assert!(
            text.contains("‹tailscale0›") && text.contains("your own tunnel"),
            "{text}"
        );
        assert!(
            text.contains("WHAT YOU ARE ALLOWING"),
            "D5's warning: {text}"
        );
        assert!(text.contains("push branches"), "{text}");
        assert!(text.contains("1 browser holds access"), "{text}");
        assert!(text.contains("Esc close"), "the legend: {text}");
    }

    /// 2a State B's `● 1 browser holds access · 192.168.2.20 · Safari/iOS ·
    /// 14m`, and the digit that takes it out (`remote-control-gk94`, §6.5 R25).
    /// The count alone is what this line used to be, and it could not answer
    /// the question it was drawn for.
    #[test]
    fn state_b_names_each_holder_and_the_digit_that_revokes_it() {
        let mut view = network_access_view();
        view.browsers.push(crate::web::access::BrowserRow {
            key: Some('2'),
            address: Some("192.168.2.31".to_string()),
            browser: Some("Chrome on macOS".to_string()),
            granted_secs_ago: 2 * 3600,
        });

        let text = painted(120, 44, &view);
        assert!(
            text.contains("2 browsers hold access — 1-2 revokes one"),
            "the header counts them and names the keys: {text}"
        );
        assert!(
            text.contains("1 192.168.2.20 · Safari on iOS · 14m"),
            "the first row, with its digit: {text}"
        );
        assert!(
            text.contains("2 192.168.2.31 · Chrome on macOS · 2h"),
            "the second row: {text}"
        );
    }

    /// A row the host knows nothing about is drawn short, never padded out with
    /// a stand-in for a fact nobody observed.
    #[test]
    fn a_holder_with_no_address_and_no_claim_is_drawn_without_them() {
        let mut view = network_access_view();
        view.browsers = vec![crate::web::access::BrowserRow {
            key: Some('1'),
            address: None,
            browser: None,
            granted_secs_ago: 45,
        }];

        let text = painted(120, 44, &view);
        assert!(text.contains("1 45s"), "the age is all there is: {text}");
        assert!(
            !text.contains("unknown") && !text.contains("(none)"),
            "no placeholder: {text}"
        );
    }

    #[test]
    fn hiding_the_code_takes_the_qr_with_it_and_says_how_to_get_it_back() {
        // What `r` produces: the view arrives with no code and no art at all,
        // which is what makes "hidden" impossible to half-apply.
        let mut view = network_access_view();
        view.code = None;
        view.code_hidden = true;
        view.qr_rows = Vec::new();
        view.qr_width = 0;

        let text = painted(120, 44, &view);
        assert!(!text.contains("8 4 1 2"), "{text}");
        assert!(!text.contains("###"), "the QR goes with the code: {text}");
        assert!(text.contains("code and QR hidden — r to show"), "{text}");
        // Everything that is not the credential stays: the user still needs to
        // see what this binding allows while the code is put away.
        assert!(text.contains("WHAT YOU ARE ALLOWING"), "{text}");
        assert!(text.contains("192.168.2.14"), "{text}");
    }

    #[test]
    fn an_expired_code_is_reported_with_the_way_to_replace_it() {
        let mut view = network_access_view();
        view.code = None;
        view.code_expired = true;
        view.seconds_remaining = None;
        view.qr_rows = Vec::new();
        view.qr_width = 0;

        let text = painted(120, 44, &view);
        assert!(
            text.contains("code expired — Space for a new one"),
            "{text}"
        );
        assert!(!text.contains("expires in"), "{text}");
    }

    #[test]
    fn the_qr_fits_a_terminal_that_can_hold_it_alongside_the_required_rows() {
        let view = network_access_view();
        let l = web_access_layout(&view, Rect::new(0, 0, 120, 44));
        assert!(l.show_qr, "a 17-row QR fits a 44-row terminal");
        assert!(l.note.is_empty());
        assert!(l.content_w >= view.qr_width as u16);
    }

    #[test]
    fn a_small_terminal_drops_the_qr_and_names_the_size_it_would_need() {
        // The same degradation the phone pairing overlay already had: the art
        // gives way, the code does not, and the note carries both sizes so the
        // user knows which dimension to grow.
        let view = network_access_view();
        let l = web_access_layout(&view, Rect::new(0, 0, 60, 18));
        assert!(!l.show_qr);
        let note = l.note.join(" ");
        assert!(
            note.contains("have 60x18"),
            "the note names the terminal it has: {note}"
        );
        let text = painted(60, 18, &view);
        assert!(
            text.contains("8 4 1 2"),
            "the code survives when the art cannot: {text}"
        );
        assert!(text.contains("Terminal too small for the QR"), "{text}");
    }

    #[test]
    fn a_short_terminal_keeps_the_published_address_and_the_key_legend() {
        // The tiering rule, at the size where it bites: prose and echoed rows
        // give way, but the address being published and the keys that change it
        // are what the overlay *is*.
        let view = network_access_view();
        for height in 10..=44u16 {
            let text = painted(100, height, &view);
            assert!(
                text.contains("192.168.2.14"),
                "the published address survives at height {height}: {text}"
            );
            assert!(
                text.contains("Esc close"),
                "the key legend survives at height {height}: {text}"
            );
        }
    }

    #[test]
    fn a_notice_is_shown_and_never_outlives_the_frame_that_carried_it() {
        let mut view = network_access_view();
        view.notice = Some("1 browser revoked — new code issued.".to_string());
        let text = painted(120, 44, &view);
        assert!(text.contains("1 browser revoked"), "{text}");

        view.notice = None;
        let text = painted(120, 44, &view);
        assert!(!text.contains("revoked — new code"), "{text}");
    }

    #[test]
    fn an_overlay_with_no_mode_draws_nothing() {
        // `WebAccessView::default()` is what a renderer would get if a snapshot
        // were ever built without a state. It must paint nothing rather than an
        // empty frame claiming to be the access surface.
        let text = painted(80, 24, &WebAccessView::default());
        assert!(!text.contains("Web Interface"), "{text}");
    }

    #[test]
    fn fit_rows_drops_whole_tiers_from_the_bottom_before_touching_the_next() {
        let row = |tier: Tier, text: &str| (tier, Line::raw(text.to_string()));
        let rows = vec![
            row(REQUIRED, "a"),
            row(TIER_PROSE, "b"),
            row(TIER_SPACER, "c"),
            row(TIER_ECHOED, "d"),
            row(TIER_SPACER, "e"),
            row(REQUIRED, "f"),
        ];
        let text = |lines: Vec<Line<'static>>| {
            lines
                .iter()
                .map(|l| l.spans[0].content.to_string())
                .collect::<String>()
        };

        assert_eq!(text(fit_rows(rows.clone(), 6)), "abcdef");
        // Spacers first, from the bottom.
        assert_eq!(text(fit_rows(rows.clone(), 5)), "abcdf");
        assert_eq!(text(fit_rows(rows.clone(), 4)), "abdf");
        // Then the echoed row, then the prose — required rows never go.
        assert_eq!(text(fit_rows(rows.clone(), 3)), "abf");
        assert_eq!(text(fit_rows(rows.clone(), 2)), "af");
        // Smaller than the required set: clipped, not reordered.
        assert_eq!(text(fit_rows(rows, 1)), "a");
    }

    // --- Pairing overlay layout (remote pairing on small terminals) --------

    /// A `RemotePairing` carrying art the size a real `fdr1:` payload produces:
    /// 57 cells wide, 29 half-block rows (measured through
    /// `remote::pairing::qr_art`). The exact numbers are the point — they are
    /// what the fixed chrome budget used to push off screen.
    fn displaying_pairing() -> RemotePairing {
        RemotePairing {
            status_line: "Scan the QR or type the code on your phone — waiting…".to_string(),
            code: Some("4729".to_string()),
            qr_rows: vec!["#".repeat(57); 29],
            qr_width: 57,
            seconds_remaining: Some(120),
            done: false,
            failed: false,
        }
    }

    #[test]
    fn pairing_layout_shows_qr_and_code_in_thirty_row_terminal() {
        // A default-size Windows Terminal: 29 QR rows + the code line is exactly
        // 30, so the QR gets in — borderless, with the droppable status gone.
        let l = pairing_layout(&displaying_pairing(), Rect::new(0, 0, 120, 30));
        assert!(l.show_qr, "the QR must fit a 120x30 terminal");
        assert!(!l.bordered, "the border must give way before the QR does");
        assert!(l.has_code, "the manual code is never dropped");
        assert!(l.status.is_empty(), "the status yields at this height");
        assert!(!l.show_countdown && !l.show_esc);
        assert_eq!(
            l.content_h(29),
            30,
            "must fill the height exactly, not exceed it"
        );
    }

    #[test]
    fn pairing_layout_keeps_border_and_chrome_when_tall_enough() {
        let l = pairing_layout(&displaying_pairing(), Rect::new(0, 0, 120, 40));
        assert!(l.show_qr && l.bordered);
        assert_eq!(l.status.len(), 1);
        assert!(l.show_countdown, "the countdown returns once there is room");
        assert!(l.show_esc);
        assert!(l.content_h(29) + PAIRING_BORDER_H <= 40);
    }

    #[test]
    fn pairing_layout_restores_chrome_in_priority_order_as_height_grows() {
        // Each extra row buys back the next-most-useful piece of chrome, and no
        // layout ever claims more rows than the terminal has.
        let p = displaying_pairing();
        let mut seen_status = false;
        let mut seen_countdown = false;
        for h in 30..=40u16 {
            let l = pairing_layout(&p, Rect::new(0, 0, 120, h));
            let used = l.content_h(29) + if l.bordered { PAIRING_BORDER_H } else { 0 };
            assert!(used <= h, "layout at height {h} used {used} rows");
            assert!(l.show_qr, "the QR fits every height from 30 up");
            if !l.status.is_empty() {
                seen_status = true;
            }
            if l.show_countdown {
                assert!(seen_status, "the status is restored before the countdown");
                seen_countdown = true;
            }
            if l.show_esc {
                assert!(seen_countdown, "the countdown is restored before the hint");
            }
        }
        assert!(seen_countdown, "a 40-row terminal shows the countdown");
    }

    #[test]
    fn pairing_layout_names_the_size_the_qr_needs_when_it_cannot_fit() {
        let l = pairing_layout(&displaying_pairing(), Rect::new(0, 0, 80, 20));
        assert!(!l.show_qr);
        assert!(l.bordered, "the fallback keeps the dialog frame");
        let note = l.note.join(" ");
        assert!(
            note.contains("57x30") && note.contains("80x20"),
            "the note must name both the needed and the actual size: {note}"
        );
        assert!(l.has_code, "the code is what the user falls back to");
        assert!(
            !l.status.is_empty(),
            "with no QR the status is required, wrapped to the 44-column box"
        );
    }

    #[test]
    fn pairing_layout_wraps_a_long_relay_failure_message() {
        // The relay-refused message is far longer than the overlay is wide; it
        // must wrap rather than be truncated to its first 44 columns.
        let pairing = RemotePairing {
            status_line: "the relay refused the connection: relay password required. \
                          Check [remote] relay_url / relay_password in your configuration, \
                          then try again."
                .to_string(),
            code: None,
            qr_rows: Vec::new(),
            qr_width: 0,
            seconds_remaining: None,
            done: false,
            failed: true,
        };
        let l = pairing_layout(&pairing, Rect::new(0, 0, 120, 30));
        assert!(
            l.status.len() > 2,
            "long failure text must wrap: {:?}",
            l.status
        );
        for row in &l.status {
            assert!(row.chars().count() <= l.content_w as usize);
        }
        assert!(l.note.is_empty(), "no QR was offered, so no size note");
    }

    #[test]
    fn draw_remote_overlay_paints_qr_rows_in_a_thirty_row_terminal() {
        let pairing = displaying_pairing();
        let mut term = test_terminal(120, 30);
        term.draw(|f| {
            let area = f.area();
            draw_remote_overlay(f, &pairing, area);
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        let text: String = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(
            text.contains(&"#".repeat(57)),
            "a full QR row must reach the buffer at 120x30"
        );
        assert!(text.contains("Code  4729"));
    }

    #[test]
    fn draw_remote_overlay_survives_a_tiny_terminal() {
        let pairing = displaying_pairing();
        let mut term = test_terminal(20, 4);
        term.draw(|f| {
            let area = f.area();
            draw_remote_overlay(f, &pairing, area);
        })
        .unwrap();
    }
}
