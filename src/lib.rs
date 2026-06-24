//! FlightDeck: a terminal UI for orchestrating multiple local AI coding agents
//! working in parallel on the same Git project (SPECS §1).
//!
//! Architecture (SPECS §27): business logic lives in testable services behind
//! traits ([`contracts`]); the TUI dispatches commands into them and never
//! executes git/fs/pty directly. The SPECS §5 git-ownership boundary is
//! enforced by construction — no service can rewrite history or create PRs.

pub mod contracts;
pub mod testing;

pub mod agents;
pub mod app;
pub mod config;
pub mod fs;
pub mod git;
pub mod persistence;
pub mod terminal;
pub mod tui;

use std::path::Path;
use std::time::Duration;

use crate::app::commands::{CloseAction, Command, Effect, PushConfirm};
use crate::app::state::{AppState, Services};
use crate::config::init::initialize;
use crate::config::load::load_config;
use crate::config::schema::default_config;
use crate::contracts::error::{FlightDeckError, Result};
use crate::contracts::real::{RealClock, RealFs};
use crate::contracts::{FileSystem, ManualStatus, PtySize, TabId};
use crate::fs::ignore::ensure_flightdeck_gitignore;
use crate::fs::paths::to_absolute;
use crate::git::repo::{detect_base_branch, GitCli};
use crate::git::status::collect_status;
use crate::persistence::project_state::{default_state, load_state, save_state};
use crate::persistence::recovery::recover;
use crate::terminal::pty::PortablePtyBackend;
use crate::tui::input::{map_key, KeyAction};
use crate::tui::palette::{CommandPalette, PaletteAction};
use crate::tui::render::{draw, GitStatusCache, UiOverlay};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

/// How long to block waiting for an input event before looping again so PTY
/// output keeps flowing and statuses keep refreshing.
const POLL_TIMEOUT: Duration = Duration::from_millis(50);

/// Refresh the git-status cache every N ticks (a tick is one loop iteration,
/// roughly [`POLL_TIMEOUT`] when idle). Kept coarse so we never block the UI.
const GIT_REFRESH_EVERY: u64 = 40;

/// Entry point invoked by the binary: run first-run init, recover state, and
/// drive the Ratatui event loop (SPECS §4, §7, §10).
///
/// The flow is split into a (testable, no-terminal) [`startup`] phase that
/// constructs the [`AppState`], and the interactive [`event_loop`] that owns the
/// real terminal. Teardown is guaranteed in all paths (SPECS §25): the terminal
/// is restored and every tab's sessions are terminated before returning.
pub fn run() -> Result<()> {
    // 1–4. Construct services + run startup (init, gitignore, recover, build state).
    let cwd = std::env::current_dir()
        .map_err(|e| FlightDeckError::Io(format!("could not determine current directory: {e}")))?;

    let git = GitCli::discover(&cwd).map_err(|_| {
        FlightDeckError::Git(
            "not inside a Git repository (run FlightDeck from a git project)".to_string(),
        )
    })?;
    let repo_root = git.root().to_path_buf();

    let fs = RealFs;
    let pty = PortablePtyBackend;
    let clock = RealClock;

    let services = Services {
        git: &git,
        fs: &fs,
        pty: &pty,
        clock: &clock,
    };

    let mut state = startup(&services, &repo_root, &cwd)?;

    // 5–8. Initialise the terminal (raw mode + alt screen + panic-restore hook)
    // and run the loop, ensuring teardown happens no matter how we exit.
    let mut terminal = ratatui::try_init()
        .map_err(|e| FlightDeckError::Io(format!("failed to initialise terminal: {e}")))?;

    // Seed the PTY size from the real terminal so spawns match the viewport.
    if let Ok(size) = terminal.size() {
        state.set_pty_size(PtySize {
            rows: size.height,
            cols: size.width,
        });
    }

    let loop_result = event_loop(&mut terminal, &mut state, &services);

    // CLEAN TEARDOWN (SPECS §25): always restore the terminal, then terminate
    // every session so no orphaned child processes remain. Persist on the way
    // out (best effort) regardless of how the loop ended.
    ratatui::restore();
    let persist_result = persist_quietly(&state, &services);
    terminate_all_sessions(&mut state);

    loop_result.and(persist_result)
}

// ---------------------------------------------------------------------------
// Startup (SPECS §4, §7, §10, §13) — terminal-free, returns the built AppState.
// ---------------------------------------------------------------------------

