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
pub mod notify;
pub mod persistence;
pub mod terminal;
pub mod tui;

use std::path::Path;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::app::commands::{CloseAction, Command, Effect, PushConfirm, Selector};
use crate::app::state::{materialize_worktree, AppState, Services, TabPhase, WorktreeJob};
use crate::config::init::initialize;
use crate::config::load::{load_config, serialize_config};
use crate::config::schema::default_config;
use crate::contracts::error::{FlightDeckError, Result};
use crate::contracts::real::{RealClock, RealFs};
use crate::contracts::{FileSystem, ManualStatus, Notifier, PtySize, TabId};
use crate::fs::ignore::ensure_flightdeck_gitignore;
use crate::fs::paths::to_absolute;
use crate::git::repo::{detect_base_branch, GitCli};
use crate::git::status::{collect_status, WorktreeStatus};
use crate::notify::SystemNotifier;
use crate::persistence::project_state::{default_state, load_state, save_state};
use crate::persistence::recovery::recover;
use crate::terminal::pty::PortablePtyBackend;
use crate::tui::input::{map_key, KeyAction};
use crate::tui::palette::{CommandPalette, PaletteAction};
use crate::tui::render::{draw, hit_test, ChildTarget, GitStatusCache, HitTarget, UiOverlay};

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;

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
    if std::env::args().any(|arg| arg == "--version" || arg == "-V") {
        println!("flightdeck {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    // `--help`/`-h` must be handled explicitly: otherwise it falls through and
    // launches the full TUI instead of printing usage.
    if std::env::args().any(|arg| arg == "--help" || arg == "-h") {
        print_help();
        return Ok(());
    }

    // Subcommand dispatch. These configure the opt-in status/notification
    // features and exit without launching the TUI (SPECS §24).
    match std::env::args().nth(1).as_deref() {
        // Install the precise status hooks/plugin (Layer 2).
        Some("setup-status") => return run_setup_status(),
        // Enable OS notifications in config (off by default).
        Some("setup-notifications") => return run_setup_notifications(),
        _ => {}
    }

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

    // Enable mouse capture so tabs are clickable (best effort).
    let _ = crossterm::execute!(std::io::stdout(), EnableMouseCapture);

    // Take ownership of the terminal title so it stays a stable
    // "flightdeck — <project>" while we run, instead of inheriting (and
    // flickering with) whatever title the parent tooling keeps rewriting. The
    // previous title is pushed onto the terminal's title stack so it can be
    // restored on exit. Best effort — terminals without XTWINOPS just ignore it.
    let _ = save_and_set_terminal_title(&format!(
        "flightdeck — {}",
        derive_project_name(&repo_root)
    ));

    // Seed the PTY size from the terminal viewport (not the whole screen) so
    // agents wrap at the right width.
    if let Ok(size) = terminal.size() {
        state.set_pty_size(viewport_pty_size(PtySize {
            rows: size.height,
            cols: size.width,
        }));
    }

    // Resume: start the primary agent for every recovered/loaded tab whose
    // worktree still exists (best effort). Done here, after the viewport size is
    // known, rather than in `recover`/`AppState::new` which never spawn.
    let _ = state.resume_agents(&services);

    let notifier = SystemNotifier;
    // Background workers (worktree creation, git-status refresh) run git off
    // the UI thread; they need an owned, cloneable git handle.
    let loop_result = event_loop(&mut terminal, &mut state, &services, &notifier, git.clone());

    // CLEAN TEARDOWN (SPECS §25): always restore the terminal, then terminate
    // every session so no orphaned child processes remain. Persist on the way
    // out (best effort) regardless of how the loop ended.
    let _ = crossterm::execute!(std::io::stdout(), DisableMouseCapture);
    let _ = restore_terminal_title();
    ratatui::restore();
    let persist_result = persist_quietly(&state, &services);
    terminate_all_sessions(&mut state);

    loop_result.and(persist_result)
}

/// `flightdeck setup-status`: install the opt-in precise agent status
/// integrations (hooks/plugin) into `<repo>/.flightdeck/integrations/` and add
/// the `.flightdeck/agent-status` `.gitignore` entry, then print wiring
/// instructions. Does not launch the TUI (SPECS §24, Layer 2).
fn run_setup_status() -> Result<()> {
    let cwd = std::env::current_dir()
        .map_err(|e| FlightDeckError::Io(format!("could not determine current directory: {e}")))?;
    let git = GitCli::discover(&cwd).map_err(|_| {
        FlightDeckError::Git(
            "not inside a Git repository (run `flightdeck setup-status` from a git project)"
                .to_string(),
        )
    })?;
    let repo_root = git.root().to_path_buf();

    let fs = RealFs;
    let report = crate::agents::setup::write_status_integrations(&fs, &repo_root)?;

    let dir = repo_root.join(crate::agents::setup::INTEGRATIONS_DIR);
    println!("FlightDeck: wrote status integrations to {}", dir.display());
    for p in &report.written {
        if let Some(name) = p.file_name() {
            println!("  - {}", name.to_string_lossy());
        }
    }
    if report.gitignore_added {
        println!("FlightDeck: added .flightdeck/agent-status to .gitignore (commit this).");
    }
    println!();
    println!("Status detection works out of the box (output-activity based).");
    println!("To enable PRECISE status, wire one or more agents — see:");
    println!("  {}/README.md", dir.display());
    println!();
    println!("  Claude Code → merge claude-code.settings.json into ~/.claude/settings.json");
    println!("  Codex CLI   → append codex-config.toml to ~/.codex/config.toml");
    println!("  OpenCode    → copy opencode-flightdeck.js to ~/.config/opencode/plugin/");
    Ok(())
}

/// `flightdeck setup-notifications`: turn on OS notifications by setting
/// `notifications.enabled = true` in `<repo>/.flightdeck/config.toml` (creating
/// the config on first run), then print how to tune or disable them. Does not
/// launch the TUI (SPECS §24). Notifications are off by default; this is the
/// quick way to opt in without hand-editing the config.
fn run_setup_notifications() -> Result<()> {
    let cwd = std::env::current_dir()
        .map_err(|e| FlightDeckError::Io(format!("could not determine current directory: {e}")))?;
    let git = GitCli::discover(&cwd).map_err(|_| {
        FlightDeckError::Git(
            "not inside a Git repository (run `flightdeck setup-notifications` from a git project)"
                .to_string(),
        )
    })?;
    let repo_root = git.root().to_path_buf();
    let fs = RealFs;
    let config_path = repo_root.join(".flightdeck").join("config.toml");

    // Ensure a config exists (first run writes the default), then load it.
    if !fs.exists(&config_path) {
        let project_name = derive_project_name(&repo_root);
        let base_branch = detect_base_branch(&git, &cwd, None)?;
        initialize(&fs, &repo_root, &project_name, &base_branch)?;
    }
    let mut config = load_config(&fs, &config_path)?;

    if config.notifications.enabled {
        println!(
            "FlightDeck: OS notifications are already enabled in {}.",
            config_path.display()
        );
    } else {
        config.notifications.enabled = true;
        fs.write(&config_path, &serialize_config(&config)?)?;
        println!(
            "FlightDeck: enabled OS notifications in {}.",
            config_path.display()
        );
    }
    println!();
    println!("You'll be notified when an agent finishes a task, waits for input, or fails.");
    println!("Tune per-category under [notifications] (set enabled = false to turn off):");
    println!("  enabled    = true   # master switch");
    println!("  on_finish  = true   # agent went idle / completed");
    println!("  on_waiting = true   # agent is waiting for input / needs attention");
    println!("  on_failed  = true   # agent errored out");
    println!();
    println!("macOS delivery: `brew install terminal-notifier` for best reliability,");
    println!("or allow Script Editor under System Settings → Notifications.");
    Ok(())
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

/// Print usage for `flightdeck --help`/`-h`.
fn print_help() {
    println!("flightdeck {}", env!("CARGO_PKG_VERSION"));
    println!("Terminal UI for orchestrating multiple local AI coding agents.");
    println!();
    println!("USAGE:");
    println!("    flightdeck [SUBCOMMAND]");
    println!();
    println!("Run with no arguments inside a Git repository to launch the TUI.");
    println!();
    println!("SUBCOMMANDS:");
    println!("    setup-status           Install the opt-in precise agent-status integrations");
    println!("    setup-notifications    Enable OS notifications when agents finish");
    println!();
    println!("OPTIONS:");
    println!("    -h, --help       Print this help");
    println!("    -V, --version    Print version");
}

/// Push the current window/icon title onto the terminal's title stack
/// (XTWINOPS `CSI 22;0t`) and set our own stable title (OSC 0). Best effort:
/// terminals without XTWINOPS ignore the push, and the title is restored on
/// exit by [`restore_terminal_title`].
fn save_and_set_terminal_title(title: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut out = std::io::stdout();
    write!(out, "\x1b[22;0t\x1b]0;{title}\x07")?;
    out.flush()
}

/// Pop the title saved by [`save_and_set_terminal_title`] back off the
/// terminal's title stack (XTWINOPS `CSI 23;0t`).
fn restore_terminal_title() -> std::io::Result<()> {
    use std::io::Write;
    let mut out = std::io::stdout();
    write!(out, "\x1b[23;0t")?;
    out.flush()
}

// ---------------------------------------------------------------------------
// Overlay / prompt state machine
// ---------------------------------------------------------------------------

/// An interactive secondary prompt the loop is currently collecting (SPECS §22,
/// §25). These are the multi-step flows the palette/keys require: text entry for
/// New/Rename, single-key choice menus for Set Status / Close / Push.
enum Prompt {
    /// Pick which agent a new tab should run, before naming it. Holds the
    /// `(key, display_name)` of each registered agent; a number key selects one
    /// and advances to [`Prompt::NewTabName`] (SPECS §4, §22).
    SelectAgent { agents: Vec<(String, String)> },
    /// Free-text entry for a new Agent Tab name; dispatches `NewAgentTab` with
    /// the agent chosen in [`Prompt::SelectAgent`] (`None` = configured default).
    NewTabName {
        buffer: String,
        agent_key: Option<String>,
    },
    /// Free-text entry for renaming the selected tab; dispatches `RenameAgentTab`.
    RenameTab { buffer: String },
    /// Pick a manual status (or clear); dispatches `SetManualStatus`.
    SetManualStatus,
    /// Choose how to handle running processes when closing (SPECS §25).
    CloseTab { actions: Vec<CloseAction> },
    /// Confirm a push despite uncommitted changes (SPECS §14).
    PushConfirm,
    /// Confirm abandoning a worktree that has uncommitted changes (SPECS §5/§15).
    AbandonConfirm,
    /// Confirm a local merge-back; on success the worktree is removed and the
    /// tab closed, stopping the agent if it is still running (SPECS §15).
    MergeConfirm {
        agent_branch: String,
        base_branch: String,
        primary_running: bool,
    },
}

/// An in-progress mouse text selection drag over the terminal viewport (SPECS
/// §20). The selection itself lives on the active [`crate::terminal::session::Terminal`];
/// this only tracks the latest pointer position so the event loop can auto-scroll
/// while the pointer sits at (or beyond) a viewport edge.
struct DragState {
    /// Latest absolute pointer column (terminal coordinates).
    col: u16,
    /// Latest absolute pointer row (terminal coordinates).
    row: u16,
}

/// The full interactive UI state layered over [`AppState`]: which overlay is
/// drawn, plus any in-progress prompt.
#[derive(Default)]
struct Ui {
    overlay: UiOverlay,
    palette: Option<CommandPalette>,
    prompt: Option<PromptState>,
    /// Set when a dispatched [`Effect::Quit`] asks the app to exit (e.g. the
    /// "Quit" command palette action). The event loop checks this each turn.
    should_quit: bool,
    /// Active mouse text-selection drag, if the left button is held over the
    /// terminal viewport (SPECS §20).
    drag: Option<DragState>,
    /// Worktree-creation jobs queued by [`AppState::begin_new_agent_tab`] this
    /// turn, awaiting hand-off to a background worker by the event loop. Keeps
    /// the slow `git worktree add` off the UI thread.
    pending_jobs: Vec<WorktreeJob>,
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
    notifier: &dyn Notifier,
    worker_git: GitCli,
) -> Result<()> {
    let mut ui = Ui::default();
    let mut cache: GitStatusCache = GitStatusCache::new();
    let mut tick: u64 = 0;

    // Suppress notifications briefly at startup so resumed/just-launched agents
    // settling to idle don't produce a burst of "finished" alerts (SPECS §24).
    state.begin_notification_grace(services.clock.now_millis());

    // Background-work channels (SPECS §16/§17/§21): slow git runs off the UI
    // thread so the loop keeps drawing, draining PTYs, and handling input.
    let (create_tx, create_rx) = std::sync::mpsc::channel::<CreateOutcome>();
    let (status_tx, status_rx) = std::sync::mpsc::channel::<StatusMsg>();
    let mut status_in_flight = false;
    // Serializes THIS instance's `git worktree add`s so two quick new-tab
    // requests don't race on the repo's index/worktree locks.
    let git_lock: Arc<Mutex<()>> = Arc::new(Mutex::new(()));

    loop {
        let now_ms = services.clock.now_millis();

        // --- Drain PTY output and feed each terminal's VT parser + status. ---
        drain_pty_output(state, now_ms);

        // --- Apply completed background worktree-creation jobs. ---
        drain_create_outcomes(&create_rx, state, services, &mut ui);

        // --- Apply background git-status results into the cache. ---
        while let Ok(msg) = status_rx.try_recv() {
            match msg {
                StatusMsg::Update(id, status) => {
                    cache.insert(id, status);
                }
                StatusMsg::Done => status_in_flight = false,
            }
        }

        // --- Poll opt-in agent status files (precise idle/working signals). ---
        state.poll_status_files(services, now_ms);

        // --- Fire OS notifications for agents that just finished a task. ---
        for n in state.take_finish_notifications(now_ms) {
            notifier.notify(&n);
        }

        // --- Periodically refresh the git-status cache off the UI thread. ---
        if tick.is_multiple_of(GIT_REFRESH_EVERY)
            && !status_in_flight
            && spawn_status_refresh(state, &worker_git, &status_tx)
        {
            status_in_flight = true;
        }
        tick = tick.wrapping_add(1);

        // --- Auto-scroll the terminal while a selection drag rests at an edge. ---
        if ui.drag.is_some() {
            if let Ok(size) = terminal.size() {
                autoscroll_drag(state, &ui, Rect::new(0, 0, size.width, size.height));
            }
        }

        // --- Render. ---
        let overlay = ui.render_overlay();
        terminal
            .draw(|frame| draw(frame, state, &cache, &overlay, now_ms))
            .map_err(|e| FlightDeckError::Io(format!("render failed: {e}")))?;

        // --- Poll for input (short timeout so PTY output keeps flowing). ---
        let has_event = event::poll(POLL_TIMEOUT)
            .map_err(|e| FlightDeckError::Io(format!("event poll failed: {e}")))?;
        if !has_event {
            continue;
        }

        match event::read().map_err(|e| FlightDeckError::Io(format!("event read failed: {e}")))? {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                if handle_key(key, state, services, &mut ui)? {
                    break; // Quit requested via the Ctrl-q key action.
                }
            }
            Event::Mouse(me) => {
                let area = match terminal.size() {
                    Ok(s) => Rect::new(0, 0, s.width, s.height),
                    Err(_) => continue,
                };
                handle_mouse(me, area, state, services, &mut ui);
            }
            Event::Resize(cols, rows) => {
                let size = viewport_pty_size(PtySize { rows, cols });
                state.set_pty_size(size);
                resize_sessions(state, size);
            }
            _ => {}
        }

        // --- Hand off any queued worktree-creation jobs to background workers
        //     so `git worktree add` never blocks the loop (SPECS §16/§17). ---
        for job in ui.pending_jobs.drain(..) {
            spawn_worktree_job(job, &worker_git, &git_lock, &create_tx);
        }

        // A dispatched Effect::Quit (e.g. the "Quit" palette action) also exits.
        if ui.should_quit {
            break;
        }
    }

    Ok(())
}

/// One completed background worktree-creation job: which placeholder tab to
/// finalize, and whether materialization succeeded (SPECS §16/§17).
struct CreateOutcome {
    tab_id: String,
    result: Result<()>,
}

/// A message from the background git-status worker (SPECS §21).
enum StatusMsg {
    /// A tab's freshly collected worktree status, keyed by tab id.
    Update(String, WorktreeStatus),
    /// The refresh batch finished (clears the in-flight guard).
    Done,
}

/// Spawn a background worker that materializes `job`'s worktree (the slow
/// `git worktree add`) and reports the outcome back over `create_tx`. The
/// `git_lock` serializes this instance's worktree adds so concurrent new-tab
/// requests don't race on the repo's index/worktree locks.
fn spawn_worktree_job(
    job: WorktreeJob,
    worker_git: &GitCli,
    git_lock: &Arc<Mutex<()>>,
    create_tx: &Sender<CreateOutcome>,
) {
    let git = worker_git.clone();
    let lock = Arc::clone(git_lock);
    let tx = create_tx.clone();
    std::thread::spawn(move || {
        let result = {
            // Recover from a poisoned lock (a previous worker panicked) rather
            // than cascading the panic into every future creation.
            let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
            materialize_worktree(&git, &job)
        };
        let _ = tx.send(CreateOutcome {
            tab_id: job.tab_id,
            result,
        });
    });
}

/// Drain finished worktree-creation jobs and reflect them in [`AppState`]:
/// finalize (spawn the agent, flip to Ready) on success, or remove the
/// placeholder tab and surface the error on failure.
fn drain_create_outcomes(
    create_rx: &Receiver<CreateOutcome>,
    state: &mut AppState,
    services: &Services,
    ui: &mut Ui,
) {
    while let Ok(outcome) = create_rx.try_recv() {
        match outcome.result {
            Ok(()) => match state.finalize_new_tab(&outcome.tab_id, services) {
                Ok(effect) => apply_effect(effect, state, ui),
                Err(e) => {
                    state.fail_new_tab(&outcome.tab_id);
                    ui.message(format!("Failed to start agent: {e}"));
                }
            },
            Err(e) => {
                state.fail_new_tab(&outcome.tab_id);
                ui.message(format!("Failed to create worktree: {e}"));
            }
        }
    }
}

/// Snapshot every [`TabPhase::Ready`] tab's parameters and spawn a single
/// background worker that runs `collect_status` for each, publishing results
/// over `status_tx`. Returns whether a worker was actually spawned (i.e. there
/// was at least one tab to refresh). Keeps git status off the UI thread so a
/// busy repo — e.g. another instance running `git worktree add` — never freezes
/// the UI (SPECS §21).
fn spawn_status_refresh(state: &AppState, worker_git: &GitCli, status_tx: &Sender<StatusMsg>) -> bool {
    struct StatusReq {
        tab_id: String,
        branch: String,
        base_branch: String,
        base_commit_sha: String,
        worktree_abs: std::path::PathBuf,
    }

    let reqs: Vec<StatusReq> = state
        .tabs
        .iter()
        .filter(|t| t.phase == TabPhase::Ready)
        .map(|t| StatusReq {
            tab_id: t.meta.id.clone(),
            branch: t.meta.branch.clone(),
            base_branch: t.meta.base_branch.clone(),
            base_commit_sha: t.meta.base_commit_sha.clone(),
            worktree_abs: to_absolute(&state.repo_root, Path::new(&t.meta.worktree_path_relative)),
        })
        .collect();

    if reqs.is_empty() {
        return false;
    }

    let git = worker_git.clone();
    let tx = status_tx.clone();
    std::thread::spawn(move || {
        for r in reqs {
            if let Ok(status) = collect_status(
                &git,
                &r.branch,
                &r.base_branch,
                &r.base_commit_sha,
                &r.worktree_abs,
            ) {
                let _ = tx.send(StatusMsg::Update(r.tab_id, status));
            }
        }
        let _ = tx.send(StatusMsg::Done);
    });
    true
}

/// Compute the PTY/terminal-viewport size from the full terminal size. Agents
/// must wrap at the viewport width (total minus the sidebar/borders), not the
/// whole screen.
fn viewport_pty_size(full: PtySize) -> PtySize {
    let ml = crate::tui::layout::compute(Rect::new(0, 0, full.cols, full.rows));
    PtySize {
        rows: ml.terminal.height.max(1),
        cols: ml.terminal.width.max(1),
    }
}

/// Number of scrollback lines moved per mouse-wheel notch.
const SCROLL_LINES: usize = 3;
/// xterm protocol button code for a wheel-up event.
const MOUSE_WHEEL_UP: u8 = 64;
/// xterm protocol button code for a wheel-down event.
const MOUSE_WHEEL_DOWN: u8 = 65;

/// Handle a mouse event (SPECS §20, §22 — keyboard-first, but mouse-assisted):
/// a left click selects the clicked Agent Tab or child-terminal tab, the wheel
/// scrolls the active terminal, and a left-button drag over the terminal
/// viewport selects text for copy/paste (auto-copying on release).
///
/// When the hosted application has its own mouse reporting enabled (a full-screen
/// TUI), plain button/drag events are forwarded to it so it still works; holding
/// Shift forces local text selection instead.
fn handle_mouse(
    me: MouseEvent,
    area: Rect,
    state: &mut AppState,
    services: &Services,
    ui: &mut Ui,
) {
    // Ignore mouse while a modal/prompt/overlay is capturing input.
    if ui.modal_active() {
        return;
    }
    let term_area = crate::tui::layout::compute(area).terminal;
    match me.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            // A click on a tab switches to it (and never starts a selection).
            if let Some(hit) = hit_test(area, state, me.column, me.row) {
                ui.drag = None;
                match hit {
                    HitTarget::AgentTab(i) => {
                        let _ =
                            state.dispatch(Command::SwitchAgentTab(Selector::Index(i)), services);
                    }
                    HitTarget::Child(ChildTarget::Primary) => {
                        if let Some(tab) = state.selected_mut() {
                            tab.session.focus_primary();
                        }
                    }
                    HitTarget::Child(ChildTarget::Child(i)) => {
                        let _ = state
                            .dispatch(Command::SwitchChildTerminal(Selector::Index(i)), services);
                    }
                }
                return;
            }
            // A press inside the terminal viewport begins a selection (or is
            // forwarded to a mouse-aware hosted app unless Shift is held).
            if rect_contains(term_area, me.column, me.row) {
                let shift = me.modifiers.contains(KeyModifiers::SHIFT);
                let col = me.column.saturating_sub(term_area.x);
                let row = me.row.saturating_sub(term_area.y);
                if let Some(term) = state.selected_mut().and_then(|t| t.session.active_mut()) {
                    if term.wants_mouse() && !shift {
                        let bytes = encode_mouse_button(term.mouse_encoding(), 0, col, row, true);
                        let _ = term.session_mut().write_input(&bytes);
                        ui.drag = None;
                    } else {
                        term.begin_selection(row, col);
                        ui.drag = Some(DragState {
                            col: me.column,
                            row: me.row,
                        });
                    }
                }
            }
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            if ui.drag.is_some() {
                if let Some(d) = ui.drag.as_mut() {
                    d.col = me.column;
                    d.row = me.row;
                }
                // Clamp the pointer into the viewport so a drag past an edge keeps
                // extending along that edge (auto-scroll reveals more each tick).
                let col = me
                    .column
                    .saturating_sub(term_area.x)
                    .min(term_area.width.saturating_sub(1));
                let row = me
                    .row
                    .saturating_sub(term_area.y)
                    .min(term_area.height.saturating_sub(1));
                if let Some(term) = state.selected_mut().and_then(|t| t.session.active_mut()) {
                    term.update_selection(row, col);
                }
            } else if rect_contains(term_area, me.column, me.row) {
                // Forwarded drag for a mouse-aware hosted app.
                let col = me.column.saturating_sub(term_area.x);
                let row = me.row.saturating_sub(term_area.y);
                if let Some(term) = state.selected_mut().and_then(|t| t.session.active_mut()) {
                    if term.wants_mouse() {
                        let bytes = encode_mouse_button(term.mouse_encoding(), 32, col, row, true);
                        let _ = term.session_mut().write_input(&bytes);
                    }
                }
            }
        }
        MouseEventKind::Up(MouseButton::Left) => {
            if ui.drag.take().is_some() {
                // End of a local selection: copy it (and keep it highlighted as
                // confirmation), or clear a zero-length click.
                if let Some(term) = state.selected_mut().and_then(|t| t.session.active_mut()) {
                    if term.has_selection() {
                        if let Some(text) = term.selected_text() {
                            crate::tui::clipboard::copy(&text);
                        }
                    } else {
                        term.clear_selection();
                    }
                }
            } else if rect_contains(term_area, me.column, me.row) {
                // Forwarded release for a mouse-aware hosted app.
                let col = me.column.saturating_sub(term_area.x);
                let row = me.row.saturating_sub(term_area.y);
                if let Some(term) = state.selected_mut().and_then(|t| t.session.active_mut()) {
                    if term.wants_mouse() {
                        let bytes = encode_mouse_button(term.mouse_encoding(), 0, col, row, false);
                        let _ = term.session_mut().write_input(&bytes);
                    }
                }
            }
        }
        MouseEventKind::ScrollUp => handle_scroll(state, area, me, true),
        MouseEventKind::ScrollDown => handle_scroll(state, area, me, false),
        _ => {}
    }
}

