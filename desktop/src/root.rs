//! The window's root view: the shell once a project is open, or the empty
//! state when the app was launched with nowhere to work.
//!
//! ## Launching without a repository
//!
//! The TUI exits with "not inside a Git repository" when started elsewhere.
//! A desktop app is started from Finder, the Start menu or a launcher, where
//! the working directory is `/` or the home folder, so exiting would be a
//! window that never appears. Instead the app opens this root in its
//! *launcher* state: no `AppHost` exists yet (a workspace always has an active
//! project, and that invariant is not bent for an empty one), just the
//! projects remembered from the last session and an "Open project…" button.
//! Choosing a folder builds the host exactly as a launch from that folder
//! would (`AppHost::open` on it, which also reopens the remembered projects),
//! and the root swaps the launcher for the shell in the same window.
//!
//! An `--isolated` run never lands here: it is defined by the directory it
//! was started in, so a non-repository directory is the TUI's error.
//!
//! ## Who owns what
//!
//! [`AppRoot`] owns the host model and the shell once running, and is
//! published as the [`RootHandle`] global so the app-level menu handlers
//! (`menus`) and the quit hook can reach the running host without a window.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use flightdeck::host::{AppHost, HostEvent};
use flightdeck_desktop::keys::KeymapAction;
use gpui::{
    div, px, App, AppContext, Context, Entity, FocusHandle, FontWeight, Global, InteractiveElement,
    IntoElement, ParentElement, PathPromptOptions, Render, StatefulInteractiveElement, Styled,
    Task, Window,
};
use gpui_component::{h_flex, v_flex};

use crate::commands::keymap;
use crate::host::HostModel;
use crate::shell::FlightDeckWindow;
use crate::theme::Palette;
use crate::views::icons;

/// How often the launcher looks for a shutdown signal (the running app's
/// host does it every turn).
const SIGNAL_POLL: Duration = Duration::from_millis(250);

/// Opens the workspace for a chosen folder and brings its services up: the
/// real one is `app::open_workspace`; tests hand in one over fakes.
pub type Opener = Rc<dyn Fn(&Path) -> Result<AppHost<'static>, String>>;

/// The root view of the app's window, published as a global.
pub struct RootHandle(pub Entity<AppRoot>);

impl Global for RootHandle {}

/// What the root shows.
struct Running {
    model: Entity<HostModel>,
    shell: Entity<FlightDeckWindow>,
}

pub struct AppRoot {
    opener: Opener,
    /// Remembered projects, offered while no project is open.
    recent: Vec<PathBuf>,
    /// Why the last folder could not be opened, shown under the button.
    error: Option<String>,
    running: Option<Running>,
    shutdown: Option<Arc<AtomicBool>>,
    /// The app's own mode: a started host is driven by its own timer and
    /// reports the needs-you count to the OS. Tests take turns themselves and
    /// must not touch the developer's Dock.
    live: bool,
    focus: FocusHandle,
    _signal_poll: Option<Task<()>>,
}

impl AppRoot {
    /// A root over `initial` (the launch project's host) or, with none, the
    /// launcher. `opener` builds a host for a folder chosen later. `live` is
    /// the app's mode (see the field).
    pub fn new(
        initial: Option<AppHost<'static>>,
        opener: Opener,
        recent: Vec<PathBuf>,
        shutdown: Option<Arc<AtomicBool>>,
        live: bool,
        cx: &mut Context<Self>,
    ) -> AppRoot {
        let mut root = AppRoot {
            opener,
            recent,
            error: None,
            running: None,
            shutdown,
            live,
            focus: cx.focus_handle(),
            _signal_poll: None,
        };
        if let Some(host) = initial {
            root.start(host, cx);
        } else {
            root.watch_for_signals(cx);
        }
        root
    }

    /// The running host's model, once a project is open.
    pub fn model(&self) -> Option<&Entity<HostModel>> {
        self.running.as_ref().map(|r| &r.model)
    }

    /// Why the last folder could not be opened.
    #[cfg(test)]
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Whether the launcher (no project open) is showing.
    #[cfg(test)]
    pub fn is_launcher(&self) -> bool {
        self.running.is_none()
    }

    /// Open the folder a picker (or a recent-projects row) chose. Launcher:
    /// build the host for it and show the shell; running: the host's own
    /// `OpenProject` (validated, or refused, as the TUI's folder browser).
    pub fn open_folder(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if let Some(running) = &self.running {
            running.model.update(cx, |model, cx| {
                model.dispatch(HostEvent::OpenProject(path), cx)
            });
            return;
        }
        match (self.opener)(&path) {
            Ok(host) => {
                self.error = None;
                self.start(host, cx);
            }
            Err(e) => {
                self.error = Some(e);
                cx.notify();
            }
        }
    }