/// Run the SPECS §7 startup sequence and build the [`AppState`]. Pure of any
/// terminal I/O so it can be exercised with the fakes in [`crate::testing`].
///
/// Steps: detect base branch, first-run init, load config (default fallback),
/// `.gitignore` update + §6 notice, load + recover state, build [`AppState`],
/// and record the §13 dirty-base warning if the base repo is dirty at startup.
fn startup(services: &Services, repo_root: &Path, cwd: &Path) -> Result<AppState> {
    // Detect the base branch using the configured value if a config already
    // exists, otherwise the current branch (SPECS §7 step 3, §12).
    let flightdeck_dir = repo_root.join(".flightdeck");
    let config_path = flightdeck_dir.join("config.toml");
    let state_path = flightdeck_dir.join("state.json");
    let worktrees_root = repo_root.join(".flightdeck").join("worktrees");

    let pre_configured_base = read_configured_base(services.fs, &config_path);
    let base_branch = detect_base_branch(services.git, cwd, pre_configured_base.as_deref())?;

    // First-run init: create .flightdeck/, config.toml, state.json, worktrees/.
    let project_name = derive_project_name(repo_root);
    initialize(services.fs, repo_root, &project_name, &base_branch)?;

    // Load config; fall back to a freshly-created default if loading fails.
    let config = match load_config(services.fs, &config_path) {
        Ok(cfg) => cfg,
        Err(_) => default_config(&project_name, &base_branch),
    };

    // Append .gitignore entries and surface the §6 notice if it changed.
    let update = ensure_flightdeck_gitignore(services.fs, repo_root)?;
    if update.changed {
        eprintln!(
            "FlightDeck: added {} to .gitignore: {}",
            if update.added.len() == 1 {
                "entry"
            } else {
                "entries"
            },
            update.added.join(", ")
        );
    }

    // Load state (default if missing), then recover tabs WITHOUT relaunching
    // agents (SPECS §10).
    let mut project_state =
        load_state(services.fs, &state_path).unwrap_or_else(|_| default_state(&base_branch));
    let _report = recover(
        services.fs,
        services.git,
        repo_root,
        &worktrees_root,
        &mut project_state,
    )?;

    let mut state = AppState::new(config, project_state, repo_root, &state_path);

    // SPECS §13: dirty base at startup → persistent warning (merge disabled).
    if services.git.is_dirty(repo_root).unwrap_or(false) {
        let warning = "Base repo dirty: local merge disabled".to_string();
        if !state.warnings.contains(&warning) {
            state.warnings.push(warning);
        }
    }

    Ok(state)
}

/// Read the `default_base_branch` out of an existing config, if present, without
/// failing startup when the file is missing or unparsable.
fn read_configured_base(fs: &dyn FileSystem, config_path: &Path) -> Option<String> {
    if !fs.exists(config_path) {
        return None;
    }
    let contents = fs.read_to_string(config_path).ok()?;
    let config = crate::config::load::parse_config(&contents).ok()?;
    Some(config.project.default_base_branch)
}

/// Derive a human-readable project name from the repo root directory name.
fn derive_project_name(repo_root: &Path) -> String {
    repo_root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "project".to_string())
}

// ---------------------------------------------------------------------------
// Overlay / prompt state machine
// ---------------------------------------------------------------------------

/// An interactive secondary prompt the loop is currently collecting (SPECS §22,
/// §25). These are the multi-step flows the palette/keys require: text entry for
/// New/Rename, single-key choice menus for Set Status / Close / Push.
enum Prompt {
    /// Free-text entry for a new Agent Tab name; dispatches `NewAgentTab`.
    NewTabName { buffer: String },
    /// Free-text entry for renaming the selected tab; dispatches `RenameAgentTab`.
    RenameTab { buffer: String },
    /// Pick a manual status (or clear); dispatches `SetManualStatus`.
    SetManualStatus,
    /// Choose how to handle running processes when closing (SPECS §25).
    CloseTab { actions: Vec<CloseAction> },
    /// Confirm a push despite uncommitted changes (SPECS §14).
    PushConfirm,
}

/// The full interactive UI state layered over [`AppState`]: which overlay is
/// drawn, plus any in-progress prompt.
#[derive(Default)]
struct Ui {
    overlay: UiOverlay,
    palette: Option<CommandPalette>,
    prompt: Option<PromptState>,
}

/// A prompt plus the rendered hint shown to the user (drawn as a message line).
struct PromptState {
    prompt: Prompt,
    hint: String,
}

impl Ui {
    /// Whether any modal/prompt currently captures input. Used to decide whether
    /// the normal mode-aware key map should run.
    fn modal_active(&self) -> bool {
        self.palette.is_some() || self.prompt.is_some() || !matches!(self.overlay, UiOverlay::None)
    }

