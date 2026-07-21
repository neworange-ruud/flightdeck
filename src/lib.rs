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
pub mod hooks;
pub mod notify;
pub mod persistence;
pub mod remote;
pub mod runtime;
pub mod signals;
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
use crate::contracts::real::{RealClock, RealFs, SystemCommandRunner};
use crate::contracts::{
    Clock, CommandRunner, Config, ContainerRuntime, FileSystem, GitExecutor, ManualStatus,
    Notifier, ProcessState, PtyBackend, PtySize,
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
use crate::remote::client::RemoteHandle;
use crate::remote::commands::{
    build_index, encode_reply, first_task_decision, translate, CommandLedger, FirstTaskDecision,
    MainLoopAction, PendingFirstTask, ShellAction, Translation,
};
use crate::remote::identity::load_or_create_identity;
use crate::remote::pairing::{build_channel, PairingSession};
use crate::remote::shell::ShellManager;
use crate::remote::state::remote_state_path;
use crate::remote::{ProjectView, RemoteBridge, RemoteInbound, RemoteOutbound};
use crate::terminal::pty::PortablePtyBackend;
use crate::tui::config_manager::ConfigManager;
use crate::tui::input::{map_key_with_f2, KeyAction};
use crate::tui::palette::{CommandPalette, PaletteAction};
use crate::tui::render::{
    child_tab_label, dialog_hit, draw, draw_project_tab_bar, hit_test, project_tab_hit_test,
    ChildTarget, Dialog, DialogAccel, DialogButton, DialogHit, DialogListItem, GitStatusCache,
    HitTarget, ProjectHit, ProjectTabInfo, RemotePairing, UiOverlay,
};
use flightdeck_remote_protocol::{CommandAck, CommandOutcome, PairingId, ProjectId, SessionId};

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
    let command = SystemCommandRunner;
    let env = Env {
        fs: &fs,
        pty: &pty,
        clock: &clock,
        container: &container,
        command: &command,
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
    // the terminal supports it. Without it, terminals report modified keys like
    // Alt+Esc and Alt+Arrow as bare/ambiguous sequences, so the default
    // leave-focus binding and Alt-navigation shortcuts are unreliable. Users can
    // opt into F2 for leave-focus when their terminal lacks protocol support.
    // Best effort; popped on teardown only if we pushed it.
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
        let reserve =
            crate::tui::mode_style::border_enabled(&workspace.active_project().state.config.ui);
        let vp = viewport_pty_size(
            PtySize {
                rows: size.height,
                cols: size.width,
            },
            reserve,
        );
        for p in workspace.projects.iter_mut() {
            p.state.set_pty_size(vp);
        }
    }

    // Resume: start the primary agent for every recovered/loaded tab whose
    // worktree still exists (best effort) — for the ACTIVE (launched) project
    // ONLY. Other projects reopened from the workspace file are shown but their
    // agents are not auto-resumed; switching/opening one resumes it on demand
    // (see the open-project flow). Done here, after the viewport size is known,
    // rather than in `recover`/`AppState::new` which never spawn.
    {
        let active = workspace.active;
        let p = &mut workspace.projects[active];
        let services = env.services(&p.git);
        let _ = p.state.resume_agents(&services);
    }

    let notifier = SystemNotifier;
    let loop_result = event_loop(&mut terminal, &mut workspace, &env, &notifier);

    // CLEAN TEARDOWN (SPECS §25). Persist FIRST, before touching the terminal:
    // on a severed terminal (Konsole/window close closes stdin+stdout+stderr) the
    // terminal-restore step is worthless anyway, and it must never run ahead of
    // the save — otherwise a failed restore that writes to the dead stderr can
    // take the process down before the state is written.
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

    // Restore the terminal (best effort). Use `try_restore` — NOT `restore` —
    // because `ratatui::restore` `eprintln!`s on failure, and `eprintln!` itself
    // panics when stderr is gone (the exact Konsole-close case), which would
    // abort the process. `try_restore` just returns the error for us to ignore.
    if keyboard_enhanced {
        let _ = crossterm::execute!(std::io::stdout(), PopKeyboardEnhancementFlags);
    }
    let _ = crossterm::execute!(std::io::stdout(), DisableBracketedPaste);
    let _ = crossterm::execute!(std::io::stdout(), DisableMouseCapture);
    let _ = restore_terminal_title();
    let _ = ratatui::try_restore();
    // Show the cursor ourselves, then skip the `Terminal`'s own `Drop`. ratatui's
    // Drop `eprintln!`s when showing the cursor fails, and `eprintln!` panics when
    // stderr is also gone (Konsole close severs stdin+stdout+stderr) — which would
    // abort the process here, after we've already persisted. Our explicit call
    // restores the cursor on a live terminal; on a dead one the doomed write is
    // simply dropped, and `forget` prevents the aborting Drop.
    let _ = terminal.show_cursor();
    std::mem::forget(terminal);

    // Terminate every session so no orphaned child processes remain.
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
    /// The combined New Agent Session Tab form (SPECS §4, §22): pick the agent
    /// (radio, ↑/↓), type a branch name, and optionally toggle "run from base
    /// branch" (Tab), which disables the branch field and runs the agent — and
    /// any child shells — directly in the project root on the base branch (no
    /// worktree). Confirming (Enter) dispatches the async new-tab flow.
    NewAgentForm {
        /// `(key, display_name)` of each registered agent, in registry order.
        agents: Vec<(String, String)>,
        /// Index into `agents` of the highlighted radio option.
        selected: usize,
        /// The branch/tab name being typed. Ignored when `run_on_base`.
        branch: String,
        /// When true, run on the base branch in the project root (no worktree);
        /// the branch field is disabled.
        run_on_base: bool,
        /// The base branch name, shown when `run_on_base` is on.
        base_branch: String,
    },
    /// Pick which agent backend to spawn as an additional agent in the current
    /// session's worktree (the "+ agent" flow). A number key selects one and
    /// dispatches `NewAgentTerminal`. Holds each agent's `(key, display_name)`.
    SelectChildAgent { agents: Vec<(String, String)> },
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
    /// Confirm unpairing the phone (FlightDeck Remote). On confirm the event
    /// loop forgets the pairing and reverts to the passthrough sealer.
    UnpairConfirm,
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
    /// Set by the "Remote: Pair Phone" palette action; the event loop (which owns
    /// the relay channels + pairing session) starts the pairing offer next tick.
    pending_pair: bool,
    /// Set by confirming "Remote: Unpair"; the event loop forgets the pairing.
    pending_unpair: bool,
    /// Whether a phone is currently paired (FlightDeck Remote). Refreshed each
    /// tick from the live relay bridge + the persisted startup pairing, and read
    /// when opening the command palette so "Pair Phone" / "Unpair Phone" are
    /// gated by the actual pairing state (a `RemoteBridge` this UI cannot borrow
    /// directly). `false` whenever remote is disabled.
    remote_paired: bool,
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
    command: &'a dyn CommandRunner,
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
            command: self.command,
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

/// Resume the *active* project's recovered agents on demand (idempotent —
/// [`AppState::resume_agents`] only starts tabs whose primary isn't already
/// running). Called after every project switch: startup resumes only the
/// launched project's agents, so a background project reopened from the
/// workspace file has unspawned tabs until the user first switches to it —
/// without this, switching to one shows "(terminal starting…)" forever.
fn resume_active_project_agents(workspace: &mut Workspace, env: &Env) {
    let active = workspace.active;
    let p = &mut workspace.projects[active];
    let services = env.services(&p.git);
    let _ = p.state.resume_agents(&services);
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

/// One decision of the main loop's input step: what to do next.
#[derive(Debug, PartialEq, Eq)]
enum LoopStep {
    /// Shut down cleanly (a shutdown signal fired, or the input source is gone).
    Shutdown,
    /// Handle this input event.
    Input(Event),
    /// Nothing happened this tick; run the per-tick work and loop again.
    Idle,
}

/// Decide the next loop step from the shutdown flag and the input channel,
/// waiting at most `timeout` for an event.
///
/// Crucially, this NEVER blocks longer than `timeout`, so the shutdown flag is
/// always observed promptly — even when the controlling terminal has been
/// severed (Konsole/window close), where crossterm's own `event::poll`/`read`
/// busy-loops on EOF and never returns. The blocking `event::read` runs on a
/// separate thread feeding `rx`; if that thread ends (channel disconnected) we
/// also shut down. The flag is checked both before and after the wait so a
/// signal that arrives *during* the wait is caught on this same tick.
fn next_loop_step(
    shutdown: &std::sync::atomic::AtomicBool,
    rx: &Receiver<Event>,
    timeout: Duration,
) -> LoopStep {
    use std::sync::atomic::Ordering;
    use std::sync::mpsc::RecvTimeoutError;

    if shutdown.load(Ordering::Relaxed) {
        return LoopStep::Shutdown;
    }
    match rx.recv_timeout(timeout) {
        Ok(event) => LoopStep::Input(event),
        Err(RecvTimeoutError::Timeout) => {
            if shutdown.load(Ordering::Relaxed) {
                LoopStep::Shutdown
            } else {
                LoopStep::Idle
            }
        }
        // The input reader thread exited (e.g. terminal gone) → shut down.
        Err(RecvTimeoutError::Disconnected) => LoopStep::Shutdown,
    }
}

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

    // Trap SIGTERM/SIGINT/SIGHUP: on an external signal we break out of the loop
    // so the caller's clean teardown (persist `state.json` + terminate agents)
    // runs, instead of the process dying without saving or cleaning up.
    let shutdown = crate::signals::install_shutdown_flag();

    // Read terminal input on a dedicated thread that feeds a channel. The main
    // loop then waits on the channel with a timeout (`next_loop_step`) instead of
    // calling crossterm's `event::poll`/`read` directly. This decouples the loop
    // from crossterm's blocking behaviour: when the controlling terminal is
    // severed (Konsole/window close) crossterm busy-loops on EOF and never
    // returns, but the main loop still wakes every `POLL_TIMEOUT`, sees the
    // shutdown flag (SIGHUP), and exits cleanly so teardown can persist + stop
    // agents. The reader thread is detached; it ends with the process.
    let (input_tx, input_rx) = std::sync::mpsc::channel::<Event>();
    std::thread::spawn(move || {
        // `event::read` blocks until an event (or busy-loops on a dead tty); the
        // main loop no longer depends on it returning. Stop if the receiver is
        // gone or crossterm reports a hard error.
        while let Ok(event) = event::read() {
            if input_tx.send(event).is_err() {
                break;
            }
        }
    });

    // Home dir for locating agent session stores (used to pin each tab's resume
    // session id). Resolved once; `None` disables pinning.
    let store_home = crate::app::state::user_home();

    // FlightDeck Remote (optional): a long-lived relay-client thread, mirroring
    // the update-check thread idiom above. Off by default — when disabled this
    // spawns nothing and the channels stay idle, so behaviour is unchanged. The
    // `_remote_out_tx` end is retained (unused for now) because the app→relay
    // bridge that feeds it is a later task; keeping the channel here fixes the
    // wiring shape so that task is purely additive.
    let (remote_in_tx, remote_in_rx) = std::sync::mpsc::channel::<RemoteInbound>();
    let (remote_out_tx, remote_out_rx) = std::sync::mpsc::channel::<RemoteOutbound>();
    let remote_setup = start_remote(env, workspace, remote_in_tx, remote_out_rx);
    // The outbound feed bridge exists only while the relay thread does. It builds
    // the phone-facing snapshots/deltas/transcript/events each tick and seals
    // them. A passthrough sealer is the default; when an already-established
    // pairing exists, the real E2E channel is installed right away (spec §7.1).
    // When remote is disabled this stays `None`, so every tee/tick below is a
    // cheap no-op and behaviour is bit-for-bit unchanged.
    let mut remote_bridge: Option<RemoteBridge> = remote_setup
        .as_ref()
        .map(|_| RemoteBridge::passthrough(now0 + crate::app::state::NOTIFY_STARTUP_GRACE_MS));
    // Locate agent session files (per worktree) for transcript reconstruction
    // (remote-control-72k). Uses the same home the resume machinery uses.
    if let Some(b) = remote_bridge.as_mut() {
        b.set_transcript_home(store_home.clone());
    }
    if let (Some(b), Some(setup)) = (remote_bridge.as_mut(), remote_setup.as_ref()) {
        if let Some(est) = &setup.established {
            if let Ok((seal, open)) = build_channel(
                &setup.identity_scalar,
                &est.peer_ka_b64,
                est.pairing_id.as_str(),
                &est.claim_token,
            ) {
                b.install_channel(seal, open, est.last_sent_seq);
            }
        }
    }
    // The desktop pairing surface (Settings → Remote overlay). `Some` only while
    // the QR/code overlay is on screen.
    let mut pairing_session: Option<PairingSession> = None;
    // Test / E2E seam (read once at startup): when `FLIGHTDECK_REMOTE_AUTOPAIR`
    // holds a 4-digit value and remote is enabled, the desktop offers pairing
    // non-interactively on the first tick using that fixed code, so an automated
    // harness gets a deterministic claim token instead of a random one plus a
    // keypress. `None` in every normal run, so behaviour is unchanged.
    let autopair_hint: Option<String> = std::env::var("FLIGHTDECK_REMOTE_AUTOPAIR")
        .ok()
        .filter(|v| v.len() == 4 && v.bytes().all(|b| b.is_ascii_digit()));
    // Inbound command-bridge state: the idempotency ledger and the first tasks
    // of phone-created sessions awaiting a ready agent. Only ever touched when
    // the remote bridge exists, so disabled-remote behaviour is unchanged.
    let mut remote_ledger = CommandLedger::new();
    let mut remote_first_tasks: Vec<PendingFirstTask> = Vec::new();
    // Whether a phone pairing was persisted at startup and has not been
    // forgotten this session. `RemoteBridge::is_paired()` only turns true once
    // the phone reconnects, so this keeps "Unpair Phone" available (and "Pair
    // Phone" gated) for a configured-but-currently-absent phone. Cleared on
    // unpair and on a relay-side pairing rejection.
    let mut remote_has_persisted_pairing = remote_setup
        .as_ref()
        .map(|s| s.established.is_some())
        .unwrap_or(false);

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

            drain_pty_output(&mut p.state, now_ms, |sid, which, bytes| {
                if let Some(b) = remote_bridge.as_mut() {
                    // Primary (None) bytes no longer build the transcript — it is
                    // reconstructed from the agent's session file each tick (see
                    // `RemoteBridge::sync_transcript`, remote-control-72k), because
                    // full-screen agents paint the alt-screen and emit no lines.
                    // Child bytes still stream to the phone iff that child backs
                    // the session's live remote shell.
                    if let Some(child_index) = which {
                        b.shell_pump(sid, child_index, bytes);
                    }
                }
            });

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
                // Pin each freshly-launched agent's session id for later resume
                // (cheap unless a tab is still awaiting its session file).
                if let Some(home) = &store_home {
                    p.state.pin_resumable_sessions(home, &services);
                }
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

        // --- Drain relay-client events (link state, envelopes, presence) into
        //     the outbound bridge, then push this tick's feed. Inbound is
        //     handled before the tick so a just-arrived `request_snapshot` /
        //     pairing is reflected in what we send. Command envelopes beyond
        //     snapshot/transcript requests are queued for the command-bridge
        //     task via `RemoteBridge::take_pending_commands`. ---
        if let Some(b) = remote_bridge.as_mut() {
            let identity_scalar = remote_setup
                .as_ref()
                .map(|s| s.identity_scalar.as_slice())
                .unwrap_or(&[]);
            while let Ok(msg) = remote_in_rx.try_recv() {
                // Drive the pairing overlay + E2E go-live off the pairing frames.
                match &msg {
                    RemoteInbound::PairingOffered {
                        pairing_id,
                        claim_token,
                        expires_at_ms,
                    } => {
                        if let Some(ps) = pairing_session.as_mut() {
                            ps.on_offered(pairing_id.clone(), claim_token.clone(), *expires_at_ms);
                        }
                    }
                    RemoteInbound::PairingClaimed {
                        pairing_id,
                        peer_key_agreement_public_key,
                        ..
                    } => {
                        if let Some(ps) = pairing_session.as_mut() {
                            if ps.on_claimed(
                                pairing_id.clone(),
                                peer_key_agreement_public_key.clone(),
                            ) {
                                // The instant a phone joins: derive the real
                                // channel and swap it in for the passthrough.
                                if let Ok((_pid, seal, open)) = ps.derive_channel(identity_scalar) {
                                    b.install_channel(seal, open, 0);
                                }
                            }
                        }
                    }
                    RemoteInbound::PairingRejected { .. } => {
                        // The relay no longer recognizes our pairing; the client
                        // dropped the stale record and will re-offer. Give the
                        // user a clear, actionable state instead of a silent,
                        // endless "reconnecting" (remote-control-1jy).
                        pairing_session = None;
                        remote_has_persisted_pairing = false;
                        if matches!(ui.overlay, UiOverlay::Remote(_)) {
                            ui.overlay = UiOverlay::None;
                        }
                        ui.message(
                            "Phone pairing is no longer recognized by the relay. \
                             Open Settings → Remote to pair again.",
                        );
                    }
                    RemoteInbound::PairingRevoked { .. } => {
                        // The phone unpaired this Mac (spec §10.2). The client
                        // already dropped the pairing; clear the overlay/session
                        // and let the user know they can pair again.
                        pairing_session = None;
                        remote_has_persisted_pairing = false;
                        if matches!(ui.overlay, UiOverlay::Remote(_)) {
                            ui.overlay = UiOverlay::None;
                        }
                        ui.message(
                            "Your phone unpaired this Mac. \
                             Open Settings → Remote to pair again.",
                        );
                    }
                    _ => {}
                }
                b.handle_inbound(msg);
            }
            {
                let views: Vec<ProjectView> = workspace
                    .projects
                    .iter()
                    .map(|p| ProjectView {
                        id: ProjectId::new(p.name.clone()),
                        name: &p.name,
                        state: &p.state,
                        cache: &p.cache,
                    })
                    .collect();
                b.tick(&views, now_ms, &mut |out| {
                    let _ = remote_out_tx.send(out);
                });
            }
            // Inbound phone commands queued by the bridge: idempotency-check,
            // translate, execute on this (main) thread through the existing
            // Command/PTY paths, and ack each with its actual outcome.
            service_remote_commands(
                b,
                &mut remote_ledger,
                &mut remote_first_tasks,
                workspace,
                env,
                now_ms,
                &mut |out| {
                    let _ = remote_out_tx.send(out);
                },
            );
        } else {
            // Remote disabled: drain (and drop) so the channel never fills.
            while remote_in_rx.try_recv().is_ok() {}
        }

        // --- Test / E2E seam: on the first tick, auto-offer pairing with the
        //     fixed `FLIGHTDECK_REMOTE_AUTOPAIR` code when set and remote is
        //     enabled. This just requests the same offer the palette action does. ---
        if tick == 0 && autopair_hint.is_some() && remote_setup.is_some() {
            ui.pending_pair = true;
        }

        // A confirmed unpair (handled by `drive_pairing_overlay` below) forgets
        // the pairing, so drop the persisted flag before it is consumed.
        if ui.pending_unpair {
            remote_has_persisted_pairing = false;
        }
        // Refresh the palette's pairing gate: paired iff the live bridge has an
        // active pairing or a persisted one is still configured this session.
        ui.remote_paired = remote_bridge
            .as_ref()
            .map(|b| b.is_paired())
            .unwrap_or(false)
            || remote_has_persisted_pairing;

        // --- Desktop pairing surface (Settings → Remote): start an offer, keep
        //     the overlay in sync with the pairing session, and handle unpair. ---
        drive_pairing_overlay(
            &mut ui,
            &mut pairing_session,
            remote_bridge.as_mut(),
            remote_setup.as_ref(),
            &remote_out_tx,
            autopair_hint.as_deref(),
            now_ms,
        );

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
                let ml = crate::tui::layout::compute(
                    area,
                    crate::tui::mode_style::border_enabled(&p.state.config.ui),
                );
                draw_project_tab_bar(frame, ml.project_tabs, &infos, active_idx, now_ms);
                draw(frame, &p.state, &p.cache, &overlay, now_ms);
            })
            .map_err(|e| FlightDeckError::Io(format!("render failed: {e}")))?;

        // --- Wait for input via the reader thread (short timeout so PTY output
        //     keeps flowing and the shutdown flag is observed promptly). ---
        let event = match next_loop_step(&shutdown, &input_rx, POLL_TIMEOUT) {
            LoopStep::Shutdown => break,
            LoopStep::Idle => continue,
            LoopStep::Input(event) => event,
        };

        match event {
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
                let reserve = crate::tui::mode_style::border_enabled(
                    &workspace.active_project().state.config.ui,
                );
                let size = viewport_pty_size(PtySize { rows, cols }, reserve);
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

    // Tear down the relay client (best-effort join). A dropped handle also
    // signals the thread, so an early `?` return above still winds it down.
    if let Some(setup) = remote_setup {
        setup.handle.stop();
    }

    Ok(())
}