/// Number of scrollback lines moved per auto-scroll tick during a drag.
const AUTOSCROLL_LINES: usize = 1;

/// While a selection drag rests at (or beyond) a vertical edge of the terminal
/// viewport, scroll the view a step and extend the selection into the newly
/// revealed region. Called once per event-loop tick so scrolling continues even
/// when the pointer is held still (crossterm emits no events without movement).
fn autoscroll_drag(state: &mut AppState, ui: &Ui, area: Rect) {
    let Some(drag) = ui.drag.as_ref() else {
        return;
    };
    let term_area = crate::tui::layout::compute(area).terminal;
    if term_area.height == 0 {
        return;
    }
    // Top edge (pointer at or above the first row) scrolls up into history;
    // bottom edge (at or below the last row) scrolls back down.
    let up = if drag.row <= term_area.y {
        true
    } else if drag.row >= term_area.bottom().saturating_sub(1) {
        false
    } else {
        return;
    };

    let Some(term) = state.selected_mut().and_then(|t| t.session.active_mut()) else {
        return;
    };
    if term.selection().is_none() {
        return;
    }
    if up {
        term.scroll_up(AUTOSCROLL_LINES);
    } else {
        term.scroll_down(AUTOSCROLL_LINES);
    }
    // Pin the head to the edge row at the new offset so the selection grows to
    // cover the revealed line.
    let edge_row = if up {
        0
    } else {
        term_area.height.saturating_sub(1)
    };
    let col = drag
        .col
        .saturating_sub(term_area.x)
        .min(term_area.width.saturating_sub(1));
    term.update_selection(edge_row, col);
}