    /// Set a transient message toast.
    fn message(&mut self, msg: impl Into<String>) {
        self.overlay = UiOverlay::Message(msg.into());
    }

    /// Clear every overlay/prompt back to the normal main view.
    fn clear(&mut self) {
        self.overlay = UiOverlay::None;
        self.palette = None;
        self.prompt = None;
    }

    /// The overlay to render this frame: a live prompt hint takes precedence
    /// over a plain message, the palette over both.
    fn render_overlay(&self) -> UiOverlay {
        if let Some(palette) = &self.palette {
            return UiOverlay::Palette(palette.clone());
        }
        if let Some(p) = &self.prompt {
            return UiOverlay::Message(p.hint.clone());
        }
        self.overlay.clone()
    }
}

// ---------------------------------------------------------------------------
// Event loop (SPECS §23)
// ---------------------------------------------------------------------------

/// The main event loop. Drains PTY output, refreshes git status, renders, and
/// routes input until the user quits or a fatal error occurs.
fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    state: &mut AppState,
    services: &Services,
) -> Result<()> {
    let mut ui = Ui::default();
    let mut cache: GitStatusCache = GitStatusCache::new();
    let mut viewport: Vec<u8> = Vec::new();
    let mut tick: u64 = 0;

    loop {
        // --- Drain PTY output, classify it, and feed the active viewport. ---
        drain_pty_output(state, &mut viewport);

        // --- Periodically refresh the git-status cache (non-blocking-ish). ---
        if tick.is_multiple_of(GIT_REFRESH_EVERY) {
            refresh_git_cache(state, services, &mut cache);
        }
        tick = tick.wrapping_add(1);

        // --- Render. ---
        let overlay = ui.render_overlay();
        terminal
            .draw(|frame| draw(frame, state, &cache, &overlay))
            .map_err(|e| FlightDeckError::Io(format!("render failed: {e}")))?;

        // --- Poll for input (short timeout so PTY output keeps flowing). ---
        let has_event = event::poll(POLL_TIMEOUT)
            .map_err(|e| FlightDeckError::Io(format!("event poll failed: {e}")))?;
        if !has_event {
            continue;
        }

        match event::read().map_err(|e| FlightDeckError::Io(format!("event read failed: {e}")))? {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                // Drop the synthetic viewport buffer reference on focus changes
                // implicitly by always re-reading; just route the key.
                if handle_key(key, state, services, &mut ui)? {
                    break; // Quit requested.
                }
            }
            Event::Resize(cols, rows) => {
                let size = PtySize { rows, cols };
                state.set_pty_size(size);
                resize_sessions(state, size);
            }
            _ => {}
        }
    }

    Ok(())
}

/// Route a key press. Returns `Ok(true)` when the loop should quit.
fn handle_key(
    key: KeyEvent,
    state: &mut AppState,
    services: &Services,
    ui: &mut Ui,
) -> Result<bool> {
    // 1. An active prompt captures all input first.
    if ui.prompt.is_some() {
        return handle_prompt_key(key, state, services, ui).map(|_| false);
    }

    // 2. The command palette, if open, captures input next (SPECS §22).
    if ui.palette.is_some() {
        return handle_palette_key(key, state, services, ui).map(|_| false);
    }

    // 3. A non-interactive overlay (help, git status, message): any key dismisses.
    if !matches!(ui.overlay, UiOverlay::None) {
        ui.clear();
        return Ok(false);
    }

    // 4. No modal is capturing input (the three checks above are exhaustive):
    //    route through the mode-aware key map (SPECS §23).
    if ui.modal_active() {
        return Ok(false);
    }
    match map_key(state.mode(), key) {
        KeyAction::Dispatch(cmd) => {
            dispatch_command(cmd, state, services, ui)?;
            Ok(false)
        }
        KeyAction::Passthrough(bytes) => {
            write_active_pty(state, &bytes);
            Ok(false)
        }
        KeyAction::OpenPalette => {
            ui.palette = Some(CommandPalette::new());
            Ok(false)
        }
        KeyAction::OpenHelp => {
            ui.overlay = UiOverlay::Help;
            Ok(false)
        }
        KeyAction::FocusApp => {
            state.focus_app();
            Ok(false)
        }
        KeyAction::FocusTerminal => {
            state.focus_terminal();
            Ok(false)
        }
        KeyAction::Quit => Ok(true),
        KeyAction::None => Ok(false),
    }
}