/// An already-established pairing found in `remote.json` at startup: everything
/// needed to reconstruct its live E2E channel before the phone reconnects.
struct EstablishedPairing {
    pairing_id: PairingId,
    peer_ka_b64: String,
    claim_token: String,
    last_sent_seq: u64,
}

/// The result of starting FlightDeck Remote: the client thread plus the bits the
/// event loop needs to drive the pairing surface and bring E2E live.
struct RemoteSetup {
    handle: RemoteHandle,
    /// This device's identity private scalar, reused as the key-agreement key
    /// (spec §7.1) to derive the E2E channel on pairing/startup.
    identity_scalar: Vec<u8>,
    /// The effective relay URL to embed in the pairing QR.
    relay_url: String,
    /// An already-paired pairing to bring live immediately, if any.
    established: Option<EstablishedPairing>,
}

/// Construct the FlightDeck Remote client thread when `[remote]` is enabled and
/// a relay URL is configured. Returns `None` (spawning nothing) when disabled,
/// when no relay URL is set, or when the per-user identity file cannot be
/// located/created — the app runs exactly as before in every such case.
fn start_remote(
    env: &Env,
    workspace: &Workspace,
    inbound_tx: Sender<RemoteInbound>,
    outbound_rx: Receiver<RemoteOutbound>,
) -> Option<RemoteSetup> {
    let cfg = workspace.active_project().state.config.remote.clone();
    if !cfg.enabled || cfg.relay_url.is_empty() {
        return None;
    }
    let path = remote_state_path()?;
    let (identity, state) = load_or_create_identity(env.fs, &path).ok()?;
    let identity_scalar = identity.private_key_bytes();
    // A per-device relay URL override wins over config (matches the client).
    let relay_url = match &state.relay_url {
        Some(u) if !u.is_empty() => u.clone(),
        _ => cfg.relay_url.clone(),
    };
    // The first already-established pairing (single-Mac UI in v1) is brought
    // live at startup so a reconnecting phone gets real ciphertext, not the
    // passthrough sealer (spec §7.1).
    let established =
        state
            .pairings
            .iter()
            .find(|p| p.is_e2e_ready())
            .map(|p| EstablishedPairing {
                pairing_id: PairingId::new(p.pairing_id.clone()),
                peer_ka_b64: p.peer_key_agreement_public_key.clone().unwrap_or_default(),
                claim_token: p.claim_token.clone().unwrap_or_default(),
                last_sent_seq: p.last_sent_seq,
            });
    let handle = RemoteHandle::start(cfg, identity, inbound_tx, outbound_rx);
    Some(RemoteSetup {
        handle,
        identity_scalar,
        relay_url,
        established,
    })
}

/// Per-tick driver for the desktop pairing overlay: start an offer when the
/// palette asked, keep the on-screen overlay in sync with the pairing session
/// (countdown, status), handle unpair, and drop the session once the overlay is
/// dismissed.
fn drive_pairing_overlay(
    ui: &mut Ui,
    pairing_session: &mut Option<PairingSession>,
    bridge: Option<&mut RemoteBridge>,
    setup: Option<&RemoteSetup>,
    out_tx: &Sender<RemoteOutbound>,
    autopair_hint: Option<&str>,
    now_ms: u64,
) {
    if ui.pending_pair {
        ui.pending_pair = false;
        match setup {
            Some(s) => {
                // The test / E2E seam supplies a fixed code so the claim token is
                // deterministic; interactive pairing uses a fresh random code.
                let session = match autopair_hint {
                    Some(hint) => PairingSession::begin_with_hint(s.relay_url.clone(), hint),
                    None => PairingSession::begin(s.relay_url.clone()),
                };
                let _ = out_tx.send(RemoteOutbound::RequestPairing {
                    claim_token_hint: Some(session.hint().to_string()),
                });
                ui.overlay = UiOverlay::Remote(remote_pairing_view(&session, now_ms));
                *pairing_session = Some(session);
            }
            None => ui.message(
                "FlightDeck Remote is disabled — enable it in configuration to pair a phone.",
            ),
        }
    }

    if ui.pending_unpair {
        ui.pending_unpair = false;
        // Forget any pairing we know about (single-Mac UI in v1): the one loaded
        // at startup and any established this session. The client drops each from
        // persisted state; the bridge reverts to the passthrough sealer.
        if let Some(s) = setup {
            if let Some(est) = &s.established {
                let _ = out_tx.send(RemoteOutbound::Unpair {
                    pairing_id: est.pairing_id.clone(),
                });
            }
        }
        if let Some(pid) = pairing_session.as_ref().and_then(|ps| ps.pairing_id()) {
            let _ = out_tx.send(RemoteOutbound::Unpair {
                pairing_id: pid.clone(),
            });
        }
        if let Some(b) = bridge {
            b.reset_to_passthrough();
        }
        *pairing_session = None;
        if matches!(ui.overlay, UiOverlay::Remote(_)) {
            ui.overlay = UiOverlay::None;
        }
        ui.message("Phone unpaired.");
        return;
    }

    // Keep the overlay live while a session runs; drop it once dismissed.
    if let Some(ps) = pairing_session.as_ref() {
        if matches!(ui.overlay, UiOverlay::Remote(_)) {
            ui.overlay = UiOverlay::Remote(remote_pairing_view(ps, now_ms));
        } else {
            *pairing_session = None;
        }
    }
}

/// Build the render-ready [`RemotePairing`] snapshot from the pairing session.
fn remote_pairing_view(session: &PairingSession, now_ms: u64) -> RemotePairing {
    use crate::remote::pairing::{qr_art, PairingPhase};
    match session.phase() {
        PairingPhase::Idle | PairingPhase::Offering => RemotePairing {
            status_line: "Requesting a pairing code from the relay…".to_string(),
            ..RemotePairing::default()
        },
        PairingPhase::Displaying {
            code, qr_payload, ..
        } => {
            let (qr_rows, qr_width) = qr_art(qr_payload)
                .map(|a| (a.rows, a.width))
                .unwrap_or_default();
            RemotePairing {
                status_line: "Scan the QR or type the code on your phone — waiting…".to_string(),
                code: Some(code.clone()),
                qr_rows,
                qr_width,
                seconds_remaining: session.seconds_remaining(now_ms as i64),
                done: false,
                failed: false,
            }
        }
        PairingPhase::Established { .. } => RemotePairing {
            status_line: "Phone connected — paired. End-to-end encrypted.".to_string(),
            done: true,
            ..RemotePairing::default()
        },
        PairingPhase::Failed { message } => RemotePairing {
            status_line: message.clone(),
            failed: true,
            ..RemotePairing::default()
        },
    }
}

