//! Application start-up, the main window, and teardown.
//!
//! `flightdeck-desktop [--isolated|-I]` opens exactly what the TUI opens: the
//! project for the current working directory (which must be inside a git
//! repository) plus every project remembered from the last session, through
//! the same `AppHost::open`. Then it seeds the PTY size, resumes the launch
//! project's agents, starts the background services and drives the host from
//! GPUI's executor ([`crate::host`]).
//!
//! Quitting — closing the window, Cmd-Q on macOS, Ctrl-q (the table's Quit),
//! a confirmed quit dialog, SIGTERM/SIGINT/SIGHUP — all end in `cx.quit()`,
//! and GPUI's quit hook runs the TUI's teardown order before the process
//! exits.

use flightdeck::host::AppHost;
#[cfg(target_os = "macos")]
use gpui::KeyBinding;
use gpui::{
    actions, px, size, App, AppContext, Bounds, TitlebarOptions, WindowBounds, WindowOptions,
};
use gpui_component::Root;

use crate::host::{HostModel, RealServices};
use crate::shell::{FlightDeckWindow, NOMINAL_PTY_SIZE};
use crate::terminal::spike::{split_snapshot, SnapshotOptions};
use crate::theme;

actions!(flightdeck, [Quit]);

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
    // Opened before GPUI starts, so "not a git repository" is a plain error on
    // stderr and a non-zero exit, as it is for the TUI.
    let mut host = match AppHost::open(services.env(), services.notifier(), &cwd, launch.isolated) {
        Ok(host) => host,
        Err(e) => {
            eprintln!("flightdeck error: {e}");
            std::process::exit(1);
        }
    };
    // Agents spawn at the right width: seed the size first (the terminal
    // element's first frame then measures the real one), then resume.
    host.seed_pty_sizes(|_| NOMINAL_PTY_SIZE);
    if let Err(e) = host.resume_launch_project() {
        // An isolated run's one session failing is fatal in the TUI too —
        // after the full teardown.
        eprintln!("flightdeck error: {e}");
        host.stop_services();
        let _ = host.persist();
        host.terminate_sessions();
        host.cleanup_isolated();
        std::process::exit(1);
    }
    host.start();
    let shutdown = flightdeck::signals::install_shutdown_flag();
    let snapshot = launch.snapshot;

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

            let model = cx.new(|cx| {
                let mut model = HostModel::new(host);
                model.set_shutdown_flag(shutdown);
                model.start_ticking(cx);
                model
            });

            // Teardown, once, whatever ended the app.
            let for_quit = model.clone();
            cx.on_app_quit(move |cx| {
                for_quit.update(cx, |model, _| model.teardown());
                async {}
            })
            .detach();

            cx.on_action(|_: &Quit, cx| cx.quit());
            // The platform's quit chord. On macOS there is no app menu yet to
            // carry Cmd-Q, so bind it directly. Ctrl-q is the table's own Quit
            // on every OS (`HostEvent::Quit`), so it needs no binding here.
            #[cfg(target_os = "macos")]
            cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);

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
                let view = cx.new(|cx| FlightDeckWindow::new(model.clone(), cx));
                // gpui-component's `Root` hosts its overlays (menus, tooltips)
                // above our view; every window needs one at its top.
                cx.new(|cx| Root::new(view, window, cx))
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
    fn an_unknown_argument_is_refused() {
        assert!(parse_args(args(&["--frobnicate"])).is_err());
    }
}