/// Dispatch a [`Command`], translating the returned [`Effect`] into UI state.
///
/// Some keybound commands carry empty payloads that require a prompt first
/// (NewAgentTab with an empty name, Rename, SetManualStatus, Close); those are
/// intercepted and turned into prompts rather than dispatched immediately.
fn dispatch_command(
    cmd: Command,
    state: &mut AppState,
    services: &Services,
    ui: &mut Ui,
) -> Result<()> {
    // Intercept commands that need interactive input before dispatch.
    match &cmd {
        Command::NewAgentTab { name, .. } if name.is_empty() => {
            start_prompt(
                ui,
                Prompt::NewTabName {
                    buffer: String::new(),
                },
            );
            return Ok(());
        }
        Command::RenameAgentTab { new_name } if new_name.is_empty() => {
            if state.selected().is_none() {
                ui.message("No Agent Tab selected.");
                return Ok(());
            }
            start_prompt(
                ui,
                Prompt::RenameTab {
                    buffer: String::new(),
                },
            );
            return Ok(());
        }
        Command::SetManualStatus(None) => {
            if state.selected().is_none() {
                ui.message("No Agent Tab selected.");
                return Ok(());
            }
            start_prompt(ui, Prompt::SetManualStatus);
            return Ok(());
        }
        Command::CloseAgentTab { action: None } => {
            // Fall through: dispatch returns the option set, which we surface
            // as a Close prompt (SPECS §25, never auto-escalate).
        }
        _ => {}
    }

    let effect = state.dispatch(cmd, services)?;
    apply_effect(effect, state, ui);
    Ok(())
}

/// Map a dispatch [`Effect`] onto the [`Ui`] overlays/prompts (SPECS §22).
fn apply_effect(effect: Effect, _state: &AppState, ui: &mut Ui) {
    match effect {
        Effect::None => ui.clear(),
        Effect::Quit => {} // Handled by the Quit key action, not via Effect here.
        Effect::Message(m) => ui.message(m),
        Effect::Warning(m) => ui.message(format!("WARNING: {m}")),
        Effect::Refused(m) => ui.message(format!("Refused: {m}")),
        Effect::PrUrl(url) => ui.message(format!("PR: {url}")),
        Effect::AttachedExisting { branch } => {
            ui.message(format!("Attached to existing branch {branch}"))
        }
        Effect::PushWarning(_plan) => {
            start_prompt(ui, Prompt::PushConfirm);
        }
        Effect::CloseTabOptions(opts) => {
            start_prompt(
                ui,
                Prompt::CloseTab {
                    actions: opts.actions,
                },
            );
        }
        Effect::GitStatus(status) => {
            ui.overlay = UiOverlay::GitStatus {
                status: *status,
                pr_url: None,
            };
        }
        Effect::ShowHelp => ui.overlay = UiOverlay::Help,
    }
}

/// Begin an interactive prompt, computing its on-screen hint.
fn start_prompt(ui: &mut Ui, prompt: Prompt) {
    let hint = prompt_hint(&prompt, "");
    ui.palette = None;
    ui.overlay = UiOverlay::None;
    ui.prompt = Some(PromptState { prompt, hint });
}

/// Build the message-line hint for a prompt given the current text buffer.
fn prompt_hint(prompt: &Prompt, buffer: &str) -> String {
    match prompt {
        Prompt::NewTabName { .. } => {
            format!("New Agent Tab name: {buffer}_   (Enter to create, Esc to cancel)")
        }
        Prompt::RenameTab { .. } => {
            format!("Rename tab to: {buffer}_   (Enter to apply, Esc to cancel)")
        }
        Prompt::SetManualStatus => {
            "Set status — [i]n progress  [w]aiting  [b]locked  [d]one  [c]lear  (Esc cancel)"
                .to_string()
        }
        Prompt::CloseTab { actions } => {
            let mut parts = Vec::new();
            for (i, a) in actions.iter().enumerate() {
                parts.push(format!("[{}] {}", i + 1, close_action_label(*a)));
            }
            format!("Close tab — {}  (Esc cancel)", parts.join("  "))
        }
        Prompt::PushConfirm => {
            "Worktree has uncommitted changes. [p] push committed only  [c] cancel  (Esc cancel)"
                .to_string()
        }
    }
}

/// Hint for the two free-text prompts, parameterised by whether it is the
/// New-Tab (vs Rename) prompt and the current buffer.
fn text_prompt_hint(is_new: bool, buffer: &str) -> String {
    if is_new {
        format!("New Agent Tab name: {buffer}_   (Enter to create, Esc to cancel)")
    } else {
        format!("Rename tab to: {buffer}_   (Enter to apply, Esc to cancel)")
    }
}