// ---------------------------------------------------------------------------
// FlightDeck Remote: inbound command bridge (phone → desktop)
// ---------------------------------------------------------------------------

/// Drain the phone commands the outbound bridge queued this tick, run each
/// through the idempotency ledger and the pure translator
/// ([`crate::remote::commands`]), execute the translation on the main thread
/// — a PTY write to the target session's primary terminal, an
/// [`AppState::dispatch`] through the existing safety-guarded [`Command`]
/// layer, or the two-phase new-tab flow — and ack every command with its
/// **actual** outcome. Also delivers queued first tasks of phone-created
/// sessions once their agent is ready.
///
/// Never called when remote is disabled (the bridge is `None`), so disabled
/// behaviour is bit-for-bit unchanged.
#[allow(clippy::too_many_arguments)]
fn service_remote_commands(
    bridge: &mut RemoteBridge,
    ledger: &mut CommandLedger,
    first_tasks: &mut Vec<PendingFirstTask>,
    workspace: &mut Workspace,
    env: &Env,
    now_ms: u64,
    send: &mut dyn FnMut(RemoteOutbound),
) {
    // Flush any deferred keystrokes now due — e.g. Claude's multi-select submit
    // Enter, held back until the Tab-driven Confirm-tab switch has rendered so
    // the Ink TUI does not drop it (remote-control-dc9). Re-resolve the tab
    // since indices may have shifted during the delay.
    for (session_id, bytes) in bridge.take_due_deferred_pty(now_ms) {
        if let Some((pi, ti)) = resolve_primary_tab(workspace, &session_id) {
            if let Some(p) = workspace.projects.get_mut(pi) {
                let _ = write_primary_pty(&mut p.state, ti, &bytes);
            }
        }
    }

    for cmd in bridge.take_pending_commands() {
        // Idempotency: a retransmitted command id is acked, never re-applied.
        if let Some(ack) = ledger.duplicate_ack(&cmd.command_id) {
            bridge.send_ack(ack, now_ms as i64, send);
            continue;
        }
        // A fresh index per command: an earlier command in this batch may have
        // closed a tab and shifted indices.
        let index = {
            let views: Vec<ProjectView> = workspace
                .projects
                .iter()
                .map(|p| ProjectView {
                    id: ProjectId::new(p.name.clone()),
                    name: &p.name,
                    state: &p.state,
                    cache: &p.cache,
                })
                .collect();
            build_index(&views, now_ms, &|sid| bridge.pending_prompt_id(sid))
        };
        let translation = translate(&cmd.body, &index);
        let (outcome, message) = match translation {
            // Two-phase keystroke write: send the toggles + Tab now, and defer
            // the submit Enter so it lands after Claude's Confirm tab renders
            // (remote-control-dc9). The deferred write is flushed on a later tick.
            Translation::PtyInputThenDeferred {
                project,
                tab,
                session_id,
                immediate,
                deferred,
                delay_ms,
            } => match workspace.projects.get_mut(project) {
                Some(p) => {
                    if write_primary_pty(&mut p.state, tab, &immediate) {
                        bridge.enqueue_deferred_pty(session_id, now_ms + delay_ms, deferred);
                        (CommandOutcome::Applied, None)
                    } else {
                        (
                            CommandOutcome::Failed,
                            Some("could not write to the agent terminal".to_string()),
                        )
                    }
                }
                None => remote_target_gone(),
            },
            other => execute_remote_translation(
                other,
                workspace,
                env,
                now_ms,
                first_tasks,
                bridge.shells_mut(),
            ),
        };
        ledger.record(cmd.command_id.clone(), outcome, message.clone());
        bridge.send_ack(
            CommandAck {
                command_id: cmd.command_id,
                outcome,
                message,
            },
            now_ms as i64,
            send,
        );
    }
    deliver_first_tasks(first_tasks, workspace, now_ms);
    // Report any remote shell whose process has exited (flushed next tick).
    poll_remote_shell_exits(bridge, workspace);
}

/// Poll each live remote shell's backing child terminal and report a one-shot
/// `exited` event when its process has stopped. The event is queued on the
/// shell manager and sealed/sent by the next [`RemoteBridge::tick`].
fn poll_remote_shell_exits(bridge: &mut RemoteBridge, workspace: &Workspace) {
    for (session_id, child_index) in bridge.shells().active_shells() {
        // Resolve the session's tab across all open projects.
        let child_state = workspace.projects.iter().find_map(|p| {
            p.state
                .tabs
                .iter()
                .find(|t| t.meta.id == session_id.as_str())
                .and_then(|t| t.session.child(child_index))
                .map(|c| c.process_state())
        });
        match child_state {
            Some(ProcessState::Exited(code)) => {
                bridge
                    .shells_mut()
                    .mark_exit(&session_id, child_index, Some(code));
            }
            // Stopped (or the child vanished): treat as an exit with no code so
            // the phone learns the shell is dead rather than hanging forever.
            Some(ProcessState::Stopped) | None => {
                bridge
                    .shells_mut()
                    .mark_exit(&session_id, child_index, None);
            }
            _ => {}
        }
    }
}

/// Resolve a session id to its `(project index, tab index)` in the live
/// workspace, or `None` if the session/tab no longer exists. Used to place a
/// deferred PTY write on the right tab even if indices shifted during the delay.
fn resolve_primary_tab(workspace: &Workspace, session_id: &SessionId) -> Option<(usize, usize)> {
    workspace.projects.iter().enumerate().find_map(|(pi, p)| {
        p.state
            .tabs
            .iter()
            .position(|t| t.meta.id == session_id.as_str())
            .map(|ti| (pi, ti))
    })
}

/// The ack for a session/project that vanished between translation and
/// execution (possible only if an earlier command in the same batch removed it).
fn remote_target_gone() -> (CommandOutcome, Option<String>) {
    (
        CommandOutcome::Failed,
        Some("the target session no longer exists".to_string()),
    )
}

/// Execute one [`Translation`] and report the honest ack outcome.
#[allow(clippy::too_many_arguments)]
fn execute_remote_translation(
    translation: Translation,
    workspace: &mut Workspace,
    env: &Env,
    now_ms: u64,
    first_tasks: &mut Vec<PendingFirstTask>,
    shells: &mut ShellManager,
) -> (CommandOutcome, Option<String>) {
    match translation {
        Translation::Reject { reason } => (CommandOutcome::Rejected, Some(reason)),

        Translation::Shell {
            project,
            tab,
            session_id,
            action,
        } => execute_shell_action(shells, workspace, env, project, tab, &session_id, action),

        // PtyInputThenDeferred is intercepted in `service_remote_commands` (which
        // owns the deferred-write queue). If it ever reaches the generic executor
        // — e.g. a direct test call — degrade to writing the immediate part; the
        // trailing submit Enter is dropped in that path.
        Translation::PtyInputThenDeferred {
            project,
            tab,
            immediate,
            ..
        } => {
            let Some(p) = workspace.projects.get_mut(project) else {
                return remote_target_gone();
            };
            if write_primary_pty(&mut p.state, tab, &immediate) {
                (CommandOutcome::Applied, None)
            } else {
                (
                    CommandOutcome::Failed,
                    Some("could not write to the agent terminal".to_string()),
                )
            }
        }

        Translation::PtyInput {
            project,
            tab,
            bytes,
        } => {
            let Some(p) = workspace.projects.get_mut(project) else {
                return remote_target_gone();
            };
            if write_primary_pty(&mut p.state, tab, &bytes) {
                (CommandOutcome::Applied, None)
            } else {
                (
                    CommandOutcome::Failed,
                    Some("could not write to the agent terminal".to_string()),
                )
            }
        }

        Translation::Dispatch {
            project,
            tab,
            command,
        } => {
            let Some(p) = workspace.projects.get_mut(project) else {
                return remote_target_gone();
            };
            match command {
                // Merge-back mirrors the TUI's two-phase flow so the ack is
                // honest: phase 1 (unconfirmed, read-only) surfaces the
                // dirty-base warning / precondition refusals as rejections;
                // only a MergeConfirm proceeds to the confirmed merge.
                Command::FinishLocalMerge { .. } => dispatch_remote_merge_back(p, env, tab),
                // A confirmed abandon that returns a Warning did NOT remove
                // the worktree (the session could not be stopped) — that is a
                // failure, not an applied-with-caveat.
                Command::AbandonWorktree { confirm: true } => {
                    match dispatch_remote_effect(p, env, tab, command) {
                        None => remote_target_gone(),
                        Some(Ok(Effect::Warning(w))) => (CommandOutcome::Failed, Some(w)),
                        Some(result) => fold_remote_effect(result),
                    }
                }
                _ => dispatch_remote_command(p, env, tab, command),
            }
        }

        Translation::NeedsMainLoop(MainLoopAction::NewAgent {
            project,
            name,
            agent_key,
            first_task,
        }) => {
            let Some(p) = workspace.projects.get_mut(project) else {
                return remote_target_gone();
            };
            // Mirror the desktop palette flow: reserve the placeholder tab
            // (cheap, validation-first) and queue the slow `git worktree add`
            // on the project's background worker. Keep the desktop user's
            // on-screen selection where it was — a phone-initiated create
            // must not yank the TUI to the new tab.
            let prev_selected = p.state.selected().map(|t| t.meta.id.clone());
            let begun = {
                let services = env.services(&p.git);
                p.state
                    .begin_new_agent_tab(&name, Some(&agent_key), &services)
            };
            if let Some(prev) = prev_selected {
                if let Some(idx) = p.state.tabs.iter().position(|t| t.meta.id == prev) {
                    p.state.selected_tab = Some(idx);
                }
            }
            match begun {
                Ok(job) => {
                    let branch = job.branch.clone();
                    let tab_id = job.tab_id.clone();
                    spawn_worktree_job(job, &p.git, &p.git_lock, &p.create_tx);
                    if !first_task.trim().is_empty() {
                        first_tasks.push(PendingFirstTask {
                            tab_id,
                            text: first_task,
                            queued_at_ms: now_ms,
                        });
                    }
                    (
                        CommandOutcome::Accepted,
                        Some(format!(
                            "Creating worktree for {branch}; the first task will \
                             be sent when the agent is ready."
                        )),
                    )
                }
                Err(e) => (CommandOutcome::Failed, Some(e.to_string())),
            }
        }
    }
}

/// Dispatch an app [`Command`] against a specific tab on behalf of the phone:
/// temporarily select the target (dispatch acts on the selection), run it
/// through [`AppState::dispatch`] — inheriting every safety guard — then
/// restore the user's selection by id (indices may have shifted if the
/// command removed a tab). Returns the raw dispatch result for the caller to
/// fold into an ack, or `None` when the tab is already gone.
fn dispatch_remote_effect(
    p: &mut Project,
    env: &Env,
    tab: usize,
    command: Command,
) -> Option<Result<Effect>> {
    if tab >= p.state.tabs.len() {
        return None;
    }
    let prev_selected = p.state.selected().map(|t| t.meta.id.clone());
    p.state.selected_tab = Some(tab);
    let result = {
        let services = env.services(&p.git);
        p.state.dispatch(command, &services)
    };
    if let Some(prev) = prev_selected {
        if let Some(idx) = p.state.tabs.iter().position(|t| t.meta.id == prev) {
            p.state.selected_tab = Some(idx);
        }
        // else: the previously selected tab is the one that was removed;
        // dispatch already fixed the selection to a sensible neighbour.
    }
    Some(result)
}

/// Fold a dispatch result into an honest ack outcome instead of surfacing it
/// as desktop UI. `Warning` maps to applied-with-caveat — callers whose
/// command treats a warning as "nothing happened" (e.g. merge-back's
/// dirty-base warning) must intercept it before folding.
fn fold_remote_effect(result: Result<Effect>) -> (CommandOutcome, Option<String>) {
    match result {
        Ok(Effect::Refused(reason)) => (CommandOutcome::Rejected, Some(reason)),
        Ok(Effect::Message(m)) => (CommandOutcome::Applied, Some(m)),
        Ok(Effect::Warning(w)) => (CommandOutcome::Applied, Some(w)),
        Ok(_) => (CommandOutcome::Applied, None),
        Err(e) => (CommandOutcome::Failed, Some(e.to_string())),
    }
}

/// [`dispatch_remote_effect`] + [`fold_remote_effect`], for commands whose
/// effects need no special interpretation.
fn dispatch_remote_command(
    p: &mut Project,
    env: &Env,
    tab: usize,
    command: Command,
) -> (CommandOutcome, Option<String>) {
    match dispatch_remote_effect(p, env, tab, command) {
        None => remote_target_gone(),
        Some(result) => fold_remote_effect(result),
    }
}

