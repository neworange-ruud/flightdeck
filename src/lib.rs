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
pub mod web;

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
use crate::config::load::{
    global_config_path, load_config, load_layered_config, serialize_global_config,
    set_project_default_base,
};
use crate::config::schema::{default_config, default_global_config};
use crate::contracts::error::{FlightDeckError, Result};
use crate::contracts::real::{RealClock, RealFs, SystemCommandRunner};
use crate::contracts::{
    AgentDef, Clock, CommandRunner, Config, ContainerRuntime, FileSystem, GitExecutor,
    ManualStatus, Notifier, ProcessState, PtyBackend, PtySize, STATE_VERSION,
};
use crate::fs::ignore::ensure_flightdeck_gitignore;
use crate::fs::paths::to_absolute;
use crate::git::repo::{detect_base_branch, GitCli};
use crate::git::status::{collect_status, WorktreeStatus};
use crate::notify::SystemNotifier;
use crate::persistence::project_state::{default_state, load_state, save_state};
use crate::persistence::recovery::{recover, RecoveryReport};
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

    let argv: Vec<String> = std::env::args().collect();
    let isolated = parse_isolated(&argv)?;
    let isolated_root: Option<PathBuf> = if isolated {
        Some(isolated_status_dir())
    } else {
        None
    };

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
    let launch = open_project(&env, &cwd, isolated_root.as_deref()).map_err(|e| {
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
    // An isolated run is exactly one project by definition: skip the reopen
    // loop entirely (SPECS §32). This must not merely pass `None` through to
    // `open_project` for each remembered project and discard the result —
    // `open_project` runs `startup` (init, config writes, `.gitignore`
    // update, state load + recovery) against every one of them, including
    // the launch repo's own root when it is already in the workspace file
    // (the normal case for a repo the user has opened before), before the
    // `contains_root` guard below throws the duplicate away. Binding
    // `ws_path` to `None` also makes teardown skip writing the workspace
    // file for free (Task 8).
    let ws_path = if isolated {
        None
    } else {
        workspace_state_path()
    };
    if let Some(ref wp) = ws_path {
        if let Ok(saved) = load_workspace(&fs, wp) {
            for p in &saved.projects {
                let pr = Path::new(p);
                if !fs.is_dir(pr) {
                    continue;
                }
                match open_project(&env, pr, None) {
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

    // Undo the modes above if we panic. `ratatui::try_init` installs a hook that
    // leaves raw mode and the alternate screen, but it knows nothing about the
    // mouse capture, bracketed paste, keyboard flags, or title we enabled after
    // it — and the teardown below is unwound straight past. Without this, a panic
    // drops the user at a shell whose every mouse movement arrives as escape
    // sequences printed as text. Chained ahead of ratatui's hook, which still
    // restores the screen and prints the panic afterwards.
    {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            // A panic from inside the VT parser is caught and handled by
            // `Terminal::process_output`, which rebuilds the parser and carries
            // on. Tearing the terminal down and printing a backtrace over a live
            // UI for a panic that never reaches the top of the stack would turn a
            // recovered pane into a dead session.
            if crate::terminal::session::parser_panic_expected() {
                return;
            }
            restore_terminal_modes(keyboard_enhanced);
            previous(info);
        }));
    }

    // Seed the PTY size from the terminal viewport (not the whole screen) so
    // agents wrap at the right width — for every open project.
    if let Ok(size) = terminal.size() {
        let full = PtySize {
            rows: size.height,
            cols: size.width,
        };
        for p in workspace.projects.iter_mut() {
            let reserve = crate::tui::mode_style::border_enabled(&p.state.config.ui);
            let vp = viewport_pty_size(full, p.state.mode(), reserve);
            p.state.set_pty_size(vp);
        }
    }

    // Resume: start the primary agent for every recovered/loaded tab whose
    // worktree still exists (best effort) — for the ACTIVE (launched) project
    // ONLY. Other projects reopened from the workspace file are shown but their
    // agents are not auto-resumed; switching/opening one resumes it on demand
    // (see the open-project flow). Done here, after the viewport size is known,
    // rather than in `recover`/`AppState::new` which never spawn.
    // An isolated run's single session failing to start is fatal (not a
    // `warnings.push`): `AppState::warnings` has no renderer anywhere in
    // `src/tui/`, so surfacing it that way would launch a blank TUI with no
    // session and no visible message. There is nothing useful to show, so
    // the error is threaded past `event_loop` instead, to reach `main` as
    // `flightdeck error: <msg>` — while still running the full teardown below
    // (terminal restore, session termination) exactly like any other
    // `loop_result` error.
    let isolated_session_result = {
        let active = workspace.active;
        let p = &mut workspace.projects[active];
        let services = env.services(&p.git);
        if isolated {
            // One fresh session; nothing to resume, because nothing was
            // recovered (SPECS §32).
            start_isolated_session(&mut p.state, &services)
        } else {
            let _ = p.state.resume_agents(&services);
            Ok(())
        }
    };

    let notifier = SystemNotifier;
    let loop_result = match isolated_session_result {
        Err(e) => Err(e),
        Ok(()) => event_loop(&mut terminal, &mut workspace, &env, &notifier),
    };

    // CLEAN TEARDOWN (SPECS §25). Persist FIRST, before touching the terminal:
    // on a severed terminal (Konsole/window close closes stdin+stdout+stderr) the
    // terminal-restore step is worthless anyway, and it must never run ahead of
    // the save — otherwise a failed restore that writes to the dead stderr can
    // take the process down before the state is written.
    let mut persist_result = Ok(());
    if !isolated {
        for p in workspace.projects.iter() {
            let services = env.services(&p.git);
            if let Err(e) = persist_quietly(&p.state, &services) {
                persist_result = Err(e);
            }
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
    restore_terminal_modes(keyboard_enhanced);
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

    // Remove the temp status directory only after every agent is dead —
    // otherwise a hook still running mid-teardown could recreate files under
    // a directory just deleted, or write into a partially-removed tree.
    if isolated {
        cleanup_isolated_run(&fs, &isolated_status_dir());
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

/// The temp directory an isolated run keeps its agent status plumbing in
/// (SPECS §32). Per-process so two concurrent isolated runs cannot collide.
fn isolated_status_dir() -> PathBuf {
    std::env::temp_dir().join(format!("flightdeck-isolated-{}", std::process::id()))
}

/// Create the one session an isolated run consists of: the default agent, in
/// the repository root, on the branch already checked out, with no worktree
/// and no git mutation (SPECS §32). The base-tab path's [`WorktreeJob`] has
/// `needs_create == false`, so [`materialize_worktree`] is deliberately not
/// called here.
///
/// A `finalize_new_tab` failure (missing container image, PTY spawn error,
/// ...) must not leave the placeholder behind in `TabPhase::Creating` — the
/// same contract `cmd_new_agent_tab` and `drain_create_outcomes` already
/// honour (`src/app/state.rs:906-908`) applies here too, so the placeholder
/// is removed via `fail_new_tab` before the error is propagated.
fn start_isolated_session(state: &mut AppState, services: &Services) -> Result<()> {
    let job = state.begin_new_agent_tab_ex("", None, true, services)?;
    debug_assert!(
        !job.needs_create,
        "an isolated session never creates a worktree"
    );
    if let Err(e) = state.finalize_new_tab(&job.tab_id, services) {
        state.fail_new_tab(&job.tab_id);
        return Err(e);
    }
    Ok(())
}

/// Compute the effective config the same way a normal run would — global base
/// layered under project overrides (SPECS §8) — without writing anything to
/// disk. Used by an isolated run, which reads existing config but never
/// creates a global base file. When no global file exists on disk, the
/// in-memory default global is serialized and re-parsed into a table; this
/// reuses the exact code path a normal run's on-disk global base goes
/// through (so the two cannot drift), and runs at most once per process, so
/// the round-trip cost is immaterial.
fn effective_config_without_writing(
    fs: &dyn FileSystem,
    global_path: Option<&Path>,
    config_path: &Path,
) -> Result<Config> {
    // Mirrors `crate::config::load::read_table` (private to that module):
    // a missing file layers as an empty table; when `lenient`, an unparsable
    // file is also treated as empty (with a notice) rather than failing the
    // whole load — used for the global base so a corrupt user-level file
    // never blocks a project's own config, exactly as `load_layered_config`
    // behaves for a normal run.
    let read_table = |path: &Path, lenient: bool| -> Result<toml::Table> {
        if !fs.exists(path) {
            return Ok(toml::Table::new());
        }
        let contents = fs.read_to_string(path)?;
        match crate::config::load::parse_table(&contents) {
            Ok(t) => Ok(t),
            Err(e) if lenient => {
                eprintln!(
                    "FlightDeck: ignoring unparsable global config {}: {e}",
                    path.display()
                );
                Ok(toml::Table::new())
            }
            Err(e) => Err(e),
        }
    };
    let global = match global_path {
        Some(gp) if fs.exists(gp) => read_table(gp, true)?,
        _ => crate::config::load::parse_table(&serialize_global_config(&default_global_config())?)?,
    };
    let project = read_table(config_path, false)?;
    crate::config::load::effective_config(global, project)
}

/// Run the SPECS §7 startup sequence and build the [`AppState`]. Pure of any
/// terminal I/O so it can be exercised with the fakes in [`crate::testing`].
///
/// Steps: detect base branch, first-run init, load config (default fallback),
/// `.gitignore` update + §6 notice, load + recover state, build [`AppState`],
/// and record the §13 dirty-base warning if the base repo is dirty at startup.
///
/// `isolated`: `None` for a normal run. `Some(status_root)` for an isolated
/// run (SPECS §32) — nothing is written under the project (no first-run init,
/// no global config base, no `.gitignore` update, no state load/recovery),
/// though existing config on disk is still read; the agent status plumbing
/// lives at `status_root` instead.
fn startup(
    services: &Services,
    repo_root: &Path,
    cwd: &Path,
    isolated: Option<&Path>,
) -> Result<AppState> {
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
    // Skipped entirely for an isolated run (SPECS §32): nothing is written.
    let project_name = derive_project_name(repo_root);
    if isolated.is_none() {
        initialize(services.fs, repo_root, &project_name, &base_branch)?;
    }

    // Ensure the per-user global base exists (documents every overridable
    // setting), then load the effective config by layering it under this
    // project's overrides (SPECS §8). Fall back to a freshly-built default if
    // loading fails, or when there is no home dir to host a global config.
    //
    // An isolated run never creates the global base file, but still computes
    // the same effective config a normal run would (global base — on disk if
    // present, else the in-memory default — layered under the project's
    // overrides), so a partial project config.toml is honoured even on a
    // machine that has never run FlightDeck normally (SPECS §4).
    let global_path = global_config_path();
    let loaded_config = if isolated.is_none() {
        if let Some(gp) = &global_path {
            let _ = ensure_global_config(services.fs, gp);
        }
        match &global_path {
            Some(gp) => load_layered_config(services.fs, gp, &config_path),
            None => load_config(services.fs, &config_path),
        }
    } else {
        effective_config_without_writing(services.fs, global_path.as_deref(), &config_path)
    };
    let project_config_loaded = loaded_config.is_ok();
    let mut config = loaded_config.unwrap_or_else(|_| default_config(&project_name, &base_branch));
    // With no project setting (notably an isolated first run), use the detected
    // checked-out branch. An explicit setting remains visible even when invalid
    // so startup can warn instead of silently switching the default.
    if pre_configured_base.is_none() {
        config.project.default_base_branch = base_branch.clone();
    }

    // Append .gitignore entries and surface the §6 notice if it changed.
    // Skipped for an isolated run: nothing under the project is touched.
    if isolated.is_none() {
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
    }

    // Load state (default if missing), then recover tabs WITHOUT relaunching
    // agents (SPECS §10). An isolated run reads nothing and recovers nothing:
    // no continuation (SPECS §32).
    let (project_state, report) = if isolated.is_some() {
        (default_state(&base_branch), RecoveryReport::default())
    } else {
        let mut ps =
            load_state(services.fs, &state_path).unwrap_or_else(|_| default_state(&base_branch));
        let migrated_legacy_state = ps.version < STATE_VERSION;
        if migrated_legacy_state && project_config_loaded {
            // Before v2 the top-level state value was the real runtime source of
            // truth, and editing state.json was the only supported workaround
            // for changing it. Preserve that user choice once by migrating it
            // into the now-authoritative committed project config.
            let legacy_base = ps.base_branch.clone();
            if legacy_base != config.project.default_base_branch
                && services.git.branch_exists(&legacy_base).unwrap_or(false)
            {
                let contents = services.fs.read_to_string(&config_path)?;
                services.fs.write(
                    &config_path,
                    &set_project_default_base(&contents, &legacy_base)?,
                )?;
                config.project.default_base_branch = legacy_base;
            }
            ps.version = STATE_VERSION;
        } else if migrated_legacy_state
            && services.git.branch_exists(&ps.base_branch).unwrap_or(false)
        {
            // Keep a v1 user's chosen base in memory when malformed project
            // TOML prevented migration. Startup must remain usable; migration
            // will retry after the user repairs the config.
            config.project.default_base_branch = ps.base_branch.clone();
        }
        // The committed project config is authoritative for the default used by
        // newly-created and newly-discovered tabs. Existing tabs retain their
        // own persisted targets during recovery.
        ps.base_branch = config.project.default_base_branch.clone();
        let report = recover(
            services.fs,
            services.git,
            repo_root,
            &worktrees_root,
            &mut ps,
        )?;
        if migrated_legacy_state && project_config_loaded {
            save_state(services.fs, &state_path, &ps)?;
        }
        (ps, report)
    };

    let mut state = AppState::new(config, project_state, repo_root, &state_path);
    if let Some(status_root) = isolated {
        state.set_isolated(Some(status_root.to_path_buf()));
        // Forced off, not merely defaulted: without this, "no continuation"
        // would hold only until the first Restart Agent replayed a captured
        // resume command (SPECS §32 §4).
        state.config.ui.auto_continue = false;
    }

    if !services
        .git
        .branch_exists(&state.base_branch)
        .unwrap_or(false)
    {
        state.invalid_base_branch = Some(state.base_branch.clone());
        state.warnings.push(format!(
            "Configured default base '{}' is not a local branch. Use 'Change Project Default Base' to select one.",
            state.base_branch
        ));
    }

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

/// Subcommands that configure or inspect FlightDeck and exit without the TUI.
/// Kept next to [`parse_isolated`] so a new subcommand cannot silently become
/// combinable with `--isolated`.
const SUBCOMMANDS: &[&str] = &[
    "setup-status",
    "setup-notifications",
    "setup-update",
    "update",
    "image",
    "doctor",
];

/// Whether this invocation asked for an isolated run (SPECS §32).
///
/// `args` is the full argv, argv[0] included. Isolated mode launches the TUI, so
/// it cannot be combined with a subcommand — that combination is a hard error
/// rather than a silent ignore, because silently dropping the flag would let a
/// user believe a run was isolated when it was not.
fn parse_isolated(args: &[String]) -> Result<bool> {
    let isolated = args.iter().any(|a| a == "--isolated" || a == "-I");
    if !isolated {
        return Ok(false);
    }
    if let Some(sub) = args.iter().find(|a| SUBCOMMANDS.contains(&a.as_str())) {
        return Err(FlightDeckError::Config(format!(
            "--isolated launches the TUI and cannot be combined with the '{sub}' subcommand"
        )));
    }
    Ok(true)
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
    println!("    -I, --isolated   Throwaway run: one fresh session in the current");
    println!("                     directory. No continuation, no worktrees, no");
    println!("                     other projects, and nothing written to the project.");
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
    /// (radio, ↑/↓), type a new branch name, select an existing local branch,
    /// or run directly from the base branch. Tab cycles those target modes.
    /// Confirming (Enter) dispatches the async new-tab flow.
    NewAgentForm {
        /// `(key, display_name)` of each registered agent, in registry order.
        agents: Vec<(String, String)>,
        /// Index into `agents` of the highlighted radio option.
        selected: usize,
        /// The branch/tab name being typed. Ignored when `run_on_base`.
        branch: String,
        /// Local branches available to the existing-branch target mode.
        existing_branches: Vec<String>,
        /// Index into the filtered `existing_branches` list.
        branch_selected: usize,
        /// When true, the input filters `existing_branches` instead of naming a
        /// newly-created branch.
        use_existing_branch: bool,
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
    /// Pick a local branch to become the project default for future agents.
    /// Existing agents retain their individually-persisted target branches.
    ChangeProjectBase {
        branches: Vec<String>,
        filter: String,
        selected: usize,
    },
    /// Confirm unpairing the phone (FlightDeck Remote). On confirm the event
    /// loop forgets the pairing and reverts to the passthrough sealer.
    UnpairConfirm,
    /// Confirm quitting FlightDeck: every agent it is running is stopped.
    ///
    /// Only an **unconfirmed** `Command::Quit` opens this, and the only row that
    /// carries one is the browser's (D16 — a `host only` badge is not enough for
    /// quit). The desktop's `Ctrl-q` and its palette row dispatch the confirmed
    /// value and never see it, which is why SPECS §23's "quit just quits" is
    /// unchanged for the person at the keyboard. It is a D13 dialog like any
    /// other once open: shared, origin-tagged, cancellable from either surface —
    /// and from a browser its `y` is behind artboard 1g's typed-name step.
    QuitConfirm,
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
    /// Set by the "Start Web Interface" palette action; the event loop (which
    /// owns the listener) starts it next tick (D10).
    pending_web_start: bool,
    /// Set by "Stop Web Interface"; the event loop drains the viewers and
    /// releases the listener (Q5).
    pending_web_stop: bool,
    /// Whether the embedded web server is currently listening. Refreshed each
    /// tick and read when opening the palette, so exactly one of the two
    /// lifecycle commands is ever offered — the same gating idiom as
    /// [`Ui::remote_paired`].
    web_running: bool,
    /// What the last dispatch produced, recorded so a browser's `Command` frame
    /// can be acked honestly instead of being told `Applied` while the desktop
    /// showed the user a refusal.
    ///
    /// The desktop reads its outcome off the screen — that is what
    /// [`Ui::message`] is for — but a browser cannot, and
    /// `specs/WEB_INTERFACE.md` §5.1 does not allow a guess. So the classifying
    /// sites ([`apply_effect`], [`dispatch_command`]'s error arm, and
    /// [`switch_project`]'s isolated refusal) record what they decided here, and
    /// [`run_web_command`] clears it before dispatching and reads it after.
    /// Overwritten constantly by the desktop's own keypresses, which is fine:
    /// only the value written during one web dispatch is ever read.
    web_outcome: Option<WebDispatch>,
    /// D13: the origin to stamp on the next dialog this build opens.
    ///
    /// `Some` only for the duration of one [`run_web_command`] dispatch, exactly
    /// like [`Ui::web_outcome`] — a dialog opened while a browser's frame is
    /// being applied was opened *by* that browser, and every other dialog was
    /// opened at this keyboard. Set and cleared in one place, so no prompt-
    /// opening site has to know a browser exists.
    web_dialog_origin: Option<crate::web::protocol::DialogOrigin>,
    /// Dialogs that reached a real decision this tick, oldest first.
    ///
    /// The published-state diff (`crate::web::stream::deltas`) can see that a
    /// dialog *went away* but not why, so it reports [`DialogOutcome::Superseded`]
    /// — honest for a dialog that was replaced, wrong for one somebody answered.
    /// [`handle_prompt_key`] records the real outcome here and the event loop
    /// upgrades the diff's frame with it (`resolve_dialog_outcomes`). Drained
    /// every tick.
    dialog_decisions: Vec<(
        crate::web::protocol::DialogId,
        crate::web::protocol::DialogOutcome,
    )>,
    /// Monotonic counter behind [`Ui::mint_dialog_id`]. Never reset, so an id is
    /// never reused within a process and a stale answer cannot land on a new
    /// dialog by matching its id.
    dialog_seq: u64,
}

/// What one dispatch produced, in the vocabulary a
/// [`crate::web::protocol::Ack`] needs (see [`Ui::web_outcome`]).
#[derive(Debug, Clone)]
enum WebDispatch {
    /// It happened. The string is the sentence the desktop showed, if any.
    Applied(Option<String>),
    /// A safety guard said no, with its reason. Nothing happened.
    Refused(String),
    /// The dispatch itself failed, with the error. Nothing happened.
    Failed(String),
}

/// A queued worktree-creation job plus the index of the project that owns it.
struct PendingJob {
    project: usize,
    job: WorktreeJob,
}

/// A prompt plus the modal dialog rendered for it (title + buttons).
///
/// **This is D13's "no new state".** A dialog on the wire is this struct read
/// out (see [`web_dialog_view`]); a browser answering one is a keypress fed into
/// [`handle_prompt_key`]. There is deliberately no second dialog store, no
/// browser-only dialog kind, and no path that performs a dialog's action
/// twice — the failure mode `specs/WEB_INTERFACE.md` §1 exists to prevent.
struct PromptState {
    prompt: Prompt,
    dialog: Dialog,
    /// Stable identity for the life of this prompt, minted by [`start_prompt`].
    /// The browser names it when it answers, so an answer that arrives for a
    /// dialog that has since been replaced is refused instead of applied to
    /// whatever is on screen now.
    id: crate::web::protocol::DialogId,
    /// D13: who asked for it. [`DialogOrigin::Desktop`] for the person at this
    /// keyboard; [`DialogOrigin::Browser`] when a `Command` frame opened it, in
    /// which case the desktop renders the origin line and the browser does not
    /// (it already knows — it asked).
    origin: crate::web::protocol::DialogOrigin,
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

    /// A fresh dialog id (D13). Process-unique by construction.
    fn mint_dialog_id(&mut self) -> crate::web::protocol::DialogId {
        self.dialog_seq += 1;
        crate::web::protocol::DialogId::new(format!("dialog-{}", self.dialog_seq))
    }

    /// The open dialog's id, if any.
    fn dialog_id(&self) -> Option<crate::web::protocol::DialogId> {
        self.prompt.as_ref().map(|p| p.id.clone())
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

// ---------------------------------------------------------------------------
// FlightDeck Web: the embedded browser surface (`specs/WEB_INTERFACE.md`)
// ---------------------------------------------------------------------------

/// Everything the event loop needs to run the embedded web interface.
///
/// One of these exists for the whole process whether or not the server is
/// running, because the pieces have different lifetimes: the credential store
/// is shared with the server so a revocation lands on the very next connection
/// (D5), and the inbound channel and the replay registry must survive
/// `Stop Web Interface` followed by `Start Web Interface` — recreating the
/// registry would throw away every terminal's scrollback on a restart.
///
/// **The tee only runs while the server does.** A replay ring is
/// `[web] replay_bytes` (256 KiB by default) per live terminal, and `[web]
/// enabled` is off by default, so a user who never opens a browser must not pay
/// for buffers nobody will read. The accepted cost is the other way round: the
/// first viewer after `Start Web Interface` paints from the moment the server
/// started, not from the start of the session. That is honest on the wire — the
/// stream really does begin there, `offset` and `replay_from` say so — and Q2
/// already accepts one imperfect first repaint. Seeding the ring from the
/// desktop's `vt100` grid was considered and rejected: that grid is a *parse
/// result*, and writing it into a byte stream would be inventing bytes the PTY
/// never emitted.
struct WebSurface {
    /// Shared with the server (D5): the TUI mints bootstrap codes and revokes,
    /// the server verifies on every connection.
    credentials: Arc<Mutex<crate::web::credentials::CredentialStore>>,
    /// Per-terminal replay rings plus the per-viewer input watermark.
    streams: crate::web::stream::TerminalStreams,
    /// D11's activity feed: the browser's **entire** substitute for OS
    /// notifications, because Web Push is structurally blocked under D1.
    ///
    /// Recorded into **whether or not the server is running**, unlike the replay
    /// rings above. The two costs are not comparable: a ring is 256 KiB per live
    /// terminal, while the feed is bounded at
    /// [`crate::web::activity::MAX_EVENTS`] small events for the whole process
    /// — tens of kilobytes at worst, and proportional to transitions that
    /// actually happened rather than to bytes an agent happened to print. Paying
    /// it always is what makes `Start Web Interface` → open a tab land on
    /// history instead of silence, which is the requirement D11 exists for; and
    /// it is why the feed survives a `Stop` / `Start` cycle for the same reason
    /// the rings do.
    activity: crate::web::activity::ActivityStore,
    /// Finished feed rows held back while a one-shot `git status` counts the
    /// files their session touched (`crate::web::activity`'s "the finish edge
    /// asks git itself"). Drained by [`WebSurface::drain_finish_counts`], which
    /// also lets go of anything that waited too long — a held row always ends
    /// up in `activity`, with the clause or without it.
    pending_finishes: crate::web::activity::PendingFinishes,
    /// Handed to `server::start`; kept so a restart reuses the same channel.
    inbound_tx: Sender<crate::web::server::WebInbound>,
    /// Drained every tick.
    inbound_rx: Receiver<crate::web::server::WebInbound>,
    /// Where the finish-edge git workers report back. Kept on the surface
    /// rather than per project because the request id, not the channel, says
    /// which row an answer belongs to — so a background project's worker needs
    /// no plumbing of its own.
    count_tx: Sender<FinishCount>,
    count_rx: Receiver<FinishCount>,
    /// `Some` while the server is running.
    handle: Option<crate::web::server::WebServerHandle>,
    /// The last state published, so `publish_state` is paired with the deltas
    /// that describe the difference rather than a server-side guess.
    published: crate::web::server::HostState,
}

impl WebSurface {
    /// Build the surface. Does **not** start the server — `[web] enabled` is
    /// the TUI's decision to make (D10), and the server deliberately ignores it.
    fn new(config: &crate::contracts::domain::WebConfig) -> WebSurface {
        let path = crate::web::credentials::web_credentials_path()
            // No `$HOME`: run without persistence rather than failing, the same
            // idiom as `remote.json`. The token then lasts one session.
            .unwrap_or_else(|| std::path::PathBuf::from("web.json"));
        let store = crate::web::credentials::CredentialStore::open(
            Arc::new(RealFs),
            Arc::new(RealClock),
            path,
        );
        let (inbound_tx, inbound_rx) = std::sync::mpsc::channel();
        let (count_tx, count_rx) = std::sync::mpsc::channel();
        WebSurface {
            credentials: Arc::new(Mutex::new(store)),
            streams: crate::web::stream::TerminalStreams::new(config.replay_bytes),
            activity: crate::web::activity::ActivityStore::new(),
            pending_finishes: crate::web::activity::PendingFinishes::new(),
            inbound_tx,
            inbound_rx,
            count_tx,
            count_rx,
            handle: None,
            published: crate::web::server::HostState::default(),
        }
    }

    fn running(&self) -> bool {
        self.handle.is_some()
    }

    /// Start listening, returning the bound address and whether it is reachable
    /// off this machine (D5 — the caller warns when it is).
    fn start(
        &mut self,
        config: &crate::contracts::domain::WebConfig,
        initial: crate::web::server::HostState,
    ) -> std::result::Result<
        (std::net::SocketAddr, crate::web::server::BindExposure),
        crate::web::server::StartError,
    > {
        if let Some(handle) = self.handle.as_ref() {
            return Ok((handle.bound_addr(), handle.exposure()));
        }
        self.published = initial.clone();
        let handle = crate::web::server::start(
            config,
            Arc::clone(&self.credentials),
            Arc::new(RealClock),
            initial,
            self.inbound_tx.clone(),
        )?;
        let reported = (handle.bound_addr(), handle.exposure());
        self.handle = Some(handle);
        Ok(reported)
    }

    /// Tell every viewer why the socket is closing, then release the listener
    /// (Q5). The replay rings and watermarks survive, so a later start resumes
    /// rather than restarts.
    fn stop(&mut self, notice: crate::web::server::ShutdownNotice) {
        if let Some(handle) = self.handle.take() {
            handle.stop(notice);
        }
        // Nothing can be attached any more, so no viewport is current. The
        // watermarks stay: they are keyed by viewer id and a returning browser
        // still needs them to avoid retyping its held queue.
        self.published = crate::web::server::HostState::default();
    }

    /// **Test-only seam (debug builds only).** Keep a *known* bootstrap code
    /// live while the server runs, so the Playwright suite can authenticate a
    /// real browser through the real `POST /auth/exchange` (D15).
    ///
    /// Called every tick and idempotent: it mints only when no code is live, so
    /// a code that has just been spent (they are single use) or expired is
    /// replaced and the next test in the run can authenticate too. Nothing else
    /// changes — the exchange, the TTL and both rate limiters are the shipped
    /// ones. See `CredentialStore::mint_fixed_bootstrap_code` for why this
    /// cannot exist in a release binary.
    #[cfg(debug_assertions)]
    fn ensure_test_bootstrap_code(&self, digits: &str) {
        if self.handle.is_none() {
            return;
        }
        if let Ok(mut store) = self.credentials.lock() {
            if store.bootstrap_code().is_none() {
                store.mint_fixed_bootstrap_code(digits);
            }
        }
    }

    /// Record one desktop status change into D11's feed — the second record of
    /// the signal `take_finish_notifications` has just turned into an OS
    /// notification.
    ///
    /// The honesty policy (which reason strings are real, and §5.1's
    /// `unknown → unknown` for an agent with no lifecycle hooks) lives in
    /// [`crate::web::activity::observe`], and the fact it keys off is
    /// [`crate::web::stream::lifecycle_reporting`] — the same helper
    /// `build_web_host_state` uses for the sidebar, so a feed row and a session
    /// row can never disagree about whether an agent reports a lifecycle.
    ///
    /// A *finished* row is the one case that cannot be completed here: artboard
    /// 2e's `finished, 18 files touched` needs a number only git has, so the row
    /// is parked (`pending_finishes`) and a [`FinishCountRequest`] is returned
    /// for the caller to run off the event loop. Everything else — including a
    /// finish edge whose `worktree_abs` is unknown, which is the honest-empty
    /// path — is recorded here and now.
    fn record_transition(
        &mut self,
        clock: &dyn Clock,
        project_id: crate::web::protocol::ProjectId,
        project_name: &str,
        agent: Option<&AgentDef>,
        worktree_abs: Option<PathBuf>,
        change: crate::app::state::TabStatusChange,
    ) -> Option<FinishCountRequest> {
        let observed = crate::web::activity::observe(
            change.was,
            change.now,
            agent
                .map(|def| def.display_name.as_str())
                .unwrap_or(&change.agent_key),
            crate::web::stream::lifecycle_reporting(agent),
        );
        let wants_count = crate::web::activity::wants_file_count(&observed);
        let transition = crate::web::activity::Transition {
            project_id,
            project_name: project_name.to_string(),
            session_id: change.tab_id,
            session_name: change.tab_name,
            from: observed.from,
            to: observed.to,
            manual: observed.manual,
            reason: observed.reason,
        };
        match worktree_abs {
            Some(worktree_abs) if wants_count => {
                let request = self
                    .pending_finishes
                    .park(clock.now_millis() as i64, transition);
                Some(FinishCountRequest {
                    request,
                    worktree_abs,
                })
            }
            _ => {
                self.activity.record(clock, transition);
                None
            }
        }
    }

    /// Record the parked finished rows whose git refresh has answered, then let
    /// go of any that has waited past
    /// [`crate::web::activity::FINISH_COUNT_DEADLINE_MS`].
    ///
    /// Called every tick, before anything reads the feed, so a row is at most
    /// one tick behind the answer it was waiting for.
    fn drain_finish_counts(&mut self, clock: &dyn Clock, now_ms: u64) {
        while let Ok(count) = self.count_rx.try_recv() {
            self.pending_finishes
                .resolve(&mut self.activity, clock, count.request, count.files);
        }
        self.pending_finishes
            .expire(&mut self.activity, clock, now_ms as i64);
    }

    /// The retained feed for a `HostState`, both §5.1 bounds enforced against
    /// `clock` **first**.
    ///
    /// Eviction is a read-time fact by design (see `crate::web::activity`'s
    /// module doc): no background timer exists, so an idle feed only ages itself
    /// down to artboard 2e's "Nothing has changed in 24 hours" because the code
    /// that builds a read view asks it to.
    fn activity_events(&mut self, clock: &dyn Clock) -> Vec<crate::web::protocol::ActivityEvent> {
        self.activity.evict(clock);
        self.activity.events().cloned().collect()
    }

    /// One chunk of raw PTY output, from `drain_pty_output`'s tee.
    fn tee(&mut self, tab_id: &str, child: Option<usize>, mint: u64, bytes: &[u8]) {
        let Some(handle) = self.handle.as_ref() else {
            return;
        };
        let terminal_id = match child {
            None => crate::web::stream::primary_terminal_id(tab_id),
            Some(_) => crate::web::stream::child_terminal_id(tab_id, mint),
        };
        if let Some(frame) = self.streams.pty_output(&terminal_id, bytes) {
            handle.send(crate::web::server::WebOutbound::All(
                crate::web::protocol::ServerMsg::TermBytes(frame),
            ));
        }
    }
}

/// A per-tab, one-shot git refresh the event loop still has to run: a finished
/// session's feed row is parked until it answers (D11 §5.1).
#[derive(Debug, Clone, PartialEq, Eq)]
struct FinishCountRequest {
    /// The parked row this answer belongs to.
    request: crate::web::activity::FinishCountId,
    /// The worktree to ask about — that tab's, not the repository root.
    worktree_abs: PathBuf,
}

/// One completed finish-edge count, from its worker thread.
struct FinishCount {
    request: crate::web::activity::FinishCountId,
    /// `None` when git could not answer. The row is then recorded without the
    /// clause, exactly as it was before any of this existed.
    files: Option<u32>,
}

/// Tee one project's status transitions into D11's feed, returning the one-shot
/// git refreshes its finished rows are waiting on.
///
/// **This runs for every open project, on screen or not** — the event loop
/// calls it inside the same per-project pass that drains PTYs and fires
/// notifications, so a session that finished in a project nobody is looking at
/// gets its file count on exactly the same terms as the active one. That is the
/// whole reason the count is fetched *here*, at the edge, rather than read out
/// of the periodic git-status cache, which only ever refreshes
/// `workspace.projects[active]` (see `GIT_REFRESH_EVERY`) and is left untouched
/// by this: no project gains any periodic git work it did not already have.
///
/// The refreshes are returned rather than spawned so the whole decision — which
/// edges want a count, and which worktree to ask about — is a pure function of
/// the project's state, testable against a fake git with no threads in it.
fn record_web_transitions(
    web: &mut WebSurface,
    project: &mut Project,
    clock: &dyn Clock,
    now_ms: u64,
) -> Vec<FinishCountRequest> {
    let changes = project.state.take_status_transitions(now_ms);
    if changes.is_empty() {
        // Built only when there is something to attribute, so a quiet tick
        // costs no allocation on a loop that runs at frame rate.
        return Vec::new();
    }
    let project_id = crate::web::protocol::ProjectId::new(project.git.root().display().to_string());
    let mut requests = Vec::new();
    for change in changes {
        let agent = project.state.registry.get(&change.agent_key);
        // Only a `Ready` tab has a worktree on disk to count; anything else
        // takes the honest-empty path rather than asking git about a directory
        // that is not there yet.
        let worktree_abs = project
            .state
            .tabs
            .iter()
            .find(|t| t.meta.id == change.tab_id.0 && t.phase == TabPhase::Ready)
            .map(|t| {
                to_absolute(
                    &project.state.repo_root,
                    Path::new(&t.meta.worktree_path_relative),
                )
            });
        if let Some(request) = web.record_transition(
            clock,
            project_id.clone(),
            &project.name,
            agent,
            worktree_abs,
            change,
        ) {
            requests.push(request);
        }
    }
    requests
}

/// Run one finish-edge count off the UI thread and post the answer back.
///
/// A whole thread for a single `git status --porcelain` is the same trade
/// `spawn_status_refresh` makes and for the same reason (SPECS §21): a repo
/// another instance is holding a lock on must never freeze the loop. It costs
/// one thread per finished session, which is rare — this is not the periodic
/// refresh and deliberately does not become one.
fn spawn_finish_count(git: &GitCli, tx: &Sender<FinishCount>, req: FinishCountRequest) {
    let git = git.clone();
    let tx = tx.clone();
    std::thread::spawn(move || {
        let files = crate::web::activity::file_count(&git, &req.worktree_abs);
        let _ = tx.send(FinishCount {
            request: req.request,
            files,
        });
    });
}

/// The [`TerminalHost`] the web interface writes keystrokes through: every open
/// project's tabs, searched by wire terminal id.
///
/// Every project, not just the active one, because a browser can be looking at
/// a session in a project that is not on screen and D3's shared selection moves
/// the desktop to it — but the keystroke must land even in the tick before that
/// happens.
///
/// [`TerminalHost`]: crate::web::stream::TerminalHost
struct WorkspaceTerminals<'a> {
    projects: &'a mut [Project],
}

impl crate::web::stream::TerminalHost for WorkspaceTerminals<'_> {
    fn write_terminal_input(
        &mut self,
        terminal_id: &crate::web::protocol::TerminalId,
        bytes: &[u8],
    ) -> crate::web::stream::Written {
        for project in self.projects.iter_mut() {
            for tab in project.state.tabs.iter_mut() {
                let tab_id = tab.meta.id.clone();
                if let Some(written) = crate::web::stream::write_into_session(
                    &mut tab.session,
                    &tab_id,
                    terminal_id,
                    bytes,
                ) {
                    return written;
                }
            }
        }
        crate::web::stream::Written::NoSuchTerminal
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
///
/// `isolated`: `None` for a normal project. `Some(status_root)` for an
/// isolated run (SPECS §32) whose status plumbing lives at that root —
/// forwarded straight through to [`startup`].
fn open_project(env: &Env, path: &Path, isolated: Option<&Path>) -> Result<Project> {
    let git = GitCli::discover(path)?;
    let root = git.root().to_path_buf();
    let name = derive_project_name(&root);
    let state = {
        let services = env.services(&git);
        startup(&services, &root, &root, isolated)?
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

/// Switch the active project, resuming its agents (SPECS §22). Refused in an
/// isolated run, which has exactly one project by construction (SPECS §32).
fn switch_project(workspace: &mut Workspace, env: &Env, sel: Selector, ui: &mut Ui) {
    if workspace.active_project().state.isolated {
        ui.web_outcome = Some(WebDispatch::Refused(ISOLATED_REFUSAL.to_string()));
        ui.message(ISOLATED_REFUSAL);
        return;
    }
    workspace.switch(sel);
    resume_active_project_agents(workspace, env);
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
    // An isolated run makes no network call and writes no cache (SPECS §32).
    let check_enabled = !workspace.active_project().state.isolated
        && workspace.active_project().state.config.update.check;
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

    // FlightDeck Web (optional): the embedded browser surface. Constructed
    // always (it is cheap and holds no buffers until the server runs) and
    // started here only when `[web] enabled` opted in — D10 makes auto-start the
    // TUI's decision, which is why `server::start` deliberately ignores the flag.
    let mut web_surface = WebSurface::new(&workspace.active_project().state.config.web);
    // Test / E2E seam, debug builds only (read once at startup): when
    // `FLIGHTDECK_WEB_TEST_CODE` holds four digits, the running web server
    // always has *that* bootstrap code live, so the Playwright suite (D15) can
    // exchange it in a real browser instead of screen-scraping a TUI overlay for
    // a random one. `None` in every normal run, and absent entirely from a
    // release build — see `WebSurface::ensure_test_bootstrap_code`.
    #[cfg(debug_assertions)]
    let web_test_code: Option<String> = std::env::var("FLIGHTDECK_WEB_TEST_CODE")
        .ok()
        .filter(|v| v.len() == 4 && v.bytes().all(|b| b.is_ascii_digit()));
    if workspace.active_project().state.config.web.enabled {
        let config = workspace.active_project().state.config.web.clone();
        let activity = web_surface.activity_events(env.clock);
        let initial = build_web_host_state(
            workspace,
            &web_surface.streams,
            activity,
            web_dialog_view(
                &ui,
                &workspace.active_project().name,
                &workspace.active_project().state,
            ),
            now0,
        );
        match web_surface.start(&config, initial) {
            Ok((addr, exposure)) => ui.message(web_started_message(addr, exposure)),
            Err(e) => ui.message(format!("Web interface did not start: {e}")),
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

            drain_pty_output(&mut p.state, now_ms, |sid, which, mint, bytes| {
                // FlightDeck Web (D2): the raw chunk into this terminal's replay
                // ring, and straight out to every attached viewer. Only while the
                // server is running — see `WebSurface` on the memory this costs
                // and why it is not paid by a user who never starts it.
                web_surface.tee(sid, which, mint, bytes);
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

            // FlightDeck Web (D11): the same lifecycle signal, recorded a second
            // time for the browser's activity feed. Deliberately a *tee at the
            // source* rather than a second read of the notifications above:
            // `take_finish_notifications` spends each tab's arming and drops
            // whatever `[notifications]` disabled or the startup grace window
            // suppressed, so a feed built from its output would be missing
            // exactly the events D11 exists to deliver. Running after it is
            // therefore free of consequence for the desktop — the two keep
            // separate per-tab edge memory — and this record happens whether or
            // not the server is up, so a browser opened later lands on history
            // rather than silence (`WebSurface::activity`).
            //
            // A finished session's row also wants the count artboard 2e shows
            // (`finished, 18 files touched`), which only git knows: each
            // returned request is one `git status --porcelain` on that tab's
            // worktree, spawned here and answered into `count_tx`. This is the
            // *only* git work the feed adds, it is per finished session rather
            // than per tick, and the periodic cache above still refreshes the
            // active project alone.
            for request in record_web_transitions(&mut web_surface, p, env.clock, now_ms) {
                spawn_finish_count(&p.git, &web_surface.count_tx, request);
            }
        }

        // Land the finish-edge counts that came back, and let go of any row
        // that has waited too long for one — before anything reads the feed
        // this tick.
        web_surface.drain_finish_counts(env.clock, now_ms);

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
                    RemoteInbound::HandshakeFailed { reason, retrying } => {
                        // The relay link never reached `auth_ok`, so no pairing
                        // code can arrive. Tell the overlay why: a refusal (no
                        // relay password configured, for instance) fails the
                        // attempt, a transient failure just explains the wait.
                        // Without this the overlay showed "Requesting a pairing
                        // code from the relay…" forever while the client
                        // backoff-looped in silence.
                        if let Some(ps) = pairing_session.as_mut() {
                            ps.on_handshake_failed(reason, *retrying);
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

        // --- FlightDeck Web: drain what the browsers said, then publish the
        //     state and the deltas that describe how it changed.
        //
        //     Ordering matters and is deliberate. Inbound is drained *first*, so
        //     a selection the browser just moved (D3) is reflected in the state
        //     published on this same tick rather than a tick later. And publish
        //     comes after `sync_terminal_sizes` above, so the geometry the
        //     browser letterboxes is the grid the PTY actually has (D4). ---
        if web_surface.running() {
            let inbound: Vec<crate::web::server::WebInbound> =
                web_surface.inbound_rx.try_iter().collect();
            for event in inbound {
                // A `Command` frame is the browser's palette pressing Enter:
                // `run_web_command` routes it into the same `run_palette_action`
                // the desktop's own palette calls, and answers with the ack that
                // dispatch earned. The server has already refused an unknown
                // name, a read-only seat's frame (D14) and every command whose
                // effect must not land for a browser (D16, including `quit`), so
                // reaching here means a controller sent something runnable.
                if let crate::web::server::WebInbound::Command {
                    viewer_id,
                    label,
                    command,
                } = &event
                {
                    // D13: a dialog this command opens is tagged with the seat
                    // that asked, so the desktop can say `opened from browser ·
                    // 192.168.2.20` about a modal nobody at this keyboard
                    // requested.
                    let origin = crate::web::protocol::DialogOrigin::Browser {
                        viewer_id: Some(viewer_id.clone()),
                        label: label.clone(),
                    };
                    let ack = run_web_command(
                        command,
                        &origin,
                        workspace,
                        env,
                        &mut ui,
                        &mut web_surface.activity,
                    );
                    if let Some(handle) = web_surface.handle.as_ref() {
                        handle.send(crate::web::server::WebOutbound::Viewer {
                            viewer_id: viewer_id.clone(),
                            msg: crate::web::protocol::ServerMsg::Ack(ack),
                        });
                    }
                    continue;
                }
                let mut host = WorkspaceTerminals {
                    projects: &mut workspace.projects,
                };
                let out = web_surface.streams.apply_inbound(&event, &mut host);
                if let Some(handle) = web_surface.handle.as_ref() {
                    for frame in out {
                        handle.send(frame);
                    }
                }
            }

            let activity = web_surface.activity_events(env.clock);
            let next = build_web_host_state(
                workspace,
                &web_surface.streams,
                activity,
                web_dialog_view(
                    &ui,
                    &workspace.active_project().name,
                    &workspace.active_project().state,
                ),
                now_ms,
            );
            let decided = std::mem::take(&mut ui.dialog_decisions);
            if next != web_surface.published {
                // Publish, *then* the matching deltas: publishing changes what
                // the next attach sees and notifies nobody, deliberately, so the
                // host is the one that says what changed (see `HostState`).
                let mut frames = crate::web::stream::deltas(&web_surface.published, &next);
                // D13: the diff can only say `Superseded` about a dialog that is
                // gone. Where somebody actually decided, say so.
                resolve_dialog_outcomes(&mut frames, &decided);
                if let Some(handle) = web_surface.handle.as_ref() {
                    handle.publish_state(next.clone());
                    for delta in frames {
                        handle.send(crate::web::server::WebOutbound::All(
                            crate::web::protocol::ServerMsg::Delta(delta),
                        ));
                    }
                }
                web_surface.published = next;
            }
        }
        // Drained whether or not anyone is watching (D13). A desktop-only run
        // still decides dialogs, and a list nobody ever reads would grow for the
        // life of the process — so the take above is paired with a clear here
        // rather than living inside the `running()` branch.
        ui.dialog_decisions.clear();

        // --- Start / stop the web interface, when the palette asked (D10). ---
        if ui.pending_web_start {
            ui.pending_web_start = false;
            let config = workspace.active_project().state.config.web.clone();
            let activity = web_surface.activity_events(env.clock);
            // A dialog can already be open when the server starts (the palette
            // that ran `Start Web Interface` is gone by now, but a prompt behind
            // it is not), so the first snapshot carries it rather than lying.
            let initial = build_web_host_state(
                workspace,
                &web_surface.streams,
                activity,
                web_dialog_view(
                    &ui,
                    &workspace.active_project().name,
                    &workspace.active_project().state,
                ),
                now_ms,
            );
            match web_surface.start(&config, initial) {
                Ok((addr, exposure)) => ui.message(web_started_message(addr, exposure)),
                Err(e) => ui.message(format!("Web interface did not start: {e}")),
            }
        }
        if ui.pending_web_stop {
            ui.pending_web_stop = false;
            if web_surface.running() {
                web_surface.stop(crate::web::server::ShutdownNotice::server_stopped());
                ui.message("Web interface stopped.".to_string());
            }
        }
        #[cfg(debug_assertions)]
        if let Some(digits) = web_test_code.as_deref() {
            web_surface.ensure_test_bootstrap_code(digits);
        }
        ui.web_running = web_surface.running();

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
                let chrome = crate::tui::layout::chrome_for(area, p.state.mode());
                let ml = crate::tui::layout::compute(
                    area,
                    chrome,
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
                let full = PtySize { rows, cols };
                // Resize every project's sessions so a background agent's output
                // wraps correctly the moment the user switches back to it.
                for p in workspace.projects.iter_mut() {
                    let reserve = crate::tui::mode_style::border_enabled(&p.state.config.ui);
                    let size = viewport_pty_size(full, p.state.mode(), reserve);
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

    // Tell any attached browser that FlightDeck itself is going away, before the
    // listener closes (Q5), so it enters a terminal state instead of spinning in
    // "reconnecting…" against a host that no longer exists.
    web_surface.stop(crate::web::server::ShutdownNotice::host_quit(None));

    // Tear down the relay client (best-effort join). A dropped handle also
    // signals the thread, so an early `?` return above still winds it down.
    if let Some(setup) = remote_setup {
        setup.handle.stop();
    }

    Ok(())
}

/// Build the state the browser paints from, out of every open project.
///
/// This is the whole desktop → wire adaptation, assembled from
/// [`crate::web::stream`]'s per-piece converters so the interesting decisions
/// (R2's git mapping, the lifecycle fact, a terminal's byte-stream numbers) live
/// next to their tests rather than in the event loop.
///
/// Every project is included, not just the active one: a browser can be looking
/// at a session in a background project, and D3's shared selection means
/// clicking it moves the desktop rather than the browser being told it may not
/// look.
///
/// `activity` is the retained D11 feed, already evicted against both §5.1
/// bounds by [`WebSurface::activity_events`] — carried in whole so a freshly
/// attached tab's `Snapshot` backfills history rather than opening on silence,
/// and so `crate::web::stream::deltas` can spot a genuinely new event and turn
/// it into a `Delta::Activity` without the tab reloading.
///
/// `dialog` is the one open dialog (D13), read off the desktop's own prompt state
/// by [`web_dialog_view`] and passed in rather than derived here — this function
/// takes a `&Workspace` and a dialog lives on the [`Ui`], which is the layer
/// above. Absent rather than guessed when none is open.
fn build_web_host_state(
    workspace: &Workspace,
    streams: &crate::web::stream::TerminalStreams,
    activity: Vec<crate::web::protocol::ActivityEvent>,
    dialog: Option<crate::web::protocol::DialogView>,
    now_ms: u64,
) -> crate::web::server::HostState {
    use crate::web::protocol as wire;
    use crate::web::stream as ws;

    let active = workspace.active_project();
    let mut projects = Vec::with_capacity(workspace.projects.len());
    let mut selection = wire::Selection {
        split_view: active.state.split_view,
        ..wire::Selection::default()
    };

    for project in workspace.projects.iter() {
        // The repository root, not the folder name: two open projects can be
        // called `web` and the browser keys everything by this id.
        let root = project.git.root().display().to_string();
        let project_id = wire::ProjectId::new(root.clone());
        let mut sessions = Vec::with_capacity(project.state.tabs.len());

        for (index, tab) in project.state.tabs.iter().enumerate() {
            let mut terminals = Vec::new();
            if let Some(primary) = tab.session.primary() {
                terminals.push(terminal_facts(
                    ws::primary_terminal_id(&tab.meta.id),
                    primary,
                ));
            }
            for c in 0..tab.session.child_count() {
                if let Some(child) = tab.session.child(c) {
                    terminals.push(terminal_facts(
                        ws::child_terminal_id(&tab.meta.id, child.stream_id()),
                        child,
                    ));
                }
            }

            if std::ptr::eq(project, active) && project.state.selected_tab == Some(index) {
                selection.project_id = Some(project_id.clone());
                selection.session_id = Some(tab.id());
                selection.terminal_id = Some(match tab.session.selected_child() {
                    None => ws::primary_terminal_id(&tab.meta.id),
                    Some(c) => tab
                        .session
                        .child(c)
                        .map(|child| ws::child_terminal_id(&tab.meta.id, child.stream_id()))
                        .unwrap_or_else(|| ws::primary_terminal_id(&tab.meta.id)),
                });
            }

            sessions.push(ws::session_view(
                &ws::SessionFacts {
                    project_id: &project_id,
                    tab_id: &tab.meta.id,
                    name: &tab.meta.name,
                    agent: &tab.meta.agent,
                    agent_def: project.state.config.agents.get(&tab.meta.agent),
                    phase: match tab.phase {
                        TabPhase::Creating => wire::SessionPhase::Creating,
                        TabPhase::Ready => wire::SessionPhase::Ready,
                    },
                    display: tab.display_status(now_ms),
                    // The per-turn timer lives in the phone bridge's own turn
                    // tracker, which this loop does not own. Reported as zero
                    // rather than fabricated; wiring it is a follow-up.
                    running_time_secs: 0,
                    git: ws::GitFacts {
                        status: project.cache.get(&tab.meta.id),
                        fallback_branch: &tab.meta.branch,
                    },
                    recovered: tab.meta.recovered,
                    attached_existing_branch: tab.meta.attached_existing_branch,
                    terminals,
                },
                streams,
            ));
        }

        projects.push(ws::project_view(
            &project_id,
            &project.name,
            &root,
            &project.state.base_branch,
            sessions,
        ));
    }

    crate::web::server::HostState {
        host_version: env!("CARGO_PKG_VERSION").to_string(),
        projects,
        selection,
        geometry: ws::geometry_of(active.state.pty_size),
        replay_capacity_bytes: streams.capacity_bytes() as u64,
        activity,
        dialog,
    }
}

/// One terminal's wire facts, read off the live [`crate::terminal::session::Terminal`].
fn terminal_facts(
    terminal_id: crate::web::protocol::TerminalId,
    terminal: &crate::terminal::session::Terminal,
) -> crate::web::stream::TerminalFacts {
    let (rows, cols) = terminal.screen().size();
    let state = terminal.process_state();
    crate::web::stream::TerminalFacts {
        terminal_id,
        role: crate::web::protocol::TerminalRole::from(terminal.kind),
        title: terminal.title.clone(),
        // The grid the desktop's own parser is using, which is the grid the PTY
        // has — D4's "the host owns this", read from the horse's mouth rather
        // than from a remembered config value.
        geometry: crate::web::protocol::Geometry { cols, rows },
        alive: matches!(state, ProcessState::Running | ProcessState::Starting),
        exit_code: match state {
            ProcessState::Exited(code) => Some(code),
            _ => None,
        },
    }
}

/// The line the desktop shows when the server comes up, carrying D5's warning
/// whenever the bound address is reachable from other machines.
///
/// The warning is not decoration: binding a routable address is the one web
/// setting that changes who can reach the user's agents, and it is only ever
/// reached because they typed it into `config.toml` themselves.
fn web_started_message(
    addr: std::net::SocketAddr,
    exposure: crate::web::server::BindExposure,
) -> String {
    match exposure {
        crate::web::server::BindExposure::Loopback => {
            format!("Web interface listening on http://{addr} (this machine only).")
        }
        crate::web::server::BindExposure::Routable => format!(
            "Web interface listening on http://{addr} — WARNING: reachable from your \
             network, not just this machine. Anyone who can reach this address and \
             holds the access code can drive your agents."
        ),
    }
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
            // A stalling relay handshake names itself here; only a genuinely
            // quiet wait gets the bland line.
            status_line: session
                .stall_reason()
                .map(str::to_string)
                .unwrap_or_else(|| "Requesting a pairing code from the relay…".to_string()),
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
            // Timed keystroke sequence: write the first chunk now and queue the
            // rest at increasing due times so Claude's Ink TUI re-renders between
            // keys (remote-control-dc9). The queued chunks flush on later ticks.
            Translation::PtyInputSequence {
                project,
                tab,
                session_id,
                chunks,
                step_delay_ms,
            } => match workspace.projects.get_mut(project) {
                None => remote_target_gone(),
                Some(p) => {
                    let first_ok = chunks
                        .first()
                        .map(|c| write_primary_pty(&mut p.state, tab, c))
                        .unwrap_or(true);
                    if first_ok {
                        for (i, chunk) in chunks.iter().enumerate().skip(1) {
                            bridge.enqueue_deferred_pty(
                                session_id.clone(),
                                now_ms + (i as u64) * step_delay_ms,
                                chunk.clone(),
                            );
                        }
                        (CommandOutcome::Applied, None)
                    } else {
                        (
                            CommandOutcome::Failed,
                            Some("could not write to the agent terminal".to_string()),
                        )
                    }
                }
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

        // PtyInputSequence is intercepted in `service_remote_commands` (which owns
        // the deferred-write queue that spaces the chunks). If it ever reaches the
        // generic executor — e.g. a direct test call — degrade to writing every
        // chunk back-to-back (no inter-key spacing).
        Translation::PtyInputSequence {
            project,
            tab,
            chunks,
            ..
        } => {
            let Some(p) = workspace.projects.get_mut(project) else {
                return remote_target_gone();
            };
            let ok = chunks
                .iter()
                .all(|chunk| write_primary_pty(&mut p.state, tab, chunk));
            if ok {
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

        Translation::NeedsMainLoop(MainLoopAction::ResumeAndReply {
            project,
            tab_id,
            text,
        }) => {
            let Some(p) = workspace.projects.get_mut(project) else {
                return remote_target_gone();
            };
            // Resume the not-started agent (continuing its session, exactly like
            // navigating to it on the desktop), then queue the reply so it is
            // delivered once the terminal is ready — reusing the first-task
            // readiness gate (remote-control-1l4).
            let resumed = {
                let services = env.services(&p.git);
                p.state.resume_tab_by_id(&tab_id, &services)
            };
            match resumed {
                Ok(()) => {
                    first_tasks.push(PendingFirstTask {
                        tab_id,
                        text,
                        queued_at_ms: now_ms,
                    });
                    (
                        CommandOutcome::Accepted,
                        Some(
                            "Starting the agent; your message will be sent once it is ready."
                                .to_string(),
                        ),
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

/// Undo every terminal mode FlightDeck turned on after `ratatui::try_init`.
///
/// Shared by the normal teardown and the panic hook so the two can never drift:
/// a panic that skipped any of these leaves the user's shell echoing mouse
/// escape sequences as text. All best effort — a terminal that ignored the
/// enable will ignore the disable.
fn restore_terminal_modes(keyboard_enhanced: bool) {
    if keyboard_enhanced {
        let _ = crossterm::execute!(std::io::stdout(), PopKeyboardEnhancementFlags);
    }
    let _ = crossterm::execute!(std::io::stdout(), DisableBracketedPaste);
    let _ = crossterm::execute!(std::io::stdout(), DisableMouseCapture);
    let _ = restore_terminal_title();
}

/// Compute the PTY/terminal-viewport size from the full terminal size. Agents
/// must wrap at the viewport width (total minus the sidebar/borders), not the
/// whole screen. `mode` matters because collapsed chrome hands the sidebar's
/// columns and the hidden bars' rows to the viewport.
fn viewport_pty_size(full: PtySize, mode: InputMode, reserve_border: bool) -> PtySize {
    let area = Rect::new(0, 0, full.cols, full.rows);
    let ml = crate::tui::layout::compute(
        area,
        crate::tui::layout::chrome_for(area, mode),
        reserve_border,
    );
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
        let chrome = crate::tui::layout::chrome_for(area, workspace.active_project().state.mode());
        let ml = crate::tui::layout::compute(
            area,
            chrome,
            crate::tui::mode_style::border_enabled(&workspace.active_project().state.config.ui),
        );
        let names: Vec<String> = workspace.projects.iter().map(|p| p.name.clone()).collect();
        if let Some(hit) = project_tab_hit_test(ml.project_tabs, &names, me.column, me.row) {
            ui.drag = None;
            match hit {
                ProjectHit::Tab(i) => {
                    switch_project(workspace, env, Selector::Index(i), ui);
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
        crate::tui::layout::chrome_for(area, state.mode()),
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
        crate::tui::layout::chrome_for(area, state.mode()),
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
/// The FlightDeck repository, taken from `Cargo.toml` at compile time so the
/// URL can never drift from the crate metadata.
const REPOSITORY_URL: &str = env!("CARGO_PKG_REPOSITORY");

/// Whether a key press arriving while an overlay is open is the "open the
/// FlightDeck repository" gesture — a second press of a help key on the help
/// panel — rather than an ordinary dismissal.
///
/// Either global help key counts, so the gesture is "press it again" whichever
/// key you reached for. It keys off the help overlay *being open*, not off how
/// it was opened, so opening help from the command palette and then pressing F1
/// works exactly like F1 then F1.
fn is_help_repo_gesture(overlay: &UiOverlay, key: KeyEvent) -> bool {
    if !matches!(overlay, UiOverlay::Help) {
        return false;
    }
    let bare_f1 = key.code == KeyCode::F(1) && key.modifiers.is_empty();
    let alt_h = key.code == KeyCode::Char('h') && key.modifiers == KeyModifiers::ALT;
    bare_f1 || alt_h
}

#[cfg(test)]
mod help_repo_gesture_tests {
    use super::*;

    fn f1() -> KeyEvent {
        KeyEvent::new(KeyCode::F(1), KeyModifiers::empty())
    }

    #[test]
    fn f1_on_the_help_overlay_is_the_gesture() {
        assert!(is_help_repo_gesture(&UiOverlay::Help, f1()));
    }

    #[test]
    fn f1_without_the_help_overlay_is_not_the_gesture() {
        // With no overlay F1 must fall through to the key map and *open* help;
        // on another overlay it must dismiss, as any key does.
        assert!(!is_help_repo_gesture(&UiOverlay::None, f1()));
        assert!(!is_help_repo_gesture(&UiOverlay::About, f1()));
    }

    #[test]
    fn alt_h_on_the_help_overlay_is_also_the_gesture() {
        // "Press the help key again" must hold for whichever help key was used.
        assert!(is_help_repo_gesture(
            &UiOverlay::Help,
            KeyEvent::new(KeyCode::Char('h'), KeyModifiers::ALT)
        ));
    }

    #[test]
    fn bare_h_on_the_help_overlay_still_dismisses() {
        assert!(!is_help_repo_gesture(
            &UiOverlay::Help,
            KeyEvent::new(KeyCode::Char('h'), KeyModifiers::empty())
        ));
    }

    #[test]
    fn other_keys_on_the_help_overlay_are_not_the_gesture() {
        for code in [KeyCode::Esc, KeyCode::Char('q'), KeyCode::F(2)] {
            assert!(
                !is_help_repo_gesture(&UiOverlay::Help, KeyEvent::new(code, KeyModifiers::empty())),
                "{code:?} must still dismiss the help overlay",
            );
        }
    }

    #[test]
    fn modified_f1_on_the_help_overlay_is_not_the_gesture() {
        assert!(!is_help_repo_gesture(
            &UiOverlay::Help,
            KeyEvent::new(KeyCode::F(1), KeyModifiers::CONTROL)
        ));
    }

    #[test]
    fn the_repository_url_is_a_github_https_url() {
        assert!(
            REPOSITORY_URL.starts_with("https://github.com/"),
            "Cargo.toml `repository` must stay a GitHub https URL, got: {REPOSITORY_URL}",
        );
    }

    #[test]
    fn the_repository_url_survives_the_windows_cmd_launcher() {
        // Windows opens URLs via `cmd /c start`, which re-parses the argument.
        // A `repository` value carrying a cmd metacharacter would be mangled on
        // Windows only — a platform-specific break CI cannot catch, since no
        // test spawns a real browser. Assert the requirement at the call site.
        assert!(
            crate::tui::opener::url_is_cmd_safe(REPOSITORY_URL),
            "Cargo.toml `repository` must be free of cmd metacharacters, got: {REPOSITORY_URL}",
        );
    }
}

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

    // 3a. A second bare F1 while the help overlay is open opens the FlightDeck
    //     repository in the browser. The panel deliberately stays up: pressing
    //     F1 again is a shortcut out of help, not a way to leave it. Failures
    //     are silent by design — see `is_help_repo_gesture`.
    if is_help_repo_gesture(&ui.overlay, key) {
        let _ = crate::tui::opener::open_url(REPOSITORY_URL);
        return Ok(false);
    }

    // 3b. A non-interactive overlay (help, git status, message): any key dismisses.
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
            switch_project(workspace, env, sel, ui);
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
            palette.set_web_running(ui.web_running);
            // Hide the project/new-tab entries in an isolated run: one session,
            // one project (SPECS §32). The flows refuse independently; this is
            // presentation only.
            palette.set_isolated(workspace.active_project().state.isolated);
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
            start_new_tab_flow(state, services, ui);
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

    // Which half of a two-phase flow this is, read before `cmd` is consumed.
    // Only used to classify the outcome for a browser (below) — the desktop's
    // rendering is unchanged either way.
    let unconfirmed =
        crate::web::commands::confirmation_of(&cmd) == crate::web::commands::Confirmation::Pending;

    // A command that can't run (e.g. an action needing a selected tab when the
    // project has none, or a git failure) must surface as a message, never
    // crash the event loop. Errors always become a toast; only the Ok path
    // maps its effect onto the UI.
    match state.dispatch(cmd, services) {
        Ok(effect) => {
            // `Effect::Warning` is genuinely two different facts, and only the
            // command's phase separates them. From a *confirmed* dispatch it
            // means the operation landed and the cleanup after it did not —
            // applied-with-caveat, which is what `apply_effect` records. From an
            // **unconfirmed** one it means a guard stopped the flow before it
            // ever asked: SPECS §13's dirty base is the case that matters, where
            // nothing merged and the browser must not be told otherwise. The
            // phone path draws exactly this line in `dispatch_remote_merge_back`
            // ("no merge happened, so it is a rejection"); this is the same rule
            // for the browser, stated once for every two-phase command rather
            // than per command. The sentence is the guard's own either way.
            let warned = matches!(effect, Effect::Warning(_));
            apply_effect(effect, state, ui);
            if unconfirmed && warned {
                ui.web_outcome = match ui.web_outcome.take() {
                    Some(WebDispatch::Applied(Some(reason))) => Some(WebDispatch::Refused(reason)),
                    other => other,
                };
            }
        }
        Err(e) => {
            ui.web_outcome = Some(WebDispatch::Failed(e.to_string()));
            ui.message(format!("Error: {e}"));
        }
    }
    Ok(())
}

/// Map a dispatch [`Effect`] onto the [`Ui`] overlays/prompts (SPECS §22).
///
/// Also records the outcome in [`Ui::web_outcome`], so the same dispatch can be
/// acked to a browser without a second interpretation of what it did. A
/// prompt-opening effect is recorded as a refusal: from a browser's point of
/// view a modal that appeared on someone else's screen is not an application.
fn apply_effect(effect: Effect, _state: &AppState, ui: &mut Ui) {
    ui.web_outcome = Some(match &effect {
        Effect::Refused(m) => WebDispatch::Refused(m.clone()),
        // Warnings map to applied-with-caveat, exactly as `fold_remote_effect`
        // does for the phone; a caller whose command treats a warning as
        // "nothing happened" must intercept it before dispatching.
        Effect::Message(m) | Effect::Warning(m) => WebDispatch::Applied(Some(m.clone())),
        Effect::PrUrl(url) => WebDispatch::Applied(Some(url.clone())),
        Effect::AttachedExisting { branch } => {
            WebDispatch::Applied(Some(format!("Attached to existing branch {branch}")))
        }
        Effect::None | Effect::Quit => WebDispatch::Applied(None),
        Effect::OpenInFileManager { .. } => {
            WebDispatch::Refused(crate::web::commands::HOST_ONLY_REFUSAL.to_string())
        }
        // D13: a dialog is now app state on both surfaces, so opening one is
        // not a refusal any more. The sentence the browser reads is
        // `DIALOG_OPENED_DETAIL`, worded once in `run_web_command`, which is
        // also the only caller that can tell a *newly* opened dialog from one
        // that was already up.
        Effect::CloseTabOptions(_) => WebDispatch::Applied(None),
        // The git confirmations (`remote-control-ll5.5`). SPECS §5 gates every
        // history-touching operation behind one of these, and D13 publishes it
        // to both surfaces — so opening one is the *point* of the row, exactly
        // as it is for `CloseTabOptions` above. `run_web_command` notices the
        // newly-opened dialog and acks `DIALOG_OPENED_DETAIL`; nothing has
        // merged, pushed or been rewritten yet, and the browser is told that by
        // being shown the question rather than a success.
        //
        // The destructive pair joined them in `remote-control-ll5.4`, for the
        // same reason and with one more step behind the question: opening
        // §5/§15's abandon warning, or D16's quit confirmation, is what the row
        // is *for*. Nothing has been discarded and nothing has stopped — the
        // browser is told that by being shown the question, and its answer to it
        // has to pass artboard 1g's typed-name gate (`browser_confirm_gate`).
        Effect::PushWarning(_)
        | Effect::MergeConfirm { .. }
        | Effect::RebaseConfirm { .. }
        | Effect::AbandonWarning { .. }
        | Effect::QuitConfirm => WebDispatch::Applied(None),
        // Not dialogs: read-only overlays with nothing to answer, and no browser
        // design yet (`remote-control-ll5.8`, design turn 3).
        Effect::GitStatus { .. } | Effect::ShowHelp | Effect::ShowAbout => WebDispatch::Refused(
            "This opens a read-only overlay on the desktop, which the browser has \
             no design for yet. Nothing is being asked, so there is nothing to \
             answer from here."
                .to_string(),
        ),
    });
    match effect {
        Effect::None => ui.clear(),
        Effect::Quit => ui.should_quit = true,
        Effect::Message(m) => ui.message(m),
        Effect::Warning(m) => ui.message(format!("WARNING: {m}")),
        Effect::Refused(m) => ui.message(format!("Refused: {m}")),
        Effect::PrUrl(url) => ui.message(format!("PR: {url}")),
        Effect::OpenInFileManager { path, command } => {
            // Success is silent — the file-manager window is the feedback.
            if let Err(e) = crate::tui::file_manager::open(&path, &command) {
                ui.message(format!("Refused: {e}"));
            }
        }
        Effect::AttachedExisting { branch } => {
            ui.message(format!("Attached to existing branch {branch}"))
        }
        Effect::PushWarning(_plan) => {
            start_prompt(ui, Prompt::PushConfirm);
        }
        Effect::AbandonWarning { dirty } => {
            start_prompt(ui, Prompt::AbandonConfirm { dirty });
        }
        Effect::QuitConfirm => {
            start_prompt(ui, Prompt::QuitConfirm);
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
        Effect::ShowAbout => ui.overlay = UiOverlay::About,
    }
}

/// Begin an interactive prompt, building its modal dialog.
///
/// D13 lands here and nowhere else. The origin comes from
/// [`Ui::web_dialog_origin`] — set for exactly as long as one browser frame is
/// being applied — so every one of the two dozen call sites keeps knowing
/// nothing about browsers, and a dialog can never be published without an
/// origin because there is no other way to open one.
fn start_prompt(ui: &mut Ui, prompt: Prompt) {
    let origin = ui
        .web_dialog_origin
        .clone()
        .unwrap_or(crate::web::protocol::DialogOrigin::Desktop);
    let mut dialog = prompt_dialog(&prompt);
    if let Some(label) = dialog_origin_label(&origin) {
        dialog = dialog.from_origin(label);
    }
    let id = ui.mint_dialog_id();
    ui.palette = None;
    ui.overlay = UiOverlay::None;
    ui.prompt = Some(PromptState {
        prompt,
        dialog,
        id,
        origin,
    });
}

/// D13's origin line, or `None` for a dialog this keyboard opened.
///
/// `None` for [`DialogOrigin::Desktop`] is not an omission: the person reading
/// the modal is the person who asked for it, and a line telling them so would be
/// the decoration D13 is explicit this is not.
fn dialog_origin_label(origin: &crate::web::protocol::DialogOrigin) -> Option<String> {
    match origin {
        crate::web::protocol::DialogOrigin::Desktop => None,
        crate::web::protocol::DialogOrigin::Browser { label, .. } => {
            Some(format!("opened from browser · {label}"))
        }
    }
}

/// The ack detail for a browser command whose outcome is a dialog (D13). One
/// sentence, so every dialog-opening row reads the same, and it says the thing
/// the browser has to know: it is a shared question, answerable from here.
const DIALOG_OPENED_DETAIL: &str =
    "A dialog is open. It is on the desktop too, tagged with where it came from, \
     and either surface can answer it.";

/// Why an action is unavailable in an isolated run (SPECS §32). One string, so
/// every refusal reads identically wherever the user meets it.
const ISOLATED_REFUSAL: &str =
    "Not available in an isolated run (--isolated): it has one session in this \
     directory and opens nothing else.";

/// Begin the New Agent Tab flow (SPECS §4, §22): open the combined form —
/// agent radio and new/existing/base target modes — with the configured default
/// agent preselected.
fn start_new_tab_flow(state: &AppState, services: &Services, ui: &mut Ui) {
    if state.isolated {
        ui.message(ISOLATED_REFUSAL);
        return;
    }
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
    let existing_branches = match services.git.list_local_branches() {
        Ok(branches) => branches
            .into_iter()
            // The base branch already has the project-root worktree; its
            // dedicated no-worktree mode is the next Tab target instead.
            .filter(|branch| branch != &state.base_branch)
            .collect(),
        Err(e) => {
            ui.message(format!("Error listing local branches: {e}"));
            return;
        }
    };
    start_prompt(
        ui,
        Prompt::NewAgentForm {
            agents,
            selected,
            branch: String::new(),
            existing_branches,
            branch_selected: 0,
            use_existing_branch: false,
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
        start_new_tab_flow(state, services, ui);
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
    if workspace.active_project().state.isolated {
        ui.message(ISOLATED_REFUSAL);
        return;
    }
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

/// Begin the project-default-base flow with every local branch available for
/// filtering. The current default is preselected; existing tabs are not part of
/// this choice because each keeps its own persisted target branch.
fn start_change_project_base_flow(workspace: &Workspace, ui: &mut Ui) {
    let p = workspace.active_project();
    if p.state.isolated {
        ui.message(ISOLATED_REFUSAL);
        return;
    }
    match p.git.list_local_branches() {
        Ok(branches) if branches.is_empty() => ui.message("This repository has no local branches."),
        Ok(branches) => {
            let selected = branches
                .iter()
                .position(|branch| branch == &p.state.base_branch)
                .unwrap_or(0);
            start_prompt(
                ui,
                Prompt::ChangeProjectBase {
                    branches,
                    filter: String::new(),
                    selected,
                },
            );
        }
        Err(e) => ui.message(format!("Could not list local branches: {e}")),
    }
}

fn matching_branches<'a>(branches: &'a [String], filter: &str) -> Vec<&'a String> {
    let needle = filter.to_lowercase();
    branches
        .iter()
        .filter(|branch| needle.is_empty() || branch.to_lowercase().contains(&needle))
        .collect()
}

/// Persist a new project default in the committed project config, reload its
/// effective config, and mirror the value into state.json. Existing tabs remain
/// pinned to their own target branches.
fn change_project_default_base(
    workspace: &mut Workspace,
    env: &Env,
    branch: &str,
) -> Result<(String, usize)> {
    let active = workspace.active;
    let p = &workspace.projects[active];
    if !p.git.branch_exists(branch)? {
        return Err(FlightDeckError::Git(format!(
            "local branch '{branch}' does not exist"
        )));
    }
    let previous = p.state.base_branch.clone();
    let existing_tabs = p.state.tabs.len();
    let project_path = p.git.root().join(".flightdeck").join("config.toml");
    let contents = if env.fs.exists(&project_path) {
        env.fs.read_to_string(&project_path)?
    } else {
        String::new()
    };
    env.fs
        .write(&project_path, &set_project_default_base(&contents, branch)?)?;

    let loaded = match global_config_path() {
        Some(global_path) => load_layered_config(env.fs, &global_path, &project_path)?,
        None => effective_config_without_writing(env.fs, None, &project_path)?,
    };
    let p = &mut workspace.projects[active];
    p.state.reload_config(loaded);
    p.state.invalid_base_branch = None;
    p.state
        .warnings
        .retain(|warning| !warning.starts_with("Configured default base '"));
    let services = env.services(&p.git);
    persist_quietly(&p.state, &services)?;
    Ok((previous, existing_tabs))
}

/// Handle the searchable local-branch picker and apply the selected project
/// default on Enter.
fn handle_change_project_base_key(
    key: KeyEvent,
    workspace: &mut Workspace,
    env: &Env,
    ui: &mut Ui,
) -> Result<()> {
    let Some(mut pstate) = ui.prompt.take() else {
        return Ok(());
    };

    if key.code == KeyCode::Enter {
        let branch = match &pstate.prompt {
            Prompt::ChangeProjectBase {
                branches,
                filter,
                selected,
            } => matching_branches(branches, filter)
                .get(*selected)
                .cloned()
                .cloned(),
            _ => None,
        };
        let Some(branch) = branch else {
            ui.prompt = Some(pstate);
            return Ok(());
        };
        match change_project_default_base(workspace, env, &branch) {
            Ok((previous, _)) if previous == branch => {
                ui.message(format!("Project default base saved as '{branch}'."));
            }
            Ok((previous, existing_tabs)) => {
                let retained = if existing_tabs == 1 {
                    "1 existing agent keeps its current target".to_string()
                } else {
                    format!("{existing_tabs} existing agents keep their current targets")
                };
                ui.message(format!(
                    "Project default base changed: {previous} -> {branch}. {retained}."
                ));
            }
            Err(e) => ui.message(format!("Could not change project default base: {e}")),
        }
        return Ok(());
    }

    let Prompt::ChangeProjectBase {
        branches,
        filter,
        selected,
    } = &mut pstate.prompt
    else {
        ui.prompt = Some(pstate);
        return Ok(());
    };
    match key.code {
        KeyCode::Up => *selected = selected.saturating_sub(1),
        KeyCode::Down => {
            let len = matching_branches(branches, filter).len();
            if *selected + 1 < len {
                *selected += 1;
            }
        }
        KeyCode::Backspace => {
            filter.pop();
            *selected = 0;
        }
        KeyCode::Char(c)
            if !key.modifiers.contains(KeyModifiers::CONTROL)
                || key.modifiers.contains(KeyModifiers::ALT) =>
        {
            filter.push(c);
            *selected = 0;
        }
        _ => {}
    }
    pstate.dialog = prompt_dialog(&pstate.prompt);
    ui.prompt = Some(pstate);
    Ok(())
}

/// Begin the Close Project flow: confirm first (SPECS §25 no-surprise rule).
/// Refuses to close the only remaining project — that is what Ctrl-q is for.
fn start_close_project_flow(workspace: &Workspace, ui: &mut Ui, index: usize) {
    if workspace.active_project().state.isolated {
        ui.message(ISOLATED_REFUSAL);
        return;
    }
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
        match open_project(env, &target, None) {
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
            existing_branches,
            branch_selected,
            use_existing_branch,
            run_on_base,
            base_branch,
        } => {
            let list: Vec<DialogListItem> = if *use_existing_branch {
                let matches = matching_branches(existing_branches, branch);
                if matches.is_empty() {
                    vec![DialogListItem {
                        label: "(no matching local branches)".to_string(),
                        selected: false,
                    }]
                } else {
                    matches
                        .iter()
                        .enumerate()
                        .map(|(i, branch)| DialogListItem {
                            label: (*branch).clone(),
                            selected: i == *branch_selected,
                        })
                        .collect()
                }
            } else {
                // Agents as a radio list: the highlighted row is both selected
                // and marked, so ↑/↓ moves the choice in new/base modes.
                agents
                    .iter()
                    .enumerate()
                    .map(|(i, (_key, display))| {
                        let marker = if i == *selected { "(•)" } else { "( )" };
                        DialogListItem {
                            label: format!("{marker} {display}"),
                            selected: i == *selected,
                        }
                    })
                    .collect()
            };
            let title = if *run_on_base {
                format!(
                    "New Agent Session Tab   (↑/↓ agent · Tab changes target)\n\
                     Runs on base branch '{base_branch}' in the project root — no worktree."
                )
            } else if *use_existing_branch {
                let agent = agents
                    .get(*selected)
                    .map(|(_, display)| display.as_str())
                    .unwrap_or("default agent");
                format!(
                    "New Agent Session Tab — existing branch   \
                     (type to filter · ↑/↓ select · Tab changes target)\n\
                     Agent: {agent}"
                )
            } else {
                "New Agent Session Tab — new branch   \
                 (↑/↓ agent · type task name · Tab changes target)"
                    .to_string()
            };
            let target_label = if *run_on_base {
                format!("Target: base ({base_branch})")
            } else if *use_existing_branch {
                "Target: existing branch".to_string()
            } else {
                "Target: new branch".to_string()
            };
            let confirm_label = if *use_existing_branch {
                "Use branch"
            } else {
                "Create"
            };
            let buttons = vec![
                DialogButton::new(DialogAccel::Enter, confirm_label),
                DialogButton::new(DialogAccel::Tab, target_label),
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
        Prompt::QuitConfirm => Dialog::confirm(
            "Quit FlightDeck? Every agent it is running is stopped.",
            vec![
                DialogButton::new(DialogAccel::Char('y'), "Quit"),
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
                1 => " (target advanced 1 commit)".to_string(),
                n => format!(" (target advanced {n} commits)"),
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
        Prompt::ChangeProjectBase {
            branches,
            filter,
            selected,
        } => {
            let matches = matching_branches(branches, filter);
            let list = if matches.is_empty() {
                vec![DialogListItem {
                    label: "(no matching local branches)".to_string(),
                    selected: false,
                }]
            } else {
                matches
                    .iter()
                    .enumerate()
                    .map(|(i, branch)| DialogListItem {
                        label: (*branch).clone(),
                        selected: i == *selected,
                    })
                    .collect()
            };
            Dialog::browser(
                "Change project default base   (type to filter · ↑/↓ select · Enter apply)",
                filter.clone(),
                list,
                vec![
                    DialogButton::new(DialogAccel::Enter, "Use branch"),
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
    // D13: record what this keypress decided, before anything downstream can
    // replace the prompt. One wrapper rather than an edit at each of the dozen
    // `ui.prompt = None` / `ui.clear()` sites, because a site that forgot would
    // report `Superseded` to the other surface — i.e. "nobody decided" about a
    // dialog somebody just answered, which is the one thing D13 must not say.
    let decided = ui
        .prompt
        .as_ref()
        .map(|p| (p.id.clone(), dialog_decision(&p.dialog, key)));
    let result = handle_prompt_key_inner(key, workspace, env, ui);
    if let Some((id, outcome)) = decided {
        let still_open = ui.prompt.as_ref().is_some_and(|p| p.id == id);
        if !still_open {
            ui.dialog_decisions.push((id, outcome));
        }
    }
    result
}

/// What one keypress *means* for an open dialog, read off the dialog's own
/// buttons rather than from a table of key spellings.
///
/// The dialogs do not agree on a cancel key — `n` in the close confirmations,
/// `c` in the push confirmation, `Esc` in the forms — but they all agree on the
/// *label*, because [`prompt_dialog`] writes it. So the button whose accelerator
/// this key fires is the authority, and `Esc` is cancel everywhere by rule
/// (`handle_prompt_key_inner` clears on it before looking at anything else).
///
/// Anything else that closes a dialog is a decision: `Clear` in the status menu
/// and `Abandon` in the sidebar's close menu are choices, not dismissals.
fn dialog_decision(dialog: &Dialog, key: KeyEvent) -> crate::web::protocol::DialogOutcome {
    use crate::web::protocol::DialogOutcome;
    if key.code == KeyCode::Esc {
        return DialogOutcome::Cancelled;
    }
    let pressed = dialog.buttons.iter().find(|b| match b.accel {
        DialogAccel::Char(c) => key.code == KeyCode::Char(c),
        DialogAccel::Enter => key.code == KeyCode::Enter,
        DialogAccel::Esc => key.code == KeyCode::Esc,
        DialogAccel::Tab => key.code == KeyCode::Tab,
    });
    match pressed {
        Some(button) if button.label == "Cancel" => DialogOutcome::Cancelled,
        _ => DialogOutcome::Confirmed,
    }
}

fn handle_prompt_key_inner(
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
        Some(Prompt::ChangeProjectBase { .. }) => {
            return handle_change_project_base_key(key, workspace, env, ui)
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
        // Quit belongs to no project, so it is answered here beside unpair.
        // `y` is the same key the desktop's own dialog prints, which is what
        // lets a browser's confirm reach it as a synthetic keypress rather
        // than through an arm of its own (D13, R8).
        Some(Prompt::QuitConfirm) => {
            ui.prompt = None;
            if key.code == KeyCode::Char('y') {
                ui.should_quit = true;
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
                    if let Prompt::NewAgentForm {
                        selected,
                        branch_selected,
                        use_existing_branch,
                        ..
                    } = &mut pstate.prompt
                    {
                        if *use_existing_branch {
                            *branch_selected = branch_selected.saturating_sub(1);
                        } else {
                            *selected = selected.saturating_sub(1);
                        }
                    }
                    pstate.dialog = prompt_dialog(&pstate.prompt);
                    ui.prompt = Some(pstate);
                }
                KeyCode::Down => {
                    if let Prompt::NewAgentForm {
                        selected,
                        agents,
                        branch,
                        existing_branches,
                        branch_selected,
                        use_existing_branch,
                        ..
                    } = &mut pstate.prompt
                    {
                        let branch_count = matching_branches(existing_branches, branch).len();
                        if *use_existing_branch && *branch_selected + 1 < branch_count {
                            *branch_selected += 1;
                        } else if !*use_existing_branch && *selected + 1 < agents.len() {
                            *selected += 1;
                        }
                    }
                    pstate.dialog = prompt_dialog(&pstate.prompt);
                    ui.prompt = Some(pstate);
                }
                // Tab cycles new branch → existing branch → base branch. Skip
                // the existing mode when there are no non-base local branches.
                KeyCode::Tab => {
                    if let Prompt::NewAgentForm {
                        branch,
                        existing_branches,
                        branch_selected,
                        use_existing_branch,
                        run_on_base,
                        ..
                    } = &mut pstate.prompt
                    {
                        if *run_on_base {
                            *run_on_base = false;
                        } else if *use_existing_branch {
                            *use_existing_branch = false;
                            *run_on_base = true;
                        } else if existing_branches.is_empty() {
                            *run_on_base = true;
                        } else {
                            *use_existing_branch = true;
                        }
                        branch.clear();
                        *branch_selected = 0;
                    }
                    pstate.dialog = prompt_dialog(&pstate.prompt);
                    ui.prompt = Some(pstate);
                }
                KeyCode::Enter => {
                    let (agent_key, name, use_existing_branch, run_on_base) = match &pstate.prompt {
                        Prompt::NewAgentForm {
                            agents,
                            selected,
                            branch,
                            existing_branches,
                            branch_selected,
                            use_existing_branch,
                            run_on_base,
                            ..
                        } => {
                            let name = if *use_existing_branch {
                                matching_branches(existing_branches, branch)
                                    .get(*branch_selected)
                                    .map(|branch| (*branch).clone())
                                    .unwrap_or_default()
                            } else {
                                branch.trim().to_string()
                            };
                            (
                                agents.get(*selected).map(|(k, _)| k.clone()),
                                name,
                                *use_existing_branch,
                                *run_on_base,
                            )
                        }
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
                    let result = if use_existing_branch {
                        state.begin_new_agent_tab_for_existing_branch(
                            &name,
                            agent_key.as_deref(),
                            services,
                        )
                    } else {
                        state.begin_new_agent_tab_ex(
                            &name,
                            agent_key.as_deref(),
                            run_on_base,
                            services,
                        )
                    };
                    match result {
                        Ok(job) => {
                            let branch = job.branch.clone();
                            let msg = if run_on_base {
                                format!("Starting agent on base branch {branch}…")
                            } else if use_existing_branch {
                                format!("Creating worktree for existing branch {branch}…")
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
                        branch_selected,
                        run_on_base,
                        ..
                    } = &mut pstate.prompt
                    {
                        if !*run_on_base {
                            branch.pop();
                            *branch_selected = 0;
                        }
                    }
                    pstate.dialog = prompt_dialog(&pstate.prompt);
                    ui.prompt = Some(pstate);
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if let Prompt::NewAgentForm {
                        branch,
                        branch_selected,
                        run_on_base,
                        ..
                    } = &mut pstate.prompt
                    {
                        if !*run_on_base {
                            branch.push(c);
                            *branch_selected = 0;
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
        Prompt::OpenProject { .. }
        | Prompt::ChangeProjectBase { .. }
        | Prompt::CloseProjectConfirm { .. }
        | Prompt::UnpairConfirm
        | Prompt::QuitConfirm => {
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
        Effect::OpenInFileManager { path, command } => {
            // Success is silent — the file-manager window is the feedback.
            if let Err(e) = crate::tui::file_manager::open(&path, &command) {
                ui.message(format!("Refused: {e}"));
            }
        }
        Effect::AttachedExisting { branch } => {
            ui.message(format!("Attached to existing branch {branch}"))
        }
        Effect::PushWarning(_) => start_prompt(ui, Prompt::PushConfirm),
        Effect::AbandonWarning { dirty } => start_prompt(ui, Prompt::AbandonConfirm { dirty }),
        Effect::QuitConfirm => start_prompt(ui, Prompt::QuitConfirm),
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
        Effect::ShowAbout => ui.overlay = UiOverlay::About,
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
            switch_project(workspace, env, Selector::Next, ui);
            return Ok(());
        }
        PaletteAction::SwitchProjectPrev => {
            switch_project(workspace, env, Selector::Prev, ui);
            return Ok(());
        }
        PaletteAction::ChangeProjectBase => {
            start_change_project_base_flow(workspace, ui);
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
        // FlightDeck Web (D10). Deferred to the event loop, which owns the
        // listener and can report the bound address and D5's warning; a palette
        // action cannot bind a socket from here.
        PaletteAction::StartWebInterface => {
            ui.pending_web_start = true;
            return Ok(());
        }
        PaletteAction::StopWebInterface => {
            ui.pending_web_stop = true;
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
            start_new_tab_flow(state, &services, ui);
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
        | PaletteAction::ChangeProjectBase
        | PaletteAction::OpenConfig
        | PaletteAction::PairPhone
        | PaletteAction::UnpairPhone
        | PaletteAction::StartWebInterface
        | PaletteAction::StopWebInterface => Ok(()),
    }
}

// ---------------------------------------------------------------------------
// FlightDeck Web: the browser's command surface (specs/WEB_INTERFACE.md §1)
// ---------------------------------------------------------------------------

/// Apply one browser [`Command`](crate::web::protocol::Command) frame and return
/// the [`Ack`](crate::web::protocol::Ack) to send back to that viewer.
///
/// **The browser is a second way to choose a palette row, not a second way to
/// run one** (§1). Nothing here performs a command: a
/// [`crate::web::commands::Route::Palette`] row carries the very
/// [`PaletteAction`] the TUI's palette hands to [`run_palette_action`] on Enter,
/// and this passes it into that same function. There is deliberately no arm
/// that reimplements an effect — that drift is what the decision exists to
/// prevent.
///
/// The `Ack` is derived from what the dispatch actually did
/// ([`Ui::web_outcome`]), never assumed: a command that hit a safety guard acks
/// `Rejected` with the guard's own sentence, so the browser shows the same
/// refusal the desktop user would have read.
///
/// The refusing arms are defence in depth. [`crate::web::server`] answers them
/// before a frame is ever forwarded — which is why a bare frame naming `quit`
/// cannot reach a dispatch — so reaching them here would mean the table and the
/// server had disagreed.
fn run_web_command(
    command: &crate::web::protocol::Command,
    origin: &crate::web::protocol::DialogOrigin,
    workspace: &mut Workspace,
    env: &Env,
    ui: &mut Ui,
    activity: &mut crate::web::activity::ActivityStore,
) -> crate::web::protocol::Ack {
    use crate::web::commands::Route;
    use crate::web::protocol::{Ack, AckOutcome};

    let ack = |outcome, detail: Option<String>| Ack {
        seq: command.seq,
        outcome,
        detail,
    };

    let Some(spec) = crate::web::commands::lookup(&command.name) else {
        return ack(
            AckOutcome::Rejected,
            Some(format!(
                "`{}` is not a command this FlightDeck has.",
                command.name
            )),
        );
    };

    match &spec.route {
        // D11: read-marking is host state, so a second tab — or the same tab
        // tomorrow — backfills a feed that agrees about what has been seen.
        Route::ActivityRead => crate::web::activity::apply_mark_read(activity, command),
        // D3: the selection is shared, so this moves the desktop too.
        Route::Selection(target) => {
            match apply_web_selection(*target, command.args.as_ref(), workspace, env, ui) {
                Ok(detail) => ack(AckOutcome::Applied, detail),
                Err(reason) => ack(AckOutcome::Rejected, Some(reason)),
            }
        }
        Route::Palette(action) => {
            ui.web_outcome = None;
            // D13: for as long as this dispatch runs, a dialog it opens was
            // opened *by this browser*. `start_prompt` reads it; nothing else
            // has to know a browser exists.
            let was_open = ui.dialog_id();
            ui.web_dialog_origin = Some(origin.clone());
            let dispatched = run_palette_action(action.clone(), workspace, env, ui);
            ui.web_dialog_origin = None;
            let opened = ui.dialog_id().filter(|id| Some(id) != was_open.as_ref());
            match (dispatched, ui.web_outcome.take(), opened) {
                (Err(e), _, _) => ack(AckOutcome::Rejected, Some(e.to_string())),
                (Ok(()), Some(WebDispatch::Refused(reason)), _) => {
                    ack(AckOutcome::Rejected, Some(reason))
                }
                (Ok(()), Some(WebDispatch::Failed(error)), _) => {
                    ack(AckOutcome::Rejected, Some(error))
                }
                // A dialog *is* the outcome (D13): the row asked a question, and
                // the question is now open on both surfaces. Applied, because
                // something really happened and the browser can see it — the
                // pre-D13 `Rejected` said "a modal appeared on a screen you
                // cannot read", which is no longer true.
                (Ok(()), _, Some(_)) => {
                    ack(AckOutcome::Applied, Some(DIALOG_OPENED_DETAIL.to_string()))
                }
                (Ok(()), Some(WebDispatch::Applied(detail)), None) => {
                    ack(AckOutcome::Applied, detail)
                }
                // Nothing classified an outcome, which for the forwarded set
                // means it did its work quietly (a selection move, a split-view
                // toggle). Applied with no sentence rather than an invented one.
                (Ok(()), None, None) => ack(AckOutcome::Applied, None),
            }
        }
        // D13: either surface can answer the dialog the other one opened.
        Route::Dialog(act) => {
            match apply_web_dialog(*act, command.args.as_ref(), workspace, env, ui) {
                Ok(detail) => ack(AckOutcome::Applied, detail),
                Err(reason) => ack(AckOutcome::Rejected, Some(reason)),
            }
        }
        Route::Server => ack(
            AckOutcome::Ignored,
            Some(format!(
                "`{}` is answered by the server and should not have reached the host.",
                spec.name
            )),
        ),
        Route::Rejected(reason) | Route::NotSupported(reason) => {
            ack(AckOutcome::Rejected, Some((*reason).to_string()))
        }
    }
}

// ---------------------------------------------------------------------------
// FlightDeck Web: D13's shared dialog
// ---------------------------------------------------------------------------

/// The open dialog as the browser receives it, or `None` when none is open.
///
/// Read straight off [`Ui::prompt`] — the state the desktop is already rendering
/// — which is what D13's "no new state" means concretely. `kind` is the machine
/// name for the flow; `title` and `body` are the same words and the same buttons
/// the desktop is showing, because both come from the one
/// [`crate::tui::render::Dialog`] [`prompt_dialog`] built.
///
/// The origin line is **not** duplicated into the body: `DialogView::origin`
/// already carries it structurally, and the browser words it itself. Only the
/// desktop needs the sentence, which is why the sentence lives on the desktop's
/// render model.
fn web_dialog_view(
    ui: &Ui,
    project_name: &str,
    project: &AppState,
) -> Option<crate::web::protocol::DialogView> {
    use crate::web::commands::BrowserConfirm;
    use crate::web::protocol as wire;

    let open = ui.prompt.as_ref()?;
    // Artboard 1g's second step, if this dialog has one for a browser. The
    // expected name is *published*, because 1g draws it as the field's own hint:
    // the gate buys deliberateness, not secrecy. A gate whose subject the host
    // can no longer name is the one case that turns into an outright refusal —
    // see `GATE_UNRESOLVED_REFUSAL`.
    let (confirm_gate, refusal) = match browser_confirm_gate(&open.prompt) {
        BrowserConfirm::OneStep => (None, None),
        BrowserConfirm::TypedName(gate) => {
            match gate_expectation(gate.subject, project_name, project) {
                Some(expected) => (
                    Some(wire::ConfirmGate {
                        key: gate.key.to_string(),
                        expected,
                        instruction: gate.instruction.to_string(),
                    }),
                    None,
                ),
                None => (None, Some(crate::web::commands::GATE_UNRESOLVED_REFUSAL)),
            }
        }
    };
    let body = wire::DialogBody {
        input: open.dialog.input.clone(),
        list: open
            .dialog
            .list
            .iter()
            .map(|item| wire::DialogChoice {
                label: item.label.clone(),
                selected: item.selected,
            })
            .collect(),
        buttons: open
            .dialog
            .buttons
            .iter()
            .map(|button| wire::DialogKey {
                key: dialog_accel_key(button.accel),
                label: button.label.clone(),
            })
            .collect(),
        confirmable: refusal.is_none(),
        refusal: refusal.map(str::to_string),
        confirm_gate,
    };
    Some(wire::DialogView {
        dialog_id: open.id.clone(),
        kind: dialog_kind(&open.prompt).to_string(),
        title: open.dialog.title.clone(),
        origin: open.origin.clone(),
        // `serde_json::to_value` on a struct of `String`s and `bool`s cannot
        // fail; `None` rather than an `unwrap` so a future body that could fail
        // degrades to "no body" instead of taking the event loop with it.
        body: serde_json::to_value(&body).ok(),
    })
}

/// The wire `kind` for one prompt (D13). Stable strings: the browser switches on
/// them to pick a form, and an unknown one renders the generic shell, so
/// renaming one is a breaking change and adding one is not.
fn dialog_kind(prompt: &Prompt) -> &'static str {
    match prompt {
        Prompt::NewAgentForm { .. } => "new_agent",
        Prompt::SelectChildAgent { .. } => "new_agent_child",
        Prompt::RenameTab { .. } => "rename_session",
        Prompt::SetManualStatus => "set_manual_status",
        Prompt::CloseTab { .. } => "close_session",
        Prompt::CloseChildConfirm { .. } => "close_terminal",
        Prompt::CloseAgentChoice { .. } => "close_session_choice",
        Prompt::PushConfirm => "confirm_push",
        Prompt::AbandonConfirm { .. } => "confirm_abandon",
        Prompt::MergeConfirm { .. } => "confirm_merge",
        Prompt::RebaseConfirm { .. } => "confirm_rebase",
        Prompt::OpenProject { .. } => "open_project",
        Prompt::CloseProjectConfirm { .. } => "close_project",
        Prompt::UnpairConfirm => "unpair_phone",
        Prompt::QuitConfirm => "confirm_quit",
    }
}

/// What a **browser** must do to confirm this dialog: press the button, or press
/// it *and* type a name back (artboard 1g, `specs/WEB_INTERFACE.md` §6.5 R13).
///
/// **The trigger is the surface, not the command.** 1g's step 2 says so itself —
/// *"This browser is remote. Type the session name to run the rebase on the
/// host."* — so this function describes browsers only. The desktop's dialogs are
/// untouched: nothing reaches step 2 there, because the person answering is at
/// the machine the effect lands on. That is also why 1g's caption can enumerate
/// only two dialogs while the artboard draws a third: the caption is counting
/// the desktop's world, and this is the remote one.
///
/// From a browser the gate covers the three answers that destroy work or rewrite
/// history — §5/§15's abandon, §5.1's rebase, and D16's quit. `Push Branch` and `Finish / Local Merge`
/// deliberately stay one-step: neither rewrites history nor discards anything, a
/// push is undone by a force-push the user still owns, and a merge-back is a
/// commit on the base branch — so 1g's friction would be ceremony rather than
/// protection, and ceremony teaches people to type the name without reading it.
///
/// Exhaustive on purpose: a prompt added later must say where it stands.
///
/// **Cancelling is never gated**, here or anywhere below: dismissing a
/// confirmation cannot destroy anything, and a shared dialog a remote surface
/// can see but not dismiss would be worse than not sharing it (R8).
fn browser_confirm_gate(prompt: &Prompt) -> crate::web::commands::BrowserConfirm {
    use crate::web::commands::{
        BrowserConfirm, GateSubject, TypedNameGate, GATE_ABANDON_INSTRUCTION,
        GATE_QUIT_INSTRUCTION, GATE_REBASE_INSTRUCTION,
    };
    match prompt {
        // SPECS §5/§15: the worktree and everything uncommitted in it goes.
        // `y` is the button `prompt_dialog` prints for both spellings of the
        // question ("Abandon" / "Abandon (force)").
        Prompt::AbandonConfirm { .. } => BrowserConfirm::TypedName(TypedNameGate {
            key: "y",
            subject: GateSubject::SelectedSession,
            instruction: GATE_ABANDON_INSTRUCTION,
        }),
        // SPECS §5.1's sanctioned history rewrite, and the one artboard 1g
        // actually draws its two steps around.
        Prompt::RebaseConfirm { .. } => BrowserConfirm::TypedName(TypedNameGate {
            key: "y",
            subject: GateSubject::SelectedSession,
            instruction: GATE_REBASE_INSTRUCTION,
        }),
        // D16: quit stops FlightDeck and every agent in it. Not one session's
        // work, so not one session's name — the project the browser is looking
        // at is what it names.
        Prompt::QuitConfirm => BrowserConfirm::TypedName(TypedNameGate {
            key: "y",
            subject: GateSubject::ActiveProject,
            instruction: GATE_QUIT_INSTRUCTION,
        }),
        // The rest are one step, exactly as they are on the desktop. The two git
        // confirmations that are not a rewrite (`remote-control-ll5.5`, SPECS
        // §14/§15) are here deliberately: these dialogs **are** §5's
        // confirmation, a browser is a user surface, and the unconfirmed value
        // that opened them came from `web::commands::INVENTORY` rather than from
        // the frame. See `specs/WEB_INTERFACE.md` §6.5 R11.
        // The sidebar's close menu (`a` Abandon / `c` Close / `n` Cancel) is
        // **not** gated, and that is precision rather than a hole: `a` discards
        // nothing, it dispatches `AbandonWorktree { confirm: false }`, which
        // always asks — so the browser lands on `AbandonConfirm` above and takes
        // step 2 there, once, in front of the button that really does it.
        Prompt::CloseAgentChoice { .. }
        | Prompt::PushConfirm
        | Prompt::MergeConfirm { .. }
        | Prompt::NewAgentForm { .. }
        | Prompt::SelectChildAgent { .. }
        | Prompt::RenameTab { .. }
        | Prompt::SetManualStatus
        | Prompt::CloseTab { .. }
        | Prompt::CloseChildConfirm { .. }
        | Prompt::OpenProject { .. }
        | Prompt::CloseProjectConfirm { .. }
        | Prompt::UnpairConfirm => BrowserConfirm::OneStep,
    }
}

/// The exact name a [`crate::web::commands::TypedNameGate`] expects, read off
/// the live workspace.
///
/// One function, called from **two places that must not disagree**: the dialog
/// the browser is shown (`web_dialog_view`) and the check the confirm passes
/// (`apply_web_dialog`). A second spelling of "which name is that" is how a gate
/// becomes unpassable or, worse, passable with the wrong name.
///
/// `None` means the host cannot name the subject any more — the tab was closed
/// while its question was on screen. The confirm is refused; see
/// [`crate::web::commands::GATE_UNRESOLVED_REFUSAL`].
fn gate_expectation(
    subject: crate::web::commands::GateSubject,
    project_name: &str,
    project: &AppState,
) -> Option<String> {
    use crate::web::commands::GateSubject;
    match subject {
        // The session name, not the branch: 1g's field hints
        // `fix-login-redirect` while the dialog above it names
        // `flightdeck/fix-login-redirect`, and the name is what the sidebar
        // shows the person typing it.
        GateSubject::SelectedSession => {
            let index = project.selected_tab?;
            Some(project.tabs.get(index)?.meta.name.clone())
        }
        GateSubject::ActiveProject => Some(project_name.to_string()),
    }
}

/// The key label for an accelerator, matching what the desktop prints on the
/// button and what [`crate::web::protocol::DialogKey::key`] carries.
fn dialog_accel_key(accel: DialogAccel) -> String {
    match accel {
        DialogAccel::Char(c) => c.to_string(),
        DialogAccel::Enter => "Enter".to_string(),
        DialogAccel::Esc => "Esc".to_string(),
        DialogAccel::Tab => "Tab".to_string(),
    }
}

/// The accelerator a key label names, if any.
fn dialog_accel_from_key(key: &str) -> Option<DialogAccel> {
    match key {
        "Enter" => Some(DialogAccel::Enter),
        "Esc" => Some(DialogAccel::Esc),
        "Tab" => Some(DialogAccel::Tab),
        other => {
            let mut chars = other.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => Some(DialogAccel::Char(c)),
                _ => None,
            }
        }
    }
}

/// Answer the open dialog on behalf of a browser (D13).
///
/// **Every path here is a keypress.** The browser's confirm becomes the exact
/// sequence of [`KeyEvent`]s the desktop's own keyboard (and its dialog buttons,
/// via [`trigger_dialog_button`]) would produce, fed through
/// [`handle_prompt_key`]. That is the whole reason there is no second dialog
/// engine to keep in step: `New Agent Session Tab` confirmed from a browser runs
/// [`AppState::begin_new_agent_tab_ex`] because a synthetic `Enter` reached the
/// same arm a real one does.
///
/// It also bounds what a browser can ask for, structurally: `choice` must name a
/// button the dialog is *currently showing*, `text` is ignored by a dialog with
/// no input field, and `toggle` needs a `Tab` button to exist. A browser cannot
/// press a key the person at the desktop cannot see.
fn apply_web_dialog(
    act: crate::web::commands::DialogAct,
    args: Option<&serde_json::Value>,
    workspace: &mut Workspace,
    env: &Env,
    ui: &mut Ui,
) -> std::result::Result<Option<String>, String> {
    use crate::web::commands::DialogAct;

    let named = web_string_arg(args, "dialog_id")?;
    let Some(open) = ui.prompt.as_ref() else {
        return Err(
            "No dialog is open — it was answered on the other surface. Ask for a \
             fresh snapshot."
                .to_string(),
        );
    };
    if open.id.as_str() != named {
        // The dialog moved on between the browser rendering it and answering.
        // Refused rather than applied to whatever is on screen now, which is
        // exactly how somebody confirms something they never read.
        return Err(format!(
            "Dialog `{named}` is not the dialog that is open now — it was \
             replaced. Ask for a fresh snapshot."
        ));
    }

    if act == DialogAct::Cancel {
        feed_dialog_key(KeyCode::Esc, workspace, env, ui);
        return Ok(Some("Cancelled the dialog.".to_string()));
    }

    let gate = browser_confirm_gate(&open.prompt);
    let dialog = open.dialog.clone();
    let choice = args.and_then(|a| a.get("choice")).and_then(|c| c.as_str());
    let text = args.and_then(|a| a.get("text")).and_then(|t| t.as_str());
    let toggle = args
        .and_then(|a| a.get("toggle"))
        .and_then(|t| t.as_bool())
        .unwrap_or(false);
    let list_index = args
        .and_then(|a| a.get("list_index"))
        .and_then(|i| i.as_u64());

    // The deciding key: the button `choice` names, or the primary (every
    // `prompt_dialog` puts the affirmative action first). `Esc` is never a
    // confirm — `dialog_cancel` is the frame for that.
    let deciding = match choice {
        Some(key) => {
            let accel = dialog_accel_from_key(key)
                .filter(|accel| dialog.buttons.iter().any(|b| b.accel == *accel))
                .ok_or_else(|| {
                    format!(
                        "This dialog has no `{key}` button; it shows {}.",
                        button_keys(&dialog)
                    )
                })?;
            accel
        }
        None => dialog
            .buttons
            .first()
            .map(|b| b.accel)
            .ok_or_else(|| "This dialog has no buttons to press.".to_string())?,
    };
    if deciding == DialogAccel::Esc {
        return Err(
            "`Esc` cancels — send `dialog_cancel` for that, so the other surface \
             is told the dialog was dismissed rather than confirmed."
                .to_string(),
        );
    }

    // **Artboard 1g's second step, before a single key is fed.** Everything
    // below this point synthesises keypresses into the live prompt, so the gate
    // is checked here and nowhere later: a refusal returns with the dialog
    // untouched, which is what makes "the effect provably does not occur" a
    // property of the control flow rather than of a rollback.
    //
    // Only the button the gate names is behind it — every other answer this
    // dialog offers is one press away — and cancelling never reaches here at
    // all, because `DialogAct::Cancel` returned above.
    if let crate::web::commands::BrowserConfirm::TypedName(gate) = gate {
        if dialog_accel_key(deciding) == gate.key {
            let active = workspace.active_project();
            let expected = gate_expectation(gate.subject, &active.name, &active.state)
                .ok_or_else(|| crate::web::commands::GATE_UNRESOLVED_REFUSAL.to_string())?;
            match args
                .and_then(|a| a.get("confirm_name"))
                .and_then(|n| n.as_str())
            {
                // Step 1 only: the browser pressed the button and sent no name.
                None => {
                    return Err(crate::web::commands::gate_step_refusal(&gate, &expected));
                }
                // Byte-for-byte. No trim, no case fold, no normalisation — see
                // `gate_mismatch_refusal` for why each of those was rejected.
                Some(typed) if typed == expected => {}
                Some(typed) => {
                    return Err(crate::web::commands::gate_mismatch_refusal(
                        typed, &expected,
                    ));
                }
            }
        }
    }

    // 1. The `Tab` option (1e's "run from base branch"), if asked for.
    if toggle {
        if !dialog.buttons.iter().any(|b| b.accel == DialogAccel::Tab) {
            return Err("This dialog has no `Tab` option to toggle.".to_string());
        }
        feed_dialog_key(KeyCode::Tab, workspace, env, ui);
    }
    // 2. The choice row (1e's agent radio). Driven to the top first, so the
    //    index the browser sent is absolute rather than relative to wherever the
    //    highlight happened to be — the desktop may have moved it since.
    if let Some(index) = list_index {
        if dialog.list.is_empty() {
            return Err("This dialog has no list to choose from.".to_string());
        }
        if index as usize >= dialog.list.len() {
            return Err(format!(
                "This dialog has {} choices, so `list_index: {index}` names none of them.",
                dialog.list.len()
            ));
        }
        for _ in 0..dialog.list.len() {
            feed_dialog_key(KeyCode::Up, workspace, env, ui);
        }
        for _ in 0..index {
            feed_dialog_key(KeyCode::Down, workspace, env, ui);
        }
    }
    // 3. The text field. Typed character by character, exactly as a person
    //    would: the handlers own what a character means, including refusing it
    //    while the field is disabled.
    if let Some(text) = text {
        if dialog.input.is_none() {
            return Err("This dialog has no text field.".to_string());
        }
        for c in text.chars() {
            if c.is_control() {
                return Err("A dialog's text field takes printable characters only.".to_string());
            }
            feed_dialog_key(KeyCode::Char(c), workspace, env, ui);
        }
    }
    // 4. The decision.
    let code = match deciding {
        DialogAccel::Char(c) => KeyCode::Char(c),
        DialogAccel::Enter => KeyCode::Enter,
        DialogAccel::Tab => KeyCode::Tab,
        DialogAccel::Esc => KeyCode::Esc,
    };
    feed_dialog_key(code, workspace, env, ui);

    if ui.prompt.is_some() {
        // Nothing wrong happened: a form that rejects an empty branch name keeps
        // prompting, on both surfaces. Reported as a refusal rather than as an
        // application, because nothing was applied.
        return Err(
            "The dialog is still open — it needs something it did not get. It is \
             showing why on both surfaces."
                .to_string(),
        );
    }
    // The sentence the desktop showed, if it showed one — the same rule
    // `Ui::web_outcome` follows for a palette dispatch. Confirming a dialog and
    // the action behind it failing are two different facts, and the browser is
    // entitled to the second one in the host's own words rather than a cheerful
    // "confirmed" over a red notification the desktop is reading.
    match desktop_notification(ui) {
        Some(sentence) if sentence.starts_with("Error:") || sentence.starts_with("Refused:") => {
            Err(sentence)
        }
        Some(sentence) => Ok(Some(sentence)),
        None => Ok(Some("Confirmed the dialog.".to_string())),
    }
}

/// The notification dialog the desktop is showing, if any. `None` when the
/// screen is back to the main view, which is the silent-success case.
fn desktop_notification(ui: &Ui) -> Option<String> {
    match &ui.overlay {
        UiOverlay::Dialog(dialog) => Some(dialog.title.clone()),
        _ => None,
    }
}

/// The keys a dialog is showing, for a refusal that says what *would* work.
fn button_keys(dialog: &Dialog) -> String {
    let keys: Vec<String> = dialog
        .buttons
        .iter()
        .map(|b| format!("`{}`", dialog_accel_key(b.accel)))
        .collect();
    keys.join(", ")
}

/// One synthetic keypress into the open prompt. A no-op once the prompt has
/// closed, so a sequence that ends early (a handler that took the decision on an
/// earlier key) cannot leak keystrokes into whatever is on screen next.
fn feed_dialog_key(code: KeyCode, workspace: &mut Workspace, env: &Env, ui: &mut Ui) {
    if ui.prompt.is_none() {
        return;
    }
    let key = KeyEvent::new(code, KeyModifiers::NONE);
    if let Err(e) = handle_prompt_key(key, workspace, env, ui) {
        ui.message(format!("Error: {e}"));
    }
}

/// Upgrade the diff's `Superseded` frames with the outcomes somebody actually
/// decided (D13).
///
/// `crate::web::stream::deltas` compares two published states, so all it can
/// honestly say about a dialog that is gone is [`DialogOutcome::Superseded`] —
/// it did not witness a decision. [`handle_prompt_key`] did, and recorded it in
/// [`Ui::dialog_decisions`]. This is the one place the two meet, so the browser
/// learns `Confirmed` when the desktop pressed `y` and `Superseded` only when a
/// dialog really was replaced without an answer.
///
/// A decision with no matching frame is dropped, correctly: the dialog opened
/// and closed within one tick, so no surface was ever told it existed.
fn resolve_dialog_outcomes(
    frames: &mut [crate::web::protocol::Delta],
    decided: &[(
        crate::web::protocol::DialogId,
        crate::web::protocol::DialogOutcome,
    )],
) {
    use crate::web::protocol::Delta;
    for frame in frames.iter_mut() {
        if let Delta::DialogClosed { dialog_id, outcome } = frame {
            if let Some((_, decided)) = decided.iter().find(|(id, _)| id == dialog_id) {
                *outcome = *decided;
            }
        }
    }
}

/// Move the shared selection (D3) on behalf of a browser, through the same
/// functions the desktop's own palette and mouse clicks use: [`switch_project`]
/// for a project, [`Command::SwitchAgentTab`] for a session, [`select_target`]
/// for a terminal.
///
/// `Ok(detail)` is the sentence to ack with; `Err(reason)` is a refusal — a
/// stale id from a browser whose snapshot has drifted, or a guard (an isolated
/// run has one project by construction) saying no.
fn apply_web_selection(
    target: crate::web::commands::SelectionTarget,
    args: Option<&serde_json::Value>,
    workspace: &mut Workspace,
    env: &Env,
    ui: &mut Ui,
) -> std::result::Result<Option<String>, String> {
    use crate::web::commands::SelectionTarget;

    match target {
        SelectionTarget::Project => {
            let id = web_string_arg(args, "project_id")?;
            let index = workspace
                .projects
                .iter()
                // The same id `build_web_host_state` mints: the repository root.
                .position(|p| p.git.root().display().to_string() == id)
                .ok_or_else(|| stale_id("project", &id))?;
            select_web_project(workspace, env, ui, index)?;
            Ok(Some(format!(
                "Selected project {}",
                workspace.active_project().name
            )))
        }
        SelectionTarget::Session => {
            let id = web_string_arg(args, "session_id")?;
            let (project, tab) =
                locate_web_session(workspace, &id).ok_or_else(|| stale_id("session", &id))?;
            select_web_session(workspace, env, ui, project, tab)?;
            Ok(Some(format!(
                "Selected session {}",
                workspace.projects[project].state.tabs[tab].meta.name
            )))
        }
        SelectionTarget::Terminal => {
            let id = web_string_arg(args, "terminal_id")?;
            let (project, tab, child) =
                locate_web_terminal(workspace, &id).ok_or_else(|| stale_id("terminal", &id))?;
            // A terminal implies its session (D3 keeps one selection for the
            // whole instance), so the session moves first.
            select_web_session(workspace, env, ui, project, tab)?;
            let p = &mut workspace.projects[project];
            let services = env.services(&p.git);
            select_target(&mut p.state, &services, child);
            Ok(None)
        }
    }
}

/// One required string argument off a `Command` frame's `args` object.
fn web_string_arg(
    args: Option<&serde_json::Value>,
    key: &str,
) -> std::result::Result<String, String> {
    args.and_then(|args| args.get(key))
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .ok_or_else(|| format!("This command needs a `{key}` argument."))
}

/// The refusal for an id the host does not have — almost always a browser whose
/// snapshot predates a close (Q3), which is why it says so rather than just
/// failing.
fn stale_id(kind: &str, id: &str) -> String {
    format!("No {kind} `{id}` is open — this tab's view is out of date; ask for a fresh snapshot.")
}

/// Make `index` the active project unless it already is, reusing
/// [`switch_project`] so the isolated-run refusal (SPECS §32) and the agent
/// resume both still happen.
fn select_web_project(
    workspace: &mut Workspace,
    env: &Env,
    ui: &mut Ui,
    index: usize,
) -> std::result::Result<(), String> {
    if workspace.active == index {
        return Ok(());
    }
    ui.web_outcome = None;
    switch_project(workspace, env, Selector::Index(index), ui);
    match ui.web_outcome.take() {
        Some(WebDispatch::Refused(reason)) | Some(WebDispatch::Failed(reason)) => Err(reason),
        _ => Ok(()),
    }
}

/// Select one session, switching project first if it lives in another one — a
/// browser can be looking at a background project, and D3 says the desktop
/// follows.
fn select_web_session(
    workspace: &mut Workspace,
    env: &Env,
    ui: &mut Ui,
    project: usize,
    tab: usize,
) -> std::result::Result<(), String> {
    select_web_project(workspace, env, ui, project)?;
    if workspace.projects[project].state.selected_tab == Some(tab) {
        return Ok(());
    }
    let p = &mut workspace.projects[project];
    let services = env.services(&p.git);
    ui.web_outcome = None;
    dispatch_command(
        Command::SwitchAgentTab(Selector::Index(tab)),
        &mut p.state,
        &services,
        ui,
    )
    .map_err(|e| e.to_string())?;
    match ui.web_outcome.take() {
        Some(WebDispatch::Refused(reason)) | Some(WebDispatch::Failed(reason)) => Err(reason),
        _ => Ok(()),
    }
}

/// Find a wire session id among every open project's tabs.
fn locate_web_session(workspace: &Workspace, session_id: &str) -> Option<(usize, usize)> {
    workspace.projects.iter().enumerate().find_map(|(pi, p)| {
        p.state
            .tabs
            .iter()
            .position(|tab| tab.meta.id == session_id)
            .map(|ti| (pi, ti))
    })
}

/// Find a wire terminal id among every open project's terminals, returning the
/// [`ChildTarget`] that selects it.
///
/// The ids are rebuilt with [`crate::web::stream`]'s own minters rather than
/// parsed, so this cannot drift from the spelling the snapshot published.
fn locate_web_terminal(
    workspace: &Workspace,
    terminal_id: &str,
) -> Option<(usize, usize, ChildTarget)> {
    for (pi, p) in workspace.projects.iter().enumerate() {
        for (ti, tab) in p.state.tabs.iter().enumerate() {
            if crate::web::stream::primary_terminal_id(&tab.meta.id).as_str() == terminal_id {
                return Some((pi, ti, ChildTarget::Primary));
            }
            for c in 0..tab.session.child_count() {
                let matches = tab.session.child(c).is_some_and(|child| {
                    crate::web::stream::child_terminal_id(&tab.meta.id, child.stream_id()).as_str()
                        == terminal_id
                });
                if matches {
                    return Some((pi, ti, ChildTarget::Child(c)));
                }
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Configuration manager (SPECS §8)
// ---------------------------------------------------------------------------

/// Build and open the configuration manager for the active project, reading the
/// global base and this project's override files into an editable model. Ensures
/// the global base exists first so it is always editable — except in an
/// isolated run, which must not create `~/.flightdeck/config.toml` merely by
/// being opened (SPECS §32); the manager still opens and shows the effective
/// settings without it.
fn open_config_manager(workspace: &Workspace, env: &Env, ui: &mut Ui) {
    let global_path = global_config_path();
    // An isolated run writes nothing to the user's config on its own (SPECS
    // §32): merely opening the manager to look must not create
    // `~/.flightdeck/config.toml`. `read_table` below tolerates a missing
    // file, so the manager still opens and shows the effective settings.
    if !workspace.active_project().state.isolated {
        if let Some(gp) = &global_path {
            let _ = ensure_global_config(env.fs, gp);
        }
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
    // While an inline text field (e.g. the relay URL) is being edited, keystrokes
    // go to the editor: type to insert, Backspace to delete, Enter to commit, Esc
    // to cancel. Nothing else (navigation, save, scope switch) fires until the
    // edit is resolved.
    if cm.is_editing() {
        match key.code {
            KeyCode::Esc => cm.cancel_edit(),
            KeyCode::Enter => cm.commit_edit(),
            KeyCode::Backspace => cm.edit_backspace(),
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                cm.edit_push_char(c)
            }
            _ => {}
        }
        return Ok(());
    }
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
        if let Ok(mut cfg) = loaded {
            let base = cfg.project.default_base_branch.clone();
            if p.git.branch_exists(&base).unwrap_or(false) {
                p.state.reload_config(cfg);
                p.state.invalid_base_branch = None;
                p.state
                    .warnings
                    .retain(|warning| !warning.starts_with("Configured default base '"));
            } else {
                // Apply unrelated config edits while retaining the last valid
                // runtime default. The raw file remains untouched so the user
                // can fix the invalid branch through the picker or editor.
                cfg.project.default_base_branch = p.state.base_branch.clone();
                p.state.reload_config(cfg);
                p.state.invalid_base_branch = Some(base.clone());
                p.state
                    .warnings
                    .retain(|warning| !warning.starts_with("Configured default base '"));
                let warning = format!(
                    "Configured default base '{base}' is not a local branch; keeping '{}'.",
                    p.state.base_branch
                );
                if !p.state.warnings.contains(&warning) {
                    p.state.warnings.push(warning);
                }
            }
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
    mut tee: impl FnMut(&str, Option<usize>, u64, &[u8]),
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
                    // Tee the raw primary bytes to every consumer that wants
                    // them: the remote transcript builder and the web interface's
                    // replay ring (`None` = primary; a no-op when both are off).
                    // This is the one place a PTY is read, so it is the only
                    // honest place to tee from — the browser and the desktop's
                    // own `vt100` parse see byte-for-byte the same chunk (D2).
                    // `tab.meta` is a disjoint field from `tab.session`, so this
                    // borrows cleanly. The `0` is the primary's unused mint: a
                    // session has at most one primary, so it needs no counter.
                    tee(&tab.meta.id, None, 0, &bytes);
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
                // The mint, not the index: a browser byte cursor keyed by
                // position would resume the wrong stream after a child is
                // closed (see `web::protocol::TerminalId`).
                let stream_id = child.stream_id();
                if let Ok(bytes) = child.session_mut().try_read_output() {
                    if !bytes.is_empty() {
                        child.process_output(&bytes);
                        child.answer_cursor_position_query(&bytes);
                        tee(&tab.meta.id, Some(c), stream_id, &bytes);
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
///
/// Also re-derives `state.pty_size` from the current mode, so toggling between
/// APP and TERMINAL resizes the agent PTY to match the chrome that is drawn.
fn sync_terminal_sizes(state: &mut AppState, full: PtySize) {
    // Collapse follows the input mode, not just the window size, so re-derive
    // the viewport every frame rather than only on `Event::Resize`.
    // `resize_if_changed` below makes the frames where nothing moved free.
    state.pty_size = viewport_pty_size(
        full,
        state.mode(),
        crate::tui::mode_style::border_enabled(&state.config.ui),
    );

    let Some(idx) = state.selected_tab else {
        return;
    };

    if state.split_view {
        let area = Rect::new(0, 0, full.cols, full.rows);
        let ml = crate::tui::layout::compute(
            area,
            crate::tui::layout::chrome_for(area, state.mode()),
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
            crate::tui::layout::Chrome::Full,
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

/// Remove an isolated run's temp status directory (SPECS §32). Best effort: a
/// leftover directory under the OS temp dir is harmless, and teardown must never
/// fail on it.
fn cleanup_isolated_run(fs: &dyn FileSystem, status_dir: &Path) {
    let _ = fs.remove_dir_all(status_dir);
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
    use crate::contracts::{
        AgentDef, Config, StatusPatterns, TabState, UiConfig, WorktreeInfo, WorktreesConfig,
    };
    use crate::testing::{FakeClock, FakeFs, FakeGit, FakePty};
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    fn argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parse_isolated_accepts_both_spellings() {
        assert!(parse_isolated(&argv(&["flightdeck", "--isolated"])).unwrap());
        assert!(parse_isolated(&argv(&["flightdeck", "-I"])).unwrap());
    }

    #[test]
    fn parse_isolated_defaults_to_off() {
        assert!(!parse_isolated(&argv(&["flightdeck"])).unwrap());
    }

    #[test]
    fn parse_isolated_is_not_confused_by_lowercase_i() {
        // `-i` is not the flag; only the documented `-I` is.
        assert!(!parse_isolated(&argv(&["flightdeck", "-i"])).unwrap());
    }

    #[test]
    fn parse_isolated_refuses_a_subcommand() {
        let err = parse_isolated(&argv(&["flightdeck", "-I", "doctor"])).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("--isolated") && msg.contains("doctor"),
            "the error must name the flag and the offending subcommand: {msg}"
        );
    }

    #[test]
    fn parse_isolated_refuses_a_subcommand_given_first() {
        assert!(parse_isolated(&argv(&["flightdeck", "image", "--isolated"])).is_err());
    }

    // --- FlightDeck Web lifecycle messages (D5, D10) ----------------------

    /// D5: binding a routable address is the one web setting that changes who
    /// can reach the user's agents, so the line that reports it must warn. A
    /// loopback bind must *not* warn, or the warning becomes noise nobody reads.
    #[test]
    fn a_routable_bind_is_warned_about_and_a_loopback_one_is_not() {
        use crate::web::server::BindExposure;

        let loopback = web_started_message(
            "127.0.0.1:8477".parse().expect("a valid address"),
            BindExposure::Loopback,
        );
        assert!(loopback.contains("127.0.0.1:8477"));
        assert!(
            !loopback.to_lowercase().contains("warning"),
            "a loopback bind is the safe default and must not cry wolf: {loopback}"
        );
        assert!(loopback.contains("this machine only"));

        let routable = web_started_message(
            "0.0.0.0:8477".parse().expect("a valid address"),
            BindExposure::Routable,
        );
        assert!(
            routable.contains("WARNING"),
            "a routable bind must say so: {routable}"
        );
        assert!(
            routable.contains("drive your agents"),
            "and must say what the consequence is, not just that there is one: {routable}"
        );
    }

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
    fn project_base_picker_filters_and_marks_the_selected_branch() {
        let branches = vec![
            "develop".to_string(),
            "feature/base-ui".to_string(),
            "main".to_string(),
        ];
        let matches: Vec<&str> = matching_branches(&branches, "BASE")
            .into_iter()
            .map(String::as_str)
            .collect();
        assert_eq!(matches, vec!["feature/base-ui"]);

        let dialog = prompt_dialog(&Prompt::ChangeProjectBase {
            branches,
            filter: String::new(),
            selected: 2,
        });
        assert_eq!(dialog.input.as_deref(), Some(""));
        assert_eq!(dialog.list.len(), 3);
        assert!(dialog.list[2].selected);
        assert_eq!(dialog.list[2].label, "main");
        assert!(dialog
            .buttons
            .iter()
            .any(|button| button.accel == DialogAccel::Enter));
    }

    #[test]
    fn new_agent_form_shows_input_radio_and_buttons() {
        let p = Prompt::NewAgentForm {
            agents: vec![
                ("claude".to_string(), "Claude Code".to_string()),
                ("opencode".to_string(), "OpenCode".to_string()),
            ],
            selected: 1,
            branch: "fix bug".to_string(),
            existing_branches: vec!["feature/existing".to_string()],
            branch_selected: 0,
            use_existing_branch: false,
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
        // Create (Enter), the target toggle (Tab), and Cancel (Esc).
        assert!(dialog
            .buttons
            .iter()
            .any(|b| b.accel == DialogAccel::Enter && b.label == "Create"));
        assert!(dialog
            .buttons
            .iter()
            .any(|b| b.accel == DialogAccel::Tab && b.label.contains("new branch")));
        assert!(dialog.buttons.iter().any(|b| b.accel == DialogAccel::Esc));
    }

    #[test]
    fn new_agent_form_run_on_base_hides_branch_field() {
        let p = Prompt::NewAgentForm {
            agents: vec![("claude".to_string(), "Claude Code".to_string())],
            selected: 0,
            branch: "ignored".to_string(),
            existing_branches: Vec::new(),
            branch_selected: 0,
            use_existing_branch: false,
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
    fn new_agent_form_existing_mode_filters_local_branches() {
        let p = Prompt::NewAgentForm {
            agents: vec![("claude".to_string(), "Claude Code".to_string())],
            selected: 0,
            branch: "AUTH".to_string(),
            existing_branches: vec!["bug/cache".to_string(), "feature/auth-refresh".to_string()],
            branch_selected: 0,
            use_existing_branch: true,
            run_on_base: false,
            base_branch: "main".to_string(),
        };

        let dialog = prompt_dialog(&p);
        assert_eq!(dialog.input.as_deref(), Some("AUTH"));
        assert_eq!(dialog.list.len(), 1);
        assert_eq!(dialog.list[0].label, "feature/auth-refresh");
        assert!(dialog.list[0].selected);
        assert!(dialog
            .buttons
            .iter()
            .any(|b| b.accel == DialogAccel::Enter && b.label == "Use branch"));
    }

    #[test]
    fn new_agent_form_queues_selected_existing_branch() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let dir = tempfile::TempDir::new().unwrap();
        let agent = make_real_agent(&dir, "opencode");
        let config = config_with_agent(agent);
        let mut state = AppState::new(config, default_state("main"), "/repo", "/repo/state.json");
        let git = FakeGit::new().with_root("/repo").with_branches([
            "main",
            "bug/cache",
            "feature/auth-refresh",
        ]);
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
        let mut ui = Ui::default();

        start_new_tab_flow(&state, &services, &mut ui);
        handle_prompt_key_project(
            KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
            &mut state,
            &services,
            &mut ui,
            0,
        )
        .unwrap();
        for c in "auth".chars() {
            handle_prompt_key_project(
                KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
                &mut state,
                &services,
                &mut ui,
                0,
            )
            .unwrap();
        }
        handle_prompt_key_project(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &mut state,
            &services,
            &mut ui,
            0,
        )
        .unwrap();

        assert_eq!(ui.pending_jobs.len(), 1);
        assert_eq!(ui.pending_jobs[0].job.branch, "feature/auth-refresh");
        assert!(!ui.pending_jobs[0].job.create_branch);
        assert_eq!(state.tabs[0].meta.name, "feature/auth-refresh");
        assert!(state.tabs[0].meta.attached_existing_branch);
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

        // Starting the flow opens the combined form with the default preselected.
        start_new_tab_flow(&state, &services, &mut ui);
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

        // ↑ moves the radio selection to the first agent (claude).
        handle_prompt_key_project(
            KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
            &mut state,
            &services,
            &mut ui,
            0,
        )
        .unwrap();
        // With no non-base branches available, Tab skips the existing-branch
        // target and moves directly to "run from base branch".
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
        let vp = viewport_pty_size(full, InputMode::App, false);
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
        let plain = viewport_pty_size(full, InputMode::App, false);
        let framed = viewport_pty_size(full, InputMode::App, true);
        assert_eq!(framed.cols, plain.cols - 2);
        assert_eq!(framed.rows, plain.rows - 2);
    }

    #[test]
    fn collapsed_viewport_is_larger_only_where_the_window_is_small() {
        // Below both thresholds: terminal mode collapses and reclaims space.
        let small = PtySize {
            rows: 24,
            cols: 100,
        };
        let app = viewport_pty_size(small, InputMode::App, false);
        let terminal = viewport_pty_size(small, InputMode::Terminal, false);
        assert!(terminal.rows > app.rows, "collapsing reclaims chrome rows");
        assert!(
            terminal.cols > app.cols,
            "collapsing reclaims sidebar columns"
        );

        // A large window never collapses, so both modes agree exactly.
        let large = PtySize {
            rows: 50,
            cols: 200,
        };
        assert_eq!(
            viewport_pty_size(large, InputMode::App, false),
            viewport_pty_size(large, InputMode::Terminal, false)
        );
    }

    #[test]
    fn terminal_at_follows_the_collapsed_sidebar_in_terminal_mode() {
        use crate::persistence::project_state::default_state;

        let mut state = AppState::new(
            Config::default(),
            default_state("main"),
            "/repo",
            "/repo/state.json",
        );
        // Below both collapse thresholds (108 cols, 32 rows).
        let area = Rect::new(0, 0, 100, 24);

        // App mode keeps the 28-column sidebar, so column 5 is not the terminal.
        state.focus_app();
        assert!(terminal_at(area, &state, 5, 10).is_none());

        // Terminal mode collapses the sidebar to a 3-column strip, so the same
        // point is now inside the viewport.
        state.focus_terminal();
        let (_, viewport) =
            terminal_at(area, &state, 5, 10).expect("collapsed viewport covers column 5");
        assert_eq!(viewport.x, crate::tui::layout::COLLAPSED_SIDEBAR_WIDTH);
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
        drain_pty_output(&mut state, 1_000, |_, _, _, _| {});

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

        let state = startup(&services, repo, repo, None).expect("startup should succeed");
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

        let state = startup(&services, repo, repo, None).expect("startup should succeed");
        assert!(state.tabs.is_empty());
        assert!(!state
            .warnings
            .iter()
            .any(|w| w.contains("local merge disabled")));
    }

    #[test]
    fn startup_migrates_the_legacy_state_base_into_project_config() {
        let dir = TempDir::new().unwrap();
        let agent = make_real_agent(&dir, "opencode");
        let config = config_with_agent(agent);
        let config_toml = crate::config::load::serialize_config(&config).unwrap();
        let mut legacy_state = default_state("develop");
        legacy_state.version = 1;
        let state_json = serde_json::to_string(&legacy_state).unwrap();

        let repo = Path::new("/repo");
        let fs = FakeFs::new()
            .with_dir("/repo")
            .with_file("/repo/.flightdeck/config.toml", config_toml.as_str())
            .with_file("/repo/.flightdeck/state.json", state_json.as_str());
        let git = FakeGit::new()
            .with_root("/repo")
            .with_branches(["main", "develop"]);
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

        let state = startup(&services, repo, repo, None).unwrap();
        assert_eq!(state.base_branch, "develop");
        assert_eq!(state.to_project_state(0).version, STATE_VERSION);
        let saved_config = fs
            .file_contents(Path::new("/repo/.flightdeck/config.toml"))
            .unwrap();
        assert!(saved_config.contains("default_base_branch = \"develop\""));
        let saved_state = load_state(&fs, Path::new("/repo/.flightdeck/state.json")).unwrap();
        assert_eq!(saved_state.version, STATE_VERSION);
        assert_eq!(saved_state.base_branch, "develop");
    }

    #[test]
    fn startup_defers_legacy_base_migration_when_project_config_is_malformed() {
        let mut legacy_state = default_state("develop");
        legacy_state.version = 1;
        let repo = Path::new("/repo");
        let state_path = repo.join(".flightdeck/state.json");
        let fs = FakeFs::new()
            .with_dir(repo)
            .with_file(repo.join(".flightdeck/config.toml"), "not valid TOML ][")
            .with_file(
                state_path.clone(),
                serde_json::to_string(&legacy_state).unwrap(),
            );
        let git = FakeGit::new()
            .with_root(repo)
            .with_branches(["main", "develop"])
            .with_current_branch("main");
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

        let state = startup(&services, repo, repo, None).unwrap();
        assert_eq!(state.base_branch, "develop");
        assert_eq!(state.to_project_state(0).version, 1);
        let still_legacy = load_state(&fs, &state_path).unwrap();
        assert_eq!(still_legacy.version, 1);
        assert_eq!(still_legacy.base_branch, "develop");
    }

    #[test]
    fn startup_keeps_an_invalid_configured_base_visible_and_warns() {
        let dir = TempDir::new().unwrap();
        let agent = make_real_agent(&dir, "opencode");
        let mut config = config_with_agent(agent);
        config.project.default_base_branch = "missing".to_string();
        let config_toml = crate::config::load::serialize_config(&config).unwrap();

        let repo = Path::new("/repo");
        let fs = FakeFs::new()
            .with_dir("/repo")
            .with_file("/repo/.flightdeck/config.toml", config_toml.as_str());
        let git = FakeGit::new()
            .with_root("/repo")
            .with_branches(["main"])
            .with_current_branch("main");
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

        let state = startup(&services, repo, repo, None).unwrap();
        assert_eq!(state.base_branch, "missing");
        assert!(state
            .warnings
            .iter()
            .any(|warning| warning.contains("is not a local branch")));
    }

    #[test]
    fn project_base_picker_persists_across_restart_without_retargeting_tabs() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_path_buf();
        let run_git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&root)
                .output()
                .map(|output| output.status.success())
                .unwrap_or(false)
        };
        assert!(run_git(&["init", "--initial-branch=main"]));
        assert!(run_git(&[
            "-c",
            "user.name=FlightDeck Test",
            "-c",
            "user.email=flightdeck@example.invalid",
            "-c",
            "commit.gpgSign=false",
            "-c",
            "core.hooksPath=",
            &["com", "mit"].concat(),
            "--allow-empty",
            "-m",
            "initial",
        ]));
        assert!(run_git(&["branch", "develop"]));

        let agent = make_real_agent(&dir, "opencode");
        let config = config_with_agent(agent);
        let mut invalid_disk_config = config.clone();
        invalid_disk_config.project.default_base_branch = "missing".to_string();
        let config_path = root.join(".flightdeck/config.toml");
        let state_path = root.join(".flightdeck/state.json");
        let worktree_path = root.join(".flightdeck/worktrees/existing");
        let fs = FakeFs::new()
            .with_dir(root.clone())
            .with_dir(worktree_path.clone())
            .with_file(
                config_path.clone(),
                crate::config::load::serialize_config(&invalid_disk_config).unwrap(),
            );
        let mut project_state = default_state("main");
        project_state.tabs.push(TabState {
            id: "existing".to_string(),
            name: "Existing".to_string(),
            slug: "existing".to_string(),
            agent: "opencode".to_string(),
            branch: "flightdeck/existing".to_string(),
            worktree_path_relative: ".flightdeck/worktrees/existing".to_string(),
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
        });
        let app = AppState::new(config, project_state, &root, &state_path);
        let (create_tx, create_rx) = std::sync::mpsc::channel();
        let (status_tx, status_rx) = std::sync::mpsc::channel();
        let mut workspace = Workspace {
            projects: vec![Project {
                name: "project".to_string(),
                git: GitCli::new(root.clone()),
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
        };
        let pty = FakePty::new();
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

        // Selecting the retained runtime value must still repair an invalid
        // on-disk setting rather than returning early.
        change_project_default_base(&mut workspace, &env, "main").unwrap();
        let repaired = crate::config::load::parse_config(
            &fs.file_contents(&config_path).expect("repaired config"),
        )
        .unwrap();
        assert_eq!(repaired.project.default_base_branch, "main");

        change_project_default_base(&mut workspace, &env, "develop").unwrap();
        assert_eq!(workspace.active_project().state.base_branch, "develop");
        assert_eq!(
            workspace.active_project().state.tabs[0].meta.base_branch,
            "main"
        );
        let saved_state = load_state(&fs, &state_path).unwrap();
        assert_eq!(saved_state.base_branch, "develop");
        assert_eq!(saved_state.tabs[0].base_branch, "main");

        let restart_git = FakeGit::new()
            .with_root(root.clone())
            .with_branches(["main", "develop", "flightdeck/existing"])
            .with_current_branch("main");
        restart_git.add_existing_worktree(WorktreeInfo {
            path: worktree_path,
            branch: Some("flightdeck/existing".to_string()),
            head: Some("def456".to_string()),
        });
        let restart_services = Services {
            git: &restart_git,
            fs: &fs,
            pty: &pty,
            clock: &clock,
            container: &container,
            command: &command,
        };
        let restarted = startup(&restart_services, &root, &root, None).unwrap();
        assert_eq!(restarted.base_branch, "develop");
        assert_eq!(restarted.tabs.len(), 1);
        assert_eq!(restarted.tabs[0].meta.base_branch, "main");
    }

    #[test]
    fn isolated_startup_writes_nothing_under_the_repo() {
        let repo = Path::new("/repo");
        let fs = FakeFs::new();
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

        let state = startup(
            &services,
            repo,
            repo,
            Some(Path::new("/tmp/fd-isolated-test")),
        )
        .unwrap();

        assert!(
            fs.writes_under(Path::new("/repo")).is_empty(),
            "isolated startup must not touch the project: {:?}",
            fs.writes_under(Path::new("/repo"))
        );
        assert!(
            fs.writes().is_empty(),
            "isolated startup must not write anything at all: {:?}",
            fs.writes()
        );
        assert!(
            state.tabs.is_empty(),
            "startup itself creates no tab (Task 7 does)"
        );
        assert!(
            !state.config.ui.auto_continue,
            "auto_continue is forced off so even Restart Agent starts fresh"
        );
        assert!(state.isolated, "the run must be marked isolated");
        assert_eq!(
            state.isolated_status_root.as_deref(),
            Some(Path::new("/tmp/fd-isolated-test")),
            "the status root must reach AppState, or the redirect silently falls back to the worktree"
        );
    }

    #[test]
    fn isolated_startup_ignores_state_json_on_disk() {
        let repo = Path::new("/repo");
        // A previous run's state that a normal startup would recover.
        let fs = FakeFs::new().with_file(
            "/repo/.flightdeck/state.json",
            r#"{"version":1,"base_branch":"main","tabs":[]}"#,
        );
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

        let state = startup(
            &services,
            repo,
            repo,
            Some(Path::new("/tmp/fd-isolated-test")),
        )
        .unwrap();

        assert!(
            state.tabs.is_empty(),
            "nothing is recovered in an isolated run"
        );
        assert!(fs.writes_under(Path::new("/repo")).is_empty());
        assert!(fs.writes().is_empty());
    }

    #[test]
    fn isolated_startup_still_reads_project_config() {
        let repo = Path::new("/repo");
        // Deliberately partial (no [agents] section): a real user's trimmed
        // project override, with no global config.toml on disk anywhere in
        // this fixture (an isolated run never writes one). Do not "simplify"
        // this into a self-sufficient config — it exercises isolated mode
        // computing the same effective config a normal run would (default
        // global base + project overrides) purely in memory, so a partial
        // override on a machine that has never run FlightDeck is honoured
        // rather than silently discarded (SPECS §4).
        let fs = FakeFs::new().with_file(
            "/repo/.flightdeck/config.toml",
            "[ui]\ndefault_agent = \"claude\"\n",
        );
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

        let state = startup(
            &services,
            repo,
            repo,
            Some(Path::new("/tmp/fd-isolated-test")),
        )
        .unwrap();

        assert_eq!(
            state.config.ui.default_agent, "claude",
            "isolated mode reads existing config; it only refuses to write"
        );
        assert!(fs.writes_under(Path::new("/repo")).is_empty());
        assert!(fs.writes().is_empty());
    }

    #[test]
    fn isolated_startup_ignores_a_corrupt_global_config_like_a_normal_run_would() {
        // Mirrors load_layered_config's lenient handling of a broken global
        // base (src/config/load.rs): a hand-edited, syntactically invalid
        // ~/.flightdeck/config.toml must not blot out a perfectly valid
        // project config.toml, in an isolated run any more than in a normal
        // one. Before this test, effective_config_without_writing propagated
        // the parse error, which the outer `unwrap_or_else` silently
        // swallowed into built-in defaults — the user's own agents and
        // settings would vanish with no message.
        let dir = TempDir::new().unwrap();
        // Deliberately not one of the built-in agent keys ("opencode",
        // "claude", "codex"): default_config()'s own default_agent is
        // "opencode", so asserting a project-supplied "opencode" survives
        // would pass identically whether the project layer was honoured or
        // silently discarded in favour of built-in defaults (both produce
        // "opencode") — a non-default key is the only way this assertion can
        // actually distinguish the two.
        let agent = make_real_agent(&dir, "myagent");
        let config = config_with_agent(agent);
        let toml = crate::config::load::serialize_config(&config).unwrap();

        let repo = Path::new("/repo");
        let global_path = global_config_path().expect("HOME must be set for this test to run");
        let fs = FakeFs::new()
            .with_dir("/repo")
            .with_file("/repo/.flightdeck/config.toml", toml.as_str())
            .with_file(global_path.to_str().unwrap(), "this is not [valid toml");
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

        let state = startup(
            &services,
            repo,
            repo,
            Some(Path::new("/tmp/fd-isolated-test")),
        )
        .unwrap();

        assert_eq!(
            state.config.ui.default_agent, "myagent",
            "the project's own config must survive a corrupt global base"
        );
        assert!(fs.writes_under(Path::new("/repo")).is_empty());
        assert!(fs.writes().is_empty());
    }

    #[test]
    fn normal_startup_still_initializes_the_project() {
        // Regression guard for the untouched default path.
        let repo = Path::new("/repo");
        let fs = FakeFs::new();
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

        let _ = startup(&services, repo, repo, None).unwrap();

        assert!(
            fs.writes_under(Path::new("/repo"))
                .iter()
                .any(|p| p.ends_with("config.toml")),
            "a normal first run still writes the project config"
        );
    }

    #[test]
    fn isolated_run_creates_exactly_one_base_tab() {
        let dir = TempDir::new().unwrap();
        // Not one of the built-in agent keys: see the comment on
        // `isolated_startup_ignores_a_corrupt_global_config_...` above for why
        // a non-default key is the only fixture that can distinguish a real
        // spawn from a silently-swallowed default.
        let agent = make_real_agent(&dir, "myagent");
        let config = config_with_agent(agent);
        let toml = crate::config::load::serialize_config(&config).unwrap();

        let repo = Path::new("/repo");
        let fs = FakeFs::new()
            .with_dir("/repo")
            .with_file("/repo/.flightdeck/config.toml", toml.as_str());
        let git = FakeGit::new().with_root("/repo").with_branches(["main"]);
        let pty = FakePty::new();
        pty.queue_session();
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

        let mut state = startup(
            &services,
            repo,
            repo,
            Some(Path::new("/tmp/fd-isolated-test")),
        )
        .unwrap();

        start_isolated_session(&mut state, &services).unwrap();

        assert_eq!(state.tabs.len(), 1, "exactly one session");
        assert!(state.tabs[0].meta.runs_on_base, "it runs in the repo root");
        assert_eq!(state.selected_tab, Some(0));
        assert!(
            state.tabs[0].meta.resume_args.is_empty(),
            "a fresh session, never a continued one"
        );
        assert_eq!(state.tabs[0].meta.agent, "myagent");
        assert_eq!(pty.spawns().len(), 1, "the agent is spawned");
        assert!(
            git.added_worktrees().is_empty() && git.created_branches().is_empty(),
            "not one git mutation"
        );
        assert!(
            fs.writes_under(Path::new("/repo")).is_empty(),
            "and nothing written to the project: {:?}",
            fs.writes_under(Path::new("/repo"))
        );
        assert!(
            fs.writes().is_empty(),
            "and nothing written anywhere else either: {:?}",
            fs.writes()
        );
    }

    #[test]
    fn isolated_session_status_file_lives_outside_the_project() {
        let dir = TempDir::new().unwrap();
        let agent = make_real_agent(&dir, "myagent");
        let config = config_with_agent(agent);
        let toml = crate::config::load::serialize_config(&config).unwrap();

        let repo = Path::new("/repo");
        let fs = FakeFs::new()
            .with_dir("/repo")
            .with_file("/repo/.flightdeck/config.toml", toml.as_str());
        let git = FakeGit::new().with_root("/repo").with_branches(["main"]);
        let pty = FakePty::new();
        pty.queue_session();
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

        let mut state = startup(
            &services,
            repo,
            repo,
            Some(Path::new("/tmp/fd-isolated-test")),
        )
        .unwrap();

        start_isolated_session(&mut state, &services).unwrap();

        let status = state.tabs[0].status_file.clone().expect("a status file");
        assert!(
            status.starts_with("/tmp/fd-isolated-test"),
            "status must be redirected out of the project: {}",
            status.display()
        );
    }

    #[test]
    fn start_isolated_session_removes_the_dead_placeholder_on_spawn_failure() {
        // A `finalize_new_tab` failure (missing container image, PTY spawn
        // error, ...) must not leave the placeholder behind in
        // `TabPhase::Creating` (SPECS: the TabPhase contract at
        // `src/app/state.rs:906-908` — creation failures remove the tab
        // entirely, and a finalize-time spawn failure is no exception).
        let dir = TempDir::new().unwrap();
        let agent = make_real_agent(&dir, "myagent");
        let config = config_with_agent(agent);
        let toml = crate::config::load::serialize_config(&config).unwrap();

        let repo = Path::new("/repo");
        let fs = FakeFs::new()
            .with_dir("/repo")
            .with_file("/repo/.flightdeck/config.toml", toml.as_str());
        let git = FakeGit::new().with_root("/repo").with_branches(["main"]);
        let pty = FakePty::new();
        pty.fail_next_spawn();
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

        let mut state = startup(
            &services,
            repo,
            repo,
            Some(Path::new("/tmp/fd-isolated-test")),
        )
        .unwrap();

        let err = start_isolated_session(&mut state, &services)
            .expect_err("a spawn failure must propagate, not be swallowed");
        assert!(
            state.tabs.is_empty(),
            "the failed placeholder must not be left behind: {} tab(s) remain",
            state.tabs.len()
        );
        // Sanity: the error is the spawn failure, not something unrelated.
        let msg = err.to_string();
        assert!(
            !msg.is_empty(),
            "the propagated error should carry a message"
        );
    }

    #[test]
    fn cleanup_isolated_run_removes_the_temp_status_dir() {
        let fs = FakeFs::new()
            .with_dir("/tmp/fd-isolated-9")
            .with_file("/tmp/fd-isolated-9/.flightdeck/agent-status", "idle\n");

        cleanup_isolated_run(&fs, Path::new("/tmp/fd-isolated-9"));

        assert!(
            fs.writes()
                .iter()
                .any(|p| p == Path::new("/tmp/fd-isolated-9")),
            "the temp dir must be removed"
        );
    }

    #[test]
    fn cleanup_isolated_run_tolerates_a_missing_dir() {
        let fs = FakeFs::new();
        // Must not panic: the agent may never have started.
        cleanup_isolated_run(&fs, Path::new("/tmp/fd-isolated-absent"));
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

        /// A reply to a *not-started* agent (recovered tab whose project was not
        /// active at startup) resumes the agent and defers the text, then
        /// delivers it once the terminal is ready — instead of rejecting
        /// (remote-control-1l4).
        #[test]
        fn reply_to_not_started_agent_resumes_then_delivers_when_ready() {
            let dir = TempDir::new().unwrap();
            let agent = make_real_agent(&dir, "opencode");
            let config = config_with_agent(agent);
            let pty = FakePty::new();
            // Queue the session the resume spawn will claim; keep its handle.
            let handle = pty.queue_session();
            let state = CoreProjectState {
                version: STATE_VERSION,
                project_root_relative: ".".to_string(),
                base_branch: "main".to_string(),
                tabs: vec![tab_state("t1", "target", "opencode")],
            };
            let mut app = AppState::new(config, state, "/repo", "/repo/.flightdeck/state.json");
            app.set_pty_size(PtySize::default());
            assert_eq!(
                app.tabs[0].session.primary_state(),
                ProcessState::NotStarted
            );
            let mut workspace = workspace_with(app);
            // The worktree must exist for the resume spawn.
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
                command_id: CommandId::new("c-resume"),
                issued_at_ms: 0,
                body: CommandBody::Reply {
                    session_id: SessionId::new("t1"),
                    text: "resume and go".to_string(),
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

            // Resumed (spawned) and the reply queued — not written yet, and
            // acked as accepted rather than rejected.
            assert_eq!(
                workspace.projects[0].state.tabs[0].session.primary_state(),
                ProcessState::Running
            );
            assert_eq!(first_tasks.len(), 1);
            assert!(handle.input().is_empty());
            let acks = decode_acks(&sent);
            assert_eq!(acks.len(), 1);
            assert_eq!(acks[0].outcome, CommandOutcome::Accepted);

            // Once the ready-gate window elapses (fresh terminal never enables
            // bracketed paste), the queued reply is delivered raw + Enter.
            deliver_first_tasks(&mut first_tasks, &mut workspace, 12_000);
            assert_eq!(handle.input(), b"resume and go\r".to_vec());
            assert!(first_tasks.is_empty());
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
                drain_pty_output(&mut p.state, 1_000, |sid, which, _mint, bytes| {
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

    /// Refuse Open/Close/New-Tab/New-Button and project switching in an
    /// isolated run (specs/ISOLATED_MODE.md §6), while leaving every other
    /// mode's behaviour byte-identical.
    mod isolated_refusals {
        use super::*;

        const ISOLATED_MSG_FRAGMENT: &str = "isolated";

        /// Extract the message text from whatever a refusal set — a
        /// notification dialog, matching how `Ui::message` renders it.
        fn overlay_message(ui: &Ui) -> Option<String> {
            match &ui.overlay {
                UiOverlay::Dialog(d) => Some(d.title.clone()),
                _ => None,
            }
        }

        pub(super) fn one_project_workspace(isolated: bool) -> Workspace {
            let mut config = config_with_agent(AgentDef {
                key: "codex".to_string(),
                display_name: "Codex".to_string(),
                command: "codex".to_string(),
                ..AgentDef::default()
            });
            config.ui.default_agent = "codex".to_string();
            let mut app = AppState::new(config, default_state("main"), "/repo", "/repo/state.json");
            if isolated {
                app.set_isolated(None);
            }
            let (create_tx, create_rx) = std::sync::mpsc::channel();
            let (status_tx, status_rx) = std::sync::mpsc::channel();
            Workspace {
                projects: vec![Project {
                    name: "proj".to_string(),
                    git: GitCli::new(PathBuf::from("/repo")),
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

        pub(super) fn two_project_workspace(active_isolated: bool) -> Workspace {
            let mut ws = one_project_workspace(active_isolated);
            let mut other_config = config_with_agent(AgentDef {
                key: "claude".to_string(),
                display_name: "Claude".to_string(),
                command: "claude".to_string(),
                ..AgentDef::default()
            });
            other_config.ui.default_agent = "claude".to_string();
            let other_app = AppState::new(
                other_config,
                default_state("main"),
                "/repo2",
                "/repo2/state.json",
            );
            let (create_tx, create_rx) = std::sync::mpsc::channel();
            let (status_tx, status_rx) = std::sync::mpsc::channel();
            ws.projects.push(Project {
                name: "other".to_string(),
                git: GitCli::new(PathBuf::from("/repo2")),
                state: other_app,
                cache: GitStatusCache::new(),
                create_tx,
                create_rx,
                status_tx,
                status_rx,
                status_in_flight: false,
                git_lock: Arc::new(Mutex::new(())),
            });
            ws
        }

        pub(super) fn env<'a>(
            fs: &'a FakeFs,
            pty: &'a FakePty,
            clock: &'a FakeClock,
            container: &'a crate::testing::FakeContainerRuntime,
            command: &'a crate::testing::FakeCommandRunner,
        ) -> Env<'a> {
            Env {
                fs,
                pty,
                clock,
                container,
                command,
            }
        }

        #[test]
        fn isolated_refuses_the_new_tab_flow() {
            let ws = one_project_workspace(true);
            let mut ui = Ui::default();
            let git = FakeGit::new().with_branches(["main"]);
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

            start_new_tab_flow(&ws.active_project().state, &services, &mut ui);

            let msg = overlay_message(&ui).expect("a refusal is surfaced");
            assert!(
                msg.contains(ISOLATED_MSG_FRAGMENT),
                "the refusal must say why: {msg}"
            );
            assert!(ui.prompt.is_none(), "and no new-tab prompt may open");
        }

        #[test]
        fn a_normal_run_still_opens_the_new_tab_prompt() {
            let ws = one_project_workspace(false);
            let mut ui = Ui::default();
            let git = FakeGit::new().with_branches(["main"]);
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

            start_new_tab_flow(&ws.active_project().state, &services, &mut ui);

            assert!(ui.prompt.is_some(), "the normal flow is untouched");
        }

        #[test]
        fn isolated_refuses_opening_another_project() {
            let ws = one_project_workspace(true);
            let mut ui = Ui::default();
            let fs = FakeFs::new();
            let pty = FakePty::new();
            let clock = FakeClock::default();
            let container = crate::testing::FakeContainerRuntime::new();
            let command = crate::testing::FakeCommandRunner::new();
            let e = env(&fs, &pty, &clock, &container, &command);

            start_open_project_flow(&ws, &e, &mut ui);

            let msg = overlay_message(&ui).expect("a refusal is surfaced");
            assert!(msg.contains(ISOLATED_MSG_FRAGMENT));
            assert!(ui.prompt.is_none(), "the folder browser must not open");
        }

        #[test]
        fn a_normal_run_still_opens_the_open_project_browser() {
            let ws = one_project_workspace(false);
            let mut ui = Ui::default();
            let fs = FakeFs::new();
            let pty = FakePty::new();
            let clock = FakeClock::default();
            let container = crate::testing::FakeContainerRuntime::new();
            let command = crate::testing::FakeCommandRunner::new();
            let e = env(&fs, &pty, &clock, &container, &command);

            start_open_project_flow(&ws, &e, &mut ui);

            assert!(
                matches!(
                    ui.prompt.as_ref().map(|p| &p.prompt),
                    Some(Prompt::OpenProject { .. })
                ),
                "the normal flow is untouched"
            );
        }

        #[test]
        fn isolated_refuses_closing_project() {
            let ws = two_project_workspace(true);
            let mut ui = Ui::default();

            start_close_project_flow(&ws, &mut ui, 0);

            let msg = overlay_message(&ui).expect("a refusal is surfaced");
            assert!(msg.contains(ISOLATED_MSG_FRAGMENT));
            assert!(ui.prompt.is_none(), "the close confirmation must not open");
        }

        #[test]
        fn a_normal_run_still_opens_the_close_project_confirmation() {
            let ws = two_project_workspace(false);
            let mut ui = Ui::default();

            start_close_project_flow(&ws, &mut ui, 0);

            assert!(
                matches!(
                    ui.prompt.as_ref().map(|p| &p.prompt),
                    Some(Prompt::CloseProjectConfirm { .. })
                ),
                "the normal flow is untouched"
            );
        }

        #[test]
        fn isolated_refuses_switching_project() {
            let mut ws = two_project_workspace(true);
            let mut ui = Ui::default();
            let fs = FakeFs::new();
            let pty = FakePty::new();
            let clock = FakeClock::default();
            let container = crate::testing::FakeContainerRuntime::new();
            let command = crate::testing::FakeCommandRunner::new();
            let e = env(&fs, &pty, &clock, &container, &command);
            let before = ws.active;

            switch_project(&mut ws, &e, Selector::Next, &mut ui);

            assert_eq!(ws.active, before, "the active project must not change");
            assert!(overlay_message(&ui)
                .expect("a refusal is surfaced")
                .contains(ISOLATED_MSG_FRAGMENT));
        }

        #[test]
        fn a_normal_run_still_switches_project() {
            let mut ws = two_project_workspace(false);
            let mut ui = Ui::default();
            let fs = FakeFs::new();
            let pty = FakePty::new();
            let clock = FakeClock::default();
            let container = crate::testing::FakeContainerRuntime::new();
            let command = crate::testing::FakeCommandRunner::new();
            let e = env(&fs, &pty, &clock, &container, &command);
            let before = ws.active;

            switch_project(&mut ws, &e, Selector::Next, &mut ui);

            assert_ne!(ws.active, before, "the normal flow is untouched");
        }

        /// Behavioural pin for the `ProjectHit::Tab` mouse path (#26 regression
        /// class): a click on the project tab row must be refused the same way
        /// as the keybinding and palette switch paths in an isolated run, not
        /// silently bypass `switch_project` via a direct `set_active`.
        #[test]
        fn isolated_refuses_switching_project_via_a_tab_click() {
            let mut ws = one_project_workspace(true);
            let mut ui = Ui::default();
            let fs = FakeFs::new();
            let pty = FakePty::new();
            let clock = FakeClock::default();
            let container = crate::testing::FakeContainerRuntime::new();
            let command = crate::testing::FakeCommandRunner::new();
            let e = env(&fs, &pty, &clock, &container, &command);
            let before = ws.active;

            // Column/row land inside the single project's tab segment: the
            // project tab row starts at (area.x, HEADER_HEIGHT).
            let area = Rect {
                x: 0,
                y: 0,
                width: 80,
                height: 24,
            };
            let click = MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 1,
                row: crate::tui::layout::HEADER_HEIGHT,
                modifiers: KeyModifiers::NONE,
            };

            handle_mouse(click, area, &mut ws, &e, &mut ui);

            assert_eq!(
                ws.active, before,
                "a tab click must not switch the active project"
            );
            assert!(
                overlay_message(&ui)
                    .expect("a refusal is surfaced")
                    .contains(ISOLATED_MSG_FRAGMENT),
                "the click must be refused like every other switch path"
            );
        }

        #[test]
        fn a_normal_run_still_switches_project_via_a_tab_click() {
            let mut ws = two_project_workspace(false);
            let mut ui = Ui::default();
            let fs = FakeFs::new();
            let pty = FakePty::new();
            let clock = FakeClock::default();
            let container = crate::testing::FakeContainerRuntime::new();
            let command = crate::testing::FakeCommandRunner::new();
            let e = env(&fs, &pty, &clock, &container, &command);
            let before = ws.active;

            let area = Rect {
                x: 0,
                y: 0,
                width: 80,
                height: 24,
            };
            let ml = crate::tui::layout::compute(area, crate::tui::layout::Chrome::Full, false);
            let names: Vec<String> = ws.projects.iter().map(|p| p.name.clone()).collect();
            let row = crate::tui::layout::HEADER_HEIGHT;
            // Find a column that hits the second project's tab, wherever the
            // first project's label happens to place it.
            let column = (0..area.width)
                .find(|&col| {
                    matches!(
                        crate::tui::render::project_tab_hit_test(ml.project_tabs, &names, col, row),
                        Some(ProjectHit::Tab(1))
                    )
                })
                .expect("the second project's tab must be hit-testable");
            let click = MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column,
                row,
                modifiers: KeyModifiers::NONE,
            };

            handle_mouse(click, area, &mut ws, &e, &mut ui);

            assert_ne!(
                ws.active, before,
                "the normal flow is untouched: a tab click still switches"
            );
        }

        #[test]
        fn isolated_open_config_manager_writes_nothing() {
            // Merely opening "Open Configuration" — a viewer, not a write
            // action — must not create ~/.flightdeck/config.toml (SPECS §32,
            // ISOLATED_MODE.md §2 property 2 / §4).
            let ws = one_project_workspace(true);
            let fs = FakeFs::new();
            let pty = FakePty::new();
            let clock = FakeClock::default();
            let container = crate::testing::FakeContainerRuntime::new();
            let command = crate::testing::FakeCommandRunner::new();
            let e = env(&fs, &pty, &clock, &container, &command);
            let mut ui = Ui::default();

            open_config_manager(&ws, &e, &mut ui);

            assert!(
                ui.config.is_some(),
                "Open Configuration must stay available in an isolated run"
            );
            assert!(
                fs.writes().is_empty(),
                "opening the config manager in an isolated run must write nothing: {:?}",
                fs.writes()
            );
        }

        #[test]
        fn a_normal_run_still_creates_the_global_config_base_on_open() {
            // Guards against the isolated guard above being inverted (which
            // would make the isolated test pass for the wrong reason).
            let ws = one_project_workspace(false);
            let fs = FakeFs::new();
            let pty = FakePty::new();
            let clock = FakeClock::default();
            let container = crate::testing::FakeContainerRuntime::new();
            let command = crate::testing::FakeCommandRunner::new();
            let e = env(&fs, &pty, &clock, &container, &command);
            let mut ui = Ui::default();

            open_config_manager(&ws, &e, &mut ui);

            assert!(ui.config.is_some());
            let global_path = global_config_path().expect("HOME must be set for this test to run");
            assert!(
                fs.writes().contains(&global_path),
                "a normal run still creates the global base on open: {:?}",
                fs.writes()
            );
        }
    }

    /// The host half of the browser's command surface: `run_web_command` routes
    /// a real wire frame into the TUI's own palette path and acks what that
    /// dispatch actually did (`specs/WEB_INTERFACE.md` §1, D3, D16).
    ///
    /// The refusing cases are tested here as well as in `tests/web_server.rs`
    /// deliberately: the server refuses them before forwarding, so these prove
    /// the second line of defence would hold if the two ever disagreed.
    mod web_command_surface {
        use super::isolated_refusals::{env, one_project_workspace, two_project_workspace};
        use super::*;
        use crate::web::activity::ActivityStore;
        use crate::web::protocol::{command as names, AckOutcome, Command as WireCommand};
        use serde_json::json;

        fn frame(seq: u64, name: &str, args: Option<serde_json::Value>) -> WireCommand {
            WireCommand {
                seq,
                name: name.to_string(),
                args,
            }
        }

        /// The origin every frame in this module arrives with: one browser, at a
        /// fixed address, so the origin label a dialog carries is checkable.
        fn browser_origin() -> crate::web::protocol::DialogOrigin {
            crate::web::protocol::DialogOrigin::Browser {
                viewer_id: Some(crate::web::protocol::ViewerId::new("viewer-1")),
                label: "192.168.2.20".to_string(),
            }
        }

        /// A workspace whose one project has a real Agent Session Tab named
        /// `Task`.
        ///
        /// Built by dispatching `NewAgentTab` against a [`FakeGit`], so the tab
        /// is the real thing rather than a hand-assembled record — its name is
        /// what artboard 1g's gate expects to be typed back, and a test that
        /// invented the name would prove nothing about where the gate reads it
        /// from. The project keeps `one_project_workspace`'s scaffolding, so the
        /// name a browser must type for the *project* gate is still `proj`.
        fn workspace_with_a_tab(dir: &TempDir, git: &FakeGit, pty: &FakePty) -> Workspace {
            let agent = make_real_agent(dir, "opencode");
            let mut config = config_with_agent(agent);
            config.ui.default_agent = "opencode".to_string();
            let fs = FakeFs::new();
            let clock = FakeClock::default();
            let container = crate::testing::FakeContainerRuntime::new();
            let runner = crate::testing::FakeCommandRunner::new();
            let services = Services {
                git,
                fs: &fs,
                pty,
                clock: &clock,
                container: &container,
                command: &runner,
            };
            pty.queue_session();
            let mut state = AppState::new(
                config,
                default_state("main"),
                "/repo",
                "/repo/.flightdeck/state.json",
            );
            state
                .dispatch(
                    Command::NewAgentTab {
                        name: "Task".to_string(),
                        agent_key: None,
                    },
                    &services,
                )
                .expect("the tab is created");
            let mut ws = one_project_workspace(false);
            ws.projects[0].state = state;
            ws
        }

        /// Run one wire frame against `workspace`, returning the ack the browser
        /// would receive. Builds the fake services fresh, as the event loop
        /// builds the real ones per tick.
        fn run(
            workspace: &mut Workspace,
            ui: &mut Ui,
            command: &WireCommand,
        ) -> crate::web::protocol::Ack {
            let fs = FakeFs::new();
            let pty = FakePty::new();
            let clock = FakeClock::default();
            let container = crate::testing::FakeContainerRuntime::new();
            let runner = crate::testing::FakeCommandRunner::new();
            let e = env(&fs, &pty, &clock, &container, &runner);
            let mut activity = ActivityStore::new();
            run_web_command(command, &browser_origin(), workspace, &e, ui, &mut activity)
        }

        /// D3: the selection is shared, so a browser choosing a project moves
        /// the desktop onto it. The id is the one `build_web_host_state` mints —
        /// the repository root — read off the workspace rather than spelled out.
        #[test]
        fn selecting_a_project_moves_the_desktop_too() {
            let mut ws = two_project_workspace(false);
            let mut ui = Ui::default();
            let id = ws.projects[1].git.root().display().to_string();

            let ack = run(
                &mut ws,
                &mut ui,
                &frame(1, names::SELECT_PROJECT, Some(json!({ "project_id": id }))),
            );

            assert_eq!(ack.outcome, AckOutcome::Applied);
            assert_eq!(ack.seq, 1);
            assert_eq!(ws.active, 1, "the desktop followed the browser");
        }

        /// A browser whose snapshot predates a close names an id the host does
        /// not have. Refused with a sentence that says what to do about it, not
        /// silently ignored.
        #[test]
        fn a_stale_id_is_refused_with_a_reason() {
            let mut ws = one_project_workspace(false);
            let mut ui = Ui::default();

            let ack = run(
                &mut ws,
                &mut ui,
                &frame(
                    2,
                    names::SELECT_SESSION,
                    Some(json!({ "session_id": "tab-that-was-closed" })),
                ),
            );

            assert_eq!(ack.outcome, AckOutcome::Rejected);
            let detail = ack.detail.expect("a refusal states its reason");
            assert!(detail.contains("out of date"), "{detail}");
        }

        /// A missing argument is a refusal, not a panic and not a guess.
        #[test]
        fn a_selection_with_no_target_is_refused() {
            let mut ws = one_project_workspace(false);
            let mut ui = Ui::default();

            let ack = run(&mut ws, &mut ui, &frame(3, names::SELECT_TERMINAL, None));

            assert_eq!(ack.outcome, AckOutcome::Rejected);
            assert!(ack
                .detail
                .expect("a refusal states its reason")
                .contains("terminal_id"));
        }

        /// The ack is what the dispatch earned. `toggle_split_view` goes through
        /// `run_palette_action`, and the sentence the desktop showed is the
        /// sentence the browser gets.
        #[test]
        fn a_palette_command_acks_what_the_dispatch_did() {
            let mut ws = one_project_workspace(false);
            let mut ui = Ui::default();
            assert!(!ws.projects[0].state.split_view);

            let ack = run(&mut ws, &mut ui, &frame(4, names::TOGGLE_SPLIT_VIEW, None));

            assert_eq!(ack.outcome, AckOutcome::Applied);
            assert!(
                ws.projects[0].state.split_view,
                "the browser drove the real app state"
            );
            assert_eq!(ack.detail.as_deref(), Some("Split view on."));
        }

        /// A guard's refusal reaches the browser verbatim instead of becoming a
        /// fake success: an isolated run has one project by construction (SPECS
        /// §32), and says so in the same words the desktop would show.
        #[test]
        fn a_refused_dispatch_acks_the_guards_own_sentence() {
            let mut ws = one_project_workspace(true);
            let mut ui = Ui::default();

            let ack = run(&mut ws, &mut ui, &frame(5, names::NEXT_PROJECT, None));

            assert_eq!(ack.outcome, AckOutcome::Rejected);
            assert_eq!(ack.detail.as_deref(), Some(ISOLATED_REFUSAL));
        }

        /// **D16 + artboard 1g, end to end: quitting from a browser takes two
        /// steps, and every way of taking only one provably does not quit.**
        ///
        /// `ui.should_quit` is what makes this the clearest of the destructive
        /// tests: the effect is a boolean on the host, so "the effect did not
        /// occur" is asserted directly rather than inferred from an absence.
        #[test]
        fn quitting_from_a_browser_takes_the_typed_project_name() {
            let mut ws = one_project_workspace(false);
            let mut ui = Ui::default();

            // Step 1. The row asks; it does not quit. D16's `host only` badge
            // would have been the alternative, and the spec says it is not
            // enough — so the row dispatches, and what it dispatches can only
            // open the question.
            let ack = run(&mut ws, &mut ui, &frame(6, names::QUIT, None));
            assert_eq!(ack.outcome, AckOutcome::Applied);
            assert!(!ui.should_quit, "the row asked, it did not quit");
            let opened = ui.dialog_id().expect("the question is open");

            let view = view(&ui, &ws);
            assert_eq!(view.kind, "confirm_quit");
            let body = body(&view);
            assert!(body.confirmable, "a browser may answer — through step 2");
            assert_eq!(body.refusal, None);
            let gate = body.confirm_gate.clone().expect("1g's step 2 is published");
            assert_eq!(gate.key, "y", "the gate stands in front of the Quit button");
            assert_eq!(
                gate.expected, "proj",
                "quit is not one session's work, so it names the project"
            );
            assert!(
                gate.instruction.contains("This browser is remote"),
                "1g's step 2 says why there is one: {}",
                gate.instruction
            );

            // Step 1 alone: the button, no name. This is what an older browser —
            // and a replayed frame — sends.
            let confirm = answer(7, names::DIALOG_CONFIRM, &ui, json!({}));
            let ack = run(&mut ws, &mut ui, &confirm);
            assert_eq!(ack.outcome, AckOutcome::Rejected);
            assert!(
                ack.detail
                    .expect("a refusal states its reason")
                    .contains("proj"),
                "the refusal repeats what to type"
            );
            assert!(!ui.should_quit, "step 1 alone must not quit");
            assert!(
                ui.prompt.is_some(),
                "the question is still on both surfaces"
            );

            // Every near miss. The comparison is exact — no trimming, no case
            // folding — so each of these is a name the host does not have.
            for wrong in ["Proj", "PROJ", "proj ", " proj", "pro", "", "projx"] {
                let confirm = answer(
                    8,
                    names::DIALOG_CONFIRM,
                    &ui,
                    json!({ "confirm_name": wrong }),
                );
                let ack = run(&mut ws, &mut ui, &confirm);
                assert_eq!(ack.outcome, AckOutcome::Rejected, "for `{wrong}`");
                assert!(!ui.should_quit, "`{wrong}` must not quit FlightDeck");
                assert!(ui.prompt.is_some(), "`{wrong}` left the question open");
                assert!(
                    ui.dialog_decisions.is_empty(),
                    "`{wrong}` was refused before any key reached the prompt"
                );
            }

            // And the name itself, exactly. Only now is a key fed into the
            // prompt — the same `y` the desktop's own button sends.
            let confirm = answer(
                9,
                names::DIALOG_CONFIRM,
                &ui,
                json!({ "confirm_name": "proj" }),
            );
            let ack = run(&mut ws, &mut ui, &confirm);
            assert_eq!(ack.outcome, AckOutcome::Applied);
            assert!(ui.should_quit, "both steps taken: FlightDeck stops");
            assert!(ui.prompt.is_none());
            assert_eq!(
                ui.dialog_decisions,
                vec![(opened, crate::web::protocol::DialogOutcome::Confirmed)],
                "the other surface is told it was confirmed, not superseded"
            );
        }

        /// The desktop's half of the same dialog, which is the ruling in one
        /// assertion: **nothing reaches step 2 there.** The person at this
        /// keyboard is at the machine that stops, so `y` is the whole answer —
        /// and SPECS §23's `Ctrl-q` never opens this dialog in the first place,
        /// because the desktop's row carries `Quit { confirm: true }`.
        #[test]
        fn the_desktop_answers_the_quit_dialog_with_one_key() {
            let mut ws = one_project_workspace(false);
            let mut ui = Ui::default();
            run(&mut ws, &mut ui, &frame(1, names::QUIT, None));
            let id = ui.dialog_id().expect("open");

            let fs = FakeFs::new();
            let pty = FakePty::new();
            let clock = FakeClock::default();
            let container = crate::testing::FakeContainerRuntime::new();
            let runner = crate::testing::FakeCommandRunner::new();
            let e = env(&fs, &pty, &clock, &container, &runner);
            let key = KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE);
            handle_prompt_key(key, &mut ws, &e, &mut ui).unwrap();

            assert!(ui.should_quit, "one key, no name, from the desktop");
            assert_eq!(
                ui.dialog_decisions,
                vec![(id, crate::web::protocol::DialogOutcome::Confirmed)]
            );
        }

        /// The same for a desktop-only action: refused with D16's sentence, and
        /// no file manager is spawned on the host's machine.
        #[test]
        fn a_desktop_only_frame_is_refused_with_the_host_only_sentence() {
            let mut ws = one_project_workspace(false);
            let mut ui = Ui::default();

            let ack = run(
                &mut ws,
                &mut ui,
                &frame(7, names::OPEN_WORKTREE_IN_FILE_MANAGER, None),
            );

            assert_eq!(ack.outcome, AckOutcome::Rejected);
            assert_eq!(
                ack.detail.as_deref(),
                Some(crate::web::commands::HOST_ONLY_REFUSAL)
            );
        }

        /// A name this build does not have never reaches a dispatch, at either
        /// layer.
        #[test]
        fn an_unknown_name_is_refused_by_the_applier_too() {
            let mut ws = one_project_workspace(false);
            let mut ui = Ui::default();

            let ack = run(&mut ws, &mut ui, &frame(8, "git_force_push", None));

            assert_eq!(ack.outcome, AckOutcome::Rejected);
            assert!(ack
                .detail
                .expect("a refusal states its reason")
                .contains("git_force_push"));
        }

        // ===============================================================
        // D13: the shared dialog
        // ===============================================================

        /// The one dialog open on the host, as the browser would receive it.
        ///
        /// The workspace is not decoration: artboard 1g's gate names a live
        /// session or project, so the view a browser gets is a function of both
        /// the prompt and the state it is about (§6.5 R13).
        fn maybe_view(ui: &Ui, ws: &Workspace) -> Option<crate::web::protocol::DialogView> {
            web_dialog_view(ui, &ws.active_project().name, &ws.active_project().state)
        }

        fn view(ui: &Ui, ws: &Workspace) -> crate::web::protocol::DialogView {
            web_dialog_view(ui, &ws.active_project().name, &ws.active_project().state)
                .expect("a dialog is open")
        }

        /// The body the host serialised into `DialogView::body`.
        fn body(view: &crate::web::protocol::DialogView) -> crate::web::protocol::DialogBody {
            serde_json::from_value(view.body.clone().expect("the dialog carries a body"))
                .expect("the body is a DialogBody")
        }

        fn keys(body: &crate::web::protocol::DialogBody) -> Vec<String> {
            body.buttons.iter().map(|b| b.key.clone()).collect()
        }

        /// A `dialog_confirm` / `dialog_cancel` frame for the dialog that is
        /// open, with whatever the browser filled in.
        fn answer(seq: u64, name: &str, ui: &Ui, args: serde_json::Value) -> WireCommand {
            let mut object = args;
            object["dialog_id"] = json!(ui.dialog_id().expect("a dialog is open").as_str());
            frame(seq, name, Some(object))
        }

        /// **D13's core claim.** A browser row whose desktop behaviour is "ask
        /// something" opens the question instead of acting, and the dialog that
        /// appears is tagged with the browser that asked — which is what makes
        /// the modal the desktop user did not ask for acceptable.
        #[test]
        fn a_command_that_needs_a_dialog_opens_one_tagged_with_its_origin() {
            let mut ws = one_project_workspace(false);
            let mut ui = Ui::default();

            let ack = run(
                &mut ws,
                &mut ui,
                &frame(1, names::NEW_AGENT_SESSION_TAB, None),
            );

            assert_eq!(ack.outcome, AckOutcome::Applied);
            assert_eq!(ack.detail.as_deref(), Some(DIALOG_OPENED_DETAIL));
            let view = view(&ui, &ws);
            assert_eq!(view.kind, "new_agent");
            assert_eq!(view.origin, browser_origin(), "D13: tagged with who asked");
            // And the desktop is rendering the origin sentence, not just holding
            // the structured fact.
            assert_eq!(
                ui.prompt.as_ref().and_then(|p| p.dialog.origin.as_deref()),
                Some("opened from browser · 192.168.2.20"),
            );
        }

        /// A dialog the *desktop* opened carries `DialogOrigin::Desktop` and no
        /// origin line — and still reaches the browser, because D13 makes the
        /// dialog app state in both directions.
        #[test]
        fn a_desktop_dialog_reaches_the_browser_with_no_origin_line() {
            let ws = one_project_workspace(false);
            let mut ui = Ui::default();

            start_new_tab_flow(&ws.projects[0].state, &mut ui);

            let view = view(&ui, &ws);
            assert_eq!(view.origin, crate::web::protocol::DialogOrigin::Desktop);
            assert!(ui
                .prompt
                .as_ref()
                .is_some_and(|p| p.dialog.origin.is_none()));
        }

        /// Artboard 1e: the new-agent dialog reaches the browser as the same
        /// shell the desktop is drawing — the agent radio as a list, the branch
        /// as an input, and `Enter` / `Tab` / `Esc` as its keys.
        #[test]
        fn the_new_agent_dialog_carries_artboard_1es_form() {
            let mut ws = one_project_workspace(false);
            let mut ui = Ui::default();

            run(
                &mut ws,
                &mut ui,
                &frame(1, names::NEW_AGENT_SESSION_TAB, None),
            );

            let view = view(&ui, &ws);
            let body = body(&view);
            assert_eq!(body.input.as_deref(), Some(""), "1e's branch field");
            assert_eq!(body.list.len(), 1, "one registered agent, one radio row");
            assert!(body.list[0].label.contains("Codex"));
            assert!(body.list[0].selected, "the default agent is preselected");
            assert_eq!(keys(&body), vec!["Enter", "Tab", "Esc"]);
            assert!(body.confirmable, "the browser may answer this one");
            assert_eq!(body.refusal, None);
        }

        /// 1e's right-hand state: `Tab` hides the branch field, because there is
        /// nothing to name. Driven from the browser, and the desktop's dialog
        /// changes with it — one dialog, two surfaces.
        #[test]
        fn toggling_run_from_base_from_the_browser_hides_the_branch_field() {
            let mut ws = one_project_workspace(false);
            let mut ui = Ui::default();
            run(
                &mut ws,
                &mut ui,
                &frame(1, names::NEW_AGENT_SESSION_TAB, None),
            );
            assert!(body(&view(&ui, &ws)).input.is_some());

            // A `Tab` with no decision key is not a thing the wire offers, so
            // the toggle rides on the confirm — and the form is gone by then.
            // What this asserts instead is the desktop half of the same key,
            // proving the browser's view tracks it.
            let key = KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE);
            let fs = FakeFs::new();
            let pty = FakePty::new();
            let clock = FakeClock::default();
            let container = crate::testing::FakeContainerRuntime::new();
            let runner = crate::testing::FakeCommandRunner::new();
            let e = env(&fs, &pty, &clock, &container, &runner);
            handle_prompt_key(key, &mut ws, &e, &mut ui).unwrap();

            let body = body(&view(&ui, &ws));
            assert_eq!(body.input, None, "1e: branch field hidden on run-from-base");
            assert!(body
                .buttons
                .iter()
                .any(|b| b.label.contains("Run from base")));
        }

        /// Either surface can confirm (D13). The browser's confirm goes through
        /// the very keypress the desktop's own `Enter` produces, so the dialog
        /// closes and the outcome recorded for the other surface is `Confirmed`.
        #[test]
        fn the_browser_can_confirm_and_the_other_surface_is_told_confirmed() {
            use crate::web::protocol::DialogOutcome;
            let mut ws = one_project_workspace(false);
            let mut ui = Ui::default();
            run(&mut ws, &mut ui, &frame(1, names::UNPAIR_PHONE, None));
            assert_eq!(view(&ui, &ws).kind, "unpair_phone");
            let id = ui.dialog_id().expect("open");

            let ack = answer(2, names::DIALOG_CONFIRM, &ui, json!({}));
            let ack = run(&mut ws, &mut ui, &ack);

            assert_eq!(ack.outcome, AckOutcome::Applied);
            assert!(ui.prompt.is_none(), "the dialog closed");
            assert!(ui.pending_unpair, "the primary action really ran");
            assert_eq!(ui.dialog_decisions, vec![(id, DialogOutcome::Confirmed)]);
        }

        /// The other direction, which is the half a browser-only implementation
        /// would get wrong: the **desktop** answers a dialog the browser opened,
        /// and the browser is told it was confirmed rather than replaced.
        #[test]
        fn the_desktop_can_confirm_a_browser_opened_dialog() {
            use crate::web::protocol::DialogOutcome;
            let mut ws = one_project_workspace(false);
            let mut ui = Ui::default();
            run(&mut ws, &mut ui, &frame(1, names::UNPAIR_PHONE, None));
            let id = ui.dialog_id().expect("open");

            let fs = FakeFs::new();
            let pty = FakePty::new();
            let clock = FakeClock::default();
            let container = crate::testing::FakeContainerRuntime::new();
            let runner = crate::testing::FakeCommandRunner::new();
            let e = env(&fs, &pty, &clock, &container, &runner);
            let key = KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE);
            handle_prompt_key(key, &mut ws, &e, &mut ui).unwrap();

            assert!(ui.prompt.is_none());
            assert_eq!(ui.dialog_decisions, vec![(id, DialogOutcome::Confirmed)]);
        }

        /// Cancelling from either surface, and the outcome the other one reads.
        #[test]
        fn the_browser_can_cancel_and_the_other_surface_is_told_cancelled() {
            use crate::web::protocol::DialogOutcome;
            let mut ws = one_project_workspace(false);
            let mut ui = Ui::default();
            run(&mut ws, &mut ui, &frame(1, names::UNPAIR_PHONE, None));
            let id = ui.dialog_id().expect("open");

            let ack = answer(2, names::DIALOG_CANCEL, &ui, json!({}));
            let ack = run(&mut ws, &mut ui, &ack);

            assert_eq!(ack.outcome, AckOutcome::Applied);
            assert!(ui.prompt.is_none());
            assert!(!ui.pending_unpair, "cancelling ran nothing");
            assert_eq!(ui.dialog_decisions, vec![(id, DialogOutcome::Cancelled)]);
        }

        /// The desktop's cancel key differs per dialog (`n`, `c`, `Esc`) and the
        /// wire outcome must not: the decision is read off the button's label,
        /// not off a table of key spellings.
        #[test]
        fn a_dialogs_own_cancel_button_reports_cancelled_whatever_its_key() {
            use crate::web::protocol::DialogOutcome;
            let n_cancel = Dialog::confirm(
                "Close shell 2?",
                vec![
                    DialogButton::new(DialogAccel::Char('y'), "Close"),
                    DialogButton::new(DialogAccel::Char('n'), "Cancel"),
                ],
            );
            let c_cancel = Dialog::confirm(
                "Push the committed changes only?",
                vec![
                    DialogButton::new(DialogAccel::Char('p'), "Push committed"),
                    DialogButton::new(DialogAccel::Char('c'), "Cancel"),
                ],
            );
            let press = |c: char| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
            assert_eq!(
                dialog_decision(&n_cancel, press('n')),
                DialogOutcome::Cancelled
            );
            assert_eq!(
                dialog_decision(&n_cancel, press('y')),
                DialogOutcome::Confirmed
            );
            assert_eq!(
                dialog_decision(&c_cancel, press('c')),
                DialogOutcome::Cancelled
            );
            assert_eq!(
                dialog_decision(&c_cancel, press('p')),
                DialogOutcome::Confirmed
            );
            // `Clear` in the status menu is a decision, not a dismissal.
            let clear = Dialog::confirm(
                "Set status override",
                vec![DialogButton::new(DialogAccel::Char('c'), "Clear")],
            );
            assert_eq!(
                dialog_decision(&clear, press('c')),
                DialogOutcome::Confirmed
            );
        }

        /// A browser may only press a key the dialog is showing. `choice` is the
        /// button's own key label, so there is no way to reach an action the
        /// person at the desktop cannot see.
        #[test]
        fn a_choice_the_dialog_is_not_showing_is_refused() {
            let mut ws = one_project_workspace(false);
            let mut ui = Ui::default();
            run(&mut ws, &mut ui, &frame(1, names::UNPAIR_PHONE, None));

            let ack = answer(2, names::DIALOG_CONFIRM, &ui, json!({ "choice": "q" }));
            let ack = run(&mut ws, &mut ui, &ack);

            assert_eq!(ack.outcome, AckOutcome::Rejected);
            let detail = ack.detail.expect("a refusal states its reason");
            assert!(detail.contains("no `q` button"), "{detail}");
            assert!(ui.prompt.is_some(), "the dialog is untouched");
        }

        /// `Esc` is a cancel, and cancelling has its own frame. Answering
        /// `dialog_confirm` with it is refused rather than quietly treated as a
        /// confirmation — the other surface would be told the wrong outcome.
        #[test]
        fn confirming_with_the_cancel_key_is_refused() {
            let mut ws = one_project_workspace(false);
            let mut ui = Ui::default();
            run(
                &mut ws,
                &mut ui,
                &frame(1, names::NEW_AGENT_SESSION_TAB, None),
            );

            let ack = answer(2, names::DIALOG_CONFIRM, &ui, json!({ "choice": "Esc" }));
            let ack = run(&mut ws, &mut ui, &ack);

            assert_eq!(ack.outcome, AckOutcome::Rejected);
            assert!(ack
                .detail
                .expect("a refusal states its reason")
                .contains("dialog_cancel"));
            assert!(ui.prompt.is_some());
        }

        /// **The 1g ruling, as a table.** Three answers are behind step 2 from a
        /// browser, and they are the ones that destroy work or rewrite history.
        ///
        /// The artboard's caption says *"Abandon and Quit are the only two that
        /// reach step 2"* while the artboard itself **draws Rebase Worktree** as
        /// its two-step example, and step 2's own copy reads *"This browser is
        /// remote…"*. The drawn artboard wins, and the copy says why: the
        /// trigger is the surface being remote, not the command being
        /// destructive. So the caption is counting the desktop's world — where
        /// nothing reaches step 2 at all — and this is the remote one, covering
        /// the superset the pixels demonstrate. See `specs/WEB_INTERFACE.md`
        /// §6.5 R13.
        #[test]
        fn exactly_three_answers_are_behind_step_two_from_a_browser() {
            use crate::web::commands::BrowserConfirm;
            let gated = |prompt: &Prompt| match browser_confirm_gate(prompt) {
                BrowserConfirm::TypedName(gate) => {
                    // The key it guards must be a button the dialog is really
                    // showing, or the gate would stand in front of nothing.
                    let dialog = prompt_dialog(prompt);
                    assert!(
                        dialog
                            .buttons
                            .iter()
                            .any(|b| dialog_accel_key(b.accel) == gate.key),
                        "the gate guards `{}`, which this dialog does not show: {:?}",
                        gate.key,
                        dialog.buttons
                    );
                    true
                }
                BrowserConfirm::OneStep => false,
            };

            for prompt in [
                Prompt::AbandonConfirm { dirty: true },
                Prompt::AbandonConfirm { dirty: false },
                Prompt::RebaseConfirm {
                    agent_branch: "flightdeck/x".to_string(),
                    base_branch: "main".to_string(),
                    drift: 2,
                    primary_running: true,
                },
                Prompt::QuitConfirm,
            ] {
                assert!(
                    gated(&prompt),
                    "{:?} destroys work or rewrites history",
                    dialog_kind(&prompt)
                );
            }

            for prompt in [
                // SPECS §14/§15: neither rewrites history nor discards work — a
                // push is undone by a push, a merge-back is a commit on base.
                Prompt::PushConfirm,
                Prompt::MergeConfirm {
                    agent_branch: "flightdeck/x".to_string(),
                    base_branch: "main".to_string(),
                    primary_running: false,
                },
                // The sidebar's close menu: `a` only dispatches the unconfirmed
                // abandon, which asks. The gate belongs on the question it
                // opens, and it is there.
                Prompt::CloseAgentChoice { index: 0 },
                Prompt::CloseTab {
                    actions: vec![CloseAction::CtrlCPrimary],
                },
                Prompt::CloseChildConfirm {
                    label: "shell 2".to_string(),
                },
                Prompt::RenameTab {
                    buffer: String::new(),
                },
                Prompt::SetManualStatus,
                Prompt::SelectChildAgent { agents: vec![] },
                Prompt::CloseProjectConfirm { index: 0 },
                Prompt::UnpairConfirm,
            ] {
                assert!(
                    !gated(&prompt),
                    "{:?} is one step, exactly as on the desktop",
                    dialog_kind(&prompt)
                );
            }
        }

        /// **The destructive family (`remote-control-ll5.4`, artboard 1g): a
        /// wrong name refuses and the worktree provably survives; cancelling is
        /// never gated.**
        ///
        /// The gate is checked before a single key is fed into the prompt, so
        /// "nothing was abandoned" is asserted three ways: the question is still
        /// open, the tab is still in the workspace, and no decision was
        /// witnessed for the other surface to be told about.
        #[test]
        fn a_destructive_confirmation_needs_the_exact_session_name() {
            let dir = TempDir::new().unwrap();
            let git = FakeGit::new();
            let pty = FakePty::new();
            let mut ws = workspace_with_a_tab(&dir, &git, &pty);
            let mut ui = Ui::default();
            // The desktop opens it; D13 shares it either way.
            start_prompt(&mut ui, Prompt::AbandonConfirm { dirty: true });

            let view = view(&ui, &ws);
            let body = body(&view);
            assert_eq!(view.kind, "confirm_abandon");
            assert!(
                body.confirmable,
                "ll5.4 lifts the flat refusal: a browser confirms through step 2"
            );
            assert_eq!(body.refusal, None);
            let gate = body.confirm_gate.clone().expect("1g's step 2 is published");
            assert_eq!(gate.key, "y");
            assert_eq!(
                gate.expected, "Task",
                "1g hints the *session* name, not the branch it lives on"
            );

            for wrong in [
                "",
                "task",
                "TASK",
                "Task ",
                " Task",
                "Tas",
                "flightdeck/Task",
            ] {
                let confirm = answer(
                    1,
                    names::DIALOG_CONFIRM,
                    &ui,
                    json!({ "confirm_name": wrong }),
                );
                let ack = run(&mut ws, &mut ui, &confirm);
                assert_eq!(ack.outcome, AckOutcome::Rejected, "for `{wrong}`");
                assert!(ui.prompt.is_some(), "`{wrong}` left the question open");
                assert_eq!(
                    ws.projects[0].state.tabs.len(),
                    1,
                    "`{wrong}` must not have abandoned anything"
                );
                assert!(
                    ui.dialog_decisions.is_empty(),
                    "`{wrong}` never reached the prompt, so nothing was decided"
                );
            }

            // Step 1 alone: pressing the button and sending no name at all.
            let confirm = answer(2, names::DIALOG_CONFIRM, &ui, json!({}));
            let ack = run(&mut ws, &mut ui, &confirm);
            assert_eq!(ack.outcome, AckOutcome::Rejected);
            assert!(ack
                .detail
                .expect("a refusal states its reason")
                .contains("confirm_name"));
            assert!(ui.prompt.is_some());
            assert_eq!(ws.projects[0].state.tabs.len(), 1);

            // And the half R8 insists on: cancelling is never gated. A shared
            // dialog a remote surface can see but not dismiss would be worse
            // than not sharing it — so no name, and it closes.
            let cancel = answer(3, names::DIALOG_CANCEL, &ui, json!({}));
            let ack = run(&mut ws, &mut ui, &cancel);
            assert_eq!(ack.outcome, AckOutcome::Applied);
            assert!(ui.prompt.is_none(), "cancelling is always allowed");
            assert_eq!(
                ws.projects[0].state.tabs.len(),
                1,
                "cancelling destroyed nothing"
            );
            assert_eq!(
                ui.dialog_decisions,
                vec![(
                    view.dialog_id.clone(),
                    crate::web::protocol::DialogOutcome::Cancelled
                )]
            );
        }

        /// 1g's own drawing is the rebase, and SPECS §5.1 is why: it is the one
        /// sanctioned history rewrite, so from a browser it takes both steps.
        /// The two git confirmations that rewrite nothing (§14 push, §15 merge)
        /// stay one step — a gate there would be ceremony, and ceremony teaches
        /// people to type the name without reading it.
        #[test]
        fn the_rewrite_is_gated_and_the_other_git_confirmations_are_not() {
            let dir = TempDir::new().unwrap();
            let git = FakeGit::new();
            let pty = FakePty::new();
            let mut ws = workspace_with_a_tab(&dir, &git, &pty);
            let mut ui = Ui::default();

            start_prompt(
                &mut ui,
                Prompt::RebaseConfirm {
                    agent_branch: "flightdeck/Task".to_string(),
                    base_branch: "main".to_string(),
                    drift: 4,
                    primary_running: true,
                },
            );
            // The browser reads the same question the desktop does — the
            // branches and §12's drift — before it answers (R11).
            let question = view(&ui, &ws);
            assert!(
                question.title.contains("base moved 4 commits"),
                "{}",
                question.title
            );
            let gate = body(&question)
                .confirm_gate
                .expect("§5.1's rewrite takes both steps from a browser");
            assert_eq!(gate.expected, "Task");
            assert_eq!(
                gate.instruction,
                crate::web::commands::GATE_REBASE_INSTRUCTION,
                "artboard 1g's step 2 copy, verbatim"
            );

            // A confirm with no name gets nowhere near `GitExecutor::rebase_onto`.
            let confirm = answer(1, names::DIALOG_CONFIRM, &ui, json!({}));
            let ack = run(&mut ws, &mut ui, &confirm);
            assert_eq!(ack.outcome, AckOutcome::Rejected);
            assert!(ui.prompt.is_some(), "the question is still open");
            assert!(
                git.rebases().is_empty(),
                "SPECS §5.1: nothing was rewritten"
            );

            for prompt in [
                Prompt::PushConfirm,
                Prompt::MergeConfirm {
                    agent_branch: "flightdeck/Task".to_string(),
                    base_branch: "main".to_string(),
                    primary_running: false,
                },
            ] {
                start_prompt(&mut ui, prompt);
                let one_step = view(&ui, &ws);
                let body = body(&one_step);
                assert!(body.confirmable);
                assert!(
                    body.confirm_gate.is_none(),
                    "§14/§15 are one step: {:?}",
                    body.confirm_gate
                );
            }
        }

        /// A gate the host cannot resolve is a refusal, not a shortcut. The tab
        /// the question was about is gone, so there is no name to check — and
        /// confirming past that would destroy something nobody named.
        #[test]
        fn a_gate_with_nothing_to_name_refuses_but_still_cancels() {
            let mut ws = one_project_workspace(false);
            let mut ui = Ui::default();
            start_prompt(&mut ui, Prompt::AbandonConfirm { dirty: false });

            let body = body(&view(&ui, &ws));
            assert!(!body.confirmable, "there is nothing to type back");
            assert_eq!(
                body.refusal.as_deref(),
                Some(crate::web::commands::GATE_UNRESOLVED_REFUSAL)
            );
            assert!(body.confirm_gate.is_none());

            let confirm = answer(
                1,
                names::DIALOG_CONFIRM,
                &ui,
                json!({ "confirm_name": "Task" }),
            );
            let ack = run(&mut ws, &mut ui, &confirm);
            assert_eq!(ack.outcome, AckOutcome::Rejected);
            assert_eq!(
                ack.detail.as_deref(),
                Some(crate::web::commands::GATE_UNRESOLVED_REFUSAL)
            );
            assert!(ui.prompt.is_some());

            let cancel = answer(2, names::DIALOG_CANCEL, &ui, json!({}));
            let ack = run(&mut ws, &mut ui, &cancel);
            assert_eq!(
                ack.outcome,
                AckOutcome::Applied,
                "cancelling is never gated"
            );
            assert!(ui.prompt.is_none());
        }

        /// **The git confirmations are answerable from a browser**
        /// (`remote-control-ll5.5`, SPECS §5.1/§14/§15). Before that task all
        /// three refused a browser's confirm outright; that gate is gone,
        /// because these dialogs *are* §5's confirmation and D13 already shares
        /// them — the browser reads the same words the desktop does before it
        /// answers.
        ///
        /// `remote-control-ll5.4` tightened one of the three: §5.1's rebase now
        /// takes artboard 1g's typed name as well, and that half is pinned in
        /// `the_rewrite_is_gated_and_the_other_git_confirmations_are_not`. This
        /// test keeps the *un*gated half honest, on §14's push.
        ///
        /// The refusal the browser gets here is the **dispatch's** own (this
        /// workspace has no tab to push), which is the whole point: the confirm
        /// was accepted and reached the real command, and the sentence came back
        /// from the guard rather than from a gate standing in front of it.
        #[test]
        fn a_git_dialog_is_confirmable_from_a_browser_and_acks_the_dispatch() {
            let mut ws = one_project_workspace(false);
            let mut ui = Ui::default();
            start_prompt(&mut ui, Prompt::PushConfirm);

            let view = view(&ui, &ws);
            let body = body(&view);
            assert_eq!(view.kind, "confirm_push");
            assert!(body.confirmable, "the git gate is lifted (ll5.5)");
            assert_eq!(body.refusal, None);
            assert!(
                body.confirm_gate.is_none(),
                "§14 rewrites nothing, so it stays one step (ll5.4)"
            );

            let confirm = answer(1, names::DIALOG_CONFIRM, &ui, json!({}));
            let ack = run(&mut ws, &mut ui, &confirm);

            // Accepted, dispatched, and refused by the command's own guard —
            // never by a gate in front of it.
            assert_eq!(ack.outcome, AckOutcome::Rejected);
            let detail = ack.detail.expect("a refusal states its reason");
            assert!(
                detail.contains("no tab selected"),
                "the guard's own words, not a gate's: {detail}"
            );
            assert!(ui.prompt.is_none(), "the dialog was answered, not blocked");
        }

        /// D13 + SPECS §5, the other half: cancelling a git confirmation is
        /// still always allowed, and still tells the other surface the dialog
        /// was dismissed rather than confirmed.
        #[test]
        fn a_git_dialog_can_still_be_cancelled_from_a_browser() {
            let mut ws = one_project_workspace(false);
            let mut ui = Ui::default();
            start_prompt(&mut ui, Prompt::PushConfirm);

            let cancel = answer(1, names::DIALOG_CANCEL, &ui, json!({}));
            let ack = run(&mut ws, &mut ui, &cancel);

            assert_eq!(ack.outcome, AckOutcome::Applied);
            assert!(ui.prompt.is_none());
        }

        /// **The git rows dispatch, and their refusals are the host's own.**
        ///
        /// This workspace has no Agent Session Tab, so every one of them hits
        /// the same guard — which is exactly what makes the assertion useful:
        /// the browser is handed the sentence the dispatch produced, not a
        /// generic "that failed" and not the blanket refusal these rows carried
        /// before this task. `pull_base` is the deliberate exception (SPECS
        /// §5.2) and answers with the boundary decision instead.
        #[test]
        fn the_git_rows_reach_the_dispatch_and_ack_its_own_words() {
            let mut ws = one_project_workspace(false);
            let mut ui = Ui::default();

            for (seq, name) in [
                (10, names::REBASE_WORKTREE),
                (11, names::PUSH_BRANCH),
                (12, names::FINISH_LOCAL_MERGE),
            ] {
                let ack = run(&mut ws, &mut ui, &frame(seq, name, None));
                assert_eq!(ack.outcome, AckOutcome::Rejected, "for `{name}`");
                let detail = ack.detail.expect("a refusal states its reason");
                assert!(
                    detail.contains("no tab selected"),
                    "`{name}` must ack the dispatch's own sentence: {detail}"
                );
                assert!(ui.prompt.is_none(), "`{name}` opened nothing");
            }

            let ack = run(&mut ws, &mut ui, &frame(13, names::PULL_BASE, None));
            assert_eq!(ack.outcome, AckOutcome::Rejected);
            assert_eq!(
                ack.detail.as_deref(),
                Some(crate::web::commands::PULL_BASE_REFUSAL),
                "§5.2's row refuses with the boundary decision, not with a guard"
            );
        }

        /// An answer for a dialog that has been replaced is refused, not applied
        /// to whatever is on screen now. That is the mechanism behind "nobody
        /// confirms something they never read".
        #[test]
        fn an_answer_for_a_replaced_dialog_is_refused() {
            let mut ws = one_project_workspace(false);
            let mut ui = Ui::default();
            run(&mut ws, &mut ui, &frame(1, names::UNPAIR_PHONE, None));
            let stale = ui.dialog_id().expect("open");
            // The desktop moves on to a different question.
            start_prompt(&mut ui, Prompt::SetManualStatus);

            let ack = run(
                &mut ws,
                &mut ui,
                &frame(
                    2,
                    names::DIALOG_CONFIRM,
                    Some(json!({ "dialog_id": stale.as_str() })),
                ),
            );

            assert_eq!(ack.outcome, AckOutcome::Rejected);
            let detail = ack.detail.expect("a refusal states its reason");
            assert!(detail.contains("replaced"), "{detail}");
            assert!(ui.prompt.is_some(), "the live dialog is untouched");
        }

        /// Answering when nothing is open is refused with the reason, not
        /// silently ignored: the browser's view is behind and needs to say so.
        #[test]
        fn an_answer_with_no_dialog_open_is_refused() {
            let mut ws = one_project_workspace(false);
            let mut ui = Ui::default();

            let ack = run(
                &mut ws,
                &mut ui,
                &frame(
                    1,
                    names::DIALOG_CANCEL,
                    Some(json!({ "dialog_id": "dialog-9" })),
                ),
            );

            assert_eq!(ack.outcome, AckOutcome::Rejected);
            assert!(ack
                .detail
                .expect("a refusal states its reason")
                .contains("No dialog is open"));
        }

        /// An answer with no `dialog_id` is a refusal, not a guess at whichever
        /// dialog happens to be open.
        #[test]
        fn an_answer_that_names_no_dialog_is_refused() {
            let mut ws = one_project_workspace(false);
            let mut ui = Ui::default();
            run(&mut ws, &mut ui, &frame(1, names::UNPAIR_PHONE, None));

            let ack = run(&mut ws, &mut ui, &frame(2, names::DIALOG_CONFIRM, None));

            assert_eq!(ack.outcome, AckOutcome::Rejected);
            assert!(ack
                .detail
                .expect("a refusal states its reason")
                .contains("dialog_id"));
            assert!(ui.prompt.is_some());
        }

        /// **The `Superseded` policy.** A second dialog arriving while one is
        /// open replaces it, and the browser is told the first one was
        /// `Superseded` — never left holding a modal it can still answer, and
        /// never told a decision was made. The diff is what says it, because the
        /// diff is the only thing that witnessed a dialog vanish without one.
        #[test]
        fn a_replaced_dialog_is_reported_superseded_and_the_new_one_opened() {
            use crate::web::protocol::{Delta, DialogOutcome};
            let mut ws = one_project_workspace(false);
            let mut ui = Ui::default();
            run(&mut ws, &mut ui, &frame(1, names::UNPAIR_PHONE, None));
            let first = ui.dialog_id().expect("open");
            let published = crate::web::server::HostState {
                dialog: maybe_view(&ui, &ws),
                ..crate::web::server::HostState::default()
            };

            start_prompt(&mut ui, Prompt::SetManualStatus);
            let next = crate::web::server::HostState {
                dialog: maybe_view(&ui, &ws),
                ..crate::web::server::HostState::default()
            };
            let second = ui.dialog_id().expect("open");
            assert_ne!(first, second, "a replacement gets its own id");

            let mut frames = crate::web::stream::deltas(&published, &next);
            resolve_dialog_outcomes(&mut frames, &ui.dialog_decisions);

            assert!(
                matches!(
                    frames.as_slice(),
                    [
                        Delta::DialogClosed {
                            outcome: DialogOutcome::Superseded,
                            ..
                        },
                        Delta::DialogOpened(_),
                    ]
                ),
                "{frames:?}"
            );
            let Delta::DialogClosed { dialog_id, .. } = &frames[0] else {
                unreachable!()
            };
            assert_eq!(dialog_id, &first);
        }

        /// The other side of the same coin: where somebody *did* decide, the
        /// diff's `Superseded` is upgraded to the real outcome. Without this the
        /// browser would be told "replaced" about a dialog the desktop answered.
        #[test]
        fn a_decided_dialog_is_reported_with_the_decision_not_superseded() {
            use crate::web::protocol::{Delta, DialogOutcome};
            let mut ws = one_project_workspace(false);
            let mut ui = Ui::default();
            run(&mut ws, &mut ui, &frame(1, names::UNPAIR_PHONE, None));
            let id = ui.dialog_id().expect("open");
            let published = crate::web::server::HostState {
                dialog: maybe_view(&ui, &ws),
                ..crate::web::server::HostState::default()
            };

            let cancel = answer(2, names::DIALOG_CANCEL, &ui, json!({}));
            run(&mut ws, &mut ui, &cancel);
            let next = crate::web::server::HostState {
                dialog: maybe_view(&ui, &ws),
                ..crate::web::server::HostState::default()
            };

            let mut frames = crate::web::stream::deltas(&published, &next);
            assert!(
                matches!(
                    frames.as_slice(),
                    [Delta::DialogClosed {
                        outcome: DialogOutcome::Superseded,
                        ..
                    }]
                ),
                "the diff alone can only say Superseded: {frames:?}"
            );
            resolve_dialog_outcomes(&mut frames, &ui.dialog_decisions);
            assert_eq!(
                frames,
                vec![Delta::DialogClosed {
                    dialog_id: id,
                    outcome: DialogOutcome::Cancelled,
                }]
            );
        }

        /// D14: a read-only observer has no input, so it cannot answer a dialog
        /// either. The check is the table's, one step before the host — an
        /// observer is told `read_only` rather than being handed the reason a
        /// command it may not send would have failed.
        #[test]
        fn an_observer_cannot_answer_a_dialog() {
            for name in [names::DIALOG_CONFIRM, names::DIALOG_CANCEL] {
                let spec = crate::web::commands::lookup(name).expect("in the inventory");
                assert!(
                    spec.requires_control(),
                    "`{name}` must be a controller's frame (D14)"
                );
            }
        }

        /// A dialog id is never reused, so a stale answer cannot land on a new
        /// dialog by matching its id.
        #[test]
        fn dialog_ids_are_never_reused() {
            let mut ui = Ui::default();
            let mut seen = std::collections::HashSet::new();
            for _ in 0..5 {
                start_prompt(&mut ui, Prompt::SetManualStatus);
                assert!(seen.insert(ui.dialog_id().expect("open")));
            }
        }

        /// D11: read-marking still works through the unified applier — the frame
        /// that used to be special-cased in the drain now routes off the same
        /// table as everything else.
        #[test]
        fn marking_activity_read_still_routes_through_the_table() {
            let spec = crate::web::commands::lookup(names::MARK_ACTIVITY_READ)
                .expect("the feed command is in the inventory");
            assert_eq!(spec.route, crate::web::commands::Route::ActivityRead);
        }
    }

    /// **The git surface's refusal paths** (`remote-control-ll5.5`; SPECS §5,
    /// §5.1, §12, §13, §14, §15, §26).
    ///
    /// `web_command_surface` above proves the *routing* — a git row reaches the
    /// dispatch and is acked with whatever that dispatch reported. This module
    /// proves the sentences it reports, against a `FakeGit` that can be made
    /// dirty, moved, or left without a branch, because that is where the guards
    /// actually live. Every test drives `dispatch_command`, which is the exact
    /// function `run_web_command` reaches through `run_palette_action`, and
    /// reads [`Ui::web_outcome`], which is the exact value the `Ack` is built
    /// from — so a sentence asserted here is a sentence the browser receives.
    ///
    /// SPECS §26 asks for the refusal paths and not only the happy ones. The
    /// four that matter for a remote surface are here: §13's dirty base, §5.1's
    /// rebase preconditions, §14's uncommitted-changes warning, and git itself
    /// failing outright.
    mod web_git_guards {
        use super::*;
        use crate::web::commands::{confirmation_of, Confirmation};

        const REPO: &str = "/repo";

        /// A project with one Agent Session Tab, on a `FakeGit` whose dirtiness,
        /// branches and drift the test drives. The agent worktree is left with
        /// its own branch checked out, which is what the §5.1 preconditions
        /// require (FakeGit's `current_branch` is one global, so it is set once
        /// the created branch name is known).
        fn one_tab(git: &FakeGit, pty: &FakePty, dir: &TempDir) -> AppState {
            let agent = make_real_agent(dir, "opencode");
            let mut config = config_with_agent(agent);
            config.ui.default_agent = "opencode".to_string();
            let fs = FakeFs::new();
            let clock = FakeClock::default();
            let container = crate::testing::FakeContainerRuntime::new();
            let command = crate::testing::FakeCommandRunner::new();
            let services = Services {
                git,
                fs: &fs,
                pty,
                clock: &clock,
                container: &container,
                command: &command,
            };
            pty.queue_session();
            let mut state = AppState::new(
                config,
                crate::persistence::project_state::default_state("main"),
                REPO,
                "/repo/.flightdeck/state.json",
            );
            state
                .dispatch(
                    Command::NewAgentTab {
                        name: "Task".to_string(),
                        agent_key: None,
                    },
                    &services,
                )
                .expect("the tab is created");
            git.set_current_branch(state.tabs[0].meta.branch.clone());
            state
        }

        /// The absolute path of the one tab's worktree, as the guards see it.
        fn worktree(state: &AppState) -> PathBuf {
            to_absolute(
                Path::new(REPO),
                Path::new(&state.tabs[0].meta.worktree_path_relative),
            )
        }

        /// One browser dispatch: the command the table would forward, run
        /// through the very function `run_palette_action` calls, with a dialog
        /// origin set for exactly its duration the way `run_web_command` sets
        /// one. Returns what the `Ack` would be built from.
        fn dispatch(
            cmd: Command,
            state: &mut AppState,
            git: &FakeGit,
            pty: &FakePty,
            ui: &mut Ui,
        ) -> Option<WebDispatch> {
            let fs = FakeFs::new();
            let clock = FakeClock::default();
            let container = crate::testing::FakeContainerRuntime::new();
            let command = crate::testing::FakeCommandRunner::new();
            let services = Services {
                git,
                fs: &fs,
                pty,
                clock: &clock,
                container: &container,
                command: &command,
            };
            ui.web_outcome = None;
            ui.web_dialog_origin = Some(crate::web::protocol::DialogOrigin::Browser {
                viewer_id: Some(crate::web::protocol::ViewerId::new("viewer-1")),
                label: "192.168.2.20".to_string(),
            });
            let result = dispatch_command(cmd, state, &services, ui);
            ui.web_dialog_origin = None;
            result.expect("a guard is a refusal, never an event-loop error");
            ui.web_outcome.take()
        }

        /// The key the desktop's own dialog button would send, fed into the open
        /// prompt exactly as `apply_web_dialog` feeds a browser's confirm.
        fn press(c: char, state: &mut AppState, git: &FakeGit, pty: &FakePty, ui: &mut Ui) {
            let fs = FakeFs::new();
            let clock = FakeClock::default();
            let container = crate::testing::FakeContainerRuntime::new();
            let command = crate::testing::FakeCommandRunner::new();
            let services = Services {
                git,
                fs: &fs,
                pty,
                clock: &clock,
                container: &container,
                command: &command,
            };
            handle_prompt_key_project(
                KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
                state,
                &services,
                ui,
                0,
            )
            .expect("a prompt key is never an event-loop error");
        }

        fn refusal(outcome: Option<WebDispatch>) -> String {
            match outcome {
                Some(WebDispatch::Refused(reason)) => reason,
                other => panic!("expected a refusal the browser can read, got {other:?}"),
            }
        }

        /// **SPECS §5.1, the carve-out in one test.** The row a browser sends
        /// carries `confirm: false`, so the first dispatch *asks*: nothing is
        /// rebased, a shared dialog opens carrying the origin that raised it and
        /// §12's drift, and only the answer to that dialog rewrites anything.
        ///
        /// This is the property the whole boundary rests on. If it ever fails,
        /// the browser can rewrite history without anybody reading a question,
        /// and the module doc's exception clause is void.
        #[test]
        fn a_browsers_rebase_asks_before_it_rewrites_and_only_then_rebases() {
            let dir = TempDir::new().unwrap();
            let git = FakeGit::new().with_root(REPO).with_branches(["main"]);
            let pty = FakePty::new();
            let mut state = one_tab(&git, &pty, &dir);
            let mut ui = Ui::default();

            // SPECS §12: the base has moved seven commits since this tab was cut.
            git.set_ahead_behind(state.tabs[0].meta.base_commit_sha.clone(), "main", 7, 0);

            // The value the table forwards — and nothing else can be forwarded,
            // because `INVENTORY` carries this one.
            let cmd = Command::RebaseWorktree { confirm: false };
            assert_eq!(confirmation_of(&cmd), Confirmation::Pending);
            let outcome = dispatch(cmd, &mut state, &git, &pty, &mut ui);

            // Nothing was rewritten, and the browser is told a question opened
            // rather than that something was done.
            assert!(git.rebases().is_empty(), "SPECS §5.1: it asks first");
            assert!(
                matches!(outcome, Some(WebDispatch::Applied(None))),
                "a question opened, and nothing else is claimed: {outcome:?}"
            );

            // The question is on both surfaces, answerable from either, and it
            // names what it will do — including the drift it will pull in.
            let view = web_dialog_view(&ui, "proj", &state).expect("the dialog is published");
            let body: crate::web::protocol::DialogBody =
                serde_json::from_value(view.body.clone().expect("a body")).expect("a DialogBody");
            assert_eq!(view.kind, "confirm_rebase");
            assert!(body.confirmable, "the browser may answer its own question");
            assert_eq!(body.refusal, None);
            assert!(
                view.title.contains("base moved 7 commits"),
                "{}",
                view.title
            );
            assert!(view.title.contains("Rewrites history"), "{}", view.title);
            assert!(
                matches!(
                    view.origin,
                    crate::web::protocol::DialogOrigin::Browser { .. }
                ),
                "the desktop must read who asked"
            );

            // Answering it — the same keypress `apply_web_dialog` synthesises —
            // is what reaches `GitExecutor::rebase_onto`, once.
            press('y', &mut state, &git, &pty, &mut ui);
            assert_eq!(
                git.rebases(),
                vec![("main".to_string(), worktree(&state))],
                "the confirmation is what rebases, and it rebases once"
            );
        }

        /// **SPECS §5.1's preconditions, in the guard's own words.** A dirty
        /// agent worktree is refused before anything is asked — FlightDeck never
        /// stashes or discards — and the browser gets the sentence naming the
        /// worktree, not a generic failure.
        #[test]
        fn a_failed_rebase_precondition_reaches_the_browser_verbatim() {
            let dir = TempDir::new().unwrap();
            let git = FakeGit::new().with_root(REPO).with_branches(["main"]);
            let pty = FakePty::new();
            let mut state = one_tab(&git, &pty, &dir);
            let mut ui = Ui::default();
            git.set_dirty_at(worktree(&state), true);

            let outcome = dispatch(
                Command::RebaseWorktree { confirm: false },
                &mut state,
                &git,
                &pty,
                &mut ui,
            );

            let reason = refusal(outcome);
            assert!(
                reason.contains("has uncommitted changes") && reason.contains("before rebasing"),
                "the precondition's own sentence: {reason}"
            );
            assert!(git.rebases().is_empty());
            assert!(ui.prompt.is_none(), "a refusal asks nothing");
        }

        /// **SPECS §13: a dirty base disables local merge**, and the browser is
        /// told so as a *refusal*.
        ///
        /// The dispatch reports this as `Effect::Warning`, which
        /// `apply_effect` records as applied-with-caveat — correct for a
        /// confirmed merge whose cleanup failed, and wrong here, where nothing
        /// merged at all. `dispatch_command` separates the two by the command's
        /// phase, the same line the phone's `dispatch_remote_merge_back` draws.
        /// A browser told `Applied` over §13's sentence would be a surface
        /// claiming something the host did not say.
        #[test]
        fn a_dirty_base_refuses_the_merge_rather_than_applying_a_warning() {
            let dir = TempDir::new().unwrap();
            let git = FakeGit::new().with_root(REPO).with_branches(["main"]);
            let pty = FakePty::new();
            let mut state = one_tab(&git, &pty, &dir);
            let mut ui = Ui::default();
            git.set_dirty_at(Path::new(REPO), true);

            let outcome = dispatch(
                Command::FinishLocalMerge { confirm: false },
                &mut state,
                &git,
                &pty,
                &mut ui,
            );

            let reason = refusal(outcome);
            assert!(
                reason.contains("Local merge is disabled"),
                "SPECS §13's own words: {reason}"
            );
            assert!(
                reason.contains("push this branch and create a PR instead"),
                "including what to do instead: {reason}"
            );
            assert!(git.merges().is_empty(), "nothing merged");
            assert!(ui.prompt.is_none(), "and nothing was asked");
        }

        /// **SPECS §15's technical preconditions.** A dirty *agent* worktree is
        /// a different refusal from §13's, and the browser gets that one instead
        /// — the point of forwarding the guard's sentence rather than wording
        /// one here.
        #[test]
        fn a_dirty_agent_worktree_refuses_the_merge_with_its_own_reason() {
            let dir = TempDir::new().unwrap();
            let git = FakeGit::new().with_root(REPO).with_branches(["main"]);
            let pty = FakePty::new();
            let mut state = one_tab(&git, &pty, &dir);
            let mut ui = Ui::default();
            git.set_dirty_at(worktree(&state), true);

            let outcome = dispatch(
                Command::FinishLocalMerge { confirm: false },
                &mut state,
                &git,
                &pty,
                &mut ui,
            );

            let reason = refusal(outcome);
            assert!(
                reason.contains("before merging"),
                "§15's precondition, not §13's: {reason}"
            );
            assert!(!reason.contains("Local merge is disabled"), "{reason}");
            assert!(git.merges().is_empty());
        }

        /// **SPECS §14.** A worktree with uncommitted changes gets the warning
        /// and the three-way choice first; the push happens only when that
        /// dialog is answered. `confirm: None` in the table is what makes the
        /// warning unskippable from a frame.
        #[test]
        fn a_push_over_uncommitted_changes_warns_before_it_pushes() {
            let dir = TempDir::new().unwrap();
            let git = FakeGit::new().with_root(REPO).with_branches(["main"]);
            let pty = FakePty::new();
            let mut state = one_tab(&git, &pty, &dir);
            let mut ui = Ui::default();
            git.set_dirty_at(worktree(&state), true);

            let outcome = dispatch(
                Command::PushBranch { confirm: None },
                &mut state,
                &git,
                &pty,
                &mut ui,
            );

            assert!(git.pushes().is_empty(), "SPECS §14: it warns first");
            assert!(
                matches!(outcome, Some(WebDispatch::Applied(None))),
                "a question opened, and nothing else is claimed: {outcome:?}"
            );
            let view = web_dialog_view(&ui, "proj", &state).expect("the warning is published");
            assert_eq!(view.kind, "confirm_push");
            assert!(
                view.title.contains("committed changes only"),
                "{}",
                view.title
            );

            // `p` is `Push committed` — the same key the desktop's button fires.
            press('p', &mut state, &git, &pty, &mut ui);
            assert_eq!(git.pushes().len(), 1, "the answer is what pushes");
        }

        /// **Git failing outright is not a guard, and must not be dressed as
        /// one.** An executor error becomes `WebDispatch::Failed`, and the
        /// browser is handed git's own message — a `Rejected` ack with the real
        /// reason rather than "that did not work".
        #[test]
        fn a_git_error_reaches_the_browser_as_gits_own_message() {
            let dir = TempDir::new().unwrap();
            let git = FakeGit::new()
                .with_root(REPO)
                .with_branches(["main"])
                .with_current_branch_error("fatal: not a git repository");
            let pty = FakePty::new();
            let mut state = one_tab(&git, &pty, &dir);
            let mut ui = Ui::default();

            let outcome = dispatch(
                Command::FinishLocalMerge { confirm: false },
                &mut state,
                &git,
                &pty,
                &mut ui,
            );

            match outcome {
                Some(WebDispatch::Failed(message)) => assert!(
                    message.contains("not a git repository"),
                    "git's own words: {message}"
                ),
                other => panic!("expected the failure to reach the browser, got {other:?}"),
            }
            assert!(git.merges().is_empty());
        }
    }

    /// D11 §5.1's `finished, 18 files touched`: the per-tab, one-shot git
    /// refresh that makes the clause literal, and the honest-empty paths that
    /// keep it from ever becoming a guess.
    ///
    /// Everything here runs the real `record_web_transitions` /
    /// `WebSurface::record_transition` / `PendingFinishes` path. The only piece
    /// standing in for production is the worker thread: `spawn_finish_count`
    /// does nothing but call `activity::file_count` and post the answer back,
    /// so `answer_with` below does exactly that, synchronously, against a
    /// `FakeGit`.
    mod web_finish_counts {
        use super::*;
        use crate::contracts::{
            AgentDef, InterpretedStatus, ProjectState as CoreProjectState, StatusPatterns,
            TabState, STATE_VERSION,
        };
        use crate::testing::FakeGit;

        /// An agent whose command basename `status_backend` recognises, so
        /// `lifecycle_reporting` is true and `observe` may report a real
        /// `working → idle` pair rather than §5.1's `unknown → unknown`.
        fn claude() -> AgentDef {
            AgentDef {
                key: "claude".to_string(),
                display_name: "Claude Code".to_string(),
                command: "claude".to_string(),
                args: vec![],
                status_patterns: StatusPatterns::default(),
            }
        }

        fn tab_state(id: &str, slug: &str) -> TabState {
            TabState {
                id: id.to_string(),
                name: slug.to_string(),
                slug: slug.to_string(),
                agent: "claude".to_string(),
                branch: format!("{slug}-branch"),
                worktree_path_relative: format!("worktrees/{slug}"),
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

        /// A project with one `Ready` tab whose primary is running, so its
        /// display status is driven by the cached lifecycle signal alone and a
        /// test can move it by hand.
        fn project(name: &str, root: &str, slug: &str, pty: &FakePty) -> Project {
            let mut config = Config::default();
            config.ui.default_agent = "claude".to_string();
            config.agents.insert("claude".to_string(), claude());
            let state = CoreProjectState {
                version: STATE_VERSION,
                project_root_relative: ".".to_string(),
                base_branch: "main".to_string(),
                tabs: vec![tab_state(&format!("{name}-t1"), slug)],
            };
            let mut app = AppState::new(
                config,
                state,
                root,
                format!("{root}/.flightdeck/state.json"),
            );
            pty.queue_session();
            app.tabs[0]
                .session
                .spawn_primary(pty, "agent", &[], Path::new(root), PtySize::default())
                .unwrap();
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

        /// A surface with no server and no real credential file behind it —
        /// only the feed and the pending-finish queue are under test.
        fn web_surface() -> WebSurface {
            let store = crate::web::credentials::CredentialStore::open(
                Arc::new(FakeFs::new()),
                Arc::new(FakeClock::default()),
                PathBuf::from("/web.json"),
            );
            let (inbound_tx, inbound_rx) = std::sync::mpsc::channel();
            let (count_tx, count_rx) = std::sync::mpsc::channel();
            WebSurface {
                credentials: Arc::new(Mutex::new(store)),
                streams: crate::web::stream::TerminalStreams::new(1024),
                activity: crate::web::activity::ActivityStore::new(),
                pending_finishes: crate::web::activity::PendingFinishes::new(),
                inbound_tx,
                inbound_rx,
                count_tx,
                count_rx,
                handle: None,
                published: crate::web::server::HostState::default(),
            }
        }

        /// Move a tab's lifecycle signal, the way `poll_status_files` does when
        /// an agent's status hook writes to its status file.
        fn set_interpreted(project: &mut Project, status: InterpretedStatus) {
            project.state.tabs[0].interpreted = Some(status);
        }

        /// What `spawn_finish_count` does, minus the thread: ask `git`, post the
        /// answer back, and let the surface drain it.
        fn answer_with(
            web: &mut WebSurface,
            git: &FakeGit,
            clock: &FakeClock,
            now_ms: u64,
            requests: Vec<FinishCountRequest>,
        ) {
            for req in requests {
                let files = crate::web::activity::file_count(git, &req.worktree_abs);
                web.count_tx
                    .send(FinishCount {
                        request: req.request,
                        files,
                    })
                    .unwrap();
            }
            web.drain_finish_counts(clock, now_ms);
        }

        /// Drive one project from `working` to `idle`, returning the refreshes
        /// the finish edge asked for. The first pass only arms the edge memory —
        /// `take_status_transitions` never reports a first sighting.
        fn finish(
            web: &mut WebSurface,
            project: &mut Project,
            clock: &FakeClock,
        ) -> Vec<FinishCountRequest> {
            set_interpreted(project, InterpretedStatus::Working);
            assert!(
                record_web_transitions(web, project, clock, 1_000).is_empty(),
                "first sighting is not a transition"
            );
            set_interpreted(project, InterpretedStatus::Idle);
            record_web_transitions(web, project, clock, 1_100)
        }

        fn reasons(web: &WebSurface) -> Vec<String> {
            web.activity.events().map(|e| e.reason.clone()).collect()
        }

        /// The row on screen: the count is what git reports for *that* tab's
        /// worktree at the finish edge, not what some cache last happened to see.
        #[test]
        fn the_active_project_s_finished_row_carries_the_count_git_reports() {
            let pty = FakePty::new();
            let clock = FakeClock::default();
            let git = FakeGit::new();
            git.set_porcelain_at(
                "/repo0/worktrees/add-tests-api",
                (0..18)
                    .map(|i| format!(" M src/file-{i}.rs"))
                    .collect::<Vec<_>>(),
            );
            let mut web = web_surface();
            let mut active = project("proj0", "/repo0", "add-tests-api", &pty);

            let requests = finish(&mut web, &mut active, &clock);
            assert_eq!(requests.len(), 1, "the finish edge asked git once");
            assert_eq!(
                requests[0].worktree_abs,
                PathBuf::from("/repo0/worktrees/add-tests-api"),
                "the tab's own worktree, not the repository root"
            );
            assert!(
                web.activity.is_empty(),
                "nothing is published while the count is outstanding"
            );

            answer_with(&mut web, &git, &clock, 1_100, requests);
            assert_eq!(reasons(&web), vec!["finished, 18 files touched"]);
        }

        /// **The acceptance criterion.** `record_web_transitions` runs inside the
        /// loop's per-project pass, which visits every open project — so a
        /// session that finished in a project nobody is looking at gets the same
        /// literal count, from its own repo and its own worktree. The active
        /// project finishes in the same test with a *different* number, so a
        /// count leaking between projects fails here rather than passing by
        /// coincidence.
        #[test]
        fn a_background_project_s_finished_row_carries_the_count_too() {
            let pty = FakePty::new();
            let clock = FakeClock::default();
            let active_git = FakeGit::new();
            active_git.set_porcelain_at("/repo0/worktrees/on-screen", [" M a.rs", " M b.rs"]);
            let background_git = FakeGit::new();
            background_git.set_porcelain_at(
                "/repo1/worktrees/off-screen",
                (0..18)
                    .map(|i| format!(" M src/file-{i}.rs"))
                    .collect::<Vec<_>>(),
            );

            let mut web = web_surface();
            let mut active = project("proj0", "/repo0", "on-screen", &pty);
            let mut background = project("proj1", "/repo1", "off-screen", &pty);

            let active_requests = finish(&mut web, &mut active, &clock);
            let background_requests = finish(&mut web, &mut background, &clock);
            assert_eq!(background_requests.len(), 1);
            assert_eq!(
                background_requests[0].worktree_abs,
                PathBuf::from("/repo1/worktrees/off-screen"),
            );

            answer_with(&mut web, &active_git, &clock, 1_100, active_requests);
            answer_with(
                &mut web,
                &background_git,
                &clock,
                1_100,
                background_requests,
            );

            let rows: Vec<(String, String)> = web
                .activity
                .events()
                .map(|e| (e.project_name.clone(), e.reason.clone()))
                .collect();
            assert_eq!(
                rows,
                vec![
                    ("proj0".to_string(), "finished, 2 files touched".to_string()),
                    (
                        "proj1".to_string(),
                        "finished, 18 files touched".to_string()
                    ),
                ]
            );
        }

        /// **No new periodic git work.** The count is bought at the edge, so a
        /// project whose sessions are not moving — the normal state of every
        /// background project — asks git nothing at all, however many ticks go
        /// by. The `GIT_REFRESH_EVERY` cache is untouched and still refreshes
        /// `workspace.projects[active]` alone.
        #[test]
        fn a_project_nobody_is_looking_at_asks_git_nothing_per_tick() {
            let pty = FakePty::new();
            let clock = FakeClock::default();
            let git = FakeGit::new();
            git.set_porcelain_at("/repo1/worktrees/off-screen", [" M src/lib.rs"]);
            let mut web = web_surface();
            let mut background = project("proj1", "/repo1", "off-screen", &pty);
            set_interpreted(&mut background, InterpretedStatus::Working);

            for t in 0..(GIT_REFRESH_EVERY * 3) {
                let requests = record_web_transitions(&mut web, &mut background, &clock, 1_000 + t);
                assert!(
                    requests.is_empty(),
                    "a session that has not moved is not a finish edge"
                );
                answer_with(&mut web, &git, &clock, 1_000 + t, requests);
            }

            assert_eq!(
                git.porcelain_calls(),
                0,
                "the feed must not turn every open project into periodic git work"
            );
            assert!(web.activity.is_empty(), "and nothing happened to report");
        }

        /// Only the finish edge pays for a refresh. A session that stopped to
        /// ask a question, or one the user set by hand, is a row the host can
        /// already explain — git is never asked about either.
        #[test]
        fn only_the_finish_edge_asks_git_anything() {
            let pty = FakePty::new();
            let clock = FakeClock::default();
            let git = FakeGit::new();
            let mut web = web_surface();
            let mut p = project("proj0", "/repo0", "add-tests-api", &pty);

            set_interpreted(&mut p, InterpretedStatus::Working);
            assert!(record_web_transitions(&mut web, &mut p, &clock, 1_000).is_empty());

            set_interpreted(&mut p, InterpretedStatus::WaitingForInput);
            assert!(
                record_web_transitions(&mut web, &mut p, &clock, 1_100).is_empty(),
                "a question is not a finish"
            );

            p.state.tabs[0].meta.manual_status = Some(ManualStatus::Done.as_str().to_string());
            assert!(
                record_web_transitions(&mut web, &mut p, &clock, 1_200).is_empty(),
                "the user's own words outrank a file count"
            );

            assert_eq!(git.porcelain_calls(), 0);
            assert_eq!(
                reasons(&web),
                vec!["".to_string(), "set by hand on the desktop".to_string()],
                "both rows are recorded straight away; neither waits on git"
            );
        }

        /// **The honest-empty path.** Git could not answer — a locked index, a
        /// worktree that has been removed — so the row arrives without the
        /// clause, exactly as it did before this mechanism existed.
        #[test]
        fn a_finish_git_cannot_count_still_produces_its_row() {
            let pty = FakePty::new();
            let clock = FakeClock::default();
            let git = FakeGit::new();
            git.set_porcelain_error("fatal: unable to read index");
            let mut web = web_surface();
            let mut p = project("proj0", "/repo0", "add-tests-api", &pty);

            let requests = finish(&mut web, &mut p, &clock);
            assert_eq!(requests.len(), 1);
            answer_with(&mut web, &git, &clock, 1_100, requests);

            let event = web.activity.events().next().expect("the row still lands");
            assert_eq!(event.reason, "");
            assert_eq!(event.tier, crate::web::protocol::ActivityTier::Finished);
            assert_eq!(event.to, InterpretedStatus::Idle);
        }

        /// A worker that never comes back — killed, or blocked on a repo lock
        /// nobody releases — costs the clause, never the row. The deadline is
        /// enforced by the same per-tick drain the answers arrive on.
        #[test]
        fn a_finish_nobody_ever_answers_for_lands_after_the_deadline() {
            let pty = FakePty::new();
            let clock = FakeClock::default();
            let mut web = web_surface();
            let mut p = project("proj0", "/repo0", "add-tests-api", &pty);

            let requests = finish(&mut web, &mut p, &clock);
            assert_eq!(requests.len(), 1);
            // The answer is simply dropped, as a panicking worker's would be.
            drop(requests);

            web.drain_finish_counts(&clock, 1_100);
            assert!(web.activity.is_empty(), "still waiting, still unpublished");

            let deadline = 1_100 + crate::web::activity::FINISH_COUNT_DEADLINE_MS as u64;
            web.drain_finish_counts(&clock, deadline);
            assert_eq!(reasons(&web), vec!["".to_string()]);
        }
    }
}