/// Handle a mouse-wheel event over the terminal viewport. When the hosted agent
/// app has mouse reporting enabled (a full-screen TUI with its own scroll
/// region, e.g. opencode), the wheel event is forwarded to its PTY so the app
/// scrolls itself — exactly as in a real terminal emulator. Otherwise we scroll
/// the terminal's own VT100 scrollback so plain output stays reviewable.
fn handle_scroll(state: &mut AppState, area: Rect, me: MouseEvent, up: bool) {
    let term_area = crate::tui::layout::compute(area).terminal;
    if !rect_contains(term_area, me.column, me.row) {
        return;
    }
    let Some(tab) = state.selected_mut() else {
        return;
    };
    let Some(term) = tab.session.active_mut() else {
        return;
    };
    if term.wants_mouse() {
        let cb = if up { MOUSE_WHEEL_UP } else { MOUSE_WHEEL_DOWN };
        let col = me.column.saturating_sub(term_area.x);
        let row = me.row.saturating_sub(term_area.y);
        let bytes = encode_mouse_report(term.mouse_encoding(), cb, col, row);
        let _ = term.session_mut().write_input(&bytes);
    } else if up {
        term.scroll_up(SCROLL_LINES);
    } else {
        term.scroll_down(SCROLL_LINES);
    }
}

