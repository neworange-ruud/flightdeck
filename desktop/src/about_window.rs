//! Opening the About window (the app menu's "About FlightDeck"). The view is
//! [`flightdeck_desktop::overlays::about::AboutWindow`]; this is the window
//! around it, built from the app's own window options, and kept to one: a
//! second "About FlightDeck" brings the open window forward.

use flightdeck_desktop::overlays::about::{AboutWindow, WINDOW_TITLE};
use gpui::{
    px, size, AnyWindowHandle, App, AppContext, Bounds, Global, WindowBounds, WindowOptions,
};

/// The About window, while it is open.
struct OpenAbout(AnyWindowHandle);

impl Global for OpenAbout {}

/// Open the About window, or bring it forward if it is already open.
pub fn open(cx: &mut App) {
    if let Some(OpenAbout(handle)) = cx.try_global::<OpenAbout>() {
        // Fails once the window is closed; then a new one opens below.
        if handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
        {
            return;
        }
    }
    let opened = cx.open_window(options(cx), |window, cx| {
        let view = cx.new(|cx| AboutWindow::new(window, cx));
        cx.new(|cx| gpui_component::Root::new(view, window, cx))
    });
    match opened {
        Ok(handle) => cx.set_global(OpenAbout(handle.into())),
        Err(e) => eprintln!("flightdeck-desktop: could not open the About window: {e}"),
    }
}

/// A small, fixed-size window with the platform's own title bar.
fn options(cx: &App) -> WindowOptions {
    let mut options = crate::app::window_options(cx);
    let window_size = size(px(380.), px(220.));
    options.window_bounds = Some(WindowBounds::Windowed(Bounds::centered(
        None,
        window_size,
        cx,
    )));
    options.window_min_size = Some(window_size);
    options.is_resizable = false;
    options.is_minimizable = false;
    // The main window draws its own title bar; this one keeps the system's,
    // so its title and close button need nothing from us.
    options.app_owns_titlebar_drag = false;
    if let Some(titlebar) = options.titlebar.as_mut() {
        titlebar.title = Some(WINDOW_TITLE.into());
        titlebar.appears_transparent = false;
        titlebar.traffic_light_position = None;
    }
    options
}
