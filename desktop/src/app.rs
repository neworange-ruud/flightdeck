//! Application start-up, the main window, and teardown.
//!
//! `flightdeck-desktop [--isolated|-I]` opens exactly what the TUI opens: the
//! project for the current working directory (inside a repository) plus every
//! project remembered from the last session, through the same `AppHost::open`.
//! Then it seeds the PTY size, resumes the launch project's agents, starts the
//! background services and drives the host from GPUI's executor
//! ([`crate::host`]).
//!
//! Started anywhere else (Finder, the Start menu, a launcher: no repository in
//! the working directory) the window opens on an empty state offering "Open
//! project…" and the remembered projects ([`crate::root`]); an `--isolated`
//! run has no such state and exits with the TUI's error.
//!
//! Quitting — closing the window, Cmd-Q on macOS, Ctrl-q (the table's Quit),
//! a confirmed quit dialog, SIGTERM/SIGINT/SIGHUP — all end in `cx.quit()`,
//! and GPUI's quit hook runs the TUI's teardown order before the process
//! exits.

use std::path::Path;
use std::rc::Rc;

use flightdeck::config::load::{global_config_path, load_config};
use flightdeck::contracts::real::RealFs;
use flightdeck::contracts::FileSystem;
use flightdeck::host::AppHost;
use gpui::{px, size, App, AppContext, Bounds, TitlebarOptions, WindowBounds, WindowOptions};
use gpui_component::Root;

use crate::host::RealServices;
use crate::root::{AppRoot, Opener, RootHandle};
use crate::shell::NOMINAL_PTY_SIZE;
use crate::terminal::spike::{split_snapshot, SnapshotOptions};
use crate::theme;

// The terminal spike's own quit (`terminal::spike`); the app quits through the
// table's `Quit` entry (`crate::menus`, `crate::root`).
gpui::actions!(flightdeck, [Quit]);

/// Where AppKit puts the traffic lights, relative to the window's top-left.
/// `y` centres the 12px buttons in the 44px titlebar (AppKit measures to the
/// button frame, which is taller than the visible circle).
#[cfg(target_os = "macos")]
const TRAFFIC_LIGHT_INSET: gpui::Point<gpui::Pixels> = gpui::point(px(16.), px(15.));

/// What the command line asked for.
struct Launch {
    isolated: bool,
    /// `--spike-snapshot PATH` & co. (a `spike-snapshot` build only): render
    /// the window offscreen after it opens, write it out, quit.
    snapshot: SnapshotOptions,
}

fn parse_args(args: Vec<String>) -> Result<Launch, String> {
    let isolated = flightdeck::parse_isolated(&args).map_err(|e| e.to_string())?;
    let rest: Vec<String> = args
        .into_iter()
        .filter(|a| a != "--isolated" && a != "-I")
        .collect();
    let (snapshot, rest) = split_snapshot(rest)?;
    if let Some(unknown) = rest.first() {
        return Err(format!("unknown argument {unknown:?}"));
    }
    #[cfg(not(feature = "spike-snapshot"))]
    if snapshot.is_requested() {
        return Err("--spike-snapshot/--spike-keys need a build with \
                    `--features spike-snapshot`"
            .to_string());
    }
    Ok(Launch { isolated, snapshot })
}

/// Open the workspace for a launch from `folder` and bring its services up:
/// `AppHost::open` (the launch project plus the projects remembered from the
/// last session), the PTY seed, resuming the launch project's agents, and the
/// background services. The one path for the launch directory and for a folder
/// chosen later in the launcher.
///
/// An isolated run's one session failing to start is fatal in the TUI too,
/// after the full teardown; the error is returned for the caller to report.
fn open_workspace(
    services: &'static RealServices,
    folder: &Path,
    isolated: bool,
) -> Result<AppHost<'static>, String> {
    let mut host = AppHost::open(services.env(), services.notifier(), folder, isolated)
        .map_err(|e| e.to_string())?;
    // Agents spawn at the right width: seed the size first (the terminal
    // element's first frame then measures the real one), then resume.
    host.seed_pty_sizes(|_| NOMINAL_PTY_SIZE);
    if let Err(e) = host.resume_launch_project() {
        host.stop_services();
        let _ = host.persist();
        host.terminate_sessions();
        host.cleanup_isolated();
        return Err(e.to_string());
    }
    host.start();
    Ok(host)
}

/// `[ui] use_f2_to_leave_terminal_focus` from the global config, for a launch
/// with no project to read the effective (layered) value from. Missing or
/// unreadable config is the default, off.
fn global_use_f2() -> bool {
    global_config_path().is_some_and(|path| use_f2_in(&RealFs, &path))
}

/// The setting in the config file at `path`; a missing or unreadable file is
/// the default, off.
fn use_f2_in(fs: &dyn FileSystem, path: &Path) -> bool {
    load_config(fs, path).is_ok_and(|config| config.ui.use_f2_to_leave_terminal_focus)
}