/// Whether `(col, row)` lies within `r`.
fn rect_contains(r: Rect, col: u16, row: u16) -> bool {
    col >= r.x
        && col < r.x.saturating_add(r.width)
        && row >= r.y
        && row < r.y.saturating_add(r.height)
}

/// Encode a mouse report for the hosted application, matching its active mouse
/// encoding. `cb` is the xterm protocol button code; `col`/`row` are 0-based
/// cell coordinates within the terminal viewport (protocol coordinates are
/// 1-based).
fn encode_mouse_report(
    encoding: vt100::MouseProtocolEncoding,
    cb: u8,
    col: u16,
    row: u16,
) -> Vec<u8> {
    let cx = col.saturating_add(1);
    let cy = row.saturating_add(1);
    match encoding {
        vt100::MouseProtocolEncoding::Sgr => format!("\x1b[<{cb};{cx};{cy}M").into_bytes(),
        // Default (X10) and, approximately, the legacy UTF-8 encoding: one
        // printable byte per field, offset by 32 and clamped to a single byte.
        _ => {
            let bx = cx.saturating_add(32).min(255) as u8;
            let by = cy.saturating_add(32).min(255) as u8;
            vec![0x1b, b'[', b'M', cb.saturating_add(32), bx, by]
        }
    }
}

