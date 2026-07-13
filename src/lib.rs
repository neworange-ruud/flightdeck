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
pub mod runtime;
pub mod terminal;
pub mod tui;
#[cfg(all(feature = "self-update", not(windows)))]
pub mod update;

// No-op stand-in when the real self-updater is not built: either the
// `self-update` feature is off (a pure-Rust build with no C toolchain), or the
// target is Windows (where the updater deps are gated out in Cargo.toml so the
// released windows-msvc binary stays pure-Rust). Keeps
// `update::run`/`update::start_check` callable so the subcommand dispatch and the
// update-notice channel plumbing below need no `cfg` of their own; `update`
// becomes a no-op and `start_check` never sends.
#[cfg(not(all(feature = "self-update", not(windows))))]
pub mod update {
    use crate::contracts::error::Result;
    use std::sync::mpsc::Sender;

    pub fn run() -> Result<()> {
        println!(
            "FlightDeck: this build was compiled without self-update support \
             (`flightdeck update` is a no-op here)."
        );
        Ok(())
    }

    pub fn start_check(_enabled: bool, _now_unix: u64, _tx: Sender<String>) -> Option<String> {
        None
    }
}

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::app::commands::{CloseAction, Command, Effect, PushConfirm, Selector};
use crate::app::modes::InputMode;
use crate::app::state::{materialize_worktree, AppState, Services, TabPhase, WorktreeJob};
use crate::config::init::{ensure_global_config, initialize};
use crate::config::load::{global_config_path, load_config, load_layered_config};
use crate::config::schema::default_config;
use crate::contracts::error::{FlightDeckError, Result};
use crate::contracts::real::{RealClock, RealFs};
use crate::contracts::{
    Clock, Config, ContainerRuntime, FileSystem, GitExecutor, ManualStatus, Notifier, PtyBackend,
    PtySize,
};
use crate::fs::ignore::ensure_flightdeck_gitignore;
use crate::fs::paths::to_absolute;
use crate::git::repo::{detect_base_branch, GitCli};
use crate::git::status::{collect_status, WorktreeStatus};
use crate::notify::SystemNotifier;
use crate::persistence::project_state::{default_state, load_state, save_state};
use crate::persistence::recovery::recover;
use crate::persistence::workspace::{
    load_workspace, save_workspace, workspace_state_path, WorkspaceState, WORKSPACE_VERSION,
};
use crate::terminal::pty::PortablePtyBackend;
use crate::tui::config_manager::ConfigManager;
use crate::tui::input::{map_key, KeyAction};
use crate::tui::palette::{CommandPalette, PaletteAction};
use crate::tui::render::{
    child_tab_label, dialog_hit, draw, draw_project_tab_bar, hit_test, project_tab_hit_test,
    ChildTarget, Dialog, DialogAccel, DialogButton, DialogHit, DialogListItem, GitStatusCache,
    HitTarget, ProjectHit, ProjectTabInfo, UiOverlay,
};

use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags, MouseButton,
    MouseEvent, MouseEventKind, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
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

    // Subcommand dispatch. These configure status/notification
    // features and exit without launching the TUI (SPECS §24).
    match std::env::args().nth(1).as_deref() {
        // Generate reusable standalone status hooks/plugins.
        Some("setup-status") => return run_setup_status(),
        // Ensure OS notifications are enabled in config (on by default).
        Some("setup-notifications") => return run_setup_notifications(),
        // Ensure the once-a-day update notice is enabled in config (SPECS §30).
        Some("setup-update") => return run_setup_update(),
        // Self-update installer-based installs from GitHub Releases (SPECS §29).
        Some("update") => return update::run(),
        // Build/inspect agent container images (SPECS §31).
        Some("image") => return run_image(),
        // Verify the container runtime + images are ready (SPECS §31).
        Some("doctor") => return run_doctor(),
        _ => {}
    }

    // 1–4. Construct the shared services + build the workspace of open projects.
    let cwd = std::env::current_dir()
        .map_err(|e| FlightDeckError::Io(format!("could not determine current directory: {e}")))?;

    let fs = RealFs;
    let pty = PortablePtyBackend;
    let clock = RealClock;
    let container = crate::runtime::PodmanCli;
    let env = Env {
        fs: &fs,
        pty: &pty,
        clock: &clock,
        container: &container,
    };

    // The launch project (the cwd's repository) must be a git repo — fail fast
    // with the friendly message if not. It is always opened and made active.
    let launch = open_project(&env, &cwd).map_err(|e| {
        FlightDeckError::Git(format!(
            "not inside a Git repository (run FlightDeck from a git project): {e}"
        ))
    })?;
    let repo_root = launch.git.root().to_path_buf();

    let mut workspace = Workspace {
        projects: vec![launch],
        active: 0,
    };

    // Reopen any other projects remembered from the previous session (best
    // effort): skip the launch project, folders that no longer exist, and any
    // that are no longer git repositories. Each project's own tabs are still
    // recovered from its `state.json` (agents are never auto-relaunched).
    let ws_path = workspace_state_path();
    if let Some(ref wp) = ws_path {
        if let Ok(saved) = load_workspace(&fs, wp) {
            for p in &saved.projects {
                let pr = Path::new(p);
                if !fs.is_dir(pr) {
                    continue;
                }
                match open_project(&env, pr) {
                    Ok(proj) if !workspace.contains_root(proj.git.root()) => {
                        workspace.projects.push(proj)
                    }
                    _ => {}
                }
            }
        }
    }

    // 5–8. Initialise the terminal (raw mode + alt screen + panic-restore hook)
    // and run the loop, ensuring teardown happens no matter how we exit.
    let mut terminal = ratatui::try_init()
        .map_err(|e| FlightDeckError::Io(format!("failed to initialise terminal: {e}")))?;

    // Enable mouse capture so tabs are clickable (best effort).
    let _ = crossterm::execute!(std::io::stdout(), EnableMouseCapture);

    // Enable bracketed paste so the host terminal delivers a multi-line paste as
    // a single `Event::Paste` instead of a stream of key events. Without it, a
    // paste arrives as line₁ + Enter + line₂ + Enter + …, and the hosted agent
    // executes the first line and queues the rest as separate prompts. Best
    // effort; disabled again on teardown.
    let _ = crossterm::execute!(std::io::stdout(), EnableBracketedPaste);

    // Enable the kitty keyboard protocol's "disambiguate escape codes" mode when
    // the terminal supports it. Without it, terminals report Alt/Option+Esc as a
    // bare Esc — indistinguishable from the agent's own Esc — so Alt+Esc can't be
    // used to leave terminal focus, and Alt-navigation shortcuts are unreliable.
    // With it, modified keys carry their real modifiers. Best effort; popped on
    // teardown only if we pushed it.
    let keyboard_enhanced = matches!(
        crossterm::terminal::supports_keyboard_enhancement(),
        Ok(true)
    );
    if keyboard_enhanced {
        let _ = crossterm::execute!(
            std::io::stdout(),
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        );
    }

    // Take ownership of the terminal title so it stays a stable
    // "flightdeck — <project>" while we run, instead of inheriting (and
    // flickering with) whatever title the parent tooling keeps rewriting. The
    // previous title is pushed onto the terminal's title stack so it can be
    // restored on exit. Best effort — terminals without XTWINOPS just ignore it.
    let _ =
        save_and_set_terminal_title(&format!("flightdeck — {}", derive_project_name(&repo_root)));

    // Seed the PTY size from the terminal viewport (not the whole screen) so
    // agents wrap at the right width — for every open project.
    if let Ok(size) = terminal.size() {
        let vp = viewport_pty_size(PtySize {
            rows: size.height,
            cols: size.width,
        });
        for p in workspace.projects.iter_mut() {
            p.state.set_pty_size(vp);
        }
    }

    // Resume: start the primary agent for every recovered/loaded tab whose
    // worktree still exists (best effort), across every open project. Done here,
    // after the viewport size is known, rather than in `recover`/`AppState::new`
    // which never spawn.
    for p in workspace.projects.iter_mut() {
        let services = env.services(&p.git);
        let _ = p.state.resume_agents(&services);
    }

    let notifier = SystemNotifier;
    let loop_result = event_loop(&mut terminal, &mut workspace, &env, &notifier);

    // CLEAN TEARDOWN (SPECS §25): always restore the terminal, then terminate
    // every session so no orphaned child processes remain. Persist on the way
    // out (best effort) regardless of how the loop ended.
    if keyboard_enhanced {
        let _ = crossterm::execute!(std::io::stdout(), PopKeyboardEnhancementFlags);
    }
    let _ = crossterm::execute!(std::io::stdout(), DisableBracketedPaste);
    let _ = crossterm::execute!(std::io::stdout(), DisableMouseCapture);
    let _ = restore_terminal_title();
    ratatui::restore();

    // Persist every project's own state.json (SPECS §9), then the workspace file
    // recording which projects were open + the active one (best effort — a save
    // failure never overrides the loop result).
    let mut persist_result = Ok(());
    for p in workspace.projects.iter() {
        let services = env.services(&p.git);
        if let Err(e) = persist_quietly(&p.state, &services) {
            persist_result = Err(e);
        }
    }
    if let Some(wp) = &ws_path {
        let ws_state = WorkspaceState {
            version: WORKSPACE_VERSION,
            projects: workspace
                .projects
                .iter()
                .map(|p| p.git.root().to_string_lossy().to_string())
                .collect(),
            active: workspace.active,
        };
        let _ = save_workspace(&fs, wp, &ws_state);
    }

    for p in workspace.projects.iter_mut() {
        terminate_all_sessions(&mut p.state);
    }

    loop_result.and(persist_result)
}

/// `flightdeck setup-status`: generate reusable global lifecycle integrations
/// for sessions launched outside FlightDeck. Normal FlightDeck sessions inject
/// equivalent launch-scoped hooks/plugins automatically.
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
    println!("FlightDeck sessions already use explicit lifecycle status automatically.");
    println!("To reuse the integration outside FlightDeck, see:");
    println!("  {}/README.md", dir.display());
    println!();
    println!("  Claude Code → merge claude-code.settings.json into ~/.claude/settings.json");
    println!("  Codex CLI   → append codex-config.toml to ~/.codex/config.toml");
    println!("  OpenCode    → copy opencode-flightdeck.js to ~/.config/opencode/plugin/");
    Ok(())
}