/// Start GPUI, open the workspace and the main window. Blocks until quit.
pub fn run(args: Vec<String>) {
    let launch = match parse_args(args) {
        Ok(launch) => launch,
        Err(e) => {
            eprintln!("flightdeck-desktop: {e}");
            std::process::exit(2);
        }
    };
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(e) => {
            eprintln!("flightdeck-desktop: could not determine current directory: {e}");
            std::process::exit(1);
        }
    };
    let services = RealServices::leak();
    // Opened before GPUI starts. A launch outside any repository (Finder, the
    // Start menu) opens the window on the empty state instead of exiting; an
    // isolated run is defined by its directory, so there it is the TUI's
    // error and a non-zero exit.
    let initial = match open_workspace(services, &cwd, launch.isolated) {
        Ok(host) => Some(host),
        Err(e) if launch.isolated => {
            eprintln!("flightdeck error: {e}");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("flightdeck-desktop: {e}; opening the project picker");
            None
        }
    };
    let recent = if initial.is_none() {
        flightdeck::host::recent_projects(&services.env())
    } else {
        Vec::new()
    };
    // The leave-focus key is read once: the bindings below are registered
    // once, so a change (or a project overriding it) applies on next launch.
    let use_f2 = match &initial {
        Some(host) => host.active_state().config.ui.use_f2_to_leave_terminal_focus,
        None => global_use_f2(),
    };
    crate::commands::set_use_f2(use_f2);
    let shutdown = flightdeck::signals::install_shutdown_flag();
    let snapshot = launch.snapshot;
    let isolated = launch.isolated;
    let opener: Opener = Rc::new(move |folder| open_workspace(services, folder, false));

    gpui_platform::application()
        .with_assets(crate::assets::Assets)
        .run(move |cx: &mut App| {
            // gpui-component first: `theme::init` edits the theme it installs,
            // and `Root` (below) needs its globals.
            gpui_component::init(cx);
            theme::init(cx);
            if let Err(e) = crate::fonts::register(cx) {
                eprintln!("flightdeck-desktop: could not register the bundled fonts: {e}");
            }
            // Every FlightDeck chord, generated from the keymap table.
            flightdeck_desktop::keys::register(cx, crate::commands::keymap());
            // Under an open overlay every Global chord is disabled (the modal
            // swallows keys, as in the TUI).
            flightdeck_desktop::overlays::register(cx, crate::commands::keymap());
            // The macOS menu bar (generated from the same table), the
            // platform-convention shortcuts (Cmd-Q, Cmd-, …) and the handlers
            // for the desktop's own menu items.
            crate::menus::install(cx, crate::commands::keymap(), isolated);

            // Teardown, once, whatever ended the app: the running host's, if
            // a project was ever opened.
            cx.on_app_quit(|cx| {
                if let Some(model) = crate::root::running_model(cx) {
                    model.update(cx, |model, _| model.teardown());
                }
                async {}
            })
            .detach();

            // One window, one process: closing it ends the app on every OS
            // (macOS would otherwise keep a windowless process in the Dock).
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();

            let options = window_options(cx);
            let opened = cx.open_window(options, |window, cx| {
                let root =
                    cx.new(|cx| AppRoot::new(initial, opener, recent, Some(shutdown), true, cx));
                cx.set_global(RootHandle(root.clone()));
                // gpui-component's `Root` hosts its overlays (menus, tooltips)
                // above our view; every window needs one at its top.
                cx.new(|cx| Root::new(root, window, cx))
            });
            let handle = match opened {
                Ok(handle) => handle,
                Err(e) => {
                    eprintln!("flightdeck-desktop: could not open the window: {e}");
                    cx.quit();
                    return;
                }
            };
            cx.activate(true);

            #[cfg(feature = "spike-snapshot")]
            if snapshot.is_requested() {
                crate::terminal::spike::snapshot::schedule(handle.into(), snapshot, cx);
            }
            #[cfg(not(feature = "spike-snapshot"))]
            let _ = (handle, snapshot);
        });
}

/// Window chrome per OS. macOS: transparent titlebar, content drawn under it,
/// traffic lights inset into our 44px bar, and the app (not AppKit) owning the
/// drag so clicks in the bar are not delayed. Windows/Linux: the native,
/// server-side decorations, with our titlebar row as a toolbar below them.
pub fn window_options(cx: &App) -> WindowOptions {
    let bounds = Bounds::centered(None, size(px(1280.), px(800.)), cx);

    #[cfg(target_os = "macos")]
    let titlebar = TitlebarOptions {
        title: Some("FlightDeck".into()),
        appears_transparent: true,
        traffic_light_position: Some(TRAFFIC_LIGHT_INSET),
    };
    #[cfg(not(target_os = "macos"))]
    let titlebar = TitlebarOptions {
        title: Some("FlightDeck".into()),
        appears_transparent: false,
        traffic_light_position: None,
    };

    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(titlebar),
        app_owns_titlebar_drag: cfg!(target_os = "macos"),
        window_min_size: Some(size(px(900.), px(560.))),
        // X11/Wayland: ask for server-side decorations (native title bar),
        // matching Windows. The compositor may still refuse; see NOTES-M0.md.
        window_decorations: Some(gpui::WindowDecorations::Server),
        app_id: Some("flightdeck-desktop".into()),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn isolated_passes_through_in_both_spellings() {
        assert!(parse_args(args(&["--isolated"])).unwrap().isolated);
        assert!(parse_args(args(&["-I"])).unwrap().isolated);
        assert!(!parse_args(args(&[])).unwrap().isolated);
    }

    #[test]
    fn the_f2_setting_is_read_from_the_config_file() {
        use flightdeck::testing::FakeFs;
        let fs = FakeFs::new();
        let path = Path::new("/home/u/.flightdeck/config.toml");
        assert!(!use_f2_in(&fs, path), "no file: the default");

        let config = |flag: bool| {
            let mut config = flightdeck::config::schema::default_config("proj", "main");
            config.ui.use_f2_to_leave_terminal_focus = flag;
            flightdeck::config::load::serialize_config(&config).unwrap()
        };
        fs.write(path, &config(true)).unwrap();
        assert!(use_f2_in(&fs, path));
        fs.write(path, &config(false)).unwrap();
        assert!(!use_f2_in(&fs, path));
        fs.write(path, "not toml [[[").unwrap();
        assert!(!use_f2_in(&fs, path), "unreadable: the default");
    }

    #[test]
    fn an_unknown_argument_is_refused() {
        assert!(parse_args(args(&["--frobnicate"])).is_err());
    }
}