/// Short label for a close action, used in the close menu hint (SPECS §25).
fn close_action_label(a: CloseAction) -> &'static str {
    match a {
        CloseAction::CtrlCPrimary => "Ctrl-C primary",
        CloseAction::CtrlCAll => "Ctrl-C all",
        CloseAction::ForceTerminate => "force terminate",
        CloseAction::IfAllStopped => "if all stopped",
    }
}

/// Handle a key while a prompt is active. On confirmation, dispatches the
/// corresponding command and clears the prompt.
fn handle_prompt_key(
    key: KeyEvent,
    state: &mut AppState,
    services: &Services,
    ui: &mut Ui,
) -> Result<()> {
    // Esc always cancels the prompt.
    if key.code == KeyCode::Esc {
        ui.clear();
        return Ok(());
    }

    let Some(mut pstate) = ui.prompt.take() else {
        return Ok(());
    };

    match &mut pstate.prompt {
        Prompt::NewTabName { .. } | Prompt::RenameTab { .. } => {
            // Capture which kind of text prompt this is without holding a borrow
            // of `pstate.prompt` across the buffer mutation below.
            let is_new = matches!(pstate.prompt, Prompt::NewTabName { .. });
            let buffer = match &mut pstate.prompt {
                Prompt::NewTabName { buffer } | Prompt::RenameTab { buffer } => buffer,
                _ => unreachable!(),
            };
            match key.code {
                KeyCode::Enter => {
                    let name = buffer.trim().to_string();
                    if name.is_empty() {
                        // Keep prompting; nothing entered yet.
                        pstate.hint = text_prompt_hint(is_new, "");
                        ui.prompt = Some(pstate);
                        return Ok(());
                    }
                    let cmd = if is_new {
                        Command::NewAgentTab {
                            name,
                            agent_key: None, // default agent for MVP
                        }
                    } else {
                        Command::RenameAgentTab { new_name: name }
                    };
                    let result = state.dispatch(cmd, services);
                    finish_prompt(result, ui);
                }
                KeyCode::Backspace => {
                    buffer.pop();
                    let buf = buffer.clone();
                    pstate.hint = text_prompt_hint(is_new, &buf);
                    ui.prompt = Some(pstate);
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    buffer.push(c);
                    let buf = buffer.clone();
                    pstate.hint = text_prompt_hint(is_new, &buf);
                    ui.prompt = Some(pstate);
                }
                _ => {
                    ui.prompt = Some(pstate);
                }
            }
        }
        Prompt::SetManualStatus => {
            let choice = match key.code {
                KeyCode::Char('i') => Some(Some(ManualStatus::InProgress)),
                KeyCode::Char('w') => Some(Some(ManualStatus::Waiting)),
                KeyCode::Char('b') => Some(Some(ManualStatus::Blocked)),
                KeyCode::Char('d') => Some(Some(ManualStatus::Done)),
                KeyCode::Char('c') => Some(None),
                _ => None,
            };
            match choice {
                Some(status) => {
                    let result = state.dispatch(Command::SetManualStatus(status), services);
                    finish_prompt(result, ui);
                }
                None => ui.prompt = Some(pstate), // ignore other keys
            }
        }
        Prompt::CloseTab { actions } => {
            // Number keys 1..=N pick an action.
            if let KeyCode::Char(c @ '1'..='9') = key.code {
                let idx = (c as usize) - ('1' as usize);
                if let Some(&action) = actions.get(idx) {
                    let result = state.dispatch(
                        Command::CloseAgentTab {
                            action: Some(action),
                        },
                        services,
                    );
                    finish_prompt(result, ui);
                    return Ok(());
                }
            }
            ui.prompt = Some(pstate);
        }
        Prompt::PushConfirm => {
            let confirm = match key.code {
                KeyCode::Char('p') => Some(PushConfirm::PushCommitted),
                KeyCode::Char('c') => Some(PushConfirm::Cancel),
                _ => None,
            };
            match confirm {
                Some(confirm) => {
                    let result = state.dispatch(
                        Command::PushBranch {
                            confirm: Some(confirm),
                        },
                        services,
                    );
                    finish_prompt(result, ui);
                }
                None => ui.prompt = Some(pstate),
            }
        }
    }

    Ok(())
}

/// Apply the result of a prompt-confirmed dispatch: surface the effect or the
/// error as a message, and clear the prompt either way.
fn finish_prompt(result: Result<Effect>, ui: &mut Ui) {
    ui.prompt = None;
    match result {
        Ok(effect) => apply_effect_no_state(effect, ui),
        Err(e) => ui.message(format!("Error: {e}")),
    }
}