/// `flightdeck setup-notifications`: ensure OS notifications are on for this
/// project by writing `notifications.enabled = true` as an override in
/// `<repo>/.flightdeck/config.toml` (creating the config on first run), then
/// print how to tune or disable them. Does not launch the TUI (SPECS §24).
/// Notifications are on by default (via the global config); this command is the
/// quick way to re-enable them for a project that turned them off, without
/// hand-editing the config.
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

    // Ensure a project config exists (first run writes the minimal default),
    // then load the effective (global + project) config to check the state.
    if !fs.exists(&config_path) {
        let project_name = derive_project_name(&repo_root);
        let base_branch = detect_base_branch(&git, &cwd, None)?;
        initialize(&fs, &repo_root, &project_name, &base_branch)?;
    }
    let config = load_effective_for_repo(&fs, &repo_root)?;

    if config.notifications.enabled {
        println!("FlightDeck: OS notifications are already enabled (they are on by default).");
    } else {
        // They were turned off (globally or for this project). Re-enable them
        // as an explicit project override, leaving other settings inherited.
        set_project_bool_override(&fs, &config_path, "notifications", "enabled", true)?;
        println!(
            "FlightDeck: enabled OS notifications for this project in {}.",
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
    println!("  sound      = true   # play distinct sounds for completion and input");
    println!();
    println!("macOS delivery: `brew install terminal-notifier` for best reliability,");
    println!("or allow Script Editor under System Settings → Notifications.");
    Ok(())
}

/// `flightdeck setup-update`: turn on the update notice by setting
/// `update.check = true` in `<repo>/.flightdeck/config.toml` (creating the
/// config on first run), then explain the behavior. Does not launch the TUI
/// (SPECS §24, §30). The check is on by default; this keeps the command useful
/// for configs that previously disabled it.
fn run_setup_update() -> Result<()> {
    let cwd = std::env::current_dir()
        .map_err(|e| FlightDeckError::Io(format!("could not determine current directory: {e}")))?;
    let git = GitCli::discover(&cwd).map_err(|_| {
        FlightDeckError::Git(
            "not inside a Git repository (run `flightdeck setup-update` from a git project)"
                .to_string(),
        )
    })?;
    let repo_root = git.root().to_path_buf();
    let fs = RealFs;
    let config_path = repo_root.join(".flightdeck").join("config.toml");

    // Ensure a project config exists (first run writes the minimal default),
    // then load the effective (global + project) config to check the state.
    if !fs.exists(&config_path) {
        let project_name = derive_project_name(&repo_root);
        let base_branch = detect_base_branch(&git, &cwd, None)?;
        initialize(&fs, &repo_root, &project_name, &base_branch)?;
    }
    let config = load_effective_for_repo(&fs, &repo_root)?;

    if config.update.check {
        println!("FlightDeck: the update notice is already enabled (it is on by default).");
    } else {
        set_project_bool_override(&fs, &config_path, "update", "check", true)?;
        println!(
            "FlightDeck: enabled the update notice for this project in {}.",
            config_path.display()
        );
    }
    println!();
    println!("On startup FlightDeck will check GitHub Releases at most once a day (in the");
    println!("background) and show a status-bar hint when a newer version is available.");
    println!(
        "It never auto-updates — run `flightdeck update` (or `brew update && brew upgrade flightdeck`)."
    );
    println!("Disable any time by setting `check = false` under [update].");
    #[cfg(not(all(feature = "self-update", not(windows))))]
    println!(
        "Note: this build was compiled without self-update support, so the check above will never run."
    );
    Ok(())
}

/// `flightdeck image build [agent]` — build (or rebuild) an agent's container
/// image from the FlightDeck base + project customization (SPECS §31).
fn run_image() -> Result<()> {
    use crate::contracts::ContainerRuntime;

    let action = std::env::args().nth(2);
    if action.as_deref() != Some("build") {
        println!("usage: flightdeck image build [agent]");
        println!();
        println!("Builds the container image for an agent (default: the configured");
        println!("default agent) from its FlightDeck base image plus any [containers]");
        println!("customization (packages / setup_script / containerfile).");
        return Ok(());
    }

    let cwd = std::env::current_dir()
        .map_err(|e| FlightDeckError::Io(format!("could not determine current directory: {e}")))?;
    let git = GitCli::discover(&cwd).map_err(|_| {
        FlightDeckError::Git(
            "not inside a Git repository (run `flightdeck image build` from a git project)"
                .to_string(),
        )
    })?;
    let repo_root = git.root().to_path_buf();
    let fs = RealFs;
    let config_path = repo_root.join(".flightdeck").join("config.toml");
    if !fs.exists(&config_path) {
        let project_name = derive_project_name(&repo_root);
        let base_branch = detect_base_branch(&git, &cwd, None)?;
        initialize(&fs, &repo_root, &project_name, &base_branch)?;
    }
    let config = load_effective_for_repo(&fs, &repo_root)?;

    // `validate_containers` skips its checks whenever `enabled` is false (so a
    // disabled-but-malformed table never blocks an ordinary launch), but
    // `image build` is an explicit, container-specific action that customizes
    // the image regardless of `enabled` — validate as if enabled so a bad
    // combination (e.g. `containerfile` + `packages`) is rejected here instead
    // of silently dropping the customization in `ensure_image`.
    let mut containers_for_validation = config.containers.clone();
    containers_for_validation.enabled = true;
    crate::config::schema::validate_containers(&containers_for_validation)?;

    let agent = std::env::args()
        .nth(3)
        .unwrap_or_else(|| config.ui.default_agent.clone());
    if !config.agents.contains_key(&agent) {
        return Err(FlightDeckError::Config(format!(
            "unknown agent '{agent}' (not in config.toml)"
        )));
    }

    let podman = crate::runtime::PodmanCli;
    podman.available()?;

    let rhash = crate::runtime::name::repo_hash(&repo_root);
    let tag = crate::runtime::image::resolve_image_tag(&rhash, &agent, &config.containers);
    println!(
        "FlightDeck: building image '{tag}' for agent '{agent}' (this may take a few minutes)…"
    );
    let built = crate::runtime::image::ensure_image(
        &podman,
        &fs,
        &repo_root,
        &rhash,
        &agent,
        &config.containers,
    )?;
    println!("FlightDeck: image ready → {built}");
    Ok(())
}

/// `flightdeck doctor` — verify the container runtime + images are ready
/// (SPECS §31). Reports rather than mutating anything.
fn run_doctor() -> Result<()> {
    use crate::contracts::ContainerRuntime;

    let cwd = std::env::current_dir()
        .map_err(|e| FlightDeckError::Io(format!("could not determine current directory: {e}")))?;
    let git = GitCli::discover(&cwd).map_err(|_| {
        FlightDeckError::Git(
            "not inside a Git repository (run `flightdeck doctor` from a git project)".to_string(),
        )
    })?;
    let repo_root = git.root().to_path_buf();
    let fs = RealFs;
    let config = load_effective_for_repo(&fs, &repo_root)?;

    println!("FlightDeck doctor");
    if !config.containers.enabled {
        println!("  • container execution: disabled ([containers] enabled = false)");
        println!("    Agents run locally; nothing else to check.");
        return Ok(());
    }
    println!(
        "  • container execution: enabled (runtime = {})",
        config.containers.runtime
    );

    let podman = crate::runtime::PodmanCli;
    match podman.available() {
        Ok(()) => println!("  • podman: ready"),
        Err(e) => {
            // `available()` already returns actionable, platform-specific
            // install/start guidance (indented here under the bullet). Drop the
            // generic "operation refused: " error prefix for a clean read.
            println!("  • podman: NOT ready");
            let msg = e.to_string();
            let msg = msg.strip_prefix("operation refused: ").unwrap_or(&msg);
            for line in msg.lines() {
                println!("    {line}");
            }
            return Ok(());
        }
    }

    let rhash = crate::runtime::name::repo_hash(&repo_root);
    for agent in config.agents.keys() {
        let tag = crate::runtime::image::resolve_image_tag(&rhash, agent, &config.containers);
        let present = podman.image_exists(&tag).unwrap_or(false);
        let mark = if present { "present" } else { "MISSING" };
        println!("  • image for '{agent}': {tag} — {mark}");
        if !present {
            println!("    Build it with `flightdeck image build {agent}`.");
        }
    }
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
    // Capture dirtiness before any of FlightDeck's own first-run writes below
    // (config.toml, .gitignore) touch the working tree — otherwise those
    // bootstrap writes would themselves make an actually-clean repo look dirty
    // (SPECS §13).
    let dirty = services.git.is_dirty(repo_root).unwrap_or(false);

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

    // Ensure the per-user global base exists (documents every overridable
    // setting), then load the effective config by layering it under this
    // project's overrides (SPECS §8). Fall back to a freshly-built default if
    // loading fails, or when there is no home dir to host a global config.
    let global_path = global_config_path();
    if let Some(gp) = &global_path {
        let _ = ensure_global_config(services.fs, gp);
    }
    let config = match &global_path {
        Some(gp) => load_layered_config(services.fs, gp, &config_path),
        None => load_config(services.fs, &config_path),
    }
    .unwrap_or_else(|_| default_config(&project_name, &base_branch));

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
    let report = recover(
        services.fs,
        services.git,
        repo_root,
        &worktrees_root,
        &mut project_state,
    )?;

    let mut state = AppState::new(config, project_state, repo_root, &state_path);

    // Surface stale entries (worktree missing on disk / unregistered in git)
    // so the user knows to remove them, instead of silently discarding them.
    for id in &report.stale_entries {
        state.warnings.push(format!(
            "Stale tab entry: {id} (worktree missing) — remove it from the tab actions menu"
        ));
    }

    // SPECS §13: dirty base at startup → persistent warning (merge disabled).
    if dirty {
        let warning = "Base repo dirty: local merge disabled".to_string();
        if !state.warnings.contains(&warning) {
            state.warnings.push(warning);
        }
    }

    Ok(state)
}

/// Load the effective config for the repo at `repo_root`: the per-user global
/// base layered under this project's overrides (SPECS §8), ensuring the global
/// base exists first. Falls back to the single project file when there is no
/// home dir to host a global config. Used by the non-TUI subcommands.
fn load_effective_for_repo(fs: &dyn FileSystem, repo_root: &Path) -> Result<Config> {
    let config_path = repo_root.join(".flightdeck").join("config.toml");
    match global_config_path() {
        Some(gp) => {
            let _ = ensure_global_config(fs, &gp);
            load_layered_config(fs, &gp, &config_path)
        }
        None => load_config(fs, &config_path),
    }
}

/// Set a boolean `section.key = value` override in the project config at
/// `config_path`, preserving any other overrides already present and leaving
/// everything else inherited from the global base (SPECS §8). Creates the file
/// and/or section if missing.
fn set_project_bool_override(
    fs: &dyn FileSystem,
    config_path: &Path,
    section: &str,
    key: &str,
    value: bool,
) -> Result<()> {
    let mut table = if fs.exists(config_path) {
        crate::config::load::parse_table(&fs.read_to_string(config_path)?)?
    } else {
        toml::Table::new()
    };
    let entry = table
        .entry(section.to_string())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    if let toml::Value::Table(t) = entry {
        t.insert(key.to_string(), toml::Value::Boolean(value));
    }
    let body = toml::to_string_pretty(&table)
        .map_err(|e| FlightDeckError::Config(format!("failed to serialize config: {e}")))?;
    fs.write(config_path, &body)?;
    Ok(())
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
    println!("SETUP:");
    println!("    setup-status           Generate reusable agent-status integrations");
    println!("    setup-notifications    Enable OS notifications when agents finish");
    println!("    setup-update           Enable the once-a-day update notice");
    println!();
    println!("CONTAINERS (optional — run agents in isolated Podman containers):");
    println!("    doctor                 Check the container runtime and images are ready");
    println!("    image build [agent]    Build an agent's container image (default agent");
    println!("                           if none given)");
    println!();
    println!("    Enable with `enabled = true` under [containers] in");
    println!("    .flightdeck/config.toml, then run `flightdeck doctor`.");
    println!();
    println!("MAINTENANCE:");
    println!("    update                 Update FlightDeck to the latest release");
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
    /// Pick which agent backend to spawn as an additional agent in the current
    /// session's worktree (the "+ agent" flow). A number key selects one and
    /// dispatches `NewAgentTerminal`. Holds each agent's `(key, display_name)`.
    SelectChildAgent { agents: Vec<(String, String)> },
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
    /// Confirm closing a child shell terminal (from the tab's `✕` or Ctrl-w).
    /// `label` is the shell's display name, e.g. "shell 2".
    CloseChildConfirm { label: String },
    /// Sidebar `✕`: abandon the worktree, just close the agent, or cancel.
    /// `index` is the Agent Tab the action targets.
    CloseAgentChoice { index: usize },
    /// Confirm a push despite uncommitted changes (SPECS §14).
    PushConfirm,
    /// Confirm abandoning a worktree (SPECS §5/§15). `dirty` is true when it has
    /// uncommitted changes that would be discarded, so the prompt can warn.
    AbandonConfirm { dirty: bool },
    /// Confirm a local merge-back; on success the worktree is removed and the
    /// tab closed, stopping the agent if it is still running (SPECS §15).
    MergeConfirm {
        agent_branch: String,
        base_branch: String,
        primary_running: bool,
    },
    /// Confirm rebasing the worktree onto its base branch; rewrites the branch's
    /// history and aborts on conflict (SPECS §5 carve-out).
    RebaseConfirm {
        agent_branch: String,
        base_branch: String,
        drift: u32,
        primary_running: bool,
    },
    /// Open another project (multi-project): a folder browser that also lets the
    /// user type a path. Confirming opens the folder as a new project tab.
    OpenProject { browse: BrowseState },
    /// Confirm closing an open project tab (`index`). Closing stops that
    /// project's agents and removes it from the workspace.
    CloseProjectConfirm { index: usize },
}

/// State for the project-folder browser prompt ([`Prompt::OpenProject`]): the
/// directory currently being browsed, its immediate subdirectories (navigable),
/// the highlighted entry, and any path the user has typed directly.
struct BrowseState {
    /// The directory currently shown.
    dir: PathBuf,
    /// Immediate subdirectories of `dir`, sorted (for arrow-key selection).
    entries: Vec<PathBuf>,
    /// Index of the highlighted entry within `entries`.
    selected: usize,
    /// A path typed directly by the user (takes precedence on confirm).
    typed: String,
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
    /// Which terminal the selection is being made in. Fixed for the whole drag
    /// so it keeps extending the same terminal even if the pointer leaves its
    /// column (split view) or the active terminal changes.
    target: ChildTarget,
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
    /// the slow `git worktree add` off the UI thread. Each is tagged with the
    /// index of the project it belongs to, so it is handed to the right
    /// project's worker even if the active project changes before hand-off.
    pending_jobs: Vec<PendingJob>,
    /// The configuration manager overlay, if open (SPECS §8). Held separately
    /// from `overlay` (like `palette`) because it carries its own mutable state.
    config: Option<ConfigManager>,
    /// A config file the user asked to edit in `$EDITOR` (SPECS §8). Deferred to
    /// the event loop, which owns the terminal it must suspend/restore. Tagged
    /// with the project index whose config was opened.
    pending_editor: Option<(usize, PathBuf)>,
}

/// A queued worktree-creation job plus the index of the project that owns it.
struct PendingJob {
    project: usize,
    job: WorktreeJob,
}

/// A prompt plus the modal dialog rendered for it (title + buttons).
struct PromptState {
    prompt: Prompt,
    dialog: Dialog,
}

impl Ui {
    /// Whether any modal/prompt currently captures input. Used to decide whether
    /// the normal mode-aware key map should run.
    fn modal_active(&self) -> bool {
        self.palette.is_some()
            || self.prompt.is_some()
            || self.config.is_some()
            || !matches!(self.overlay, UiOverlay::None)
    }

    /// Show a notification message as a centered modal dialog (SPECS §22).
    fn message(&mut self, msg: impl Into<String>) {
        self.overlay = UiOverlay::Dialog(Dialog::notification(msg));
    }

    /// Clear every overlay/prompt back to the normal main view.
    fn clear(&mut self) {
        self.overlay = UiOverlay::None;
        self.palette = None;
        self.prompt = None;
        self.config = None;
    }

    /// The overlay to render this frame: a live prompt dialog takes precedence
    /// over a plain notification, the palette over both, and the configuration
    /// manager over everything (it is only ever open on its own).
    fn render_overlay(&self) -> UiOverlay {
        if let Some(config) = &self.config {
            return UiOverlay::Config(config.clone());
        }
        if let Some(palette) = &self.palette {
            return UiOverlay::Palette(palette.clone());
        }
        if let Some(p) = &self.prompt {
            return UiOverlay::Dialog(p.dialog.clone());
        }
        self.overlay.clone()
    }

    /// The dialog currently accepting clicks, if any: a live prompt's dialog, or
    /// a notification dialog set as the overlay.
    fn active_dialog(&self) -> Option<Dialog> {
        if self.palette.is_some() || self.config.is_some() {
            return None;
        }
        if let Some(p) = &self.prompt {
            return Some(p.dialog.clone());
        }
        match &self.overlay {
            UiOverlay::Dialog(d) => Some(d.clone()),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Workspace: multiple open projects, each a full AppState + its own git handle
// ---------------------------------------------------------------------------

/// The stateless services shared by every project. Everything except git is
/// process-wide (a `RealFs`/`RealClock`/…); git is per-repository, so
/// [`Env::services`] pairs this bundle with a project's own [`GitCli`] to build
/// the [`Services`] a dispatch needs. Built once in [`run`].
struct Env<'a> {
    fs: &'a dyn FileSystem,
    pty: &'a dyn PtyBackend,
    clock: &'a dyn Clock,
    container: &'a dyn ContainerRuntime,
}

impl<'a> Env<'a> {
    /// Pair the shared services with a specific project's git handle.
    fn services<'b>(&'b self, git: &'b dyn GitExecutor) -> Services<'b> {
        Services {
            git,
            fs: self.fs,
            pty: self.pty,
            clock: self.clock,
            container: self.container,
        }
    }
}

/// One open project: a full [`AppState`] plus everything the event loop needs to
/// service it independently — its own repository git handle, git-status cache,
/// and per-project background-worker channels. Every open project stays live
/// (its PTYs are drained and its agents notify) even when another is on screen.
struct Project {
    /// Display name for the project tab (the repo folder name).
    name: String,
    /// This project's repository git handle (rooted at its own repo).
    git: GitCli,
    /// The project's headless application state.
    state: AppState,
    /// Git-status cache for this project's tabs (keyed by tab id).
    cache: GitStatusCache,
    /// Completed-worktree-creation channel for this project's background worker.
    create_tx: Sender<CreateOutcome>,
    create_rx: Receiver<CreateOutcome>,
    /// Background git-status refresh channel for this project.
    status_tx: Sender<StatusMsg>,
    status_rx: Receiver<StatusMsg>,
    /// Whether a git-status refresh is in flight for this project.
    status_in_flight: bool,
    /// Serializes this project's `git worktree add`s so two quick new-tab
    /// requests don't race on the repo's index/worktree locks.
    git_lock: Arc<Mutex<()>>,
}

/// Open the git project rooted at (or containing) `path`: discover its repo
/// root, run the SPECS §7 startup (init, recover — never relaunch agents), and
/// build a [`Project`] with fresh per-project worker channels. Fails if `path`
/// is not inside a git repository.
fn open_project(env: &Env, path: &Path) -> Result<Project> {
    let git = GitCli::discover(path)?;
    let root = git.root().to_path_buf();
    let name = derive_project_name(&root);
    let state = {
        let services = env.services(&git);
        startup(&services, &root, &root)?
    };
    let (create_tx, create_rx) = std::sync::mpsc::channel::<CreateOutcome>();
    let (status_tx, status_rx) = std::sync::mpsc::channel::<StatusMsg>();
    Ok(Project {
        name,
        git,
        state,
        cache: GitStatusCache::new(),
        create_tx,
        create_rx,
        status_tx,
        status_rx,
        status_in_flight: false,
        git_lock: Arc::new(Mutex::new(())),
    })
}

/// The set of open projects plus the active (on-screen) one. The active project
/// renders in the main pane; all projects are serviced in the background.
struct Workspace {
    projects: Vec<Project>,
    active: usize,
}

impl Workspace {
    /// The active project (immutable).
    fn active_project(&self) -> &Project {
        &self.projects[self.active]
    }

    /// The active project (mutable).
    fn active_project_mut(&mut self) -> &mut Project {
        let i = self.active;
        &mut self.projects[i]
    }

    /// Whether a project rooted at `root` is already open.
    fn contains_root(&self, root: &Path) -> bool {
        self.projects.iter().any(|p| p.git.root() == root)
    }

    /// Set the active project by index (clamped to a valid index).
    fn set_active(&mut self, idx: usize) {
        if idx < self.projects.len() {
            self.active = idx;
        }
    }

    /// Switch the active project relative to the current one (wrapping).
    fn switch(&mut self, sel: Selector) {
        let len = self.projects.len();
        if len == 0 {
            return;
        }
        self.active = match sel {
            Selector::Index(i) => i.min(len - 1),
            Selector::Next => (self.active + 1) % len,
            Selector::Prev => (self.active + len - 1) % len,
        };
    }

    /// Build the per-project summaries for the project tab row.
    fn tab_infos(&self, now_ms: u64) -> Vec<ProjectTabInfo> {
        self.projects
            .iter()
            .map(|p| {
                let (attention, busy) = project_status_flags(
                    p.state
                        .tabs
                        .iter()
                        .map(|tab| tab.display_status(now_ms).interpreted),
                );
                ProjectTabInfo {
                    name: p.name.clone(),
                    attention,
                    busy,
                }
            })
            .collect()
    }
}

/// Collapse agent lifecycle states into the two indicators shown on a project
/// tab. Because callers pass display-ready states, project progress follows the
/// same explicit backend events as each agent tab.
fn project_status_flags(
    statuses: impl IntoIterator<Item = crate::contracts::InterpretedStatus>,
) -> (bool, bool) {
    use crate::contracts::InterpretedStatus::*;
    let mut busy = false;
    let mut attention = false;
    for status in statuses {
        match status {
            Starting | Running | Working => busy = true,
            WaitingForInput | NeedsAttention | Failed => attention = true,
            _ => {}
        }
    }
    (attention, busy)
}

// ---------------------------------------------------------------------------
// Event loop (SPECS §23)
// ---------------------------------------------------------------------------

/// The main event loop. Services every open project's PTYs/status/notifications
/// each tick (so background projects stay live), renders the active project plus
/// the project tab row, and routes input until the user quits or a fatal error
/// occurs.
fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    workspace: &mut Workspace,
    env: &Env,
    notifier: &dyn Notifier,
) -> Result<()> {
    let mut ui = Ui::default();
    let mut tick: u64 = 0;

    // Suppress notifications briefly at startup so resumed/just-launched agents
    // settling to idle don't produce a burst of "finished" alerts (SPECS §24).
    let now0 = env.clock.now_millis();
    for p in workspace.projects.iter_mut() {
        p.state.begin_notification_grace(now0);
    }

    // Once-a-day update notice (SPECS §30): surface any cached "newer version"
    // finding immediately and, when due, kick off a background check. Applied to
    // every project so whichever is active shows the hint.
    let (update_tx, update_rx) = std::sync::mpsc::channel::<String>();
    let check_enabled = workspace.active_project().state.config.update.check;
    if let Some(latest) =
        crate::update::start_check(check_enabled, env.clock.now_unix_secs(), update_tx)
    {
        for p in workspace.projects.iter_mut() {
            p.state.update_available = Some(latest.clone());
        }
    }

    loop {
        let now_ms = env.clock.now_millis();
        let active = workspace.active;
        let n = workspace.projects.len();

        // --- Service EVERY project each tick so background projects stay live:
        //     drain their PTYs, finalize completed worktrees, poll status files,
        //     and fire notifications regardless of which project is on screen. ---
        for idx in 0..n {
            let is_active = idx == active;
            let p = &mut workspace.projects[idx];

            drain_pty_output(&mut p.state, now_ms);

            {
                let services = env.services(&p.git);
                drain_create_outcomes(&p.create_rx, &mut p.state, &services, &mut ui, is_active);
            }

            // Prune cache entries for tabs that no longer exist.
            p.cache
                .retain(|id, _| p.state.tabs.iter().any(|t| &t.meta.id == id));

            while let Ok(msg) = p.status_rx.try_recv() {
                match msg {
                    StatusMsg::Update(id, status) => {
                        p.cache.insert(id, status);
                    }
                    StatusMsg::Done => p.status_in_flight = false,
                }
            }

            {
                let services = env.services(&p.git);
                p.state.poll_status_files(&services, now_ms);
            }

            // Prefix the project name so alerts read "project: tab" — useful
            // when several projects are open at once (SPECS §24).
            for mut note in p.state.take_finish_notifications(now_ms) {
                note.title = format!("{}: {}", p.name, note.title);
                notifier.notify(&note);
            }
        }

        // --- Apply a completed background update check (SPECS §30). ---
        while let Ok(latest) = update_rx.try_recv() {
            for p in workspace.projects.iter_mut() {
                p.state.update_available = Some(latest.clone());
            }
        }

        // --- Refresh the git-status cache for the ACTIVE project only (it is
        //     the only one whose sidebar/info bar is on screen). ---
        if tick.is_multiple_of(GIT_REFRESH_EVERY) {
            let p = &mut workspace.projects[active];
            if !p.status_in_flight && spawn_status_refresh(&p.state, &p.git, &p.status_tx) {
                p.status_in_flight = true;
            }
        }
        tick = tick.wrapping_add(1);

        // --- Auto-scroll the active terminal while a drag rests at an edge. ---
        if ui.drag.is_some() {
            if let Ok(size) = terminal.size() {
                autoscroll_drag(
                    &mut workspace.projects[active].state,
                    &ui,
                    Rect::new(0, 0, size.width, size.height),
                );
            }
        }

        // --- Keep the active tab's terminals sized to the current layout. ---
        if let Ok(size) = terminal.size() {
            sync_terminal_sizes(
                &mut workspace.projects[active].state,
                PtySize {
                    rows: size.height,
                    cols: size.width,
                },
            );
        }

        // --- Render: the project tab row (workspace-level) plus the active
        //     project's full UI. The project row is painted first so any
        //     centered overlay drawn by `draw` still wins on tiny screens. ---
        let overlay = ui.render_overlay();
        let infos = workspace.tab_infos(now_ms);
        let active_idx = workspace.active;
        let p = &workspace.projects[active_idx];
        terminal
            .draw(|frame| {
                let area = frame.area();
                let ml = crate::tui::layout::compute(area);
                draw_project_tab_bar(frame, ml.project_tabs, &infos, active_idx, now_ms);
                draw(frame, &p.state, &p.cache, &overlay, now_ms);
            })
            .map_err(|e| FlightDeckError::Io(format!("render failed: {e}")))?;

        // --- Poll for input (short timeout so PTY output keeps flowing). ---
        let has_event = event::poll(POLL_TIMEOUT)
            .map_err(|e| FlightDeckError::Io(format!("event poll failed: {e}")))?;
        if !has_event {
            continue;
        }

        match event::read().map_err(|e| FlightDeckError::Io(format!("event read failed: {e}")))? {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                if handle_key(key, workspace, env, &mut ui)? {
                    break; // Quit requested via the Ctrl-q key action.
                }
            }
            Event::Mouse(me) => {
                let area = match terminal.size() {
                    Ok(s) => Rect::new(0, 0, s.width, s.height),
                    Err(_) => continue,
                };
                handle_mouse(me, area, workspace, env, &mut ui);
            }
            Event::Paste(data) => {
                handle_paste(data, workspace, env, &mut ui)?;
            }
            Event::Resize(cols, rows) => {
                let size = viewport_pty_size(PtySize { rows, cols });
                // Resize every project's sessions so a background agent's output
                // wraps correctly the moment the user switches back to it.
                for p in workspace.projects.iter_mut() {
                    p.state.set_pty_size(size);
                    resize_sessions(&mut p.state, size);
                }
            }
            _ => {}
        }

        // --- Hand off queued worktree-creation jobs to the owning project's
        //     background worker so `git worktree add` never blocks the loop. ---
        for pj in ui.pending_jobs.drain(..) {
            if let Some(p) = workspace.projects.get(pj.project) {
                spawn_worktree_job(pj.job, &p.git, &p.git_lock, &p.create_tx);
            }
        }

        // --- Open a config file in $EDITOR if requested (SPECS §8). Done here,
        //     where we own the terminal to suspend/restore, then reload every
        //     project's effective config to pick up any edits. ---
        if let Some((_project, path)) = ui.pending_editor.take() {
            if let Err(e) = open_in_editor(terminal, &path) {
                ui.message(format!("Editor failed: {e}"));
            }
            reload_all_projects_config(workspace, env);
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
    is_active: bool,
) {
    while let Ok(outcome) = create_rx.try_recv() {
        match outcome.result {
            Ok(()) => match state.finalize_new_tab(&outcome.tab_id, services) {
                // Finalize (spawn the agent, flip to Ready) happens regardless of
                // which project is on screen; only surface the toast for the
                // active one so a background project's completion is not noisy.
                Ok(effect) => {
                    if is_active {
                        apply_effect(effect, state, ui)
                    }
                }
                Err(e) => {
                    state.fail_new_tab(&outcome.tab_id);
                    if is_active {
                        ui.message(format!("Failed to start agent: {e}"));
                    }
                }
            },
            Err(e) => {
                state.fail_new_tab(&outcome.tab_id);
                if is_active {
                    ui.message(format!("Failed to create worktree: {e}"));
                }
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
fn spawn_status_refresh(
    state: &AppState,
    worker_git: &GitCli,
    status_tx: &Sender<StatusMsg>,
) -> bool {
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
fn handle_mouse(me: MouseEvent, area: Rect, workspace: &mut Workspace, env: &Env, ui: &mut Ui) {
    // A modal dialog captures clicks first: a button fires its accelerator; an
    // outside click dismisses a plain notification (confirmations ignore it so
    // they are never dismissed by accident).
    if let Some(dialog) = ui.active_dialog() {
        if me.kind == MouseEventKind::Down(MouseButton::Left) {
            match dialog_hit(area, &dialog, me.column, me.row) {
                DialogHit::Button(i) => {
                    if let Some(button) = dialog.buttons.get(i) {
                        trigger_dialog_button(button.accel, workspace, env, ui);
                    }
                }
                DialogHit::Outside if ui.prompt.is_none() => ui.clear(),
                _ => {}
            }
        }
        return;
    }

    // Ignore mouse while any other modal/overlay is capturing input.
    if ui.modal_active() {
        return;
    }

    // The project tab row (workspace-level) is checked before the active
    // project's own layout: a click switches/opens/closes a project.
    if me.kind == MouseEventKind::Down(MouseButton::Left) {
        let ml = crate::tui::layout::compute(area);
        let names: Vec<String> = workspace.projects.iter().map(|p| p.name.clone()).collect();
        if let Some(hit) = project_tab_hit_test(ml.project_tabs, &names, me.column, me.row) {
            ui.drag = None;
            match hit {
                ProjectHit::Tab(i) => workspace.set_active(i),
                ProjectHit::Close(i) => {
                    workspace.set_active(i);
                    start_close_project_flow(workspace, ui, i);
                }
                ProjectHit::NewButton => start_open_project_flow(workspace, env, ui),
            }
            return;
        }
    }

    // Otherwise route the click into the active project's UI.
    let active = workspace.active;
    let p = &mut workspace.projects[active];
    let services = env.services(&p.git);
    handle_mouse_project(me, area, &mut p.state, &services, ui);
}

/// Handle a mouse event within the active project's UI (tabs, terminals, drag
/// selection, wheel). Split out of [`handle_mouse`] so the workspace-level
/// chrome (dialogs, project tab row) is handled by the caller.
fn handle_mouse_project(
    me: MouseEvent,
    area: Rect,
    state: &mut AppState,
    services: &Services,
    ui: &mut Ui,
) {
    match me.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            // A click on a tab/header switches to it (and never starts a
            // selection). Clicking the left sidebar focuses the app chrome (APP
            // mode); clicking a child-terminal tab (or, in split view, a column
            // header) focuses that terminal — the click sets the mode the user
            // would intuitively expect (SPECS §23).
            if let Some(hit) = hit_test(area, state, me.column, me.row) {
                ui.drag = None;
                match hit {
                    HitTarget::AgentTab(i) => {
                        let _ =
                            state.dispatch(Command::SwitchAgentTab(Selector::Index(i)), services);
                        state.focus_app();
                    }
                    HitTarget::CloseAgentTab(i) => {
                        // Sidebar [x]: select the tab, then ask whether to abandon
                        // the worktree or just close the agent (never destructive
                        // without confirmation).
                        let _ =
                            state.dispatch(Command::SwitchAgentTab(Selector::Index(i)), services);
                        state.focus_app();
                        start_prompt(ui, Prompt::CloseAgentChoice { index: i });
                    }
                    HitTarget::Sidebar => {
                        // Clicking the sidebar chrome (header/heading/empty space)
                        // focuses the app without changing the selected tab, so
                        // APP mode is reachable by clicking the left panel even
                        // with zero or one agents (SPECS §23).
                        state.focus_app();
                    }
                    HitTarget::Child(target) => {
                        select_target(state, services, target);
                        state.focus_terminal();
                    }
                    HitTarget::CloseChild(target) => {
                        close_child_target(state, services, ui, target)
                    }
                    HitTarget::NewAgentButton => {
                        // Spawn another agent in the selected tab's worktree,
                        // asking which backend to use first (SPECS §19).
                        start_new_child_agent_flow(state, services, ui);
                    }
                    HitTarget::NewShellButton => {
                        if let Err(e) =
                            dispatch_command(Command::NewChildTerminal, state, services, ui)
                        {
                            ui.message(format!("Error: {e}"));
                        }
                    }
                }
                return;
            }
            // A press inside a terminal viewport begins a selection. In split
            // view this is the column under the pointer (which also becomes the
            // active terminal); otherwise the single terminal pane. Focusing it
            // sends subsequent keystrokes there (TERMINAL mode, SPECS §23).
            if let Some((target, viewport)) = terminal_at(area, state, me.column, me.row) {
                select_target(state, services, target);
                state.focus_terminal();
                begin_terminal_selection(state, ui, target, viewport, me);
            }
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            if let Some(target) = ui.drag.as_ref().map(|d| d.target) {
                if let Some(d) = ui.drag.as_mut() {
                    d.col = me.column;
                    d.row = me.row;
                }
                // Clamp the pointer into the target's viewport so a drag past an
                // edge keeps extending along that edge (auto-scroll reveals more
                // each tick).
                if let Some(vp) = viewport_for_target(area, state, target) {
                    let col = me
                        .column
                        .saturating_sub(vp.x)
                        .min(vp.width.saturating_sub(1));
                    let row = me.row.saturating_sub(vp.y).min(vp.height.saturating_sub(1));
                    if let Some(term) = terminal_for_target(state, target) {
                        term.update_selection(row, col);
                    }
                }
            } else if let Some((target, vp)) = terminal_at(area, state, me.column, me.row) {
                // Forwarded drag for a mouse-aware hosted app.
                let col = me.column.saturating_sub(vp.x);
                let row = me.row.saturating_sub(vp.y);
                if let Some(term) = terminal_for_target(state, target) {
                    if term.wants_mouse() {
                        let bytes = encode_mouse_button(term.mouse_encoding(), 32, col, row, true);
                        let _ = term.session_mut().write_input(&bytes);
                    }
                }
            }
        }
        MouseEventKind::Up(MouseButton::Left) => {
            if let Some(drag) = ui.drag.take() {
                // End of a local selection: copy it (and keep it highlighted as
                // confirmation), or clear a zero-length click.
                if let Some(term) = terminal_for_target(state, drag.target) {
                    if term.has_selection() {
                        if let Some(text) = term.selected_text() {
                            crate::tui::clipboard::copy(&text);
                        }
                    } else {
                        term.clear_selection();
                    }
                }
            } else if let Some((target, vp)) = terminal_at(area, state, me.column, me.row) {
                // Forwarded release for a mouse-aware hosted app.
                let col = me.column.saturating_sub(vp.x);
                let row = me.row.saturating_sub(vp.y);
                if let Some(term) = terminal_for_target(state, target) {
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

/// Switch focus to a child-terminal `target` within the selected tab: the
/// primary agent terminal or a child shell by index. A no-op switch (selecting
/// the already-active terminal) is harmless. Mirrors the tab-bar click handling.
fn select_target(state: &mut AppState, services: &Services, target: ChildTarget) {
    match target {
        ChildTarget::Primary => {
            if let Some(tab) = state.selected_mut() {
                tab.session.focus_primary();
            }
        }
        ChildTarget::Child(i) => {
            let _ = state.dispatch(Command::SwitchChildTerminal(Selector::Index(i)), services);
        }
    }
}

/// Fire a dialog button: synthesize its accelerator key and route it exactly
/// like a keypress, so mouse and keyboard share one code path. A notification
/// (no active prompt) is simply dismissed.
fn trigger_dialog_button(accel: DialogAccel, workspace: &mut Workspace, env: &Env, ui: &mut Ui) {
    let code = match accel {
        DialogAccel::Char(c) => KeyCode::Char(c),
        DialogAccel::Enter => KeyCode::Enter,
        DialogAccel::Esc => KeyCode::Esc,
    };
    if ui.prompt.is_some() {
        let key = KeyEvent::new(code, KeyModifiers::NONE);
        if let Err(e) = handle_prompt_key(key, workspace, env, ui) {
            ui.message(format!("Error: {e}"));
        }
    } else {
        ui.clear();
    }
}

/// Handle a click on a child-terminal tab's `✕`. The primary "agent" tab closes
/// the whole Agent Tab (its own confirming flow, SPECS §25); a shell selects
/// itself and asks a yes/no confirm before closing.
fn close_child_target(state: &mut AppState, services: &Services, ui: &mut Ui, target: ChildTarget) {
    match target {
        ChildTarget::Primary => {
            state.focus_app();
            if let Err(e) =
                dispatch_command(Command::CloseAgentTab { action: None }, state, services, ui)
            {
                ui.message(format!("Error: {e}"));
            }
        }
        ChildTarget::Child(i) => {
            let label = child_tab_label(state, ChildTarget::Child(i))
                .unwrap_or_else(|| format!("shell {}", i + 1));
            // Select the terminal so the confirmed close acts on it.
            let _ = state.dispatch(Command::SwitchChildTerminal(Selector::Index(i)), services);
            state.focus_app();
            start_prompt(ui, Prompt::CloseChildConfirm { label });
        }
    }
}

/// Mutable access to the terminal a [`ChildTarget`] names within the selected
/// tab, or `None` if there is no selected tab / the terminal is not spawned.
fn terminal_for_target(
    state: &mut AppState,
    target: ChildTarget,
) -> Option<&mut crate::terminal::session::Terminal> {
    let tab = state.selected_mut()?;
    match target {
        ChildTarget::Primary => tab.session.primary_mut(),
        ChildTarget::Child(i) => tab.session.child_mut(i),
    }
}

/// The child-terminal targets shown for the selected tab, in display order:
/// the primary agent terminal followed by one entry per child shell. Matches
/// the ordering used by the split-view layout and the child tab bar.
fn target_order(state: &AppState) -> Vec<ChildTarget> {
    let mut targets = vec![ChildTarget::Primary];
    if let Some(tab) = state.selected() {
        for i in 0..tab.session.child_count() {
            targets.push(ChildTarget::Child(i));
        }
    }
    targets
}

/// The currently active child-terminal target for the selected tab.
fn active_target(state: &AppState) -> ChildTarget {
    match state.selected().and_then(|t| t.session.selected_child()) {
        Some(i) => ChildTarget::Child(i),
        None => ChildTarget::Primary,
    }
}

/// Resolve a pointer at `(col, row)` to the terminal viewport it lies over and
/// the terminal that viewport hosts. In split view this is the body (below the
/// header) of the column under the pointer; otherwise the single terminal pane,
/// targeting whichever terminal is active. Returns `None` if the pointer is over
/// no terminal viewport (sidebar, tab bar, gutter, status bar, …).
fn terminal_at(area: Rect, state: &AppState, col: u16, row: u16) -> Option<(ChildTarget, Rect)> {
    let ml = crate::tui::layout::compute(area);
    if state.split_view {
        let region = crate::tui::layout::split_region(&ml);
        let targets = target_order(state);
        let cols = crate::tui::layout::split_columns(region, targets.len());
        targets
            .into_iter()
            .zip(cols)
            .find_map(|(t, c)| rect_contains(c.viewport, col, row).then_some((t, c.viewport)))
    } else if rect_contains(ml.terminal, col, row) {
        Some((active_target(state), ml.terminal))
    } else {
        None
    }
}

/// The viewport rect for a specific terminal `target` under the current layout:
/// the matching split-view column body, or the single terminal pane. `None` if
/// the target's column is not present (e.g. layout too small).
fn viewport_for_target(area: Rect, state: &AppState, target: ChildTarget) -> Option<Rect> {
    let ml = crate::tui::layout::compute(area);
    if !state.split_view {
        return Some(ml.terminal);
    }
    let region = crate::tui::layout::split_region(&ml);
    let targets = target_order(state);
    let idx = targets.iter().position(|t| *t == target)?;
    let cols = crate::tui::layout::split_columns(region, targets.len());
    cols.into_iter().nth(idx).map(|c| c.viewport)
}

/// Begin a text selection at the press position within `viewport` on the
/// terminal named by `target`, or — when that terminal has its own mouse
/// reporting enabled and Shift is not held — forward the press to it instead.
fn begin_terminal_selection(
    state: &mut AppState,
    ui: &mut Ui,
    target: ChildTarget,
    viewport: Rect,
    me: MouseEvent,
) {
    let shift = me.modifiers.contains(KeyModifiers::SHIFT);
    let col = me.column.saturating_sub(viewport.x);
    let row = me.row.saturating_sub(viewport.y);
    if let Some(term) = terminal_for_target(state, target) {
        if term.wants_mouse() && !shift {
            let bytes = encode_mouse_button(term.mouse_encoding(), 0, col, row, true);
            let _ = term.session_mut().write_input(&bytes);
            ui.drag = None;
        } else {
            term.begin_selection(row, col);
            ui.drag = Some(DragState {
                col: me.column,
                row: me.row,
                target,
            });
        }
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
    let target = drag.target;
    let (drag_col, drag_row) = (drag.col, drag.row);
    let Some(term_area) = viewport_for_target(area, state, target) else {
        return;
    };
    if term_area.height == 0 {
        return;
    }
    // Top edge (pointer at or above the first row) scrolls up into history;
    // bottom edge (at or below the last row) scrolls back down.
    let up = if drag_row <= term_area.y {
        true
    } else if drag_row >= term_area.bottom().saturating_sub(1) {
        false
    } else {
        return;
    };

    let Some(term) = terminal_for_target(state, target) else {
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
    let col = drag_col
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
    // Scroll the terminal under the pointer: in split view the hovered column,
    // otherwise the single terminal pane.
    let Some((target, term_area)) = terminal_at(area, state, me.column, me.row) else {
        return;
    };
    let Some(term) = terminal_for_target(state, target) else {
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

/// Route a key press. Returns `Ok(true)` when the loop should quit. Workspace-
/// level actions (switch project) act on `workspace`; everything else acts on
/// the active project's [`AppState`].
fn handle_key(key: KeyEvent, workspace: &mut Workspace, env: &Env, ui: &mut Ui) -> Result<bool> {
    // 1. An active prompt captures all input first.
    if ui.prompt.is_some() {
        return handle_prompt_key(key, workspace, env, ui).map(|_| false);
    }

    // 2. The configuration manager overlay, if open, captures input (SPECS §8).
    if ui.config.is_some() {
        return handle_config_key(key, workspace, env, ui).map(|_| false);
    }

    // 3. The command palette, if open, captures input next (SPECS §22).
    if ui.palette.is_some() {
        return handle_palette_key(key, workspace, env, ui).map(|_| false);
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
    let mode = workspace.active_project().state.mode();
    match map_key(mode, key) {
        KeyAction::Dispatch(cmd) => {
            let active = workspace.active;
            let p = &mut workspace.projects[active];
            let services = env.services(&p.git);
            dispatch_command(cmd, &mut p.state, &services, ui)?;
            Ok(false)
        }
        // Project switching is workspace-level, not an AppState command.
        KeyAction::SwitchProject(sel) => {
            workspace.switch(sel);
            Ok(false)
        }
        KeyAction::Passthrough(bytes) => {
            write_active_pty(&mut workspace.active_project_mut().state, &bytes);
            Ok(false)
        }
        KeyAction::Paste => {
            paste_into_active_pty(&mut workspace.active_project_mut().state);
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
            workspace.active_project_mut().state.focus_app();
            Ok(false)
        }
        KeyAction::FocusTerminal => {
            workspace.active_project_mut().state.focus_terminal();
            Ok(false)
        }
        KeyAction::Quit => Ok(true),
        KeyAction::None => Ok(false),
    }
}

/// Handle an `Event::Paste` — a single atomic paste from the host terminal
/// (delivered as one event because we enable bracketed paste mode at startup).
///
/// A text-editing modal (name prompt, command palette) consumes the paste as
/// literal characters, replayed as discrete key presses so the existing editing
/// logic applies — this is exactly what these modals saw before bracketed paste
/// mode coalesced a paste into one event. Otherwise, only a focused terminal
/// receives it, forwarded to the PTY via [`paste_text_into_active_pty`].
fn handle_paste(data: String, workspace: &mut Workspace, env: &Env, ui: &mut Ui) -> Result<()> {
    if ui.prompt.is_some() || ui.palette.is_some() {
        for ch in data.chars() {
            let code = match ch {
                '\n' | '\r' => KeyCode::Enter,
                c => KeyCode::Char(c),
            };
            handle_key(
                KeyEvent::new(code, KeyModifiers::empty()),
                workspace,
                env,
                ui,
            )?;
            // Stop if the modal closed mid-paste (e.g. a newline submitted it).
            if ui.prompt.is_none() && ui.palette.is_none() {
                break;
            }
        }
        return Ok(());
    }

    // Any other overlay swallows input the way a key press would; dismiss it
    // (mirroring the analogous branch in `handle_key`) rather than silently
    // dropping the paste with the overlay left stuck on screen.
    if !matches!(ui.overlay, UiOverlay::None) {
        ui.clear();
        return Ok(());
    }

    // Only a focused terminal receives pasted text; in App mode it is a no-op.
    let state = &mut workspace.active_project_mut().state;
    if state.mode() == InputMode::Terminal {
        paste_text_into_active_pty(state, &data);
    }
    Ok(())
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
                ui.message("No Agent Session Tab selected.");
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
                ui.message("No Agent Session Tab selected.");
                return Ok(());
            }
            start_prompt(ui, Prompt::SetManualStatus);
            return Ok(());
        }
        Command::CloseAgentTab { action: None } => {
            // Fall through: dispatch returns the option set, which we surface
            // as a Close prompt (SPECS §25, never auto-escalate).
        }
        Command::CloseChildTerminal => {
            // Confirm before closing a child terminal (Ctrl-w), mirroring the
            // tab's `✕` click. Acts on the currently-selected child.
            match state.selected().and_then(|t| t.session.selected_child()) {
                Some(i) => {
                    let label = child_tab_label(state, ChildTarget::Child(i))
                        .unwrap_or_else(|| format!("shell {}", i + 1));
                    start_prompt(ui, Prompt::CloseChildConfirm { label });
                }
                None => ui.message("No child terminal selected."),
            }
            return Ok(());
        }
        Command::CloseAgentTerminal => {
            // Confirm before closing the selected child agent. Refuse (no prompt)
            // when the selected terminal is not an additional agent.
            let selected_agent = state.selected().and_then(|t| {
                let i = t.session.selected_child()?;
                let is_agent = t.session.child(i).map(|c| c.kind)
                    == Some(crate::terminal::session::TerminalKind::Agent);
                is_agent.then_some(i)
            });
            match selected_agent {
                Some(i) => {
                    let label = child_tab_label(state, ChildTarget::Child(i))
                        .unwrap_or_else(|| format!("agent {}", i + 1));
                    start_prompt(ui, Prompt::CloseChildConfirm { label });
                }
                None => ui.message("No agent tab selected."),
            }
            return Ok(());
        }
        _ => {}
    }

    // A command that can't run (e.g. an action needing a selected tab when the
    // project has none, or a git failure) must surface as a message, never
    // crash the event loop. Errors always become a toast; only the Ok path
    // maps its effect onto the UI.
    match state.dispatch(cmd, services) {
        Ok(effect) => apply_effect(effect, state, ui),
        Err(e) => ui.message(format!("Error: {e}")),
    }
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
        Effect::AbandonWarning { dirty } => {
            start_prompt(ui, Prompt::AbandonConfirm { dirty });
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
        Effect::RebaseConfirm {
            agent_branch,
            base_branch,
            drift,
            primary_running,
        } => {
            start_prompt(
                ui,
                Prompt::RebaseConfirm {
                    agent_branch,
                    base_branch,
                    drift,
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
        Effect::GitStatus { status, pr_url } => {
            ui.overlay = UiOverlay::GitStatus {
                status: *status,
                pr_url,
            };
        }
        Effect::ShowHelp => ui.overlay = UiOverlay::Help,
    }
}

/// Begin an interactive prompt, building its modal dialog.
fn start_prompt(ui: &mut Ui, prompt: Prompt) {
    let dialog = prompt_dialog(&prompt);
    ui.palette = None;
    ui.overlay = UiOverlay::None;
    ui.prompt = Some(PromptState { prompt, dialog });
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

/// Begin the "+ agent" flow: spawn an additional agent in the selected session
/// tab's worktree, after picking a backend when more than one agent is
/// registered. With no session tab yet, fall back to creating a fresh Agent
/// Session Tab/worktree (there is no session to add an agent to).
fn start_new_child_agent_flow(state: &mut AppState, services: &Services, ui: &mut Ui) {
    if state.selected().is_none() {
        state.focus_app();
        start_new_tab_flow(state, ui);
        return;
    }
    let agents: Vec<(String, String)> = state
        .registry
        .all()
        .iter()
        .map(|a| (a.key.clone(), a.display_name.clone()))
        .collect();
    if agents.len() > 1 {
        start_prompt(ui, Prompt::SelectChildAgent { agents });
    } else {
        // Zero or one agent: no meaningful choice — spawn the tab's default.
        state.focus_terminal();
        if let Err(e) = dispatch_command(
            Command::NewAgentTerminal { agent_key: None },
            state,
            services,
            ui,
        ) {
            ui.message(format!("Error: {e}"));
        }
    }
}

// ---------------------------------------------------------------------------
// Project flows (multi-project): open / close / browse
// ---------------------------------------------------------------------------

/// The immediate, non-hidden subdirectories of `dir`, sorted — the navigable
/// entries in the folder browser. Best effort: an unreadable directory yields
/// an empty list rather than an error.
fn list_subdirs(fs: &dyn FileSystem, dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs
        .list_dir(dir)
        .unwrap_or_default()
        .into_iter()
        .filter(|p| fs.is_dir(p))
        .filter(|p| {
            !p.file_name()
                .map(|n| n.to_string_lossy().starts_with('.'))
                .unwrap_or(false)
        })
        .collect();
    v.sort();
    v
}

/// Begin the Open Project flow: a folder browser rooted at the sibling
/// directory of the active project (its neighbours are the likely next
/// projects), falling back to `$HOME` then the filesystem root.
fn start_open_project_flow(workspace: &Workspace, env: &Env, ui: &mut Ui) {
    let start_dir = workspace
        .active_project()
        .git
        .root()
        .parent()
        .map(|p| p.to_path_buf())
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("/"));
    let entries = list_subdirs(env.fs, &start_dir);
    start_prompt(
        ui,
        Prompt::OpenProject {
            browse: BrowseState {
                dir: start_dir,
                entries,
                selected: 0,
                typed: String::new(),
            },
        },
    );
}

/// Begin the Close Project flow: confirm first (SPECS §25 no-surprise rule).
/// Refuses to close the only remaining project — that is what Ctrl-q is for.
fn start_close_project_flow(workspace: &Workspace, ui: &mut Ui, index: usize) {
    if workspace.projects.len() <= 1 {
        ui.message("Can't close the only project. Use Ctrl-q to quit FlightDeck.");
        return;
    }
    start_prompt(ui, Prompt::CloseProjectConfirm { index });
}

/// The folder the browser opens on Enter: the typed path (absolute, or relative
/// to the browsed dir) when non-empty, else the highlighted subdirectory, else
/// the browsed directory itself.
fn resolve_browse_target(browse: &BrowseState) -> PathBuf {
    let typed = browse.typed.trim();
    if !typed.is_empty() {
        let t = PathBuf::from(typed);
        return if t.is_absolute() {
            t
        } else {
            browse.dir.join(t)
        };
    }
    if let Some(sel) = browse.entries.get(browse.selected) {
        return sel.clone();
    }
    browse.dir.clone()
}

/// Handle a key for the [`Prompt::OpenProject`] folder browser.
fn handle_open_project_key(
    key: KeyEvent,
    workspace: &mut Workspace,
    env: &Env,
    ui: &mut Ui,
) -> Result<()> {
    let Some(mut pstate) = ui.prompt.take() else {
        return Ok(());
    };

    // Enter confirms — resolve the target and open (or switch to) that project.
    if key.code == KeyCode::Enter {
        let target = match &pstate.prompt {
            Prompt::OpenProject { browse } => resolve_browse_target(browse),
            _ => {
                ui.prompt = Some(pstate);
                return Ok(());
            }
        };
        match open_project(env, &target) {
            Ok(mut proj) => {
                if workspace.contains_root(proj.git.root()) {
                    let root = proj.git.root().to_path_buf();
                    if let Some(i) = workspace.projects.iter().position(|p| p.git.root() == root) {
                        workspace.set_active(i);
                    }
                    ui.message("Project already open — switched to it.");
                } else {
                    // Seed the new project's PTY size from the active one and
                    // resume its recovered agents (never auto-relaunched beyond
                    // this explicit open), matching startup behaviour.
                    let sz = workspace.active_project().state.pty_size;
                    {
                        let services = env.services(&proj.git);
                        proj.state.set_pty_size(sz);
                        let _ = proj.state.resume_agents(&services);
                    }
                    let name = proj.name.clone();
                    workspace.projects.push(proj);
                    workspace.active = workspace.projects.len() - 1;
                    ui.message(format!("Opened project '{name}'."));
                }
            }
            Err(e) => ui.message(format!("Could not open project: {e}")),
        }
        return Ok(());
    }

    // Navigation / typing edits the browse state in place.
    {
        let Prompt::OpenProject { browse } = &mut pstate.prompt else {
            ui.prompt = Some(pstate);
            return Ok(());
        };
        match key.code {
            KeyCode::Up => {
                browse.selected = browse.selected.saturating_sub(1);
            }
            KeyCode::Down => {
                if browse.selected + 1 < browse.entries.len() {
                    browse.selected += 1;
                }
            }
            // Descend into the highlighted subdirectory.
            KeyCode::Right | KeyCode::Tab => {
                if let Some(dir) = browse.entries.get(browse.selected).cloned() {
                    browse.dir = dir;
                    browse.entries = list_subdirs(env.fs, &browse.dir);
                    browse.selected = 0;
                    browse.typed.clear();
                }
            }
            // Go to the parent directory (also Backspace when the typed path is
            // empty), highlighting the folder we came from.
            KeyCode::Left => {
                if let Some(parent) = browse.dir.parent().map(|p| p.to_path_buf()) {
                    let prev = browse.dir.clone();
                    browse.dir = parent;
                    browse.entries = list_subdirs(env.fs, &browse.dir);
                    browse.selected = browse.entries.iter().position(|e| *e == prev).unwrap_or(0);
                    browse.typed.clear();
                }
            }
            KeyCode::Backspace => {
                if browse.typed.is_empty() {
                    if let Some(parent) = browse.dir.parent().map(|p| p.to_path_buf()) {
                        let prev = browse.dir.clone();
                        browse.dir = parent;
                        browse.entries = list_subdirs(env.fs, &browse.dir);
                        browse.selected =
                            browse.entries.iter().position(|e| *e == prev).unwrap_or(0);
                    }
                } else {
                    browse.typed.pop();
                }
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                browse.typed.push(c);
            }
            _ => {}
        }
    }
    pstate.dialog = prompt_dialog(&pstate.prompt);
    ui.prompt = Some(pstate);
    Ok(())
}

/// Handle a key for the [`Prompt::CloseProjectConfirm`] confirmation. On `y` the
/// project's sessions are stopped, its state persisted, and it is removed.
fn handle_close_project_key(
    key: KeyEvent,
    workspace: &mut Workspace,
    env: &Env,
    ui: &mut Ui,
) -> Result<()> {
    let Some(pstate) = ui.prompt.take() else {
        return Ok(());
    };
    let index = match &pstate.prompt {
        Prompt::CloseProjectConfirm { index } => *index,
        _ => {
            ui.prompt = Some(pstate);
            return Ok(());
        }
    };
    match key.code {
        KeyCode::Char('y') => {
            if workspace.projects.len() <= 1 || index >= workspace.projects.len() {
                ui.message("Can't close the only project. Use Ctrl-q to quit FlightDeck.");
                return Ok(());
            }
            // Persist the closing project's tab state, then stop its sessions.
            {
                let p = &workspace.projects[index];
                let services = env.services(&p.git);
                let _ = persist_quietly(&p.state, &services);
            }
            terminate_all_sessions(&mut workspace.projects[index].state);
            let name = workspace.projects[index].name.clone();
            workspace.projects.remove(index);
            // Keep the active index pointing at a valid, sensible project.
            if index < workspace.active {
                workspace.active -= 1;
            }
            if workspace.active >= workspace.projects.len() {
                workspace.active = workspace.projects.len() - 1;
            }
            ui.message(format!("Closed project '{name}'."));
        }
        KeyCode::Char('n') => ui.clear(),
        _ => ui.prompt = Some(pstate),
    }
    Ok(())
}

/// A digit accelerator for the i-th (0-based) numbered choice, e.g. index 0 → '1'.
fn digit_accel(i: usize) -> DialogAccel {
    DialogAccel::Char(char::from_digit((i as u32 + 1) % 10, 10).unwrap_or('?'))
}

/// Build the modal [`Dialog`] for a prompt: the question/notification text plus
/// one button per available action. Each button's accelerator matches the key
/// [`handle_prompt_key`] expects, so mouse and keyboard stay in lockstep.
fn prompt_dialog(prompt: &Prompt) -> Dialog {
    let cancel = DialogButton::new(DialogAccel::Esc, "Cancel");
    match prompt {
        Prompt::SelectAgent { agents } => {
            let mut buttons: Vec<DialogButton> = agents
                .iter()
                .enumerate()
                .map(|(i, (_key, display))| DialogButton::new(digit_accel(i), display.clone()))
                .collect();
            buttons.push(cancel);
            Dialog::confirm("New Agent Session Tab — pick an agent", buttons)
        }
        Prompt::SelectChildAgent { agents } => {
            let mut buttons: Vec<DialogButton> = agents
                .iter()
                .enumerate()
                .map(|(i, (_key, display))| DialogButton::new(digit_accel(i), display.clone()))
                .collect();
            buttons.push(cancel);
            Dialog::confirm("New agent — pick a backend", buttons)
        }
        Prompt::NewTabName { buffer, .. } => Dialog::input(
            "New Agent Session Tab name",
            buffer.clone(),
            vec![DialogButton::new(DialogAccel::Enter, "Create"), cancel],
        ),
        Prompt::RenameTab { buffer } => Dialog::input(
            "Rename this Agent Session Tab",
            buffer.clone(),
            vec![DialogButton::new(DialogAccel::Enter, "Rename"), cancel],
        ),
        Prompt::SetManualStatus => Dialog::confirm(
            "Set status override",
            vec![
                DialogButton::new(DialogAccel::Char('i'), "In progress"),
                DialogButton::new(DialogAccel::Char('w'), "Waiting"),
                DialogButton::new(DialogAccel::Char('b'), "Blocked"),
                DialogButton::new(DialogAccel::Char('d'), "Done"),
                DialogButton::new(DialogAccel::Char('c'), "Clear"),
                cancel,
            ],
        ),
        Prompt::CloseTab { actions } => {
            let mut buttons: Vec<DialogButton> = actions
                .iter()
                .enumerate()
                .map(|(i, a)| DialogButton::new(digit_accel(i), close_action_label(*a)))
                .collect();
            buttons.push(cancel);
            Dialog::confirm(
                "Close tab — how should running processes be handled?",
                buttons,
            )
        }
        Prompt::CloseChildConfirm { label } => Dialog::confirm(
            format!("Close {label}?"),
            vec![
                DialogButton::new(DialogAccel::Char('y'), "Close"),
                DialogButton::new(DialogAccel::Char('n'), "Cancel"),
            ],
        ),
        Prompt::CloseAgentChoice { .. } => Dialog::confirm(
            "Abandon the worktree, or just close the agent?",
            vec![
                DialogButton::new(DialogAccel::Char('a'), "Abandon"),
                DialogButton::new(DialogAccel::Char('c'), "Close"),
                DialogButton::new(DialogAccel::Char('n'), "Cancel"),
            ],
        ),
        Prompt::PushConfirm => Dialog::confirm(
            "The worktree has uncommitted changes. Push the committed changes only?",
            vec![
                DialogButton::new(DialogAccel::Char('p'), "Push committed"),
                DialogButton::new(DialogAccel::Char('c'), "Cancel"),
            ],
        ),
        Prompt::AbandonConfirm { dirty } => {
            let (title, yes): (&str, &str) = if *dirty {
                (
                    "The worktree has uncommitted changes. Discard them and abandon it?",
                    "Abandon (force)",
                )
            } else {
                ("Abandon this worktree?", "Abandon")
            };
            Dialog::confirm(
                title,
                vec![
                    DialogButton::new(DialogAccel::Char('y'), yes),
                    DialogButton::new(DialogAccel::Char('n'), "Cancel"),
                ],
            )
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
            Dialog::confirm(
                format!(
                    "Merge {agent_branch} into {base_branch} then remove the worktree{running}?"
                ),
                vec![
                    DialogButton::new(DialogAccel::Char('y'), "Merge"),
                    DialogButton::new(DialogAccel::Char('n'), "Cancel"),
                ],
            )
        }
        Prompt::RebaseConfirm {
            agent_branch,
            base_branch,
            drift,
            primary_running,
        } => {
            let moved = match drift {
                0 => String::new(),
                1 => " (base moved 1 commit)".to_string(),
                n => format!(" (base moved {n} commits)"),
            };
            let running = if *primary_running {
                "; agent is running — its HEAD will be rewritten"
            } else {
                ""
            };
            Dialog::confirm(
                format!(
                    "Rebase {agent_branch} onto {base_branch}{moved}{running}? Rewrites history; aborts on conflict."
                ),
                vec![
                    DialogButton::new(DialogAccel::Char('y'), "Rebase"),
                    DialogButton::new(DialogAccel::Char('n'), "Cancel"),
                ],
            )
        }
        Prompt::OpenProject { browse } => {
            let title = format!(
                "Open project — {}   (↑↓ select · → open folder · ← parent · Enter to open · or type a path)",
                browse.dir.display()
            );
            let list: Vec<DialogListItem> = if browse.entries.is_empty() {
                vec![DialogListItem {
                    label: "(no subfolders)".to_string(),
                    selected: false,
                }]
            } else {
                browse
                    .entries
                    .iter()
                    .enumerate()
                    .map(|(i, e)| DialogListItem {
                        label: e
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_else(|| e.display().to_string()),
                        selected: i == browse.selected,
                    })
                    .collect()
            };
            Dialog::browser(
                title,
                browse.typed.clone(),
                list,
                vec![
                    DialogButton::new(DialogAccel::Enter, "Open"),
                    DialogButton::new(DialogAccel::Esc, "Cancel"),
                ],
            )
        }
        Prompt::CloseProjectConfirm { .. } => Dialog::confirm(
            "Close this project? Its agents will be stopped.",
            vec![
                DialogButton::new(DialogAccel::Char('y'), "Close"),
                DialogButton::new(DialogAccel::Char('n'), "Cancel"),
            ],
        ),
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

/// Handle a key while a prompt is active. Routes the two workspace-level prompts
/// (open / close project) to their own handlers and everything else to the
/// active project's prompt handler.
fn handle_prompt_key(
    key: KeyEvent,
    workspace: &mut Workspace,
    env: &Env,
    ui: &mut Ui,
) -> Result<()> {
    // Esc always cancels the prompt.
    if key.code == KeyCode::Esc {
        ui.clear();
        return Ok(());
    }

    // Workspace-level prompts don't touch the active project's AppState.
    match ui.prompt.as_ref().map(|p| &p.prompt) {
        Some(Prompt::OpenProject { .. }) => {
            return handle_open_project_key(key, workspace, env, ui)
        }
        Some(Prompt::CloseProjectConfirm { .. }) => {
            return handle_close_project_key(key, workspace, env, ui)
        }
        _ => {}
    }

    let active = workspace.active;
    let p = &mut workspace.projects[active];
    let services = env.services(&p.git);
    handle_prompt_key_project(key, &mut p.state, &services, ui, active)
}

/// Handle a key for a project-level prompt (new tab, rename, close, push, …) on
/// the active project. `active` is the active project index, tagged onto any
/// queued worktree job so it is handed to the right project's worker.
fn handle_prompt_key_project(
    key: KeyEvent,
    state: &mut AppState,
    services: &Services,
    ui: &mut Ui,
    active: usize,
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
                    let dialog = prompt_dialog(&prompt);
                    ui.prompt = Some(PromptState { prompt, dialog });
                    return Ok(());
                }
            }
            // Any other key: keep showing the picker.
            ui.prompt = Some(pstate);
        }
        Prompt::SelectChildAgent { agents } => {
            // A number key picks the backend and spawns the agent in-session.
            if let KeyCode::Char(c @ '1'..='9') = key.code {
                let idx = (c as usize) - ('1' as usize);
                if let Some((agent_key, _display)) = agents.get(idx) {
                    let result = state.dispatch(
                        Command::NewAgentTerminal {
                            agent_key: Some(agent_key.clone()),
                        },
                        services,
                    );
                    state.focus_terminal();
                    finish_prompt(result, ui);
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
            match key.code {
                KeyCode::Enter => {
                    let name = match &pstate.prompt {
                        Prompt::NewTabName { buffer, .. } | Prompt::RenameTab { buffer } => {
                            buffer.trim().to_string()
                        }
                        _ => unreachable!(),
                    };
                    if name.is_empty() {
                        // Keep prompting; nothing entered yet.
                        pstate.dialog = prompt_dialog(&pstate.prompt);
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
                                ui.pending_jobs.push(PendingJob {
                                    project: active,
                                    job,
                                });
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
                    if let Prompt::NewTabName { buffer, .. } | Prompt::RenameTab { buffer } =
                        &mut pstate.prompt
                    {
                        buffer.pop();
                    }
                    pstate.dialog = prompt_dialog(&pstate.prompt);
                    ui.prompt = Some(pstate);
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if let Prompt::NewTabName { buffer, .. } | Prompt::RenameTab { buffer } =
                        &mut pstate.prompt
                    {
                        buffer.push(c);
                    }
                    pstate.dialog = prompt_dialog(&pstate.prompt);
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
        Prompt::CloseChildConfirm { .. } => match key.code {
            KeyCode::Char('y') => {
                let result = state.dispatch(Command::CloseChildTerminal, services);
                finish_prompt(result, ui);
            }
            KeyCode::Char('n') => ui.clear(),
            _ => ui.prompt = Some(pstate),
        },
        Prompt::CloseAgentChoice { index } => {
            let index = *index;
            match key.code {
                KeyCode::Char('a') => {
                    // Route through the standard abandon flow, which always asks
                    // before discarding (warns extra loudly when dirty).
                    let _ =
                        state.dispatch(Command::SwitchAgentTab(Selector::Index(index)), services);
                    ui.prompt = None;
                    match state.dispatch(Command::AbandonWorktree { confirm: false }, services) {
                        Ok(effect) => apply_effect_no_state(effect, ui),
                        Err(e) => ui.message(format!("Error: {e}")),
                    }
                }
                KeyCode::Char('c') => {
                    // Close the agent via the standard close-options flow (§25).
                    let _ =
                        state.dispatch(Command::SwitchAgentTab(Selector::Index(index)), services);
                    ui.prompt = None;
                    match state.dispatch(Command::CloseAgentTab { action: None }, services) {
                        Ok(effect) => apply_effect_no_state(effect, ui),
                        Err(e) => ui.message(format!("Error: {e}")),
                    }
                }
                KeyCode::Char('n') => ui.clear(),
                _ => ui.prompt = Some(pstate),
            }
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
        Prompt::AbandonConfirm { .. } => match key.code {
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
        Prompt::RebaseConfirm { .. } => match key.code {
            KeyCode::Char('y') => {
                let result = state.dispatch(Command::RebaseWorktree { confirm: true }, services);
                finish_prompt(result, ui);
            }
            KeyCode::Char('n') => ui.clear(),
            _ => ui.prompt = Some(pstate),
        },
        // Workspace-level prompts are routed to their own handlers by
        // `handle_prompt_key` before reaching here; keep the prompt if one
        // slips through so it is never silently dropped.
        Prompt::OpenProject { .. } | Prompt::CloseProjectConfirm { .. } => {
            ui.prompt = Some(pstate);
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
        Effect::Quit => ui.should_quit = true,
        Effect::Message(m) => ui.message(m),
        Effect::Warning(m) => ui.message(format!("WARNING: {m}")),
        Effect::Refused(m) => ui.message(format!("Refused: {m}")),
        Effect::PrUrl(url) => ui.message(format!("PR: {url}")),
        Effect::AttachedExisting { branch } => {
            ui.message(format!("Attached to existing branch {branch}"))
        }
        Effect::PushWarning(_) => start_prompt(ui, Prompt::PushConfirm),
        Effect::AbandonWarning { dirty } => start_prompt(ui, Prompt::AbandonConfirm { dirty }),
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
        Effect::RebaseConfirm {
            agent_branch,
            base_branch,
            drift,
            primary_running,
        } => start_prompt(
            ui,
            Prompt::RebaseConfirm {
                agent_branch,
                base_branch,
                drift,
                primary_running,
            },
        ),
        Effect::CloseTabOptions(opts) => start_prompt(
            ui,
            Prompt::CloseTab {
                actions: opts.actions,
            },
        ),
        Effect::GitStatus { status, pr_url } => {
            ui.overlay = UiOverlay::GitStatus {
                status: *status,
                pr_url,
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
    workspace: &mut Workspace,
    env: &Env,
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
        KeyCode::Left => palette.select_left(),
        KeyCode::Right => palette.select_right(),
        KeyCode::Backspace => palette.pop_char(),
        KeyCode::Enter => {
            let action = palette.selected_action().cloned();
            ui.palette = None;
            if let Some(action) = action {
                run_palette_action(action, workspace, env, ui)?;
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
/// secondary prompt for payloads first), then dispatch (SPECS §22). Project
/// actions act on `workspace`; everything else on the active project.
fn run_palette_action(
    action: PaletteAction,
    workspace: &mut Workspace,
    env: &Env,
    ui: &mut Ui,
) -> Result<()> {
    // Workspace-level project actions.
    match action {
        PaletteAction::OpenProject => {
            start_open_project_flow(workspace, env, ui);
            return Ok(());
        }
        PaletteAction::CloseProject => {
            let i = workspace.active;
            start_close_project_flow(workspace, ui, i);
            return Ok(());
        }
        PaletteAction::SwitchProjectNext => {
            workspace.switch(Selector::Next);
            return Ok(());
        }
        PaletteAction::SwitchProjectPrev => {
            workspace.switch(Selector::Prev);
            return Ok(());
        }
        PaletteAction::OpenConfig => {
            open_config_manager(workspace, env, ui);
            return Ok(());
        }
        _ => {}
    }

    // Project-level actions act on the active project.
    let active = workspace.active;
    let p = &mut workspace.projects[active];
    let services = env.services(&p.git);
    let state = &mut p.state;
    match action {
        PaletteAction::Dispatch(cmd) => dispatch_command(cmd, state, &services, ui),
        PaletteAction::NewAgentTab => {
            start_new_tab_flow(state, ui);
            Ok(())
        }
        PaletteAction::NewAgentChild => {
            start_new_child_agent_flow(state, &services, ui);
            Ok(())
        }
        PaletteAction::RenameAgentTab => {
            if state.selected().is_none() {
                ui.message("No Agent Session Tab selected.");
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
            dispatch_command(
                Command::CloseAgentTab { action: None },
                state,
                &services,
                ui,
            )
        }
        PaletteAction::SetManualStatus => {
            if state.selected().is_none() {
                ui.message("No Agent Session Tab selected.");
                return Ok(());
            }
            start_prompt(ui, Prompt::SetManualStatus);
            Ok(())
        }
        // Handled above.
        PaletteAction::OpenProject
        | PaletteAction::CloseProject
        | PaletteAction::SwitchProjectNext
        | PaletteAction::SwitchProjectPrev
        | PaletteAction::OpenConfig => Ok(()),
    }
}

// ---------------------------------------------------------------------------
// Configuration manager (SPECS §8)
// ---------------------------------------------------------------------------

/// Build and open the configuration manager for the active project, reading the
/// global base and this project's override files into an editable model. Ensures
/// the global base exists first so it is always editable.
fn open_config_manager(workspace: &Workspace, env: &Env, ui: &mut Ui) {
    let global_path = global_config_path();
    if let Some(gp) = &global_path {
        let _ = ensure_global_config(env.fs, gp);
    }

    let read_table = |path: &Path| -> toml::Table {
        if env.fs.exists(path) {
            env.fs
                .read_to_string(path)
                .ok()
                .and_then(|s| crate::config::load::parse_table(&s).ok())
                .unwrap_or_default()
        } else {
            toml::Table::new()
        }
    };

    let p = workspace.active_project();
    let project_path = p.git.root().join(".flightdeck").join("config.toml");
    let global = global_path.as_deref().map(&read_table).unwrap_or_default();
    let project = read_table(&project_path);
    let agent_keys: Vec<String> = p.state.config.agents.keys().cloned().collect();

    ui.config = Some(ConfigManager::new(
        p.name.clone(),
        global_path,
        project_path,
        global,
        project,
        agent_keys,
    ));
}

/// Handle a key while the configuration manager overlay is open (SPECS §8).
fn handle_config_key(
    key: KeyEvent,
    workspace: &mut Workspace,
    env: &Env,
    ui: &mut Ui,
) -> Result<()> {
    let Some(cm) = ui.config.as_mut() else {
        return Ok(());
    };
    match key.code {
        KeyCode::Esc => ui.config = None,
        KeyCode::Up => cm.select_prev(),
        KeyCode::Down => cm.select_next(),
        KeyCode::Tab => cm.switch_scope(),
        KeyCode::Char(' ') | KeyCode::Enter => cm.toggle_selected(),
        KeyCode::Char('c') if !key.modifiers.contains(KeyModifiers::CONTROL) => cm.clear_selected(),
        KeyCode::Char('s') if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            save_config_manager(workspace, env, ui)?;
        }
        KeyCode::Char('e') if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            if let Some(path) = cm.current_path() {
                ui.pending_editor = Some((workspace.active, path));
                ui.config = None;
            } else {
                ui.config = None;
                ui.message("No global config to edit (no home directory).");
            }
        }
        _ => {}
    }
    Ok(())
}

/// Write the configuration manager's dirty scopes to disk, then reload the
/// effective config for every open project (a global change affects them all).
fn save_config_manager(workspace: &mut Workspace, env: &Env, ui: &mut Ui) -> Result<()> {
    let outputs = match ui.config.as_ref() {
        Some(cm) => cm.outputs()?,
        None => return Ok(()),
    };
    for (path, contents) in &outputs {
        if let Some(parent) = path.parent() {
            if !env.fs.exists(parent) {
                env.fs.create_dir_all(parent)?;
            }
        }
        env.fs.write(path, contents)?;
    }
    if let Some(cm) = ui.config.as_mut() {
        cm.mark_saved();
    }
    reload_all_projects_config(workspace, env);
    Ok(())
}

/// Recompute and apply the effective config for every open project by layering
/// the (possibly just-edited) global base under each project's own overrides
/// (SPECS §8). Best-effort: a project whose config fails to load keeps its
/// current config.
fn reload_all_projects_config(workspace: &mut Workspace, env: &Env) {
    let global_path = global_config_path();
    for p in workspace.projects.iter_mut() {
        let project_path = p.git.root().join(".flightdeck").join("config.toml");
        let loaded = match &global_path {
            Some(gp) => load_layered_config(env.fs, gp, &project_path),
            None => load_config(env.fs, &project_path),
        };
        if let Ok(cfg) = loaded {
            p.state.reload_config(cfg);
        }
    }
}

/// The user's preferred editor: `$VISUAL`, then `$EDITOR`, then a platform
/// default (`notepad` on Windows, `vi` elsewhere).
fn preferred_editor() -> String {
    std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| if cfg!(windows) { "notepad" } else { "vi" }.to_string())
}

/// Suspend the TUI, open `path` in the user's editor, then re-initialise the
/// terminal (SPECS §8). The editor inherits the real terminal so full-screen
/// editors work; on return the alt screen, mouse capture, and bracketed paste
/// are re-enabled and the screen is cleared for a full redraw.
fn open_in_editor(terminal: &mut ratatui::DefaultTerminal, path: &Path) -> Result<()> {
    // Tear down our terminal ownership so the editor has a clean TTY.
    let _ = crossterm::execute!(
        std::io::stdout(),
        DisableBracketedPaste,
        DisableMouseCapture
    );
    ratatui::restore();

    let editor = preferred_editor();
    let status = std::process::Command::new(&editor).arg(path).status();

    // Re-initialise the terminal regardless of how the editor exited.
    *terminal = ratatui::try_init()
        .map_err(|e| FlightDeckError::Io(format!("failed to re-initialise terminal: {e}")))?;
    let _ = crossterm::execute!(std::io::stdout(), EnableMouseCapture, EnableBracketedPaste);
    if matches!(
        crossterm::terminal::supports_keyboard_enhancement(),
        Ok(true)
    ) {
        let _ = crossterm::execute!(
            std::io::stdout(),
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        );
    }
    let _ = terminal.clear();

    match status {
        Ok(_) => Ok(()),
        Err(e) => Err(FlightDeckError::Io(format!(
            "failed to launch editor '{editor}': {e}"
        ))),
    }
}

// ---------------------------------------------------------------------------
// PTY plumbing
// ---------------------------------------------------------------------------

/// Drain output from every terminal of every tab and feed each terminal's VT
/// parser so it can be rendered. Lifecycle status is handled separately by
/// backend hooks/plugins (SPECS §24).
fn drain_pty_output(state: &mut AppState, _now_ms: u64) {
    for tab in state.tabs.iter_mut() {
        // Primary: drain into the VT parser. Lifecycle status comes only from
        // backend hooks/plugins; PTY output includes echoed user keystrokes and
        // is deliberately not treated as agent activity.
        if let Some(primary) = tab.session.primary_mut() {
            if let Ok(bytes) = primary.session_mut().try_read_output() {
                if !bytes.is_empty() {
                    primary.process_output(&bytes);
                    // Unblock ConPTY / cursor-probing TUIs (Windows): reply to
                    // any `ESC[6n` so the child renders instead of stalling.
                    primary.answer_cursor_position_query(&bytes);
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
                        child.answer_cursor_position_query(&bytes);
                    }
                }
            }
        }
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

/// Forward externally-pasted text (one bracketed paste from the host terminal)
/// to the active PTY.
///
/// When the hosted application has enabled bracketed paste mode (DECSET 2004) —
/// as Claude Code, OpenCode, and modern shells do — the text is wrapped in the
/// `ESC [200~` / `ESC [201~` guards so the app treats it as one atomic paste
/// instead of executing each line. Apps that have *not* enabled the mode receive
/// the raw text, matching how a real terminal emulator forwards a paste. Either
/// way, newlines are normalised to carriage returns, the line break a terminal
/// delivers for Enter.
fn paste_text_into_active_pty(state: &mut AppState, text: &str) {
    let wants_bracket = state
        .selected()
        .and_then(|tab| tab.session.active())
        .is_some_and(|term| term.bracketed_paste());
    let bytes = encode_paste(text, wants_bracket);
    write_active_pty(state, &bytes);
}

/// Encode pasted text for the PTY: normalise newlines to carriage returns (the
/// line break a terminal sends for Enter) and, when `bracketed` is set, wrap the
/// payload in the `ESC [200~` / `ESC [201~` guards so a bracketed-paste-aware
/// app treats it as one atomic insert rather than executing line by line.
fn encode_paste(text: &str, bracketed: bool) -> Vec<u8> {
    let normalized = text.replace("\r\n", "\r").replace('\n', "\r");
    if bracketed {
        let mut bytes = Vec::with_capacity(normalized.len() + 12);
        bytes.extend_from_slice(b"\x1b[200~");
        bytes.extend_from_slice(normalized.as_bytes());
        bytes.extend_from_slice(b"\x1b[201~");
        bytes
    } else {
        normalized.into_bytes()
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

/// Resize a terminal only when its VT grid size actually differs, so this is
/// cheap to call every frame and never drops a live mouse selection spuriously.
fn resize_if_changed(term: &mut crate::terminal::session::Terminal, size: PtySize) {
    let (rows, cols) = term.screen().size();
    if rows != size.rows || cols != size.cols {
        let _ = term.resize(size);
    }
}

/// Size the *selected* tab's terminals to match the current layout: each
/// terminal gets its split-view column viewport when split view is on, or the
/// full terminal viewport otherwise. Only the selected tab is visible, so only
/// it needs syncing; other tabs self-heal the next time they are selected.
///
/// Idempotent via [`resize_if_changed`], so calling it every frame is cheap and
/// transparently handles every transition (toggle, tab switch, child add/close,
/// terminal resize) without threading resize calls through each command.
fn sync_terminal_sizes(state: &mut AppState, full: PtySize) {
    let Some(idx) = state.selected_tab else {
        return;
    };

    if state.split_view {
        let area = Rect::new(0, 0, full.cols, full.rows);
        let ml = crate::tui::layout::compute(area);
        let region = crate::tui::layout::split_region(&ml);
        let n = state.tabs[idx].session.child_count() + 1;
        let cols = crate::tui::layout::split_columns(region, n);
        if cols.is_empty() {
            return;
        }
        let col_size = |i: usize| PtySize {
            rows: cols[i].viewport.height.max(1),
            cols: cols[i].viewport.width.max(1),
        };
        // cols[0] → primary, cols[i + 1] → child i.
        if let Some(primary) = state.tabs[idx].session.primary_mut() {
            resize_if_changed(primary, col_size(0));
        }
        let child_count = state.tabs[idx].session.child_count();
        for c in 0..child_count {
            if c + 1 >= cols.len() {
                break;
            }
            let size = col_size(c + 1);
            if let Some(child) = state.tabs[idx].session.child_mut(c) {
                resize_if_changed(child, size);
            }
        }
    } else {
        // Normal view: every terminal of the selected tab fills the viewport.
        let size = state.pty_size;
        if let Some(primary) = state.tabs[idx].session.primary_mut() {
            resize_if_changed(primary, size);
        }
        let child_count = state.tabs[idx].session.child_count();
        for c in 0..child_count {
            if let Some(child) = state.tabs[idx].session.child_mut(c) {
                resize_if_changed(child, size);
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
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    fn make_real_agent(dir: &TempDir, key: &str) -> AgentDef {
        let path = dir.path().join(key);
        std::fs::write(&path, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            let mut perms = std::fs::metadata(&path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&path, perms).unwrap();
        }
        AgentDef {
            key: key.to_string(),
            display_name: key.to_string(),
            command: path.to_str().unwrap().to_string(),
            args: vec![],
            status_patterns: StatusPatterns::default(),
        }
    }

    #[test]
    fn project_progress_uses_explicit_agent_lifecycle_states() {
        use crate::contracts::InterpretedStatus;

        assert_eq!(
            project_status_flags([InterpretedStatus::Idle]),
            (false, false)
        );
        assert_eq!(
            project_status_flags([InterpretedStatus::Idle, InterpretedStatus::Working]),
            (false, true)
        );
        assert_eq!(
            project_status_flags([
                InterpretedStatus::Working,
                InterpretedStatus::WaitingForInput,
            ]),
            (true, true)
        );
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

    // --- prompt dialogs ---------------------------------------------------

    #[test]
    fn new_tab_dialog_shows_input_and_buttons() {
        let p = Prompt::NewTabName {
            buffer: "fix bug".to_string(),
            agent_key: None,
        };
        let dialog = prompt_dialog(&p);
        assert_eq!(dialog.input.as_deref(), Some("fix bug"));
        assert!(dialog.title.to_lowercase().contains("name"));
        // Create (Enter) + Cancel (Esc).
        assert!(dialog
            .buttons
            .iter()
            .any(|b| b.accel == DialogAccel::Enter && b.label == "Create"));
        assert!(dialog.buttons.iter().any(|b| b.accel == DialogAccel::Esc));
    }

    #[test]
    fn select_agent_dialog_lists_numbered_agents() {
        let p = Prompt::SelectAgent {
            agents: vec![
                ("claude".to_string(), "Claude Code".to_string()),
                ("opencode".to_string(), "OpenCode".to_string()),
            ],
        };
        let dialog = prompt_dialog(&p);
        assert!(dialog.title.to_lowercase().contains("pick"));
        assert_eq!(dialog.buttons[0].accel, DialogAccel::Char('1'));
        assert_eq!(dialog.buttons[0].label, "Claude Code");
        assert_eq!(dialog.buttons[1].accel, DialogAccel::Char('2'));
        assert_eq!(dialog.buttons[1].label, "OpenCode");
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
        let container = crate::testing::FakeContainerRuntime::new();
        let services = Services {
            git: &git,
            fs: &fs,
            pty: &pty,
            clock: &clock,
            container: &container,
        };

        // Pressing '1' picks Claude Code and advances to the name prompt.
        handle_prompt_key_project(
            KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE),
            &mut state,
            &services,
            &mut ui,
            0,
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
    fn close_prompt_dialog_lists_numbered_actions() {
        let p = Prompt::CloseTab {
            actions: vec![CloseAction::CtrlCPrimary, CloseAction::ForceTerminate],
        };
        let dialog = prompt_dialog(&p);
        assert_eq!(dialog.buttons[0].accel, DialogAccel::Char('1'));
        assert_eq!(dialog.buttons[1].accel, DialogAccel::Char('2'));
        assert_eq!(dialog.buttons[0].label, "Ctrl-C primary");
        // Plus a trailing Cancel button.
        assert!(dialog
            .buttons
            .last()
            .is_some_and(|b| b.accel == DialogAccel::Esc));
    }

    // --- effect → overlay mapping ----------------------------------------

    #[test]
    fn effect_message_becomes_dialog_overlay() {
        let mut ui = Ui::default();
        apply_effect_no_state(Effect::Message("hi".to_string()), &mut ui);
        match ui.render_overlay() {
            UiOverlay::Dialog(d) => {
                assert_eq!(d.title, "hi");
                // A notification carries a single OK button.
                assert_eq!(d.buttons.len(), 1);
                assert_eq!(d.buttons[0].accel, DialogAccel::Enter);
            }
            other => panic!("expected dialog overlay, got {other:?}"),
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
        apply_effect_no_state(Effect::AbandonWarning { dirty: true }, &mut ui);
        assert!(ui.prompt.is_some());
        assert!(matches!(
            ui.prompt.as_ref().unwrap().prompt,
            Prompt::AbandonConfirm { dirty: true }
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
        // The dialog title names both branches and warns about stopping the agent.
        assert!(pstate.dialog.title.contains("flightdeck/feat"));
        assert!(pstate.dialog.title.contains("main"));
        assert!(pstate.dialog.title.contains("stops the running agent"));
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

    // --- bracketed paste encoding -----------------------------------------

    #[test]
    fn encode_paste_wraps_when_app_enabled_bracketed_mode() {
        // A multi-line paste must reach a bracketed-paste-aware agent as one
        // atomic insert (guarded by ESC[200~/ESC[201~), not line-by-line, so it
        // does not execute the first line and queue the rest as prompts.
        let bytes = encode_paste("line one\nline two", true);
        assert_eq!(bytes, b"\x1b[200~line one\rline two\x1b[201~".to_vec());
    }

    #[test]
    fn encode_paste_passes_raw_when_app_disabled_bracketed_mode() {
        // Without bracketed paste mode the app gets the raw text, exactly as a
        // real terminal forwards a paste — no guards inserted.
        let bytes = encode_paste("line one\nline two", false);
        assert_eq!(bytes, b"line one\rline two".to_vec());
    }

    #[test]
    fn encode_paste_normalises_crlf_and_lf_to_cr() {
        // Both CRLF (Windows clipboard) and bare LF collapse to a single CR.
        assert_eq!(encode_paste("a\r\nb\nc", false), b"a\rb\rc".to_vec());
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

    #[test]
    fn echoed_prompt_input_does_not_mark_an_agent_working() {
        let dir = TempDir::new().unwrap();
        let agent = make_real_agent(&dir, "opencode");
        let mut config = config_with_agent(agent);
        config.notifications.enabled = true;

        let git = FakeGit::new().with_root("/repo").with_branches(["main"]);
        let fs = FakeFs::new();
        let pty = FakePty::new();
        let handle = pty.queue_session();
        let clock = FakeClock::default();
        let container = crate::testing::FakeContainerRuntime::new();
        let services = Services {
            git: &git,
            fs: &fs,
            pty: &pty,
            clock: &clock,
            container: &container,
        };
        let mut state = AppState::new(
            config,
            default_state("main"),
            "/repo",
            "/repo/.flightdeck/state.json",
        );
        state
            .dispatch(
                Command::NewAgentTab {
                    name: "Typing regression".to_string(),
                    agent_key: None,
                },
                &services,
            )
            .unwrap();

        handle.push_output(b"echoed user keystrokes".to_vec());
        drain_pty_output(&mut state, 1_000);

        assert_eq!(
            state.tabs[0].display_status(1_000).interpreted,
            crate::contracts::InterpretedStatus::Idle
        );
        assert!(state.take_finish_notifications(1_000).is_empty());
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
        let container = crate::testing::FakeContainerRuntime::new();
        let services = Services {
            git: &git,
            fs: &fs,
            pty: &pty,
            clock: &clock,
            container: &container,
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
        let container = crate::testing::FakeContainerRuntime::new();
        let services = Services {
            git: &git,
            fs: &fs,
            pty: &pty,
            clock: &clock,
            container: &container,
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
