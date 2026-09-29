//! Application start-up, the main window, and its root view.
//!
//! Layout (design brief A1), top to bottom:
//!
//! ```text
//! +--------------------------------------------------------------+
//! | titlebar (44px)                                              |
//! +--------------+-----------------------------------------------+
//! | sidebar      | main area (git strip + terminal, later)       |
//! | (272px)      |                                               |
//! +--------------+-----------------------------------------------+
//! | status bar (30px)                                            |
//! +--------------------------------------------------------------+
//! ```

use gpui::{
    actions, div, px, size, App, AppContext, Bounds, Context, Entity, IntoElement, KeyBinding,
    ParentElement, Render, Styled, TitlebarOptions, Window, WindowBounds, WindowOptions,
};
use gpui_component::{h_flex, v_flex, Root};

use crate::theme::{self, Palette};
use crate::views::{sidebar::Sidebar, status_bar::StatusBar, titlebar::TitleBar};

actions!(flightdeck, [Quit]);

/// Where AppKit puts the traffic lights, relative to the window's top-left.
/// `y` centres the 12px buttons in the 44px titlebar (AppKit measures to the
/// button frame, which is taller than the visible circle).
#[cfg(target_os = "macos")]
const TRAFFIC_LIGHT_INSET: gpui::Point<gpui::Pixels> = gpui::point(px(16.), px(15.));

/// Start GPUI, install themes, open the main window. Blocks until quit.
pub fn run() {
    gpui_platform::application().run(|cx: &mut App| {
        // gpui-component first: `theme::init` edits the theme it installs,
        // and `Root` (below) needs its globals.
        gpui_component::init(cx);
        theme::init(cx);

        cx.on_action(|_: &Quit, cx| cx.quit());
        // The platform's quit chord. On macOS there is no app menu yet to
        // carry it, so bind it directly; elsewhere closing the window quits.
        #[cfg(target_os = "macos")]
        cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
        #[cfg(not(target_os = "macos"))]
        cx.bind_keys([KeyBinding::new("ctrl-q", Quit, None)]);

        // One window, one process: closing it ends the app on every OS
        // (macOS would otherwise keep a windowless process in the Dock).
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let options = window_options(cx);
        cx.open_window(options, |window, cx| {
            let view = cx.new(FlightDeckWindow::new);
            // gpui-component's `Root` hosts its overlays (dialogs, sheets,
            // notifications) above our view; every window needs one at its top.
            cx.new(|cx| Root::new(view, window, cx))
        })
        .expect("failed to open the FlightDeck window");

        cx.activate(true);
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

/// The main window's root view. Owns the stateful children; stateless regions
/// (sidebar, status bar) are rebuilt each frame.
pub struct FlightDeckWindow {
    titlebar: Entity<TitleBar>,
}

impl FlightDeckWindow {
    fn new(cx: &mut Context<Self>) -> Self {
        Self {
            titlebar: cx.new(|_| TitleBar::default()),
        }
    }
}

impl Render for FlightDeckWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::global(cx);

        v_flex()
            .size_full()
            .bg(p.surface_window.hsla())
            .text_color(p.ink.hsla())
            .child(self.titlebar.clone())
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .items_start()
                    .child(Sidebar)
                    .child(main_area(p)),
            )
            .child(StatusBar)
    }
}

/// Placeholder for the git strip + terminal: the terminal well and an empty
/// state line, so the main surface colour can be judged next to the sidebar.
fn main_area(p: &Palette) -> impl IntoElement {
    v_flex()
        .flex_1()
        .h_full()
        .bg(p.surface_terminal.hsla())
        .items_center()
        .justify_center()
        .gap_1()
        .child(
            div()
                .text_sm()
                .text_color(p.ink_2.hsla())
                .child("No agent selected"),
        )
        .child(
            div()
                .text_xs()
                .text_color(p.faint.hsla())
                .child("Its terminal will appear here."),
        )
}