/// Merge a session's branch back into its base on behalf of the phone,
/// mirroring the TUI's two-phase `FinishLocalMerge` flow. Phase 1 dispatches
/// `confirm: false` — a read-only pass that surfaces the §13 dirty-base
/// warning and every §15 precondition refusal *without merging*; both ack as
/// `Rejected` because nothing happened. Only the [`Effect::MergeConfirm`]
/// go-ahead proceeds to the confirmed merge (the phone already confirmed per
/// PRD §8), whose outcome is folded normally — a `Warning` there means the
/// merge itself landed (only cleanup failed), so applied-with-caveat is honest.
fn dispatch_remote_merge_back(
    p: &mut Project,
    env: &Env,
    tab: usize,
) -> (CommandOutcome, Option<String>) {
    match dispatch_remote_effect(p, env, tab, Command::FinishLocalMerge { confirm: false }) {
        None => remote_target_gone(),
        Some(Ok(Effect::MergeConfirm { .. })) => {
            match dispatch_remote_effect(p, env, tab, Command::FinishLocalMerge { confirm: true }) {
                None => remote_target_gone(),
                Some(result) => fold_remote_effect(result),
            }
        }
        // Dirty base (§13) arrives as a Warning from the unconfirmed pass; no
        // merge happened, so it is a rejection, not applied-with-caveat.
        Some(Ok(Effect::Warning(w))) => (CommandOutcome::Rejected, Some(w)),
        Some(Ok(Effect::Refused(reason))) => (CommandOutcome::Rejected, Some(reason)),
        Some(Ok(other)) => (
            CommandOutcome::Failed,
            Some(format!("unexpected merge-back response: {other:?}")),
        ),
        Some(Err(e)) => (CommandOutcome::Failed, Some(e.to_string())),
    }
}

/// The backing child terminal of a resolved (project, tab, index), if it exists.
fn remote_shell_terminal(
    workspace: &mut Workspace,
    project: usize,
    tab: usize,
    child_index: usize,
) -> Option<&mut crate::terminal::session::Terminal> {
    workspace
        .projects
        .get_mut(project)?
        .state
        .tabs
        .get_mut(tab)?
        .session
        .child_mut(child_index)
}

/// Apply a resolved remote-shell action against the session's child-terminal
/// machinery and the [`ShellManager`], returning the honest ack outcome. The
/// shell child is spawned through the guarded `OpenShell` command (so it is
/// container-aware and shares every desktop guard); input/interrupt/close act on
/// that child's PTY, and lifecycle events are queued for the outbound feed.
fn execute_shell_action(
    shells: &mut ShellManager,
    workspace: &mut Workspace,
    env: &Env,
    project: usize,
    tab: usize,
    session_id: &SessionId,
    action: ShellAction,
) -> (CommandOutcome, Option<String>) {
    match action {
        ShellAction::Open {
            shell_id,
            cols,
            rows,
        } => {
            // One remote shell per session (PRD §5.4): refuse a second before
            // spawning anything.
            if shells.has_shell(session_id) {
                return (
                    CommandOutcome::Rejected,
                    Some("a shell is already open for this session".to_string()),
                );
            }
            let Some(p) = workspace.projects.get_mut(project) else {
                return remote_target_gone();
            };
            // Spawn the child through the guarded, container-aware OpenShell
            // command, preserving the desktop user's on-screen selection.
            let (outcome, message) = dispatch_remote_command(p, env, tab, Command::OpenShell);
            if outcome != CommandOutcome::Applied {
                return (outcome, message);
            }
            // The freshly spawned shell is the last child of the session.
            let Some(t) = p.state.tabs.get_mut(tab) else {
                return remote_target_gone();
            };
            let Some(child_index) = t.session.child_count().checked_sub(1) else {
                return (
                    CommandOutcome::Failed,
                    Some("the shell terminal did not start".to_string()),
                );
            };
            // Size it to the phone's geometry so the remote view matches.
            if let Some(c) = t.session.child_mut(child_index) {
                let _ = c.resize(PtySize { rows, cols });
            }
            shells.opened(session_id.clone(), shell_id, child_index, cols, rows);
            (CommandOutcome::Applied, Some("shell opened".to_string()))
        }

        ShellAction::Input { shell_id, bytes } => {
            if !shells.matches(session_id, &shell_id) {
                return (
                    CommandOutcome::Rejected,
                    Some("no open shell with that id for this session".to_string()),
                );
            }
            let Some(child_index) = shells.child_index(session_id) else {
                return remote_target_gone();
            };
            match remote_shell_terminal(workspace, project, tab, child_index) {
                Some(term) => {
                    // Match desktop input behaviour: drop any selection, snap to
                    // the live bottom, then write the raw bytes.
                    term.clear_selection();
                    term.scroll_to_bottom();
                    if term.session_mut().write_input(&bytes).is_ok() {
                        (CommandOutcome::Applied, None)
                    } else {
                        (
                            CommandOutcome::Failed,
                            Some("could not write to the shell".to_string()),
                        )
                    }
                }
                None => (
                    CommandOutcome::Failed,
                    Some("the shell terminal is gone".to_string()),
                ),
            }
        }

        ShellAction::Interrupt { shell_id } => {
            if !shells.matches(session_id, &shell_id) {
                return (
                    CommandOutcome::Rejected,
                    Some("no open shell with that id for this session".to_string()),
                );
            }
            let Some(child_index) = shells.child_index(session_id) else {
                return remote_target_gone();
            };
            match remote_shell_terminal(workspace, project, tab, child_index) {
                Some(term) => {
                    let _ = term.session_mut().send_ctrl_c();
                    (CommandOutcome::Applied, None)
                }
                None => (
                    CommandOutcome::Failed,
                    Some("the shell terminal is gone".to_string()),
                ),
            }
        }

        ShellAction::Close { shell_id } => {
            let Some(child_index) = shells.close(session_id, &shell_id) else {
                return (
                    CommandOutcome::Rejected,
                    Some("no open shell with that id for this session".to_string()),
                );
            };
            if let Some(p) = workspace.projects.get_mut(project) {
                if let Some(t) = p.state.tabs.get_mut(tab) {
                    // Terminate + remove the child terminal (best effort — the
                    // Closed event has already been queued).
                    let _ = t.session.close_child(child_index);
                }
            }
            (CommandOutcome::Applied, None)
        }
    }
}

/// Deliver queued first tasks of phone-created sessions whose agent is now
/// ready, waiting for bracketed-paste support (or a fallback window) so the
/// task lands in the agent's composer exactly like a desktop paste + Enter.
fn deliver_first_tasks(
    first_tasks: &mut Vec<PendingFirstTask>,
    workspace: &mut Workspace,
    now_ms: u64,
) {
    let mut i = 0;
    while i < first_tasks.len() {
        let age_ms = now_ms.saturating_sub(first_tasks[i].queued_at_ms);
        // Locate the tab by id across all projects (creation may still be in
        // flight; a missing tab means creation failed or the tab was closed).
        let mut located: Option<(usize, usize)> = None;
        for (pi, p) in workspace.projects.iter().enumerate() {
            if let Some(ti) = p
                .state
                .tabs
                .iter()
                .position(|t| t.meta.id == first_tasks[i].tab_id)
            {
                located = Some((pi, ti));
                break;
            }
        }
        let Some((pi, ti)) = located else {
            first_tasks.remove(i);
            continue;
        };
        let tab = &workspace.projects[pi].state.tabs[ti];
        let running =
            tab.phase == TabPhase::Ready && tab.session.primary_state() == ProcessState::Running;
        let bracketed_now = tab
            .session
            .primary()
            .map(|t| t.bracketed_paste())
            .unwrap_or(false);
        match first_task_decision(running, bracketed_now, age_ms) {
            FirstTaskDecision::Wait => i += 1,
            FirstTaskDecision::Expire => {
                first_tasks.remove(i);
            }
            FirstTaskDecision::Send { bracketed } => {
                let bytes = encode_reply(&first_tasks[i].text, bracketed);
                let _ = write_primary_pty(&mut workspace.projects[pi].state, ti, &bytes);
                first_tasks.remove(i);
            }
        }
    }
}