/// Encode a mouse button press/drag/release report for a mouse-aware hosted
/// application. `cb` is the xterm button code (0 = left, +32 = motion/drag);
/// `pressed` distinguishes press/drag (`true`) from release (`false`). `col`/
/// `row` are 0-based viewport cells (protocol coordinates are 1-based).
fn encode_mouse_button(
    encoding: vt100::MouseProtocolEncoding,
    cb: u8,
    col: u16,
    row: u16,
    pressed: bool,
) -> Vec<u8> {
    let cx = col.saturating_add(1);
    let cy = row.saturating_add(1);
    match encoding {
        // SGR reports the same button code for release but terminate with 'm'.
        vt100::MouseProtocolEncoding::Sgr => {
            let end = if pressed { 'M' } else { 'm' };
            format!("\x1b[<{cb};{cx};{cy}{end}").into_bytes()
        }
        // X10 has no release button code — release is reported as button 3.
        _ => {
            let code = if pressed { cb } else { 3 };
            let bx = cx.saturating_add(32).min(255) as u8;
            let by = cy.saturating_add(32).min(255) as u8;
            vec![0x1b, b'[', b'M', code.saturating_add(32), bx, by]
        }
    }
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
        KeyAction::Paste => {
            paste_into_active_pty(state);
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
            start_new_tab_flow(state, ui);
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
        Effect::Quit => ui.should_quit = true,
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
        Effect::AbandonWarning => {
            start_prompt(ui, Prompt::AbandonConfirm);
        }
        Effect::MergeConfirm {
            agent_branch,
            base_branch,
            primary_running,
        } => {
            start_prompt(
                ui,
                Prompt::MergeConfirm {
                    agent_branch,
                    base_branch,
                    primary_running,
                },
            );
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

/// Begin the New Agent Tab flow (SPECS §4, §22): if more than one agent is
/// registered, show the agent picker first; otherwise skip straight to the name
/// prompt using the configured default agent.
fn start_new_tab_flow(state: &AppState, ui: &mut Ui) {
    let agents: Vec<(String, String)> = state
        .registry
        .all()
        .iter()
        .map(|a| (a.key.clone(), a.display_name.clone()))
        .collect();
    if agents.len() > 1 {
        start_prompt(ui, Prompt::SelectAgent { agents });
    } else {
        // Zero or one agent: no meaningful choice — use the default.
        start_prompt(
            ui,
            Prompt::NewTabName {
                buffer: String::new(),
                agent_key: None,
            },
        );
    }
}

/// Build the message-line hint for a prompt given the current text buffer.
fn prompt_hint(prompt: &Prompt, buffer: &str) -> String {
    match prompt {
        Prompt::SelectAgent { agents } => {
            let mut parts = Vec::new();
            for (i, (_key, display)) in agents.iter().enumerate() {
                parts.push(format!("[{}] {}", i + 1, display));
            }
            format!("New Agent Tab — pick agent: {}  (Esc cancel)", parts.join("  "))
        }
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
        Prompt::AbandonConfirm => {
            "Worktree has uncommitted changes — discard them? [y] abandon (force)  [n] cancel  (Esc cancel)"
                .to_string()
        }
        Prompt::MergeConfirm {
            agent_branch,
            base_branch,
            primary_running,
        } => {
            let running = if *primary_running {
                " (stops the running agent)"
            } else {
                ""
            };
            format!(
                "Merge {agent_branch} into {base_branch} then remove the worktree{running}? [y] merge  [n] cancel  (Esc cancel)"
            )
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
        Prompt::SelectAgent { agents } => {
            // A number key picks an agent and advances to the name prompt.
            if let KeyCode::Char(c @ '1'..='9') = key.code {
                let idx = (c as usize) - ('1' as usize);
                if let Some((agent_key, _display)) = agents.get(idx) {
                    let prompt = Prompt::NewTabName {
                        buffer: String::new(),
                        agent_key: Some(agent_key.clone()),
                    };
                    let hint = prompt_hint(&prompt, "");
                    ui.prompt = Some(PromptState { prompt, hint });
                    return Ok(());
                }
            }
            // Any other key: keep showing the picker.
            ui.prompt = Some(pstate);
        }
        Prompt::NewTabName { .. } | Prompt::RenameTab { .. } => {
            // Capture which kind of text prompt this is, plus the chosen agent,
            // without holding a borrow of `pstate.prompt` across the mutation.
            let is_new = matches!(pstate.prompt, Prompt::NewTabName { .. });
            let new_agent_key = match &pstate.prompt {
                Prompt::NewTabName { agent_key, .. } => agent_key.clone(),
                _ => None,
            };
            let buffer = match &mut pstate.prompt {
                Prompt::NewTabName { buffer, .. } | Prompt::RenameTab { buffer } => buffer,
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
                    if is_new {
                        // Async new-tab flow: reserve a placeholder tab now
                        // (cheap, validation-first), then queue the slow worktree
                        // creation for a background worker so the UI never blocks
                        // on `git worktree add` (SPECS §16/§17).
                        ui.prompt = None;
                        match state.begin_new_agent_tab(&name, new_agent_key.as_deref(), services) {
                            Ok(job) => {
                                let branch = job.branch.clone();
                                ui.pending_jobs.push(job);
                                ui.message(format!("Creating worktree for {branch}…"));
                            }
                            Err(e) => ui.message(format!("Error: {e}")),
                        }
                    } else {
                        let result =
                            state.dispatch(Command::RenameAgentTab { new_name: name }, services);
                        finish_prompt(result, ui);
                    }
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
        Prompt::AbandonConfirm => match key.code {
            KeyCode::Char('y') => {
                let result = state.dispatch(Command::AbandonWorktree { confirm: true }, services);
                finish_prompt(result, ui);
            }
            KeyCode::Char('n') => ui.clear(),
            _ => ui.prompt = Some(pstate),
        },
        Prompt::MergeConfirm { .. } => match key.code {
            KeyCode::Char('y') => {
                let result = state.dispatch(Command::FinishLocalMerge { confirm: true }, services);
                finish_prompt(result, ui);
            }
            KeyCode::Char('n') => ui.clear(),
            _ => ui.prompt = Some(pstate),
        },
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
        Effect::Quit => ui.should_quit = true,
        Effect::Message(m) => ui.message(m),
        Effect::Warning(m) => ui.message(format!("WARNING: {m}")),
        Effect::Refused(m) => ui.message(format!("Refused: {m}")),
        Effect::PrUrl(url) => ui.message(format!("PR: {url}")),
        Effect::AttachedExisting { branch } => {
            ui.message(format!("Attached to existing branch {branch}"))
        }
        Effect::PushWarning(_) => start_prompt(ui, Prompt::PushConfirm),
        Effect::AbandonWarning => start_prompt(ui, Prompt::AbandonConfirm),
        Effect::MergeConfirm {
            agent_branch,
            base_branch,
            primary_running,
        } => start_prompt(
            ui,
            Prompt::MergeConfirm {
                agent_branch,
                base_branch,
                primary_running,
            },
        ),
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
            start_new_tab_flow(state, ui);
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

/// Drain output from every terminal of every tab, feeding each terminal's VT
/// parser (so it can be rendered) and feeding each primary's output to status
/// classification (SPECS §24).
fn drain_pty_output(state: &mut AppState, now_ms: u64) {
    // Collect (tab id, primary bytes) first so we can call `ingest_output`
    // (which borrows `state` mutably) without overlapping the session borrow.
    let mut ingest: Vec<(TabId, Vec<u8>)> = Vec::new();

    for tab in state.tabs.iter_mut() {
        let id = TabId(tab.meta.id.clone());

        // Primary: drain → VT parser + status classification.
        if let Some(primary) = tab.session.primary_mut() {
            if let Ok(bytes) = primary.session_mut().try_read_output() {
                if !bytes.is_empty() {
                    primary.process_output(&bytes);
                    ingest.push((id.clone(), bytes));
                }
            }
        }

        // Child terminals: drain → VT parser (so they don't stall and so their
        // screen renders when selected).
        for c in 0..tab.session.child_count() {
            if let Some(child) = tab.session.child_mut(c) {
                if let Ok(bytes) = child.session_mut().try_read_output() {
                    if !bytes.is_empty() {
                        child.process_output(&bytes);
                    }
                }
            }
        }
    }

    for (id, bytes) in ingest {
        state.ingest_output(&id, &bytes, now_ms);
    }
}

/// Write key bytes to the active terminal's PTY (Terminal-mode passthrough).
fn write_active_pty(state: &mut AppState, bytes: &[u8]) {
    let Some(tab) = state.selected_mut() else {
        return;
    };
    if let Some(term) = tab.session.active_mut() {
        // Typing/sending input snaps the view back to the live bottom and drops
        // any selection, matching standard terminal behaviour when scrolled into
        // local scrollback.
        term.clear_selection();
        term.scroll_to_bottom();
        let _ = term.session_mut().write_input(bytes);
    }
}

/// Paste from the system clipboard into the active terminal (Ctrl-V).
///
/// When the clipboard holds an image, it is written to a temp file and the
/// file path is sent to the agent — matching how a terminal inserts a path when
/// you drag an image in, which agents like Claude Code recognise and attach. A
/// trailing space is appended so the user can keep typing. With no image on the
/// clipboard, a literal Ctrl-V (0x16) is forwarded, preserving prior behaviour.
fn paste_into_active_pty(state: &mut AppState) {
    match crate::tui::clipboard::save_clipboard_image() {
        Some(path) => {
            let raw = path.to_string_lossy();
            // Quote the path if it could be word-split by the agent's input.
            let mut text = if raw.contains(char::is_whitespace) {
                format!("'{}'", raw.replace('\'', "'\\''"))
            } else {
                raw.into_owned()
            };
            text.push(' ');
            write_active_pty(state, text.as_bytes());
        }
        None => write_active_pty(state, &[0x16]),
    }
}

/// Resize every live PTY session and its VT parser to the new viewport size
/// (SPECS §23 resize).
fn resize_sessions(state: &mut AppState, size: PtySize) {
    for tab in state.tabs.iter_mut() {
        if let Some(primary) = tab.session.primary_mut() {
            let _ = primary.resize(size);
        }
        for c in 0..tab.session.child_count() {
            if let Some(child) = tab.session.child_mut(c) {
                let _ = child.resize(size);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Teardown
// ---------------------------------------------------------------------------

/// Persist state on quit, swallowing (but reporting) any error so teardown can
/// proceed (SPECS §9).
fn persist_quietly(state: &AppState, services: &Services) -> Result<()> {
    let project_state = state.to_project_state(services.clock.now_millis());
    save_state(services.fs, &state.state_path, &project_state)
}

/// Force-terminate every session in every tab so no orphaned child processes
/// remain after FlightDeck exits (SPECS §25).
fn terminate_all_sessions(state: &mut AppState) {
    for tab in state.tabs.iter_mut() {
        let _ = tab.session.terminate_all();
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

    // §20: SGR mouse reports use 1-based viewport coordinates and a trailing 'M'.
    #[test]
    fn encodes_sgr_wheel_report() {
        // Wheel-up at viewport cell (0,0) → column/row 1.
        assert_eq!(
            encode_mouse_report(vt100::MouseProtocolEncoding::Sgr, MOUSE_WHEEL_UP, 0, 0),
            b"\x1b[<64;1;1M".to_vec()
        );
        // Wheel-down at cell (4,2) → column 5, row 3.
        assert_eq!(
            encode_mouse_report(vt100::MouseProtocolEncoding::Sgr, MOUSE_WHEEL_DOWN, 4, 2),
            b"\x1b[<65;5;3M".to_vec()
        );
    }

    // §20: the default (X10) encoding offsets each field by 32.
    #[test]
    fn encodes_default_wheel_report() {
        assert_eq!(
            encode_mouse_report(vt100::MouseProtocolEncoding::Default, MOUSE_WHEEL_UP, 0, 0),
            vec![0x1b, b'[', b'M', 32 + 64, 32 + 1, 32 + 1]
        );
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
            agent_key: None,
        };
        let hint = prompt_hint(&p, "fix bug");
        assert!(hint.contains("fix bug"));
        assert!(hint.to_lowercase().contains("name"));
    }

    #[test]
    fn select_agent_prompt_hint_lists_numbered_agents() {
        let p = Prompt::SelectAgent {
            agents: vec![
                ("claude".to_string(), "Claude Code".to_string()),
                ("opencode".to_string(), "OpenCode".to_string()),
            ],
        };
        let hint = prompt_hint(&p, "");
        assert!(hint.contains("[1] Claude Code"), "got: {hint}");
        assert!(hint.contains("[2] OpenCode"), "got: {hint}");
        assert!(hint.to_lowercase().contains("pick agent"), "got: {hint}");
    }

    #[test]
    fn new_tab_flow_picks_agent_then_advances_to_named_prompt() {
        use crate::app::state::AppState;
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        // Config with two agents so the picker is shown.
        let mut config = Config {
            ui: UiConfig {
                default_agent: "opencode".to_string(),
                agent_tab_position: "left".to_string(),
            },
            ..Config::default()
        };
        config.agents.insert(
            "opencode".to_string(),
            AgentDef {
                display_name: "OpenCode".to_string(),
                command: "opencode".to_string(),
                ..AgentDef::default()
            },
        );
        config.agents.insert(
            "claude".to_string(),
            AgentDef {
                display_name: "Claude Code".to_string(),
                command: "claude".to_string(),
                ..AgentDef::default()
            },
        );

        let mut state = AppState::new(config, default_state("main"), "/repo", "/repo/state.json");
        let mut ui = Ui::default();

        // Starting the flow shows the agent picker (more than one agent).
        start_new_tab_flow(&state, &mut ui);
        let agents = match &ui.prompt.as_ref().expect("prompt active").prompt {
            Prompt::SelectAgent { agents } => agents.clone(),
            _ => panic!("expected SelectAgent prompt"),
        };
        // BTreeMap key order: "claude" before "opencode".
        assert_eq!(agents[0].0, "claude");
        assert_eq!(agents[1].0, "opencode");

        // Services are required by the signature but unused by the picker branch.
        let git = FakeGit::new();
        let fs = FakeFs::new();
        let pty = FakePty::new();
        let clock = FakeClock::default();
        let services = Services {
            git: &git,
            fs: &fs,
            pty: &pty,
            clock: &clock,
        };

        // Pressing '1' picks Claude Code and advances to the name prompt.
        handle_prompt_key(
            KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE),
            &mut state,
            &services,
            &mut ui,
        )
        .unwrap();
        match &ui.prompt.as_ref().expect("name prompt active").prompt {
            Prompt::NewTabName { agent_key, .. } => {
                assert_eq!(agent_key.as_deref(), Some("claude"));
            }
            _ => panic!("expected NewTabName prompt carrying the chosen agent"),
        }
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
    fn effect_abandon_warning_opens_abandon_prompt() {
        let mut ui = Ui::default();
        apply_effect_no_state(Effect::AbandonWarning, &mut ui);
        assert!(ui.prompt.is_some());
        assert!(matches!(
            ui.prompt.as_ref().unwrap().prompt,
            Prompt::AbandonConfirm
        ));
    }

    #[test]
    fn effect_merge_confirm_opens_merge_prompt() {
        let mut ui = Ui::default();
        apply_effect_no_state(
            Effect::MergeConfirm {
                agent_branch: "flightdeck/feat".to_string(),
                base_branch: "main".to_string(),
                primary_running: true,
            },
            &mut ui,
        );
        let pstate = ui.prompt.as_ref().expect("merge prompt set");
        assert!(matches!(pstate.prompt, Prompt::MergeConfirm { .. }));
        // The hint names both branches and warns about stopping the agent.
        assert!(pstate.hint.contains("flightdeck/feat"));
        assert!(pstate.hint.contains("main"));
        assert!(pstate.hint.contains("stops the running agent"));
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
    fn effect_quit_sets_should_quit() {
        // Regression: dispatching Quit (e.g. the palette "Quit" action) must
        // actually request exit, not be a silent no-op.
        let mut ui = Ui::default();
        assert!(!ui.should_quit);
        apply_effect_no_state(Effect::Quit, &mut ui);
        assert!(ui.should_quit);
    }

    #[test]
    fn viewport_size_is_smaller_than_full_terminal() {
        // The agent PTY must wrap at the viewport width (full minus sidebar),
        // not the whole screen width.
        let full = PtySize {
            rows: 40,
            cols: 120,
        };
        let vp = viewport_pty_size(full);
        assert!(vp.cols < full.cols, "viewport narrower than full screen");
        assert!(vp.rows < full.rows, "viewport shorter than full screen");
        assert!(vp.cols >= 1 && vp.rows >= 1);
    }

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