/// `apply_effect` variant used after a prompt where we don't have a spare
/// `&AppState` borrow handy (we never read it anyway).
fn apply_effect_no_state(effect: Effect, ui: &mut Ui) {
    match effect {
        Effect::None => ui.clear(),
        Effect::Quit => {}
        Effect::Message(m) => ui.message(m),
        Effect::Warning(m) => ui.message(format!("WARNING: {m}")),
        Effect::Refused(m) => ui.message(format!("Refused: {m}")),
        Effect::PrUrl(url) => ui.message(format!("PR: {url}")),
        Effect::AttachedExisting { branch } => {
            ui.message(format!("Attached to existing branch {branch}"))
        }
        Effect::PushWarning(_) => start_prompt(ui, Prompt::PushConfirm),
        Effect::CloseTabOptions(opts) => start_prompt(
            ui,
            Prompt::CloseTab {
                actions: opts.actions,
            },
        ),
        Effect::GitStatus(status) => {
            ui.overlay = UiOverlay::GitStatus {
                status: *status,
                pr_url: None,
            }
        }
        Effect::ShowHelp => ui.overlay = UiOverlay::Help,
    }
}

// ---------------------------------------------------------------------------
// Command palette key handling (SPECS §22)
// ---------------------------------------------------------------------------

/// Handle a key while the command palette is open (SPECS §22).
fn handle_palette_key(
    key: KeyEvent,
    state: &mut AppState,
    services: &Services,
    ui: &mut Ui,
) -> Result<()> {
    let Some(palette) = ui.palette.as_mut() else {
        return Ok(());
    };

    match key.code {
        KeyCode::Esc => {
            ui.palette = None;
        }
        KeyCode::Up => palette.select_prev(),
        KeyCode::Down => palette.select_next(),
        KeyCode::Backspace => palette.pop_char(),
        KeyCode::Enter => {
            let action = palette.selected_action().cloned();
            ui.palette = None;
            if let Some(action) = action {
                run_palette_action(action, state, services, ui)?;
            }
        }
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            palette.push_char(c);
        }
        _ => {}
    }
    Ok(())
}