/// One completed background worktree-creation job: which placeholder tab to
/// finalize, and whether materialization succeeded (SPECS §16/§17).
struct CreateOutcome {
    tab_id: String,
    result: Result<()>,
    /// A best-effort warning from the `[worktree_created]` hook run (SPECS §7),
    /// surfaced after the tab is finalized. `None` when no hook ran or it passed.
    hook_warning: Option<String>,
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
        // Stateless real runner for the `[worktree_created]` hook; safe to build
        // on the worker thread (SPECS §7 hooks).
        let command = SystemCommandRunner;
        let outcome = {
            // Recover from a poisoned lock (a previous worker panicked) rather
            // than cascading the panic into every future creation.
            let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
            materialize_worktree(&git, &command, &job)
        };
        let (result, hook_warning) = match outcome {
            Ok(report) => (
                Ok(()),
                report.and_then(|r| r.warning_message("worktree_created")),
            ),
            Err(e) => (Err(e), None),
        };
        let _ = tx.send(CreateOutcome {
            tab_id: job.tab_id,
            result,
            hook_warning,
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
                        apply_effect(effect, state, ui);
                        // A failing `[worktree_created]` hook is surfaced after the
                        // tab is up (best-effort; the tab is kept — SPECS §7 hooks).
                        if let Some(warning) = outcome.hook_warning {
                            ui.message(warning);
                        }
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
fn viewport_pty_size(full: PtySize, reserve_border: bool) -> PtySize {
    let ml = crate::tui::layout::compute(Rect::new(0, 0, full.cols, full.rows), reserve_border);
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
        let ml = crate::tui::layout::compute(
            area,
            crate::tui::mode_style::border_enabled(&workspace.active_project().state.config.ui),
        );
        let names: Vec<String> = workspace.projects.iter().map(|p| p.name.clone()).collect();
        if let Some(hit) = project_tab_hit_test(ml.project_tabs, &names, me.column, me.row) {
            ui.drag = None;
            match hit {
                ProjectHit::Tab(i) => {
                    workspace.set_active(i);
                    resume_active_project_agents(workspace, env);
                }
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
        DialogAccel::Tab => KeyCode::Tab,
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
    let ml = crate::tui::layout::compute(
        area,
        crate::tui::mode_style::border_enabled(&state.config.ui),
    );
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
    let ml = crate::tui::layout::compute(
        area,
        crate::tui::mode_style::border_enabled(&state.config.ui),
    );
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
    let use_f2 = workspace
        .active_project()
        .state
        .config
        .ui
        .use_f2_to_leave_terminal_focus;
    match map_key_with_f2(mode, key, use_f2) {
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
            resume_active_project_agents(workspace, env);
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
            let mut palette = CommandPalette::new();
            // Gate the Remote entries by the live pairing state: hide "Pair
            // Phone" when already paired and "Unpair Phone" when there is no
            // pairing to forget.
            palette.set_paired(ui.remote_paired);
            ui.palette = Some(palette);
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

/// Begin the New Agent Tab flow (SPECS §4, §22): open the combined form —
/// agent radio, branch name, and the "run from base branch" toggle — with the
/// configured default agent preselected.
fn start_new_tab_flow(state: &AppState, ui: &mut Ui) {
    let agents: Vec<(String, String)> = state
        .registry
        .all()
        .iter()
        .map(|a| (a.key.clone(), a.display_name.clone()))
        .collect();
    // Preselect the configured default agent so Enter alone uses it.
    let selected = agents
        .iter()
        .position(|(k, _)| k == &state.registry.default_key)
        .unwrap_or(0);
    start_prompt(
        ui,
        Prompt::NewAgentForm {
            agents,
            selected,
            branch: String::new(),
            run_on_base: false,
            base_branch: state.base_branch.clone(),
        },
    );
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
                        resume_active_project_agents(workspace, env);
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
        Prompt::NewAgentForm {
            agents,
            selected,
            branch,
            run_on_base,
            base_branch,
        } => {
            // Agents as a radio list: the highlighted row is both selected and
            // marked, so ↑/↓ moves the choice.
            let list: Vec<DialogListItem> = agents
                .iter()
                .enumerate()
                .map(|(i, (_key, display))| {
                    let marker = if i == *selected { "(•)" } else { "( )" };
                    DialogListItem {
                        label: format!("{marker} {display}"),
                        selected: i == *selected,
                    }
                })
                .collect();
            let title = if *run_on_base {
                format!(
                    "New Agent Session Tab   (↑/↓ agent · Tab toggles base)\n\
                     Runs on base branch '{base_branch}' in the project root — no worktree."
                )
            } else {
                "New Agent Session Tab   (↑/↓ agent · type branch · Tab = run from base branch)"
                    .to_string()
            };
            let base_label = if *run_on_base {
                format!("Run from base: {base_branch}")
            } else {
                "Run from base: off".to_string()
            };
            let buttons = vec![
                DialogButton::new(DialogAccel::Enter, "Create"),
                DialogButton::new(DialogAccel::Tab, base_label),
                cancel,
            ];
            // Hide the branch textbox entirely when running on base.
            let mut dialog = Dialog::browser(title, branch.clone(), list, buttons);
            if *run_on_base {
                dialog.input = None;
            }
            dialog
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
        Prompt::UnpairConfirm => Dialog::confirm(
            "Unpair this phone? It loses access until you pair it again.",
            vec![
                DialogButton::new(DialogAccel::Char('y'), "Unpair"),
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
        Some(Prompt::UnpairConfirm) => {
            ui.prompt = None;
            if key.code == KeyCode::Char('y') {
                // Deferred to the event loop, which owns the relay channels.
                ui.pending_unpair = true;
            }
            return Ok(());
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
        Prompt::NewAgentForm { .. } => {
            match key.code {
                // ↑/↓ move the agent radio selection.
                KeyCode::Up => {
                    if let Prompt::NewAgentForm { selected, .. } = &mut pstate.prompt {
                        *selected = selected.saturating_sub(1);
                    }
                    pstate.dialog = prompt_dialog(&pstate.prompt);
                    ui.prompt = Some(pstate);
                }
                KeyCode::Down => {
                    if let Prompt::NewAgentForm {
                        selected, agents, ..
                    } = &mut pstate.prompt
                    {
                        if *selected + 1 < agents.len() {
                            *selected += 1;
                        }
                    }
                    pstate.dialog = prompt_dialog(&pstate.prompt);
                    ui.prompt = Some(pstate);
                }
                // Tab toggles "run from base branch" (disables the branch field).
                KeyCode::Tab => {
                    if let Prompt::NewAgentForm { run_on_base, .. } = &mut pstate.prompt {
                        *run_on_base = !*run_on_base;
                    }
                    pstate.dialog = prompt_dialog(&pstate.prompt);
                    ui.prompt = Some(pstate);
                }
                KeyCode::Enter => {
                    let (agent_key, name, run_on_base) = match &pstate.prompt {
                        Prompt::NewAgentForm {
                            agents,
                            selected,
                            branch,
                            run_on_base,
                            ..
                        } => (
                            agents.get(*selected).map(|(k, _)| k.clone()),
                            branch.trim().to_string(),
                            *run_on_base,
                        ),
                        _ => unreachable!(),
                    };
                    // A worktree tab needs a name; a base-branch tab does not
                    // (its branch is fixed and the field is disabled).
                    if !run_on_base && name.is_empty() {
                        pstate.dialog = prompt_dialog(&pstate.prompt);
                        ui.prompt = Some(pstate);
                        return Ok(());
                    }
                    // Async new-tab flow: reserve a placeholder tab now (cheap,
                    // validation-first), then queue the slow worktree creation for
                    // a background worker so the UI never blocks (SPECS §16/§17).
                    // A base-branch tab has nothing to materialize.
                    ui.prompt = None;
                    match state.begin_new_agent_tab_ex(
                        &name,
                        agent_key.as_deref(),
                        run_on_base,
                        services,
                    ) {
                        Ok(job) => {
                            let branch = job.branch.clone();
                            let msg = if run_on_base {
                                format!("Starting agent on base branch {branch}…")
                            } else {
                                format!("Creating worktree for {branch}…")
                            };
                            ui.pending_jobs.push(PendingJob {
                                project: active,
                                job,
                            });
                            ui.message(msg);
                        }
                        Err(e) => ui.message(format!("Error: {e}")),
                    }
                }
                KeyCode::Backspace => {
                    if let Prompt::NewAgentForm {
                        branch,
                        run_on_base,
                        ..
                    } = &mut pstate.prompt
                    {
                        if !*run_on_base {
                            branch.pop();
                        }
                    }
                    pstate.dialog = prompt_dialog(&pstate.prompt);
                    ui.prompt = Some(pstate);
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if let Prompt::NewAgentForm {
                        branch,
                        run_on_base,
                        ..
                    } = &mut pstate.prompt
                    {
                        if !*run_on_base {
                            branch.push(c);
                        }
                    }
                    pstate.dialog = prompt_dialog(&pstate.prompt);
                    ui.prompt = Some(pstate);
                }
                _ => {
                    ui.prompt = Some(pstate);
                }
            }
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
        Prompt::RenameTab { .. } => {
            match key.code {
                KeyCode::Enter => {
                    let name = match &pstate.prompt {
                        Prompt::RenameTab { buffer } => buffer.trim().to_string(),
                        _ => unreachable!(),
                    };
                    if name.is_empty() {
                        // Keep prompting; nothing entered yet.
                        pstate.dialog = prompt_dialog(&pstate.prompt);
                        ui.prompt = Some(pstate);
                        return Ok(());
                    }
                    let result =
                        state.dispatch(Command::RenameAgentTab { new_name: name }, services);
                    finish_prompt(result, ui);
                }
                KeyCode::Backspace => {
                    if let Prompt::RenameTab { buffer } = &mut pstate.prompt {
                        buffer.pop();
                    }
                    pstate.dialog = prompt_dialog(&pstate.prompt);
                    ui.prompt = Some(pstate);
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if let Prompt::RenameTab { buffer } = &mut pstate.prompt {
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
        Prompt::OpenProject { .. } | Prompt::CloseProjectConfirm { .. } | Prompt::UnpairConfirm => {
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
            resume_active_project_agents(workspace, env);
            return Ok(());
        }
        PaletteAction::SwitchProjectPrev => {
            workspace.switch(Selector::Prev);
            resume_active_project_agents(workspace, env);
            return Ok(());
        }
        PaletteAction::OpenConfig => {
            open_config_manager(workspace, env, ui);
            return Ok(());
        }
        PaletteAction::PairPhone => {
            // The event loop (which owns the relay channels + pairing session)
            // starts the offer and opens the overlay next tick.
            ui.pending_pair = true;
            return Ok(());
        }
        PaletteAction::UnpairPhone => {
            start_prompt(ui, Prompt::UnpairConfirm);
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
        | PaletteAction::OpenConfig
        | PaletteAction::PairPhone
        | PaletteAction::UnpairPhone => Ok(()),
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
fn drain_pty_output(
    state: &mut AppState,
    _now_ms: u64,
    mut tee: impl FnMut(&str, Option<usize>, &[u8]),
) {
    // Read once before the loop: auto-continuation gates resume-hint capture,
    // and the per-tab borrow below would otherwise conflict with reading config.
    let auto_continue = state.config.ui.auto_continue;
    for tab in state.tabs.iter_mut() {
        // Primary: drain into the VT parser. Lifecycle status comes only from
        // backend hooks/plugins; PTY output includes echoed user keystrokes and
        // is deliberately not treated as agent activity.
        let primary_bytes = tab.session.primary_mut().and_then(|primary| {
            match primary.session_mut().try_read_output() {
                Ok(bytes) if !bytes.is_empty() => {
                    primary.process_output(&bytes);
                    // Unblock ConPTY / cursor-probing TUIs (Windows): reply to
                    // any `ESC[6n` so the child renders instead of stalling.
                    primary.answer_cursor_position_query(&bytes);
                    // Tee the raw primary bytes to the remote transcript builder
                    // (`None` = primary; a no-op when remote is disabled).
                    // `tab.meta` is a disjoint field from `tab.session`, so this
                    // borrows cleanly.
                    tee(&tab.meta.id, None, &bytes);
                    Some(bytes)
                }
                _ => None,
            }
        });
        // Capture the agent's on-exit resume hint from that output (borrow of
        // `tab.session` has ended, so we can touch the rest of the tab).
        if let Some(bytes) = primary_bytes {
            tab.capture_resume_hint(&bytes, auto_continue);
        }

        // Child terminals: drain → VT parser (so they don't stall and so their
        // screen renders when selected), teeing each child's raw bytes so a
        // remote shell backed by that child (`Some(index)`) streams to the phone.
        for c in 0..tab.session.child_count() {
            if let Some(child) = tab.session.child_mut(c) {
                if let Ok(bytes) = child.session_mut().try_read_output() {
                    if !bytes.is_empty() {
                        child.process_output(&bytes);
                        child.answer_cursor_position_query(&bytes);
                        tee(&tab.meta.id, Some(c), &bytes);
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

/// Write bytes to a specific tab's **primary** agent terminal (the phone
/// reply/permission path). Mirrors [`write_active_pty`]'s behaviour exactly
/// (drop any selection, snap back to the live bottom) but targets the tab by
/// index and always its primary — a phone reply must reach the agent even
/// when the desktop user is focused on a child shell of another tab. Returns
/// whether the write succeeded.
fn write_primary_pty(state: &mut AppState, tab: usize, bytes: &[u8]) -> bool {
    let Some(t) = state.tabs.get_mut(tab) else {
        return false;
    };
    let Some(term) = t.session.primary_mut() else {
        return false;
    };
    term.clear_selection();
    term.scroll_to_bottom();
    term.session_mut().write_input(bytes).is_ok()
}

/// Paste from the system clipboard into the active terminal (Ctrl-V or Cmd-V
/// when the macOS terminal reports Command as a key modifier).
///
/// When the clipboard holds an image, it is written to a temp file and the
/// file path is sent to the agent — matching how a terminal inserts a path when
/// you drag an image in, which agents like Claude Code recognise and attach. A
/// trailing space is appended so the user can keep typing. With no image on the
/// clipboard, a literal Ctrl-V (0x16) is forwarded, preserving prior behaviour.
fn paste_into_active_pty(state: &mut AppState) {
    let (agent, containerized) = state
        .selected()
        .map(|tab| (tab.meta.agent.as_str(), tab.meta.containerized))
        .unwrap_or_default();

    // Codex CLI owns native image paste in its interactive composer. Let a
    // locally-running instance read the host clipboard directly rather than
    // replacing the paste with plain text. A containerized Codex cannot access
    // that clipboard, so it uses the shared-file path below.
    if use_native_codex_image_paste(agent, containerized) {
        write_active_pty(state, &[0x16]);
        return;
    }

    match crate::tui::clipboard::save_clipboard_image() {
        Some(path) => {
            // A container cannot see the host's temp path. Fresh containers
            // bind-mount FlightDeck's dedicated paste directory at the same
            // container path, so translate only paths within that directory.
            let path = if containerized {
                crate::tui::clipboard::container_image_path(
                    &path,
                    &crate::tui::clipboard::image_paste_dir(),
                    std::path::Path::new(crate::runtime::container::IMAGE_PASTE_DIR),
                )
                .unwrap_or(path)
            } else {
                path
            };
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

fn use_native_codex_image_paste(agent: &str, containerized: bool) -> bool {
    agent == "codex" && !containerized
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
        let ml = crate::tui::layout::compute(
            area,
            crate::tui::mode_style::border_enabled(&state.config.ui),
        );
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
        // Derive the size from the current terminal size AND this project's
        // border setting (mirroring the split branch) so enabling/disabling the
        // border — or switching to a project with a different mode_border —
        // reflows immediately instead of waiting for the next window resize.
        let area = Rect::new(0, 0, full.cols, full.rows);
        let ml = crate::tui::layout::compute(
            area,
            crate::tui::mode_style::border_enabled(&state.config.ui),
        );
        let size = PtySize {
            rows: ml.terminal.height.max(1),
            cols: ml.terminal.width.max(1),
        };
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

    // --- next_loop_step: the shutdown-flag / input decision --------------

    #[test]
    fn loop_step_shuts_down_when_flag_set_without_touching_input() {
        use std::sync::atomic::AtomicBool;
        let flag = AtomicBool::new(true);
        let (tx, rx) = std::sync::mpsc::channel::<Event>();
        // An event is available, but the shutdown flag must win — and we must
        // not consume the event.
        tx.send(Event::Resize(80, 24)).unwrap();
        assert_eq!(
            next_loop_step(&flag, &rx, Duration::from_millis(10)),
            LoopStep::Shutdown
        );
        assert!(rx.try_recv().is_ok(), "input event must not be consumed");
    }

    #[test]
    fn loop_step_returns_queued_input() {
        use std::sync::atomic::AtomicBool;
        let flag = AtomicBool::new(false);
        let (tx, rx) = std::sync::mpsc::channel::<Event>();
        tx.send(Event::Resize(120, 40)).unwrap();
        assert_eq!(
            next_loop_step(&flag, &rx, Duration::from_millis(10)),
            LoopStep::Input(Event::Resize(120, 40))
        );
    }

    #[test]
    fn loop_step_is_idle_on_timeout_with_no_input() {
        use std::sync::atomic::AtomicBool;
        let flag = AtomicBool::new(false);
        let (_tx, rx) = std::sync::mpsc::channel::<Event>();
        assert_eq!(
            next_loop_step(&flag, &rx, Duration::from_millis(10)),
            LoopStep::Idle
        );
    }

    #[test]
    fn loop_step_shuts_down_when_input_source_disconnected() {
        use std::sync::atomic::AtomicBool;
        let flag = AtomicBool::new(false);
        let (tx, rx) = std::sync::mpsc::channel::<Event>();
        drop(tx); // reader thread gone (e.g. terminal severed)
        assert_eq!(
            next_loop_step(&flag, &rx, Duration::from_millis(10)),
            LoopStep::Shutdown
        );
    }

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
                auto_continue: true,
                ..UiConfig::default()
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
    fn new_agent_form_shows_input_radio_and_buttons() {
        let p = Prompt::NewAgentForm {
            agents: vec![
                ("claude".to_string(), "Claude Code".to_string()),
                ("opencode".to_string(), "OpenCode".to_string()),
            ],
            selected: 1,
            branch: "fix bug".to_string(),
            run_on_base: false,
            base_branch: "main".to_string(),
        };
        let dialog = prompt_dialog(&p);
        // Branch textbox visible with its buffer.
        assert_eq!(dialog.input.as_deref(), Some("fix bug"));
        // Radio list marks the selected agent.
        assert_eq!(dialog.list.len(), 2);
        assert!(dialog.list[1].selected);
        assert!(dialog.list[1].label.contains("OpenCode"));
        // Create (Enter), the base toggle (Tab), and Cancel (Esc).
        assert!(dialog
            .buttons
            .iter()
            .any(|b| b.accel == DialogAccel::Enter && b.label == "Create"));
        assert!(dialog
            .buttons
            .iter()
            .any(|b| b.accel == DialogAccel::Tab && b.label.contains("off")));
        assert!(dialog.buttons.iter().any(|b| b.accel == DialogAccel::Esc));
    }

    #[test]
    fn new_agent_form_run_on_base_hides_branch_field() {
        let p = Prompt::NewAgentForm {
            agents: vec![("claude".to_string(), "Claude Code".to_string())],
            selected: 0,
            branch: "ignored".to_string(),
            run_on_base: true,
            base_branch: "main".to_string(),
        };
        let dialog = prompt_dialog(&p);
        // The branch textbox is disabled (hidden) when running on base.
        assert!(dialog.input.is_none());
        // The base toggle button reflects the enabled state with the base branch.
        assert!(dialog
            .buttons
            .iter()
            .any(|b| b.accel == DialogAccel::Tab && b.label.contains("main")));
    }

    #[test]
    fn new_agent_form_preselects_default_and_moves_with_arrows() {
        use crate::app::state::AppState;
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        // Config with two agents; the default (opencode) should be preselected.
        let mut config = Config {
            ui: UiConfig {
                default_agent: "opencode".to_string(),
                agent_tab_position: "left".to_string(),
                auto_continue: true,
                ..UiConfig::default()
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

        // Starting the flow opens the combined form with the default preselected.
        start_new_tab_flow(&state, &mut ui);
        // BTreeMap key order: "claude" (idx 0) before "opencode" (idx 1).
        match &ui.prompt.as_ref().expect("prompt active").prompt {
            Prompt::NewAgentForm {
                agents, selected, ..
            } => {
                assert_eq!(agents[0].0, "claude");
                assert_eq!(agents[1].0, "opencode");
                assert_eq!(*selected, 1, "default agent preselected");
            }
            _ => panic!("expected NewAgentForm prompt"),
        }

        let git = FakeGit::new();
        let fs = FakeFs::new();
        let pty = FakePty::new();
        let clock = FakeClock::default();
        let container = crate::testing::FakeContainerRuntime::new();
        let command = crate::testing::FakeCommandRunner::new();
        let services = Services {
            git: &git,
            fs: &fs,
            pty: &pty,
            clock: &clock,
            container: &container,
            command: &command,
        };

        // ↑ moves the radio selection to the first agent (claude).
        handle_prompt_key_project(
            KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
            &mut state,
            &services,
            &mut ui,
            0,
        )
        .unwrap();
        // Tab toggles "run from base branch" on.
        handle_prompt_key_project(
            KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
            &mut state,
            &services,
            &mut ui,
            0,
        )
        .unwrap();
        match &ui.prompt.as_ref().expect("form still active").prompt {
            Prompt::NewAgentForm {
                selected,
                run_on_base,
                ..
            } => {
                assert_eq!(*selected, 0, "↑ moved to claude");
                assert!(*run_on_base, "Tab enabled run-from-base");
            }
            _ => panic!("expected NewAgentForm prompt"),
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
    fn local_codex_uses_its_native_image_paste_handler() {
        assert!(use_native_codex_image_paste("codex", false));
        assert!(!use_native_codex_image_paste("codex", true));
        assert!(!use_native_codex_image_paste("claude", false));
    }

    #[test]
    fn viewport_size_is_smaller_than_full_terminal() {
        // The agent PTY must wrap at the viewport width (full minus sidebar),
        // not the whole screen width.
        let full = PtySize {
            rows: 40,
            cols: 120,
        };
        let vp = viewport_pty_size(full, false);
        assert!(vp.cols < full.cols, "viewport narrower than full screen");
        assert!(vp.rows < full.rows, "viewport shorter than full screen");
        assert!(vp.cols >= 1 && vp.rows >= 1);
    }

    #[test]
    fn viewport_pty_size_shrinks_further_with_border() {
        let full = PtySize {
            rows: 40,
            cols: 120,
        };
        let plain = viewport_pty_size(full, false);
        let framed = viewport_pty_size(full, true);
        assert_eq!(framed.cols, plain.cols - 2);
        assert_eq!(framed.rows, plain.rows - 2);
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
        let command = crate::testing::FakeCommandRunner::new();
        let services = Services {
            git: &git,
            fs: &fs,
            pty: &pty,
            clock: &clock,
            container: &container,
            command: &command,
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
        drain_pty_output(&mut state, 1_000, |_, _, _| {});

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
        let command = crate::testing::FakeCommandRunner::new();
        let services = Services {
            git: &git,
            fs: &fs,
            pty: &pty,
            clock: &clock,
            container: &container,
            command: &command,
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
        let command = crate::testing::FakeCommandRunner::new();
        let services = Services {
            git: &git,
            fs: &fs,
            pty: &pty,
            clock: &clock,
            container: &container,
            command: &command,
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

    /// Regression test for the stale-`pty_size` bug: the non-split branch of
    /// `sync_terminal_sizes` must derive the viewport from `full` + the
    /// project's own border setting (like the split branch already does),
    /// not from `state.pty_size`, so toggling `mode_border` reflows the
    /// terminal immediately instead of waiting for the next window resize.
    #[test]
    fn sync_terminal_sizes_reflows_on_border_toggle_without_window_resize() {
        use crate::contracts::TabState;

        fn tab_state() -> TabState {
            TabState {
                id: "tab-1".to_string(),
                name: "Task".to_string(),
                slug: "task".to_string(),
                agent: "opencode".to_string(),
                branch: "flightdeck/task".to_string(),
                worktree_path_relative: ".flightdeck/worktrees/task".to_string(),
                base_branch: "main".to_string(),
                base_commit_sha: "abc123".to_string(),
                created_at: "2026-01-01T00:00:00Z".to_string(),
                attached_existing_branch: false,
                recovered: false,
                last_known_status: "running".to_string(),
                manual_status: None,
                containerized: false,
                container_image: None,
                runs_on_base: false,
                resume_args: Vec::new(),
            }
        }

        fn build_state(border: &str) -> AppState {
            let config = Config {
                ui: UiConfig {
                    mode_border: border.to_string(),
                    ..UiConfig::default()
                },
                ..Config::default()
            };
            let mut project_state = default_state("main");
            project_state.tabs.push(tab_state());
            let mut state = AppState::new(config, project_state, "/repo", "/repo/state.json");

            // Stale on purpose: sync_terminal_sizes must NOT rely on this
            // field in the non-split branch, or the bug would still pass.
            state.pty_size = PtySize {
                rows: 999,
                cols: 999,
            };

            let pty = FakePty::new();
            let _handle = pty.queue_session();
            state.tabs[0]
                .session
                .spawn_primary(
                    &pty,
                    "opencode",
                    &[],
                    Path::new("/repo/.flightdeck/worktrees/task"),
                    PtySize { rows: 24, cols: 80 },
                )
                .expect("spawn_primary should succeed against FakePty");
            state
        }

        let full = PtySize {
            rows: 40,
            cols: 100,
        };

        let mut off = build_state("off");
        sync_terminal_sizes(&mut off, full);
        let (off_rows, off_cols) = off.tabs[0]
            .session
            .primary()
            .expect("primary terminal spawned")
            .screen()
            .size();

        let mut normal = build_state("normal");
        sync_terminal_sizes(&mut normal, full);
        let (on_rows, on_cols) = normal.tabs[0]
            .session
            .primary()
            .expect("primary terminal spawned")
            .screen()
            .size();

        assert_eq!(
            off_cols - on_cols,
            2,
            "border on should be exactly 2 cols narrower than border off"
        );
        assert_eq!(
            off_rows - on_rows,
            2,
            "border on should be exactly 2 rows shorter than border off"
        );
    }

    // -----------------------------------------------------------------------
    // FlightDeck Remote: inbound command bridge (phone → desktop)
    // -----------------------------------------------------------------------

    mod remote_commands {
        use super::*;
        use crate::contracts::{ProjectState as CoreProjectState, TabState, STATE_VERSION};
        use crate::remote::bridge::passthrough_seal;
        use crate::remote::commands::PendingFirstTask;
        use crate::testing::{FakeContainerRuntime, FakePtyHandle};
        use base64::engine::general_purpose::STANDARD;
        use base64::Engine;
        use flightdeck_remote_protocol::relay::EncryptedEnvelope;
        use flightdeck_remote_protocol::{
            CommandBody, CommandId, DesktopToPhone, PairingId, Role, SessionId,
        };
        use flightdeck_remote_protocol::{CommandOutcome, PhoneCommand};

        fn tab_state(id: &str, name: &str, agent: &str) -> TabState {
            TabState {
                id: id.to_string(),
                name: name.to_string(),
                slug: name.to_string(),
                agent: agent.to_string(),
                branch: format!("{name}-branch"),
                worktree_path_relative: format!("worktrees/{name}"),
                base_branch: "main".to_string(),
                base_commit_sha: "abc123".to_string(),
                created_at: "2026-01-01T00:00:00Z".to_string(),
                attached_existing_branch: false,
                recovered: false,
                last_known_status: "unknown".to_string(),
                manual_status: None,
                containerized: false,
                container_image: None,
                runs_on_base: false,
                resume_args: Vec::new(),
            }
        }

        /// An [`AppState`] with the given tabs, each spawned with a running
        /// fake primary. Returns the per-tab PTY handles for input assertions.
        fn app_with_tabs(
            config: Config,
            tabs: Vec<TabState>,
            pty: &FakePty,
        ) -> (AppState, Vec<FakePtyHandle>) {
            let state = CoreProjectState {
                version: STATE_VERSION,
                project_root_relative: ".".to_string(),
                base_branch: "main".to_string(),
                tabs,
            };
            let mut app = AppState::new(config, state, "/repo", "/repo/.flightdeck/state.json");
            let mut handles = Vec::new();
            for tab in app.tabs.iter_mut() {
                handles.push(pty.queue_session());
                tab.session
                    .spawn_primary(pty, "agent", &[], Path::new("/repo"), PtySize::default())
                    .unwrap();
            }
            (app, handles)
        }

        fn workspace_with(app: AppState) -> Workspace {
            workspace_rooted(app, PathBuf::from("/repo"))
        }

        fn workspace_rooted(app: AppState, root: PathBuf) -> Workspace {
            let (create_tx, create_rx) = std::sync::mpsc::channel();
            let (status_tx, status_rx) = std::sync::mpsc::channel();
            Workspace {
                projects: vec![Project {
                    name: "proj".to_string(),
                    git: GitCli::new(root),
                    state: app,
                    cache: GitStatusCache::new(),
                    create_tx,
                    create_rx,
                    status_tx,
                    status_rx,
                    status_in_flight: false,
                    git_lock: Arc::new(Mutex::new(())),
                }],
                active: 0,
            }
        }

        fn envelope(seq: u64, cmd: &PhoneCommand) -> EncryptedEnvelope {
            let plain = serde_json::to_vec(cmd).unwrap();
            let (nonce, ciphertext) = passthrough_seal()(&plain, seq, 0).unwrap();
            EncryptedEnvelope {
                pairing_id: PairingId::new("pair-1"),
                seq,
                sender: Role::Phone,
                sent_at_ms: 0,
                nonce,
                ciphertext,
            }
        }

        fn decode_acks(sent: &[RemoteOutbound]) -> Vec<flightdeck_remote_protocol::CommandAck> {
            sent.iter()
                .filter_map(|o| match o {
                    RemoteOutbound::SendEnvelope { ciphertext, .. } => {
                        let bytes = STANDARD.decode(ciphertext).unwrap();
                        match serde_json::from_slice::<DesktopToPhone>(&bytes).unwrap() {
                            DesktopToPhone::CommandAck(ack) => Some(ack),
                            _ => None,
                        }
                    }
                    _ => None,
                })
                .collect()
        }

        /// End-to-endish: a `reply` envelope through the full drain path —
        /// bridge inbound → ledger → translate → primary-PTY write → ack.
        #[test]
        fn reply_reaches_primary_pty_and_acks_applied() {
            let pty = FakePty::new();
            let (app, handles) = app_with_tabs(
                Config::default(),
                vec![tab_state("t1", "fix", "claude")],
                &pty,
            );
            let mut workspace = workspace_with(app);
            let fs = FakeFs::new();
            let clock = FakeClock::default();
            let container = FakeContainerRuntime::new();
            let command = crate::testing::FakeCommandRunner::new();
            let env = Env {
                fs: &fs,
                pty: &pty,
                clock: &clock,
                container: &container,
                command: &command,
            };

            let mut bridge = RemoteBridge::passthrough(0);
            let cmd = PhoneCommand {
                command_id: CommandId::new("c1"),
                issued_at_ms: 0,
                body: CommandBody::Reply {
                    session_id: SessionId::new("t1"),
                    text: "hello agent".to_string(),
                },
            };
            bridge.handle_inbound(RemoteInbound::Envelope(envelope(1, &cmd)));

            let mut ledger = CommandLedger::new();
            let mut first_tasks: Vec<PendingFirstTask> = Vec::new();
            let mut sent: Vec<RemoteOutbound> = Vec::new();
            service_remote_commands(
                &mut bridge,
                &mut ledger,
                &mut first_tasks,
                &mut workspace,
                &env,
                1_000,
                &mut |o| sent.push(o),
            );

            // The fake PTY received the exact reply bytes (raw + Enter; the
            // fresh terminal has not enabled bracketed paste).
            assert_eq!(handles[0].input(), b"hello agent\r".to_vec());
            // …and an applied ack was queued for the command id.
            let acks = decode_acks(&sent);
            assert_eq!(acks.len(), 1);
            assert_eq!(acks[0].command_id, CommandId::new("c1"));
            assert_eq!(acks[0].outcome, CommandOutcome::Applied);
        }

        /// A retransmitted command id is acked as duplicate, never re-applied.
        #[test]
        fn duplicate_command_is_acked_but_not_reapplied() {
            let pty = FakePty::new();
            let (app, handles) = app_with_tabs(
                Config::default(),
                vec![tab_state("t1", "fix", "claude")],
                &pty,
            );
            let mut workspace = workspace_with(app);
            let fs = FakeFs::new();
            let clock = FakeClock::default();
            let container = FakeContainerRuntime::new();
            let command = crate::testing::FakeCommandRunner::new();
            let env = Env {
                fs: &fs,
                pty: &pty,
                clock: &clock,
                container: &container,
                command: &command,
            };

            let mut bridge = RemoteBridge::passthrough(0);
            let cmd = PhoneCommand {
                command_id: CommandId::new("c1"),
                issued_at_ms: 0,
                body: CommandBody::Reply {
                    session_id: SessionId::new("t1"),
                    text: "again".to_string(),
                },
            };
            let mut ledger = CommandLedger::new();
            let mut first_tasks: Vec<PendingFirstTask> = Vec::new();

            // Two deliveries of the same logical command (a client retry).
            let mut sent: Vec<RemoteOutbound> = Vec::new();
            for seq in [1, 2] {
                bridge.handle_inbound(RemoteInbound::Envelope(envelope(seq, &cmd)));
                service_remote_commands(
                    &mut bridge,
                    &mut ledger,
                    &mut first_tasks,
                    &mut workspace,
                    &env,
                    1_000,
                    &mut |o| sent.push(o),
                );
            }

            // Written once, acked twice: applied then duplicate.
            assert_eq!(handles[0].input(), b"again\r".to_vec());
            let acks = decode_acks(&sent);
            assert_eq!(acks.len(), 2);
            assert_eq!(acks[0].outcome, CommandOutcome::Applied);
            assert_eq!(acks[1].outcome, CommandOutcome::Duplicate);
        }

        /// A phone `restart_agent` reaches `Command::RestartAgent` through the
        /// dispatch path (temporary selection, guards intact) and respawns the
        /// primary, leaving the desktop user's selection untouched.
        #[test]
        fn restart_dispatches_and_preserves_selection() {
            let dir = TempDir::new().unwrap();
            let agent = make_real_agent(&dir, "opencode");
            let config = config_with_agent(agent);
            let pty = FakePty::new();
            let (app, _handles) = app_with_tabs(
                config,
                vec![
                    tab_state("t0", "other", "opencode"),
                    tab_state("t1", "target", "opencode"),
                ],
                &pty,
            );
            let mut workspace = workspace_with(app);
            workspace.projects[0].state.selected_tab = Some(0);
            // The worktree must exist for the restart spawn's status snapshot.
            let fs = FakeFs::new().with_dir("/repo/worktrees/target");
            let clock = FakeClock::default();
            let container = FakeContainerRuntime::new();
            let command = crate::testing::FakeCommandRunner::new();
            let env = Env {
                fs: &fs,
                pty: &pty,
                clock: &clock,
                container: &container,
                command: &command,
            };

            let mut bridge = RemoteBridge::passthrough(0);
            let cmd = PhoneCommand {
                command_id: CommandId::new("c-restart"),
                issued_at_ms: 0,
                body: CommandBody::RestartAgent {
                    session_id: SessionId::new("t1"),
                },
            };
            bridge.handle_inbound(RemoteInbound::Envelope(envelope(1, &cmd)));

            let spawns_before = pty.spawns().len();
            let mut ledger = CommandLedger::new();
            let mut first_tasks: Vec<PendingFirstTask> = Vec::new();
            let mut sent: Vec<RemoteOutbound> = Vec::new();
            service_remote_commands(
                &mut bridge,
                &mut ledger,
                &mut first_tasks,
                &mut workspace,
                &env,
                1_000,
                &mut |o| sent.push(o),
            );

            let acks = decode_acks(&sent);
            assert_eq!(acks.len(), 1);
            assert_eq!(
                acks[0].outcome,
                CommandOutcome::Applied,
                "ack: {:?}",
                acks[0].message
            );
            // A fresh primary was spawned for the restart…
            assert_eq!(pty.spawns().len(), spawns_before + 1);
            // …and the user's on-screen selection was not yanked to the target.
            assert_eq!(workspace.projects[0].state.selected_tab, Some(0));
        }

        /// First tasks queued by `new_agent` wait for the agent, then land as
        /// a bracketed paste + Enter the moment the agent enables the mode.
        #[test]
        fn first_task_delivered_when_agent_enables_bracketed_paste() {
            let pty = FakePty::new();
            let (app, handles) = app_with_tabs(
                Config::default(),
                vec![tab_state("t1", "fix", "claude")],
                &pty,
            );
            let mut workspace = workspace_with(app);

            let mut first_tasks = vec![PendingFirstTask {
                tab_id: "t1".to_string(),
                text: "make the tests pass".to_string(),
                queued_at_ms: 0,
            }];

            // Agent up but bracketed paste not enabled yet: wait.
            deliver_first_tasks(&mut first_tasks, &mut workspace, 1_000);
            assert_eq!(first_tasks.len(), 1);
            assert!(handles[0].input().is_empty());

            // The agent enables bracketed paste (DECSET 2004): deliver.
            workspace.projects[0].state.tabs[0]
                .session
                .primary_mut()
                .unwrap()
                .process_output(b"\x1b[?2004h");
            deliver_first_tasks(&mut first_tasks, &mut workspace, 2_000);
            assert!(first_tasks.is_empty());
            assert_eq!(
                handles[0].input(),
                b"\x1b[200~make the tests pass\x1b[201~\r".to_vec()
            );

            // A task whose tab vanished (creation failed / closed) is dropped.
            let mut gone = vec![PendingFirstTask {
                tab_id: "ghost".to_string(),
                text: "hi".to_string(),
                queued_at_ms: 0,
            }];
            deliver_first_tasks(&mut gone, &mut workspace, 3_000);
            assert!(gone.is_empty());
        }

        // --- git action bridge ---------------------------------------------

        /// Run one command envelope through the full drain path and return the
        /// acks plus everything else that was sent.
        fn run_command(
            bridge: &mut RemoteBridge,
            workspace: &mut Workspace,
            env: &Env,
            seq: u64,
            cmd: &PhoneCommand,
        ) -> Vec<flightdeck_remote_protocol::CommandAck> {
            bridge.handle_inbound(RemoteInbound::Envelope(envelope(seq, cmd)));
            let mut ledger = CommandLedger::new();
            let mut first_tasks: Vec<PendingFirstTask> = Vec::new();
            let mut sent: Vec<RemoteOutbound> = Vec::new();
            service_remote_commands(
                bridge,
                &mut ledger,
                &mut first_tasks,
                workspace,
                env,
                1_000,
                &mut |o| sent.push(o),
            );
            decode_acks(&sent)
        }

        /// Abandon with a wrong type-to-confirm name is rejected before any
        /// state is touched, with the session name echoed in the reason.
        #[test]
        fn git_abandon_confirm_name_mismatch_is_rejected() {
            let pty = FakePty::new();
            let (app, _handles) = app_with_tabs(
                Config::default(),
                vec![tab_state("t1", "fix", "claude")],
                &pty,
            );
            let mut workspace = workspace_with(app);
            let fs = FakeFs::new();
            let clock = FakeClock::default();
            let container = FakeContainerRuntime::new();
            let command = crate::testing::FakeCommandRunner::new();
            let env = Env {
                fs: &fs,
                pty: &pty,
                clock: &clock,
                container: &container,
                command: &command,
            };

            let mut bridge = RemoteBridge::passthrough(0);
            let cmd = PhoneCommand {
                command_id: CommandId::new("c-abandon"),
                issued_at_ms: 0,
                body: CommandBody::GitAbandonWorktree {
                    session_id: SessionId::new("t1"),
                    confirm_name: "wrong".to_string(),
                },
            };
            let acks = run_command(&mut bridge, &mut workspace, &env, 1, &cmd);
            assert_eq!(acks.len(), 1);
            assert_eq!(acks[0].outcome, CommandOutcome::Rejected);
            let msg = acks[0].message.as_deref().unwrap();
            assert!(msg.contains("does not match"), "{msg}");
            // The tab is untouched.
            assert_eq!(workspace.projects[0].state.tabs.len(), 1);
        }

        /// Git commands against an unknown session are rejected honestly.
        #[test]
        fn git_commands_unknown_session_rejected() {
            let pty = FakePty::new();
            let (app, _handles) = app_with_tabs(
                Config::default(),
                vec![tab_state("t1", "fix", "claude")],
                &pty,
            );
            let mut workspace = workspace_with(app);
            let fs = FakeFs::new();
            let clock = FakeClock::default();
            let container = FakeContainerRuntime::new();
            let command = crate::testing::FakeCommandRunner::new();
            let env = Env {
                fs: &fs,
                pty: &pty,
                clock: &clock,
                container: &container,
                command: &command,
            };

            let mut bridge = RemoteBridge::passthrough(0);
            let cmd = PhoneCommand {
                command_id: CommandId::new("c-pull"),
                issued_at_ms: 0,
                body: CommandBody::GitPullBase {
                    session_id: SessionId::new("ghost"),
                },
            };
            let acks = run_command(&mut bridge, &mut workspace, &env, 1, &cmd);
            assert_eq!(acks[0].outcome, CommandOutcome::Rejected);
            assert!(acks[0]
                .message
                .as_deref()
                .unwrap()
                .contains("unknown session"));
        }

        /// Merge-back against a dirty base repo is REJECTED (nothing merged):
        /// the §13 dirty-base warning from the unconfirmed phase must not ack
        /// as applied. Uses a real `git init` repo so the actual GitCli
        /// precondition path runs end to end.
        #[test]
        fn git_merge_back_dirty_base_is_rejected_not_applied() {
            let dir = TempDir::new().unwrap();
            let root = dir.path().to_path_buf();
            // A fresh repo with an untracked file = a dirty base worktree.
            let ok = std::process::Command::new("git")
                .arg("init")
                .current_dir(&root)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            assert!(ok, "git init failed");
            std::fs::write(root.join("uncommitted.txt"), "dirty").unwrap();

            let pty = FakePty::new();
            let tabs = vec![tab_state("t1", "fix", "claude")];
            let state = CoreProjectState {
                version: STATE_VERSION,
                project_root_relative: ".".to_string(),
                base_branch: "main".to_string(),
                tabs,
            };
            let mut app = AppState::new(
                Config::default(),
                state,
                &root,
                root.join(".flightdeck/state.json"),
            );
            let _h = pty.queue_session();
            app.tabs[0]
                .session
                .spawn_primary(&pty, "agent", &[], &root, PtySize::default())
                .unwrap();
            let mut workspace = workspace_rooted(app, root);
            let fs = FakeFs::new();
            let clock = FakeClock::default();
            let container = FakeContainerRuntime::new();
            let command = crate::testing::FakeCommandRunner::new();
            let env = Env {
                fs: &fs,
                pty: &pty,
                clock: &clock,
                container: &container,
                command: &command,
            };

            let mut bridge = RemoteBridge::passthrough(0);
            let cmd = PhoneCommand {
                command_id: CommandId::new("c-merge"),
                issued_at_ms: 0,
                body: CommandBody::GitMergeBack {
                    session_id: SessionId::new("t1"),
                },
            };
            let acks = run_command(&mut bridge, &mut workspace, &env, 1, &cmd);
            assert_eq!(acks.len(), 1);
            assert_eq!(
                acks[0].outcome,
                CommandOutcome::Rejected,
                "dirty base must reject, not apply: {:?}",
                acks[0].message
            );
            let msg = acks[0].message.as_deref().unwrap();
            assert!(msg.contains("Local merge is disabled"), "{msg}");
            // The tab still exists — nothing was merged or torn down.
            assert_eq!(workspace.projects[0].state.tabs.len(), 1);
        }

        /// Merge-back whose git backend errors outright (no repo at the root)
        /// acks as failed — never silently applied.
        #[test]
        fn git_merge_back_git_error_acks_failed() {
            let pty = FakePty::new();
            let (app, _handles) = app_with_tabs(
                Config::default(),
                vec![tab_state("t1", "fix", "claude")],
                &pty,
            );
            // "/repo" does not exist, so every git call errors.
            let mut workspace = workspace_with(app);
            let fs = FakeFs::new();
            let clock = FakeClock::default();
            let container = FakeContainerRuntime::new();
            let command = crate::testing::FakeCommandRunner::new();
            let env = Env {
                fs: &fs,
                pty: &pty,
                clock: &clock,
                container: &container,
                command: &command,
            };

            let mut bridge = RemoteBridge::passthrough(0);
            let cmd = PhoneCommand {
                command_id: CommandId::new("c-merge"),
                issued_at_ms: 0,
                body: CommandBody::GitMergeBack {
                    session_id: SessionId::new("t1"),
                },
            };
            let acks = run_command(&mut bridge, &mut workspace, &env, 1, &cmd);
            assert_eq!(acks[0].outcome, CommandOutcome::Failed);
        }

        // --- remote shell bridge ---------------------------------------------

        /// Decode every [`DesktopToPhone`] message out of the sent envelopes.
        fn decode_msgs(sent: &[RemoteOutbound]) -> Vec<DesktopToPhone> {
            sent.iter()
                .filter_map(|o| match o {
                    RemoteOutbound::SendEnvelope { ciphertext, .. } => {
                        let bytes = STANDARD.decode(ciphertext).unwrap();
                        serde_json::from_slice::<DesktopToPhone>(&bytes).ok()
                    }
                    _ => None,
                })
                .collect()
        }

        /// End-to-endish shell round trip: sealed ShellOpen + ShellInput
        /// envelopes through `handle_inbound` → drain → the FakePty received
        /// the input bytes → scripted PTY output → drain tees it → `tick`
        /// flushes sealed ShellOutput/ShellEvent envelopes. Then interrupt,
        /// the one-shell cap, close, and input-after-close.
        #[test]
        fn shell_open_input_output_interrupt_close_round_trip() {
            use flightdeck_remote_protocol::{ShellEventKind, ShellId};

            let pty = FakePty::new();
            let (app, _handles) = app_with_tabs(
                Config::default(),
                vec![tab_state("t1", "fix", "claude")],
                &pty,
            );
            let mut workspace = workspace_with(app);
            let fs = FakeFs::new();
            let clock = FakeClock::default();
            let container = FakeContainerRuntime::new();
            let command = crate::testing::FakeCommandRunner::new();
            let env = Env {
                fs: &fs,
                pty: &pty,
                clock: &clock,
                container: &container,
                command: &command,
            };
            let mut bridge = RemoteBridge::passthrough(0);

            // The child session the ShellOpen's spawn will consume.
            let shell_pty = pty.queue_session();

            // 1. ShellOpen — spawns a child shell in the worktree, sized to
            //    the phone's geometry, and acks applied.
            let open = PhoneCommand {
                command_id: CommandId::new("c-open"),
                issued_at_ms: 0,
                body: CommandBody::ShellOpen {
                    session_id: SessionId::new("t1"),
                    shell_id: ShellId::new("s1"),
                    cols: 100,
                    rows: 30,
                },
            };
            let acks = run_command(&mut bridge, &mut workspace, &env, 1, &open);
            assert_eq!(acks[0].outcome, CommandOutcome::Applied, "{:?}", acks[0]);
            assert_eq!(
                workspace.projects[0].state.tabs[0].session.child_count(),
                1,
                "a child shell terminal was spawned"
            );
            assert!(shell_pty
                .resizes()
                .iter()
                .any(|s| s.cols == 100 && s.rows == 30));

            // 2. A second open for the same session hits the one-shell cap.
            let open2 = PhoneCommand {
                command_id: CommandId::new("c-open2"),
                issued_at_ms: 0,
                body: CommandBody::ShellOpen {
                    session_id: SessionId::new("t1"),
                    shell_id: ShellId::new("s2"),
                    cols: 80,
                    rows: 24,
                },
            };
            let acks = run_command(&mut bridge, &mut workspace, &env, 2, &open2);
            assert_eq!(acks[0].outcome, CommandOutcome::Rejected);
            assert!(acks[0].message.as_deref().unwrap().contains("already open"));
            assert_eq!(
                workspace.projects[0].state.tabs[0].session.child_count(),
                1,
                "the cap must refuse before spawning"
            );

            // 3. ShellInput — the exact bytes land on the child PTY.
            let input = PhoneCommand {
                command_id: CommandId::new("c-input"),
                issued_at_ms: 0,
                body: CommandBody::ShellInput {
                    session_id: SessionId::new("t1"),
                    shell_id: ShellId::new("s1"),
                    data: "echo hi\n".to_string(),
                },
            };
            let acks = run_command(&mut bridge, &mut workspace, &env, 3, &input);
            assert_eq!(acks[0].outcome, CommandOutcome::Applied);
            assert_eq!(shell_pty.input(), b"echo hi\n".to_vec());

            // 4. Scripted PTY output → drain tees it into the shell manager →
            //    tick flushes it as a sealed ShellOutput envelope (plus the
            //    queued `opened` lifecycle event).
            shell_pty.push_output(b"hi\r\n".to_vec());
            {
                let p = &mut workspace.projects[0];
                drain_pty_output(&mut p.state, 1_000, |sid, which, bytes| {
                    if let Some(ci) = which {
                        bridge.shell_pump(sid, ci, bytes);
                    }
                });
            }
            let mut sent: Vec<RemoteOutbound> = Vec::new();
            {
                let views: Vec<ProjectView> = workspace
                    .projects
                    .iter()
                    .map(|p| ProjectView {
                        id: ProjectId::new(p.name.clone()),
                        name: &p.name,
                        state: &p.state,
                        cache: &p.cache,
                    })
                    .collect();
                bridge.tick(&views, 1_000, &mut |o| sent.push(o));
            }
            let msgs = decode_msgs(&sent);
            let opened = msgs.iter().any(|m| {
                matches!(
                    m,
                    DesktopToPhone::ShellEvent(e)
                        if e.shell_id == ShellId::new("s1")
                            && matches!(e.kind, ShellEventKind::Opened { cols: 100, rows: 30 })
                )
            });
            assert!(opened, "opened event flushed: {msgs:?}");
            let output = msgs.iter().find_map(|m| match m {
                DesktopToPhone::ShellOutput(o) => Some(o),
                _ => None,
            });
            let output = output.expect("a ShellOutput envelope was flushed");
            assert_eq!(output.session_id, SessionId::new("t1"));
            assert_eq!(output.shell_id, ShellId::new("s1"));
            assert_eq!(output.seq, 1);
            assert_eq!(output.data, "hi\r\n");

            // 5. ShellInterrupt → Ctrl-C on the child PTY.
            let interrupt = PhoneCommand {
                command_id: CommandId::new("c-int"),
                issued_at_ms: 0,
                body: CommandBody::ShellInterrupt {
                    session_id: SessionId::new("t1"),
                    shell_id: ShellId::new("s1"),
                },
            };
            let acks = run_command(&mut bridge, &mut workspace, &env, 4, &interrupt);
            assert_eq!(acks[0].outcome, CommandOutcome::Applied);
            assert_eq!(shell_pty.ctrl_c_count(), 1);

            // 6. ShellClose → the child is terminated and removed; the closed
            //    event is flushed on the next tick.
            let close = PhoneCommand {
                command_id: CommandId::new("c-close"),
                issued_at_ms: 0,
                body: CommandBody::ShellClose {
                    session_id: SessionId::new("t1"),
                    shell_id: ShellId::new("s1"),
                },
            };
            let acks = run_command(&mut bridge, &mut workspace, &env, 5, &close);
            assert_eq!(acks[0].outcome, CommandOutcome::Applied);
            assert!(shell_pty.terminated());
            assert_eq!(workspace.projects[0].state.tabs[0].session.child_count(), 0);
            let mut sent: Vec<RemoteOutbound> = Vec::new();
            {
                let views: Vec<ProjectView> = workspace
                    .projects
                    .iter()
                    .map(|p| ProjectView {
                        id: ProjectId::new(p.name.clone()),
                        name: &p.name,
                        state: &p.state,
                        cache: &p.cache,
                    })
                    .collect();
                bridge.tick(&views, 2_000, &mut |o| sent.push(o));
            }
            let msgs = decode_msgs(&sent);
            assert!(
                msgs.iter().any(|m| matches!(
                    m,
                    DesktopToPhone::ShellEvent(e) if matches!(e.kind, ShellEventKind::Closed)
                )),
                "closed event flushed: {msgs:?}"
            );

            // 7. Input to the closed shell is rejected honestly.
            let stale = PhoneCommand {
                command_id: CommandId::new("c-stale"),
                issued_at_ms: 0,
                body: CommandBody::ShellInput {
                    session_id: SessionId::new("t1"),
                    shell_id: ShellId::new("s1"),
                    data: "ls\n".to_string(),
                },
            };
            let acks = run_command(&mut bridge, &mut workspace, &env, 6, &stale);
            assert_eq!(acks[0].outcome, CommandOutcome::Rejected);
            assert!(acks[0]
                .message
                .as_deref()
                .unwrap()
                .contains("no open shell"));

            // 8. After close, the slot is free: a fresh open succeeds.
            pty.queue_session();
            let reopen = PhoneCommand {
                command_id: CommandId::new("c-reopen"),
                issued_at_ms: 0,
                body: CommandBody::ShellOpen {
                    session_id: SessionId::new("t1"),
                    shell_id: ShellId::new("s3"),
                    cols: 80,
                    rows: 24,
                },
            };
            let acks = run_command(&mut bridge, &mut workspace, &env, 7, &reopen);
            assert_eq!(acks[0].outcome, CommandOutcome::Applied, "{:?}", acks[0]);
        }

        /// A remote shell whose process exits is reported once as an `exited`
        /// event; output stops but the slot stays until an explicit close.
        #[test]
        fn shell_exit_is_reported_via_poll() {
            use flightdeck_remote_protocol::{ShellEventKind, ShellId};

            let pty = FakePty::new();
            let (app, _handles) = app_with_tabs(
                Config::default(),
                vec![tab_state("t1", "fix", "claude")],
                &pty,
            );
            let mut workspace = workspace_with(app);
            let fs = FakeFs::new();
            let clock = FakeClock::default();
            let container = FakeContainerRuntime::new();
            let command = crate::testing::FakeCommandRunner::new();
            let env = Env {
                fs: &fs,
                pty: &pty,
                clock: &clock,
                container: &container,
                command: &command,
            };
            let mut bridge = RemoteBridge::passthrough(0);

            let shell_pty = pty.queue_session();
            let open = PhoneCommand {
                command_id: CommandId::new("c-open"),
                issued_at_ms: 0,
                body: CommandBody::ShellOpen {
                    session_id: SessionId::new("t1"),
                    shell_id: ShellId::new("s1"),
                    cols: 80,
                    rows: 24,
                },
            };
            let acks = run_command(&mut bridge, &mut workspace, &env, 1, &open);
            assert_eq!(acks[0].outcome, CommandOutcome::Applied);

            // The shell process exits; the per-tick poll (inside the command
            // service pass) detects it.
            shell_pty.set_state(ProcessState::Exited(0));
            let mut ledger = CommandLedger::new();
            let mut first_tasks: Vec<PendingFirstTask> = Vec::new();
            let mut sent: Vec<RemoteOutbound> = Vec::new();
            service_remote_commands(
                &mut bridge,
                &mut ledger,
                &mut first_tasks,
                &mut workspace,
                &env,
                2_000,
                &mut |o| sent.push(o),
            );
            let mut sent: Vec<RemoteOutbound> = Vec::new();
            {
                let views: Vec<ProjectView> = workspace
                    .projects
                    .iter()
                    .map(|p| ProjectView {
                        id: ProjectId::new(p.name.clone()),
                        name: &p.name,
                        state: &p.state,
                        cache: &p.cache,
                    })
                    .collect();
                bridge.tick(&views, 2_000, &mut |o| sent.push(o));
            }
            let msgs = decode_msgs(&sent);
            assert!(
                msgs.iter().any(|m| matches!(
                    m,
                    DesktopToPhone::ShellEvent(e)
                        if matches!(e.kind, ShellEventKind::Exited { code: Some(0) })
                )),
                "exited event flushed: {msgs:?}"
            );
        }
    }

    /// Switching to a background project must resume its recovered agents on
    /// demand. Regression guard for #26: startup resumes only the active
    /// project, so without a resume on switch a background project's tabs stay
    /// unspawned and the pane hangs on "(terminal starting…)".
    mod project_switch_resume {
        use super::*;
        use crate::contracts::{
            AgentDef, ProjectState as CoreProjectState, StatusPatterns, TabState, STATE_VERSION,
        };

        /// A launchable agent backed by a real executable in `dir` (spawning
        /// goes through `validate_agent`, which checks the binary exists).
        fn real_agent(dir: &TempDir, key: &str) -> AgentDef {
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

        fn config_with(agent: AgentDef) -> Config {
            let mut config = Config::default();
            config.ui.default_agent = agent.key.clone();
            config.agents.insert(agent.key.clone(), agent);
            config
        }

        fn tab(id: &str, name: &str, agent: &str) -> TabState {
            TabState {
                id: id.to_string(),
                name: name.to_string(),
                slug: name.to_string(),
                agent: agent.to_string(),
                branch: format!("{name}-branch"),
                worktree_path_relative: format!("worktrees/{name}"),
                base_branch: "main".to_string(),
                base_commit_sha: "abc123".to_string(),
                created_at: "2026-01-01T00:00:00Z".to_string(),
                attached_existing_branch: false,
                recovered: true,
                last_known_status: "unknown".to_string(),
                manual_status: None,
                containerized: false,
                container_image: None,
                runs_on_base: false,
                resume_args: Vec::new(),
            }
        }

        /// A project whose single recovered tab has an unspawned (NotStarted)
        /// primary — exactly the state of a background project loaded from the
        /// workspace file before it is first switched to.
        fn recovered_project(name: &str, root: &str, config: Config) -> Project {
            let agent = config.ui.default_agent.clone();
            let state = CoreProjectState {
                version: STATE_VERSION,
                project_root_relative: ".".to_string(),
                base_branch: "main".to_string(),
                tabs: vec![tab(&format!("{name}-t1"), name, &agent)],
            };
            let mut app = AppState::new(
                config,
                state,
                root,
                format!("{root}/.flightdeck/state.json"),
            );
            app.set_pty_size(PtySize { rows: 24, cols: 80 });
            let (create_tx, create_rx) = std::sync::mpsc::channel();
            let (status_tx, status_rx) = std::sync::mpsc::channel();
            Project {
                name: name.to_string(),
                git: GitCli::new(PathBuf::from(root)),
                state: app,
                cache: GitStatusCache::new(),
                create_tx,
                create_rx,
                status_tx,
                status_rx,
                status_in_flight: false,
                git_lock: Arc::new(Mutex::new(())),
            }
        }

        #[test]
        fn shift_right_resumes_background_projects_agents() {
            use crate::contracts::ProcessState;
            let dir = TempDir::new().unwrap();
            let pty = FakePty::new();
            // Both projects' worktrees exist on disk so resume can spawn.
            let fs = FakeFs::new()
                .with_dir("/repo0/worktrees/proj0")
                .with_dir("/repo1/worktrees/proj1");
            let clock = FakeClock::default();
            let container = crate::testing::FakeContainerRuntime::new();
            let command = crate::testing::FakeCommandRunner::new();
            let env = Env {
                fs: &fs,
                pty: &pty,
                clock: &clock,
                container: &container,
                command: &command,
            };

            let mut workspace = Workspace {
                projects: vec![
                    recovered_project("proj0", "/repo0", config_with(real_agent(&dir, "claude"))),
                    recovered_project("proj1", "/repo1", config_with(real_agent(&dir, "claude"))),
                ],
                active: 0,
            };

            // Mirror startup: only the active project's agents are resumed.
            resume_active_project_agents(&mut workspace, &env);
            assert_eq!(
                workspace.projects[1].state.tabs[0].session.primary_state(),
                ProcessState::NotStarted,
                "background project must start unspawned",
            );

            // Shift+Right switches to project 1 — this must resume its agent.
            let key = KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT);
            let mut ui = Ui::default();
            handle_key(key, &mut workspace, &env, &mut ui).unwrap();

            assert_eq!(workspace.active, 1, "switched to the background project");
            assert!(
                workspace.projects[1].state.tabs[0].session.active().is_some(),
                "switching must resume the background project's primary (was hanging on '(terminal starting…)')",
            );
        }
    }
}