    /// Hand `event` to the running host; nothing to do in the launcher.
    pub fn dispatch(&mut self, event: HostEvent, cx: &mut Context<Self>) {
        if let Some(running) = &self.running {
            running
                .model
                .update(cx, |model, cx| model.dispatch(event, cx));
        }
    }

    /// Bring a started host on screen.
    fn start(&mut self, host: AppHost<'static>, cx: &mut Context<Self>) {
        let model = cx.new(|cx| {
            let mut model = HostModel::new(host);
            if let Some(flag) = &self.shutdown {
                model.set_shutdown_flag(flag.clone());
            }
            if self.live {
                model.report_attention();
                model.start_ticking(cx);
            }
            model
        });
        let shell = cx.new(|cx| FlightDeckWindow::new(model.clone(), cx));
        self.running = Some(Running { model, shell });
        self._signal_poll = None;
        cx.notify();
    }

    /// With no host to poll the flag, the launcher does: SIGTERM must still
    /// end the app.
    fn watch_for_signals(&mut self, cx: &mut Context<Self>) {
        let Some(flag) = self.shutdown.clone() else {
            return;
        };
        self._signal_poll = Some(cx.spawn(async move |_, cx| loop {
            cx.background_executor().timer(SIGNAL_POLL).await;
            if flag.load(Ordering::Relaxed) {
                cx.update(|cx| cx.quit());
                break;
            }
        }));
    }

    /// A table chord or menu item reached the root: only Quit is meaningful
    /// here (the shell answers the rest first, and lets Quit through while an
    /// overlay is up, where Cmd-Q must still work).
    fn on_keymap_action(&mut self, action: &KeymapAction, _: &mut Window, cx: &mut Context<Self>) {
        if action.entry(keymap()).is_some_and(|e| e.id == "Quit") {
            cx.quit();
        }
    }
}

/// Choose a folder with the OS's own dialog and open it (see
/// [`AppRoot::open_folder`]). A cancelled dialog does nothing.
pub fn pick_folder(cx: &mut App) {
    let chosen = cx.prompt_for_paths(PathPromptOptions {
        files: false,
        directories: true,
        multiple: false,
        prompt: Some("Open Project".into()),
    });
    cx.spawn(async move |cx| {
        let Ok(Ok(Some(paths))) = chosen.await else {
            return;
        };
        let Some(path) = paths.into_iter().next() else {
            return;
        };
        cx.update(|cx| open_folder(path, cx));
    })
    .detach();
}

/// [`AppRoot::open_folder`] on the app's root, if there is one.
pub fn open_folder(path: PathBuf, cx: &mut App) {
    if let Some(root) = cx.try_global::<RootHandle>().map(|h| h.0.clone()) {
        root.update(cx, |root, cx| root.open_folder(path, cx));
    }
}

/// [`AppRoot::dispatch`] on the app's root, if there is one.
pub fn dispatch(event: HostEvent, cx: &mut App) {
    if let Some(root) = cx.try_global::<RootHandle>().map(|h| h.0.clone()) {
        root.update(cx, |root, cx| root.dispatch(event, cx));
    }
}

/// The running host's model, if a project is open: what the quit hook tears
/// down.
pub fn running_model(cx: &App) -> Option<Entity<HostModel>> {
    cx.try_global::<RootHandle>()?.0.read(cx).model().cloned()
}

impl Render for AppRoot {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let root = div()
            .id("app-root")
            .size_full()
            .on_action(cx.listener(Self::on_keymap_action));
        if let Some(running) = &self.running {
            return root.child(running.shell.clone());
        }
        // Actions only reach a view on the focus path.
        if !self.focus.is_focused(window) {
            window.focus(&self.focus, cx);
        }
        root.track_focus(&self.focus).child(launcher(self, cx))
    }
}