/// Convert a confirmed [`PaletteAction`] into a command (possibly opening a
/// secondary prompt for payloads first), then dispatch (SPECS §22).
fn run_palette_action(
    action: PaletteAction,
    state: &mut AppState,
    services: &Services,
    ui: &mut Ui,
) -> Result<()> {
    match action {
        PaletteAction::Dispatch(cmd) => dispatch_command(cmd, state, services, ui),
        PaletteAction::NewAgentTab => {
            start_prompt(
                ui,
                Prompt::NewTabName {
                    buffer: String::new(),
                },
            );
            Ok(())
        }
        PaletteAction::RenameAgentTab => {
            if state.selected().is_none() {
                ui.message("No Agent Tab selected.");
                return Ok(());
            }
            start_prompt(
                ui,
                Prompt::RenameTab {
                    buffer: String::new(),
                },
            );
            Ok(())
        }
        PaletteAction::CloseAgentTab => {
            // Ask dispatch for the option set, then present the menu (SPECS §25).
            dispatch_command(Command::CloseAgentTab { action: None }, state, services, ui)
        }
        PaletteAction::SetManualStatus => {
            if state.selected().is_none() {
                ui.message("No Agent Tab selected.");
                return Ok(());
            }
            start_prompt(ui, Prompt::SetManualStatus);
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// PTY plumbing
// ---------------------------------------------------------------------------

/// Drain output from every terminal of every tab. Output from each tab's
/// primary feeds status classification (SPECS §24); the active terminal's
/// output is also appended to the rendered viewport buffer.
fn drain_pty_output(state: &mut AppState, viewport: &mut Vec<u8>) {
    // Collect (tab id, primary bytes) first so we can call `ingest_output`
    // (which borrows `state` mutably) without overlapping the session borrow.
    let mut ingest: Vec<(TabId, Vec<u8>)> = Vec::new();
    let active_idx = state.selected_tab;

    for (i, tab) in state.tabs.iter_mut().enumerate() {
        let id = TabId(tab.meta.id.clone());

        // Primary output → classification + (if active terminal) viewport.
        if let Some(primary) = tab.session.primary_mut() {
            if let Ok(bytes) = primary.session_mut().try_read_output() {
                if !bytes.is_empty() {
                    if active_idx == Some(i) && tab.session_selected_child_is_none() {
                        viewport.extend_from_slice(&bytes);
                    }
                    ingest.push((id.clone(), bytes));
                }
            }
        }

        // Child terminals: drain so their PTYs don't stall; forward the active
        // child's output to the viewport.
        let selected_child = tab.session.selected_child();
        for c in 0..tab.session.child_count() {
            if let Some(child) = tab.session.child_mut(c) {
                if let Ok(bytes) = child.session_mut().try_read_output() {
                    if !bytes.is_empty() && active_idx == Some(i) && selected_child == Some(c) {
                        viewport.extend_from_slice(&bytes);
                    }
                }
            }
        }
    }

    for (id, bytes) in ingest {
        state.ingest_output(&id, &bytes);
    }

    // Keep the viewport buffer bounded so it cannot grow without limit.
    const MAX_VIEWPORT: usize = 256 * 1024;
    if viewport.len() > MAX_VIEWPORT {
        let drop = viewport.len() - MAX_VIEWPORT;
        viewport.drain(0..drop);
    }
}

/// Write key bytes to the active terminal's PTY (Terminal-mode passthrough).
fn write_active_pty(state: &mut AppState, bytes: &[u8]) {
    let Some(tab) = state.selected_mut() else {
        return;
    };
    match tab.session.selected_child() {
        Some(c) => {
            if let Some(child) = tab.session.child_mut(c) {
                let _ = child.session_mut().write_input(bytes);
            }
        }
        None => {
            if let Some(primary) = tab.session.primary_mut() {
                let _ = primary.session_mut().write_input(bytes);
            }
        }
    }
}

/// Resize every live PTY session to the new terminal size (SPECS §23 resize).
fn resize_sessions(state: &mut AppState, size: PtySize) {
    for tab in state.tabs.iter_mut() {
        if let Some(primary) = tab.session.primary_mut() {
            let _ = primary.session_mut().resize(size);
        }
        for c in 0..tab.session.child_count() {
            if let Some(child) = tab.session.child_mut(c) {
                let _ = child.session_mut().resize(size);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Git status refresh
// ---------------------------------------------------------------------------

/// Refresh the git-status cache for every tab (SPECS §20, §21). Best-effort:
/// failures for a single tab leave its previous entry untouched.
fn refresh_git_cache(state: &AppState, services: &Services, cache: &mut GitStatusCache) {
    for tab in &state.tabs {
        let worktree = to_absolute(
            &state.repo_root,
            Path::new(&tab.meta.worktree_path_relative),
        );
        if let Ok(status) = collect_status(
            services.git,
            &tab.meta.branch,
            &tab.meta.base_branch,
            &tab.meta.base_commit_sha,
            &worktree,
        ) {
            cache.insert(tab.meta.id.clone(), status);
        }
    }
}

// ---------------------------------------------------------------------------
// Teardown
// ---------------------------------------------------------------------------

/// Persist state on quit, swallowing (but reporting) any error so teardown can
/// proceed (SPECS §9).
fn persist_quietly(state: &AppState, services: &Services) -> Result<()> {
    let project_state = state.to_project_state();
    save_state(services.fs, &state.state_path, &project_state)
}

/// Force-terminate every session in every tab so no orphaned child processes
/// remain after FlightDeck exits (SPECS §25).
fn terminate_all_sessions(state: &mut AppState) {
    for tab in state.tabs.iter_mut() {
        let _ = tab.session.terminate_all();
    }
}

// Small helper on RuntimeTab via an extension trait kept local to wiring.
trait SessionViewportExt {
    fn session_selected_child_is_none(&self) -> bool;
}

impl SessionViewportExt for crate::app::state::RuntimeTab {
    fn session_selected_child_is_none(&self) -> bool {
        self.session.selected_child().is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::{AgentDef, Config, StatusPatterns, UiConfig, WorktreesConfig};
    use crate::testing::{FakeClock, FakeFs, FakeGit, FakePty};
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    fn make_real_agent(dir: &TempDir, key: &str) -> AgentDef {
        let path = dir.path().join(key);
        std::fs::write(&path, "#!/bin/sh\n").unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        AgentDef {
            key: key.to_string(),
            display_name: key.to_string(),
            command: path.to_str().unwrap().to_string(),
            args: vec![],
            status_patterns: StatusPatterns::default(),
        }
    }

    fn config_with_agent(agent: AgentDef) -> Config {
        let mut config = Config {
            ui: UiConfig {
                default_agent: agent.key.clone(),
                agent_tab_position: "left".to_string(),
            },
            worktrees: WorktreesConfig {
                root: ".flightdeck/worktrees".to_string(),
            },
            ..Config::default()
        };
        config.agents.insert(agent.key.clone(), agent);
        config
    }

    // --- prompt hints -----------------------------------------------------

    #[test]
    fn new_tab_prompt_hint_includes_buffer() {
        let p = Prompt::NewTabName {
            buffer: String::new(),
        };
        let hint = prompt_hint(&p, "fix bug");
        assert!(hint.contains("fix bug"));
        assert!(hint.to_lowercase().contains("name"));
    }

    #[test]
    fn close_prompt_hint_lists_numbered_actions() {
        let p = Prompt::CloseTab {
            actions: vec![CloseAction::CtrlCPrimary, CloseAction::ForceTerminate],
        };
        let hint = prompt_hint(&p, "");
        assert!(hint.contains("[1]"));
        assert!(hint.contains("[2]"));
        assert!(hint.contains("Ctrl-C primary"));
    }

    // --- effect → overlay mapping ----------------------------------------

    #[test]
    fn effect_message_becomes_message_overlay() {
        let mut ui = Ui::default();
        apply_effect_no_state(Effect::Message("hi".to_string()), &mut ui);
        match ui.render_overlay() {
            UiOverlay::Message(m) => assert_eq!(m, "hi"),
            other => panic!("expected message overlay, got {other:?}"),
        }
    }

    #[test]
    fn effect_push_warning_opens_push_prompt() {
        let mut ui = Ui::default();
        apply_effect_no_state(
            Effect::PushWarning(crate::git::remote::PushPlan::UncommittedChanges),
            &mut ui,
        );
        assert!(ui.prompt.is_some());
        assert!(matches!(
            ui.prompt.as_ref().unwrap().prompt,
            Prompt::PushConfirm
        ));
    }

    #[test]
    fn effect_close_options_opens_close_prompt() {
        let mut ui = Ui::default();
        let opts = crate::app::commands::CloseTabOptions::standard();
        apply_effect_no_state(Effect::CloseTabOptions(opts), &mut ui);
        assert!(matches!(
            ui.prompt.as_ref().unwrap().prompt,
            Prompt::CloseTab { .. }
        ));
    }

    // --- modal capture ----------------------------------------------------

    #[test]
    fn modal_active_when_prompt_present() {
        let mut ui = Ui::default();
        assert!(!ui.modal_active());
        start_prompt(&mut ui, Prompt::SetManualStatus);
        assert!(ui.modal_active());
        ui.clear();
        assert!(!ui.modal_active());
    }

    // --- startup builds an AppState with the fakes (no terminal) ----------

    #[test]
    fn startup_builds_state_and_records_dirty_base_warning() {
        let dir = TempDir::new().unwrap();
        let agent = make_real_agent(&dir, "opencode");
        let config = config_with_agent(agent);
        let toml = crate::config::load::serialize_config(&config).unwrap();

        let repo = Path::new("/repo");
        let fs = FakeFs::new()
            .with_dir("/repo")
            .with_file("/repo/.flightdeck/config.toml", toml.as_str());
        let git = FakeGit::new().with_root("/repo").with_branches(["main"]);
        git.set_dirty_at(repo, true);
        let pty = FakePty::new();
        let clock = FakeClock::default();
        let services = Services {
            git: &git,
            fs: &fs,
            pty: &pty,
            clock: &clock,
        };

        let state = startup(&services, repo, repo).expect("startup should succeed");
        assert_eq!(state.base_branch, "main");
        assert!(state
            .warnings
            .iter()
            .any(|w| w.contains("local merge disabled")));
    }

    #[test]
    fn startup_falls_back_to_default_state_when_missing() {
        let dir = TempDir::new().unwrap();
        let agent = make_real_agent(&dir, "opencode");
        let config = config_with_agent(agent);
        let toml = crate::config::load::serialize_config(&config).unwrap();

        let repo = Path::new("/repo");
        let fs = FakeFs::new()
            .with_dir("/repo")
            .with_file("/repo/.flightdeck/config.toml", toml.as_str());
        let git = FakeGit::new().with_root("/repo").with_branches(["main"]);
        let pty = FakePty::new();
        let clock = FakeClock::default();
        let services = Services {
            git: &git,
            fs: &fs,
            pty: &pty,
            clock: &clock,
        };

        let state = startup(&services, repo, repo).expect("startup should succeed");
        assert!(state.tabs.is_empty());
        assert!(!state
            .warnings
            .iter()
            .any(|w| w.contains("local merge disabled")));
    }

    #[test]
    fn derive_project_name_uses_dir_name() {
        assert_eq!(derive_project_name(Path::new("/a/b/myproj")), "myproj");
    }
}