/// The empty state: what FlightDeck is, "Open project…", and the remembered
/// projects.
fn launcher(root: &AppRoot, cx: &mut Context<AppRoot>) -> impl IntoElement {
    let p = *Palette::global(cx);
    let open = div()
        .id("launcher-open")
        .debug_selector(|| "launcher-open".into())
        .flex_none()
        .h(px(34.))
        .px_4()
        .flex()
        .items_center()
        .gap_2()
        .rounded(px(7.))
        .bg(p.button_primary_bg.hsla())
        .text_color(p.button_primary_ink.hsla())
        .text_size(px(13.))
        .font_weight(FontWeight::MEDIUM)
        .cursor_pointer()
        .child("Open project…")
        .on_click(|_, _, cx| pick_folder(cx));
    // FlightDeck Desktop as a remote control for another machine's FlightDeck
    // (`crate::remote::connect`), beside Open project.
    let connect = div()
        .id("launcher-connect")
        .debug_selector(|| "launcher-connect".into())
        .flex_none()
        .h(px(34.))
        .px_4()
        .flex()
        .items_center()
        .gap_2()
        .rounded(px(7.))
        .border_1()
        .border_color(p.border_strong.hsla())
        .text_size(px(13.))
        .font_weight(FontWeight::MEDIUM)
        .cursor_pointer()
        .child("Connect to remote…")
        .on_click(|_, _, cx| crate::remote::connect::open_connect_window(None, cx));

    let recent = (!root.recent.is_empty()).then(|| {
        v_flex()
            .w_full()
            .gap(px(2.))
            .child(
                div()
                    .px_3()
                    .pb_1()
                    .text_size(px(11.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(p.muted.hsla())
                    .child("RECENT PROJECTS"),
            )
            .children(
                root.recent
                    .iter()
                    .enumerate()
                    .map(|(i, path)| recent_row(i, path, &p, cx)),
            )
    });

    let error = root.error.clone().map(|message| {
        div()
            .text_size(px(12.))
            .text_color(p.danger.hsla())
            .child(message)
    });

    v_flex()
        .id("launcher")
        .debug_selector(|| "launcher".into())
        .size_full()
        .bg(p.surface_window.hsla())
        .text_color(p.ink.hsla())
        .font_family(crate::fonts::UI_FAMILY)
        // The macOS titlebar is transparent and ours to draw and drag; the
        // launcher has no bar of its own, so keep the traffic lights clear
        // and let the top strip move the window.
        .child(drag_strip())
        .child(
            v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .gap_4()
                .child(icons::icon(crate::assets::icon::PROJECTS, px(28.), p.muted))
                .child(
                    div()
                        .text_size(px(20.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("Open a project to get started"),
                )
                .child(
                    div()
                        .max_w(px(380.))
                        .text_center()
                        .text_size(px(13.))
                        .text_color(p.ink_2.hsla())
                        .child(
                            "FlightDeck runs your coding agents in their own git worktrees. \
                             Pick a folder inside a git repository.",
                        ),
                )
                .child(h_flex().gap_3().child(open).child(connect))
                .children(error)
                .children(recent.map(|list| div().mt_4().w(px(420.)).child(list))),
        )
}

/// One remembered project: its folder name and full path; a click opens it.
fn recent_row(
    index: usize,
    path: &Path,
    p: &Palette,
    cx: &mut Context<AppRoot>,
) -> impl IntoElement {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let target = path.to_path_buf();
    h_flex()
        .id(("recent-project", index))
        .debug_selector(|| format!("recent-{index}"))
        .w_full()
        .h(px(40.))
        .px_3()
        .gap_3()
        .rounded(px(7.))
        .cursor_pointer()
        .hover(|s| s.bg(p.surface_raised.hsla()))
        .child(icons::icon(crate::assets::icon::PROJECTS, px(14.), p.muted))
        .child(
            // A fixed column, so the paths line up down the list.
            div()
                .flex_none()
                .w(px(120.))
                .truncate()
                .text_size(px(13.))
                .font_weight(FontWeight::MEDIUM)
                .child(name),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_size(px(11.5))
                .font_family(crate::fonts::MONO_FAMILY)
                .text_color(p.muted.hsla())
                .child(path.display().to_string()),
        )
        .on_click(cx.listener(move |root, _, _, cx| root.open_folder(target.clone(), cx)))
}

/// The 44px strip above the launcher's content: room for the macOS traffic
/// lights, and where a press-and-move drags the window (Windows and Linux
/// have a native title bar above the whole window instead).
fn drag_strip() -> impl IntoElement {
    let strip = div().flex_none().w_full().h(px(44.));
    #[cfg(target_os = "macos")]
    let strip = strip.on_mouse_down(gpui::MouseButton::Left, |event, window, _| {
        if event.click_count >= 2 {
            window.titlebar_double_click();
        } else {
            window.start_window_move();
        }
    });
    strip
}
