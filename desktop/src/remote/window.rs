//! A remote window's root view (`specs/DESKTOP_REMOTE_CONTROL_PLAN.md` §2.3,
//! §2.4): the Projects view's layout, drawn from a [`RemoteModel`].
//!
//! ```text
//! +--------------------------------------------------------------+
//! | titlebar: host · address · link state · project tabs · Command|
//! +--------------+-----------------------------------------------+
//! | sidebar      | git strip                                     |
//! | (shared      +-----------------------------------------------+
//! |  view)       | terminal (the mirrored one this window shows) |
//! +--------------+-----------------------------------------------+
//! | status bar: mode · hints · notice · seat · input lock         |
//! +--------------------------------------------------------------+
//!   + the overlay layer (palette, the host's shared dialog, help)
//! ```
//!
//! The sidebar and git strip are the local window's own elements
//! ([`crate::views`]), handed the core's view structs built from the mirror
//! and a [`Surface::Remote`] for their clicks. Keys follow the local window's
//! rule: the root carries the `"Global"` context and performs table chords —
//! through [`crate::commands::perform_remote_entry`] — the sidebar `"App"`,
//! the terminal `"Terminal"`, and focus follows this window's own input mode.
//!
//! Mission control is the local window's own view
//! ([`crate::views::mission::MissionControl::for_remote`]), switched to with
//! Alt-m or the titlebar's Projects | Mission control switch, drawn in place
//! of the sidebar and main area. Its selection, main view and scope are this
//! window's (R2), for the window's lifetime; chords and inline prompt answers
//! go to the host as every other control's do. Split view has no remote
//! counterpart: it arranges the host's own screen.
//!
//! The first tab, before the project tabs, is the SSH shell
//! ([`super::shell`]): a terminal this window runs itself, drawn full width
//! in place of the sidebar, git strip and mirrored terminal while it is on
//! screen. On a worker it is the worker's own shell instead, mirrored like
//! the host's terminals and drawn the same way; a worker also gets its
//! project picker ([`super::projects`]) in place of the body while it is
//! open, a version-skew banner under the titlebar when its FlightDeck is
//! not this app's, and Reconnect when another window took it over.
//!
//! A worker that takes uploads (protocol v8) also gets **Transfer files…**
//! in the titlebar, files dropped anywhere on the window (a highlight while
//! they hover), and the Transfer Files panel ([`super::transfer`]) over
//! everything while it is open — hidden while its Upload tab's Choose… has
//! the picker up. A worker at v9 gives downloads too: the panel's Download
//! tab ([`super::download`]).

use std::rc::Rc;

use flightdeck::app::modes::InputMode;
use flightdeck::contracts::PtyBackend;
use flightdeck::terminal::pty::PortablePtyBackend;
use flightdeck::web::client::views;
use flightdeck::web::client::LinkEnd;
use flightdeck::web::protocol::{Seat, SeatRequest};
use flightdeck_desktop::keys::{app_key_down, KeymapAction};
use flightdeck_desktop::overlays::{Emit, OverlayContext, OverlayLayer};
use gpui::prelude::FluentBuilder;
use gpui::{
    div, px, App, AppContext, Context, Entity, ExternalPaths, FocusHandle, Focusable, FontWeight,
    InteractiveElement, IntoElement, MouseButton, ParentElement, Render,
    StatefulInteractiveElement, Styled, Window,
};
use gpui_component::input::{Escape, Input, InputEvent, InputState};
use gpui_component::{h_flex, v_flex};

use super::projects::ProjectPickerView;
use super::transfer::TransferPanelView;
use super::{RemoteModel, Tab};
use crate::commands::{
    intent_for, keycap, keymap, perform_remote_entry, perform_remote_id, Intent,
};
use crate::fonts::MONO_FAMILY;
use crate::surface::Surface;
use crate::terminal::view::TerminalView;
use crate::theme::Palette;
use crate::views::git_strip::git_strip;
use crate::views::icons;
use crate::views::mission::{mission_hints, MissionControl};
use crate::views::sidebar::{sidebar, SidebarData};
use crate::views::status_bar::STATUS_BAR_HEIGHT;
use crate::views::titlebar::{view_switcher, TITLEBAR_HEIGHT};
use flightdeck::persistence::workspace::MainView;

/// Room for the macOS traffic lights, as the local titlebar leaves.
#[cfg(target_os = "macos")]
const LEADING_INSET: gpui::Pixels = px(84.);
#[cfg(not(target_os = "macos"))]
const LEADING_INSET: gpui::Pixels = px(12.);

pub struct RemoteWindow {
    remote: Entity<RemoteModel>,
    terminal: Entity<TerminalView>,
    app_focus: FocusHandle,
    /// Mission control (A2/A3) over the host, drawn instead of the sidebar
    /// and main area while it is this window's main view.
    mission: Entity<MissionControl>,
    layer: Entity<OverlayLayer>,
    /// The SSH shell, once its tab was first shown.
    shell: Option<Entity<TerminalView>>,
    /// The SSH target the shell was last started on.
    shell_target: String,
    /// Why the SSH shell could not start (no `ssh` on `PATH`).
    shell_error: Option<String>,
    /// What the SSH shell runs on: the real PTY, a fake one in tests.
    shell_pty: Rc<dyn PtyBackend>,
    /// A worker's project picker, while the model has it open.
    picker: Option<Entity<ProjectPickerView>>,
    /// The name the OS window title was last given, so a rename retitles it.
    titled: String,
    /// Rename Remote's name field, while the model has the dialog open.
    rename: Option<Entity<InputState>>,
    /// The upload panel's view, while the model has the panel open.
    upload: Option<Entity<TransferPanelView>>,
    /// macOS: a press that then moves drags the window, as the local
    /// titlebar does.
    #[cfg(target_os = "macos")]
    drag_pending: bool,
    /// The session browsers' views, by tab id, with the key each relay was
    /// last given (`specs/SESSION_BROWSER_PRD.md`). Built from the model's
    /// list and dropped with it; macOS only for now.
    #[cfg(target_os = "macos")]
    browser_views:
        std::collections::HashMap<u64, (Entity<crate::browser::view::BrowserView>, String)>,
    /// The browser the main area shows this frame.
    shown_browser: Option<u64>,
    /// The Worker tab's read-only panes, side by side (W35): every shell but
    /// the one on screen, which is [`RemoteWindow::terminal`].
    shell_panes: std::collections::HashMap<
        flightdeck::web::protocol::TerminalId,
        Entity<crate::views::split::PaneView>,
    >,
}

impl RemoteWindow {
    pub fn new(remote: Entity<RemoteModel>, cx: &mut Context<Self>) -> Self {
        cx.observe(&remote, |_, _, cx| cx.notify()).detach();
        let emit: Emit = {
            let remote = remote.clone();
            Rc::new(move |event, _, cx| remote.update(cx, |model, cx| model.dispatch(event, cx)))
        };
        let terminal = cx.new(|cx| TerminalView::for_remote(remote.clone(), cx));
        let layer = cx.new(|cx| OverlayLayer::new(emit, cx));
        let app_focus = cx.focus_handle();
        let mission = cx.new(|cx| {
            MissionControl::for_remote(remote.clone(), terminal.clone(), app_focus.clone(), cx)
        });
        // `open_remote_window` titles the window with the name it opened on.
        let titled = remote.read(cx).label().to_string();
        Self {
            remote,
            terminal,
            app_focus,
            mission,
            layer,
            shell: None,
            shell_target: String::new(),
            shell_error: None,
            shell_pty: Rc::new(PortablePtyBackend),
            picker: None,
            titled,
            rename: None,
            upload: None,
            #[cfg(target_os = "macos")]
            drag_pending: false,
            #[cfg(target_os = "macos")]
            browser_views: std::collections::HashMap::new(),
            shown_browser: None,
            shell_panes: std::collections::HashMap::new(),
        }
    }

    /// Build Rename Remote's field, filled with the current name and focused,
    /// when the model opened the dialog; drop it when it closed.
    fn sync_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let open = self.remote.read(cx).renaming();
        match (open, &self.rename) {
            (true, None) => {
                let label = self.remote.read(cx).label().to_string();
                let input = cx.new(|cx| {
                    InputState::new(window, cx)
                        .placeholder("Name")
                        .default_value(label)
                });
                cx.subscribe_in(&input, window, |this, input, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        this.save_rename(input.read(cx).value().to_string(), cx);
                    }
                })
                .detach();
                input.update(cx, |state, cx| state.focus(window, cx));
                self.rename = Some(input);
            }
            (false, Some(_)) => self.rename = None,
            _ => {}
        }
    }

    fn save_rename(&mut self, typed: String, cx: &mut Context<Self>) {
        self.remote
            .update(cx, |model, cx| model.commit_rename(&typed, cx));
    }

    /// Type into Rename Remote's field, as a person would (tests).
    #[cfg(test)]
    pub(crate) fn type_rename(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(input) = &self.rename {
            input.update(cx, |s, cx| s.set_value(name.to_string(), window, cx));
        }
    }

    /// Rename Remote's dialog: the name field, Save and Cancel, over a scrim.
    /// Enter saves; Escape, Cancel or a click on the scrim keeps the name.
    fn rename_dialog(
        &self,
        input: &Entity<InputState>,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let model = self.remote.read(cx);
        let address = model.address().to_string();
        let error = model.rename_error().map(str::to_string);
        let mut scrim = p.scrim.hsla();
        scrim.a = crate::theme::SCRIM_ALPHA;
        let save = {
            let input = input.clone();
            div()
                .id("remote-rename-save")
                .debug_selector(|| "remote-rename-save".into())
                .h(px(30.))
                .px_4()
                .flex()
                .items_center()
                .rounded(px(7.))
                .bg(p.button_primary_bg.hsla())
                .text_color(p.button_primary_ink.hsla())
                .font_weight(FontWeight::MEDIUM)
                .cursor_pointer()
                .child("Save")
                .on_click(cx.listener(move |this, _, _, cx| {
                    let typed = input.read(cx).value().to_string();
                    this.save_rename(typed, cx);
                }))
        };
        let cancel = div()
            .id("remote-rename-cancel")
            .debug_selector(|| "remote-rename-cancel".into())
            .h(px(30.))
            .px_4()
            .flex()
            .items_center()
            .rounded(px(7.))
            .border_1()
            .border_color(p.border.hsla())
            .cursor_pointer()
            .child("Cancel")
            .on_click(cx.listener(|this, _, _, cx| {
                this.remote.update(cx, |model, cx| model.cancel_rename(cx))
            }));
        div()
            .id("remote-rename")
            .debug_selector(|| "remote-rename".into())
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .bg(scrim)
            .occlude()
            .flex()
            .items_center()
            .justify_center()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.remote.update(cx, |model, cx| model.cancel_rename(cx))
                }),
            )
            .on_action(cx.listener(|this, _: &Escape, _, cx| {
                this.remote.update(cx, |model, cx| model.cancel_rename(cx))
            }))
            .child(
                v_flex()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .w(px(380.))
                    .gap_3()
                    .p_5()
                    .rounded(px(10.))
                    .border_1()
                    .border_color(p.border.hsla())
                    .bg(p.surface_raised.hsla())
                    .text_size(px(12.5))
                    .child(
                        div()
                            .text_size(px(15.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Rename remote"),
                    )
                    .child(div().text_color(p.ink_2.hsla()).child(format!(
                        "The name this machine shows for {address}. The host is not told."
                    )))
                    .child(Input::new(input))
                    .children(error.map(|message| {
                        div()
                            .debug_selector(|| "remote-rename-error".into())
                            .text_color(p.danger.hsla())
                            .child(message)
                    }))
                    .child(h_flex().justify_end().gap_2().child(cancel).child(save)),
            )
    }

    /// Retitle the OS window after a rename of this remote.
    fn sync_title(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let label = self.remote.read(cx).label();
        if label != self.titled {
            self.titled = label.to_string();
            window.set_window_title(&window_title(label));
        }
    }

    /// Run the SSH shell on `pty` instead of a real one.
    #[cfg(test)]
    pub(crate) fn set_shell_pty(&mut self, pty: Rc<dyn PtyBackend>) {
        self.shell_pty = pty;
    }

    /// The SSH shell's view, once started.
    #[cfg(test)]
    pub(crate) fn shell_view(&self) -> Option<Entity<TerminalView>> {
        self.shell.clone()
    }

    /// Start the SSH shell if its tab is on screen and it has not started.
    /// An ended one stays, its last words readable, until Reconnect.
    fn ensure_shell(&mut self, cx: &mut Context<Self>) {
        if !self.remote.read(cx).shell_active()
            || self.shell.is_some()
            || self.shell_error.is_some()
        {
            return;
        }
        let target = self.remote.read(cx).ssh_target();
        self.shell_target = target.clone();
        match super::shell::spawn(self.shell_pty.as_ref(), &target) {
            Ok(terminal) => {
                self.shell = Some(cx.new(|cx| TerminalView::new(terminal, cx).app_framed()));
            }
            Err(e) => {
                self.shell_error = Some(format!(
                    "Could not run `{} {target}`: {e}",
                    super::shell::SSH
                ));
            }
        }
    }

    /// Start the SSH shell again (its Reconnect).
    fn reconnect_shell(&mut self, cx: &mut Context<Self>) {
        self.shell = None;
        self.shell_error = None;
        self.ensure_shell(cx);
        cx.notify();
    }

    /// The terminal view, for tests that drive it.
    #[cfg(test)]
    pub(crate) fn terminal_view(&self) -> Entity<TerminalView> {
        self.terminal.clone()
    }

    /// The project picker's view, while it is open.
    #[cfg(test)]
    pub(crate) fn picker_view(&self) -> Option<Entity<ProjectPickerView>> {
        self.picker.clone()
    }

    /// Build the picker's view when the model opened it, drop it when the
    /// model closed it.
    fn sync_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let open = self.remote.read(cx).picker().is_some();
        match (open, &self.picker) {
            (true, None) => {
                let remote = self.remote.clone();
                self.picker = Some(cx.new(|cx| ProjectPickerView::new(remote, window, cx)));
            }
            (false, Some(_)) => self.picker = None,
            _ => {}
        }
    }

    /// The upload panel's view, while it is open.
    #[cfg(test)]
    pub(crate) fn upload_view(&self) -> Option<Entity<TransferPanelView>> {
        self.upload.clone()
    }

    /// Build the upload panel's view when the model opened the panel (it
    /// takes the keyboard), drop it when the model closed it.
    fn sync_upload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let open = self.remote.read(cx).transfer_tab().is_some();
        match (open, &self.upload) {
            (true, None) => {
                let remote = self.remote.clone();
                let view = cx.new(|cx| TransferPanelView::new(remote, window, cx));
                let focus = view.read(cx).focus_handle(cx);
                window.focus(&focus, cx);
                self.upload = Some(view);
            }
            (false, Some(_)) => self.upload = None,
            _ => {}
        }
    }

    /// Files dropped on the window: the upload panel opens with them, or
    /// takes them while it is open and still choosing. Folders are left out
    /// with its note.
    fn drop_files(&mut self, paths: &ExternalPaths, cx: &mut Context<Self>) {
        let paths = paths.paths().to_vec();
        self.remote.update(cx, |model, cx| {
            if model.supports_upload() {
                model.open_upload_with(&paths);
                cx.notify();
            }
        });
    }

    fn on_keymap_action(&mut self, action: &KeymapAction, _: &mut Window, cx: &mut Context<Self>) {
        // The upload panel takes its own keys; table chords wait behind it
        // (unless its Choose… has the picker up).
        let upload_shown = self.upload.is_some() && !self.remote.read(cx).choosing_upload_dir();
        if self.remote.read(cx).overlay().is_some() || self.rename.is_some() || upload_shown {
            cx.propagate();
            return;
        }
        if let Some(entry) = action.entry(keymap()) {
            // Quit FlightDeck (Cmd-Q, Ctrl-q) quits this app, as it does from
            // any window — never the host's FlightDeck.
            if entry.action == flightdeck::app::keymap::Action::Quit {
                cx.quit();
                return;
            }
            // The shell takes a paste itself; the model keeps the rest of
            // the terminal's input from the host's hidden one.
            if let (Intent::PasteClipboard, Some(shell)) = (intent_for(&entry.action), &self.shell)
            {
                if self.remote.read(cx).shell_active() {
                    shell.update(cx, |view, cx| view.paste_clipboard(cx));
                    return;
                }
            }
            // In Mission control the moving keys follow tile order.
            let surface = Surface::Remote(self.remote.clone());
            if self.mission_shown(cx) && MissionControl::intercept_on(&surface, entry, cx) {
                return;
            }
            perform_remote_entry(entry, &self.remote, cx);
        }
    }

    /// Whether Mission control is what the window draws: it is the main view,
    /// a project (not a shell's tab) is on screen, and the host is there to
    /// show — a worker with no project yet keeps its "open a project" card.
    fn mission_shown(&self, cx: &App) -> bool {
        let model = self.remote.read(cx);
        let ws = model.workspace();
        model.main_view() == MainView::Mission
            && model.tab() == Tab::Project
            && ws.ready
            && model.ended().is_none()
            && model.picker().is_none()
            && !(model.is_worker() && ws.projects.is_empty())
    }

    fn titlebar(&mut self, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let model = self.remote.read(cx);
        let ws = model.workspace();
        let tabs = views::project_tabs(ws);
        let main_view = model.main_view();
        let needs_you = ws
            .projects
            .iter()
            .flat_map(|p| p.sessions.iter())
            .filter(|s| flightdeck::app::activity::needs_you(s.status.interpreted))
            .count();
        let tab = model.tab();
        let on_project = tab == Tab::Project;
        let worker = model.is_worker();
        let live = model.client().state().is_live();
        let link = model.link_label();
        let name = model.label().to_string();
        let address = model.address().to_string();
        let remote = self.remote.clone();

        let tabs_row = h_flex()
            .id("remote-project-tabs")
            .flex_1()
            .min_w_0()
            .overflow_x_scroll()
            .text_size(px(12.5))
            .child(if worker {
                worker_shell_tab(tab == Tab::WorkerShell, p, &self.remote).into_any_element()
            } else {
                shell_tab(tab == Tab::SshShell, p, &self.remote).into_any_element()
            })
            .children(tabs.iter().enumerate().map(|(i, tab)| {
                let remote = remote.clone();
                h_flex()
                    .id(("remote-project-tab", i))
                    .debug_selector(move || format!("remote-project-tab-{i}"))
                    .flex_none()
                    .h(px(30.))
                    .px(px(10.))
                    .gap(px(8.))
                    .rounded(px(7.))
                    .cursor_pointer()
                    .child(icons::project_glyph(
                        ("remote-project-status", i),
                        tab.status,
                        p,
                    ))
                    .child(div().max_w(px(160.)).truncate().child(tab.name.clone()))
                    .when_else(
                        tab.active && on_project,
                        |t| {
                            t.border_1()
                                .border_color(p.border.hsla())
                                .bg(p.surface_raised.hsla())
                                .text_color(p.ink.hsla())
                                .font_weight(FontWeight::MEDIUM)
                        },
                        |t| {
                            t.text_color(p.muted.hsla())
                                .hover(|s| s.bg(p.surface_raised.hsla()))
                        },
                    )
                    .on_click(move |_, _, cx| {
                        remote.update(cx, |model, cx| {
                            model.select_project(i);
                            cx.notify();
                        })
                    })
            }));

        let identity = h_flex()
            .id("remote-identity")
            .debug_selector(|| "remote-identity".into())
            .flex_none()
            .gap(px(8.))
            .child(
                div().size(px(8.)).rounded_full().bg(if live {
                    p.status_working
                } else {
                    p.status_attention
                }
                .hsla()),
            )
            .child(
                // The address is only a hover away: the title bar keeps the
                // name and the link state, not the full host:port.
                div()
                    .id("remote-name")
                    .debug_selector(|| "remote-name".into())
                    .text_size(px(12.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(p.ink.hsla())
                    .tooltip(icons::tooltip(address))
                    .child(name),
            )
            .child(
                div()
                    .debug_selector(|| "remote-link-state".into())
                    .text_size(px(11.))
                    .text_color(if live { p.muted } else { p.status_attention }.hsla())
                    .child(link),
            );

        let upload = model
            .supports_upload()
            .then(|| transfer_files_button(p, &self.remote));
        let emit_remote = self.remote.clone();
        let command = h_flex()
            .id("remote-command-field")
            .debug_selector(|| "remote-command-field".into())
            .flex_none()
            .w(px(180.))
            .h(px(30.))
            .pl(px(12.))
            .pr(px(8.))
            .gap(px(10.))
            .rounded(px(7.))
            .border_1()
            .border_color(p.border.hsla())
            .bg(p.surface_input.hsla())
            .text_size(px(12.5))
            .text_color(p.muted.hsla())
            .cursor_pointer()
            .child(icons::icon(crate::assets::icon::SEARCH, px(13.), p.muted))
            .child(div().flex_1().child("Command…"))
            .child(icons::keycap(
                keycap("OpenPalette").unwrap_or_default(),
                p.ink_2,
            ))
            .on_click(move |_, _, cx| perform_remote_id("OpenPalette", &emit_remote, cx));

        let bar = h_flex()
            .id("remote-titlebar")
            .flex_shrink_0()
            .h(TITLEBAR_HEIGHT)
            .pl(LEADING_INSET)
            .pr_3()
            .gap(px(14.))
            .bg(p.surface_window.hsla())
            .border_b_1()
            .border_color(p.hairline.hsla())
            .child(identity)
            .child(view_switcher(
                p,
                needs_you,
                main_view,
                &Surface::Remote(self.remote.clone()),
            ))
            .child(div().flex_none().w(px(1.)).h(px(20.)).bg(p.border.hsla()))
            .child(tabs_row)
            .children(upload)
            .child(command);

        #[cfg(target_os = "macos")]
        let bar = bar
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, event: &gpui::MouseDownEvent, window, _| {
                    if event.click_count >= 2 {
                        window.titlebar_double_click();
                    } else {
                        this.drag_pending = true;
                    }
                }),
            )
            .on_mouse_up(
                gpui::MouseButton::Left,
                cx.listener(|this, _, _, _| this.drag_pending = false),
            )
            .on_mouse_move(cx.listener(|this, _, window, _| {
                if std::mem::take(&mut this.drag_pending) {
                    window.start_window_move();
                }
            }));
        bar
    }

    fn status_bar(&self, p: &Palette, cx: &Context<Self>) -> impl IntoElement {
        let mission = self.mission_shown(cx);
        let model = self.remote.read(cx);
        let ws = model.workspace();
        let mode = model.mode();
        let (pill_bg, pill_ink, word, pill_entry) = match mode {
            // The shell has no APP mode: its keys are all the shell's.
            _ if model.shell_active() => (
                p.pill_terminal_bg,
                p.pill_terminal_ink,
                "SSH",
                "FocusTerminal",
            ),
            // Nor has the worker shell.
            _ if model.worker_shell_active() => (
                p.pill_terminal_bg,
                p.pill_terminal_ink,
                "SHELL",
                "FocusTerminal",
            ),
            InputMode::Terminal => (
                p.pill_terminal_bg,
                p.pill_terminal_ink,
                "TERMINAL",
                "FocusApp",
            ),
            InputMode::App => (p.pill_app_bg, p.pill_app_ink, "APP", "FocusTerminal"),
        };
        let remote = self.remote.clone();
        let pill = div()
            .id("remote-mode-pill")
            .debug_selector(|| "remote-mode-pill".into())
            .flex_none()
            .px_2()
            .py(px(2.))
            .rounded(px(4.))
            .bg(pill_bg.hsla())
            .text_color(pill_ink.hsla())
            .font_family(MONO_FAMILY)
            .text_size(px(10.5))
            .font_weight(FontWeight::MEDIUM)
            .cursor_pointer()
            .child(word)
            .on_click(move |_, _, cx| perform_remote_id(pill_entry, &remote, cx));

        let observing = ws.seat == Some(Seat::Observing);
        let holder = ws
            .input_holder()
            .filter(|s| !s.is_you)
            .map(|s| s.label.clone());
        let seat_text = match (observing, &holder) {
            (true, _) => "observing".to_string(),
            (false, Some(label)) => format!("{label} is typing"),
            (false, None) => "in control".to_string(),
        };
        let seat_button = {
            let remote = self.remote.clone();
            let (id, label, seat) = if observing {
                ("remote-seat-control", "Take control", SeatRequest::Write)
            } else if holder.is_some() {
                ("remote-seat-take-over", "Take over", SeatRequest::TakeOver)
            } else {
                ("remote-seat-observe", "Observe", SeatRequest::Observe)
            };
            h_flex()
                .id(id)
                .debug_selector(move || id.to_string())
                .flex_none()
                .px_2()
                .py(px(1.))
                .rounded(px(4.))
                .border_1()
                .border_color(p.border.hsla())
                .cursor_pointer()
                .hover(|s| s.bg(p.surface_raised.hsla()))
                .child(label)
                .on_click(move |_, _, cx| {
                    remote.update(cx, |model, cx| {
                        model.request_seat(seat);
                        cx.notify();
                    })
                })
        };
        let notice = model.notice().map(str::to_string);

        h_flex()
            .flex_shrink_0()
            .h(STATUS_BAR_HEIGHT)
            .px(px(14.))
            .gap_4()
            .bg(p.surface_window.hsla())
            .border_t_1()
            .border_color(p.hairline.hsla())
            .text_size(px(11.5))
            .text_color(p.muted.hsla())
            .child(pill)
            .when_else(
                mission,
                |bar| {
                    bar.children(mission_hints(
                        mode,
                        &Surface::Remote(self.remote.clone()),
                        p,
                    ))
                },
                |bar| bar.child(hint(p, "OpenPalette", "commands")),
            )
            .child(div().flex_1().min_w_0().truncate().children(notice))
            .child(
                div()
                    .debug_selector(|| "remote-seat".into())
                    .flex_none()
                    .child(seat_text),
            )
            .child(seat_button)
    }

    /// The main area: the terminal, or why there is none.
    fn main_area(&self, p: &Palette, cx: &Context<Self>) -> gpui::AnyElement {
        let model = self.remote.read(cx);
        let ws = model.workspace();
        // The shell does not need the link: SSH reaches the host on its own.
        if model.shell_active() {
            return self.shell_area(p, cx);
        }
        if let Some(end) = model.ended() {
            return ended(p, end, model, &self.remote).into_any_element();
        }
        if !ws.ready {
            return notice_card(
                p,
                &format!("Connecting to {}…", model.address()),
                &model.link_label(),
            )
            .into_any_element();
        }
        if model.worker_shell_active() {
            return self.worker_shell_area(p, cx);
        }
        if model.is_worker() && ws.projects.is_empty() {
            return v_flex()
                .flex_1()
                .size_full()
                .items_center()
                .justify_center()
                .gap_2()
                .child(notice_card(
                    p,
                    "This worker has no projects yet.",
                    "Open a repository on it, or clone one.",
                ))
                .child(open_project_button(p, &self.remote))
                .into_any_element();
        }
        if let Some(id) = self.shown_browser {
            return self.browser_area(id, p, cx);
        }
        if model.active_terminal().is_some() {
            return div()
                .flex_1()
                .min_h_0()
                .child(self.terminal.clone())
                .into_any_element();
        }
        let text = if ws.selected_session().is_none() {
            "No agent selected"
        } else {
            "This agent's terminal has not started on the host yet."
        };
        notice_card(p, text, "").into_any_element()
    }
}

impl RemoteWindow {
    /// Build a view for every browser tab the worker granted a key, give a
    /// relay a reconnect's new key, drop the views of closed tabs, and copy
    /// page titles into the model for the sidebar. Returns the browser on
    /// screen.
    fn sync_browsers(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<u64> {
        let shown = self.remote.update(cx, |model, _| model.browser_on_screen());
        #[cfg(target_os = "macos")]
        {
            let model = self.remote.read(cx);
            let tabs: Vec<(u64, String, Option<String>)> = model
                .browsers()
                .all()
                .iter()
                .map(|t| (t.id, t.worktree.clone(), t.key.clone()))
                .collect();
            let access = model.http_access();
            let via = model.label().to_string();
            let worker = model.address().to_string();
            self.browser_views
                .retain(|id, _| tabs.iter().any(|(tab, ..)| tab == id));
            for (id, worktree, key) in tabs {
                let Some(key) = key else { continue };
                match self.browser_views.get_mut(&id) {
                    Some((view, given)) => {
                        if *given != key {
                            view.read(cx).set_key(key.clone());
                            *given = key;
                        }
                    }
                    None => {
                        let Some(access) = access.clone() else {
                            continue;
                        };
                        let profile = crate::browser::profile_id(&worker, &worktree);
                        let via = via.clone();
                        let given = key.clone();
                        let view = cx.new(|cx| {
                            crate::browser::view::BrowserView::new(
                                access, key, via, profile, window, cx,
                            )
                        });
                        cx.observe(&view, |_, _, cx| cx.notify()).detach();
                        self.browser_views.insert(id, (view, given));
                    }
                }
            }
            let titles: Vec<(u64, String)> = self
                .browser_views
                .iter()
                .map(|(id, (view, _))| (*id, view.read(cx).title()))
                .collect();
            self.remote.update(cx, |model, _| {
                for (id, title) in titles {
                    model.set_browser_title(id, title);
                }
            });
        }
        // The profiles of worktrees the worker abandoned go (B19), once the
        // views that used them are gone.
        let abandoned = self
            .remote
            .update(cx, |model, _| model.take_abandoned_worktrees());
        #[cfg(target_os = "macos")]
        {
            let worker = self.remote.read(cx).address().to_string();
            for worktree in abandoned {
                crate::browser::view::forget_profile(
                    crate::browser::profile_id(&worker, &worktree),
                    cx,
                );
            }
        }
        #[cfg(not(target_os = "macos"))]
        let _ = (window, abandoned);
        shown
    }

    /// Show browser `shown`'s web view, hiding every other.
    fn set_browsers_shown(&mut self, shown: Option<u64>, cx: &mut Context<Self>) {
        #[cfg(target_os = "macos")]
        for (id, (view, _)) in &self.browser_views {
            let on = shown == Some(*id);
            view.update(cx, |view, _| view.set_shown(on));
        }
        #[cfg(not(target_os = "macos"))]
        let _ = (shown, cx);
    }

    /// Browser `id`'s area: its view once the worker granted its key, else
    /// what it is waiting for or why it cannot open.
    fn browser_area(&self, id: u64, p: &Palette, cx: &Context<Self>) -> gpui::AnyElement {
        #[cfg(target_os = "macos")]
        if let Some((view, _)) = self.browser_views.get(&id) {
            return div()
                .flex_1()
                .min_h_0()
                .size_full()
                .child(view.clone())
                .into_any_element();
        }
        let model = self.remote.read(cx);
        let (title, detail) = match model.browsers().tab(id) {
            Some(tab) if tab.refusal.is_some() => (
                "The browser could not open.".to_string(),
                tab.refusal.clone().unwrap_or_default(),
            ),
            _ => (
                "Opening a browser on the worker…".to_string(),
                String::new(),
            ),
        };
        notice_card(p, &title, &detail).into_any_element()
    }

    /// The Worker tab: its row of shells (W35) over the shell on screen,
    /// full width; with Restart shell once that one exited.
    fn worker_shell_area(&self, p: &Palette, cx: &Context<Self>) -> gpui::AnyElement {
        let model = self.remote.read(cx);
        let row = worker_shell_row(model, p, &self.remote);
        let restart = || {
            let remote = self.remote.clone();
            div()
                .id("remote-worker-shell-restart")
                .debug_selector(|| "remote-worker-shell-restart".into())
                .flex_none()
                .px_3()
                .py_1()
                .rounded(px(6.))
                .bg(p.button_primary_bg.hsla())
                .text_color(p.button_primary_ink.hsla())
                .cursor_pointer()
                .text_size(px(12.))
                .child("Restart shell")
                .on_click(move |_, _, cx| {
                    remote.update(cx, |model, cx| {
                        model.restart_worker_shell();
                        cx.notify();
                    })
                })
        };
        let Some(shell) = model.worker_shell() else {
            // Every shell closed (W35): open one. A worker older than v10
            // has its one shell, which only ever stopped.
            let body = if model.opens_worker_shells() {
                v_flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .child(notice_card(p, "No shells are open on this worker.", ""))
                    .child(open_shell_button(p, &self.remote))
            } else {
                v_flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .child(notice_card(p, "The worker shell is not running.", ""))
                    .child(restart())
            };
            return v_flex()
                .flex_1()
                .size_full()
                .child(row)
                .child(body)
                .into_any_element();
        };
        let exited = (!shell.alive).then(|| {
            let why = match shell.exit_code {
                Some(code) => format!("{} exited ({code}).", shell.title),
                None => format!("{} exited.", shell.title),
            };
            h_flex()
                .debug_selector(|| "remote-worker-shell-ended".into())
                .flex_none()
                .px(px(14.))
                .py(px(8.))
                .gap_3()
                .border_t_1()
                .border_color(p.hairline.hsla())
                .bg(p.surface_window.hsla())
                .text_size(px(12.))
                .text_color(p.muted.hsla())
                .child(div().flex_1().min_w_0().truncate().child(why))
                .child(restart())
        });
        v_flex()
            .flex_1()
            .min_h_0()
            .size_full()
            .child(row)
            .child(self.worker_shells_body(p, cx))
            .children(exited)
            .into_any_element()
    }

    /// Keep a read-only pane for every worker shell but the one on screen
    /// while the Worker tab is split, and none otherwise.
    fn sync_shell_panes(&mut self, cx: &mut Context<Self>) {
        let model = self.remote.read(cx);
        let wanted: Vec<flightdeck::web::protocol::TerminalId> =
            if model.worker_shell_active() && model.worker_split() {
                let on_screen = model.worker_shell().map(|s| s.terminal_id.clone());
                model
                    .worker_shells()
                    .iter()
                    .map(|s| s.terminal_id.clone())
                    .filter(|id| Some(id) != on_screen.as_ref())
                    .collect()
            } else {
                Vec::new()
            };
        self.shell_panes.retain(|id, _| wanted.contains(id));
        for id in wanted {
            if !self.shell_panes.contains_key(&id) {
                let remote = self.remote.clone();
                let pane =
                    cx.new(|cx| crate::views::split::PaneView::for_remote(remote, id.clone(), cx));
                self.shell_panes.insert(id, pane);
            }
        }
    }

    /// The Worker tab's terminal area: the shell on screen, or every shell
    /// side by side (W35) — the one on screen live, the others read-only and
    /// dimmed, as the local split view draws a session's terminals. A click
    /// in another pane makes it the shell on screen.
    fn worker_shells_body(&self, p: &Palette, cx: &Context<Self>) -> gpui::AnyElement {
        use crate::views::split::{PANE_HEADER, PANE_PAD_X, PANE_PAD_Y};
        let model = self.remote.read(cx);
        let shells = model.worker_shells();
        if !model.worker_split() || shells.len() < 2 {
            return div()
                .flex_1()
                .min_h_0()
                .child(self.terminal.clone())
                .into_any_element();
        }
        let on_screen = model.worker_shell().map(|s| s.terminal_id.clone());
        let mut row = h_flex().flex_1().min_h_0().w_full().items_start();
        for (i, shell) in shells.iter().enumerate() {
            let active = on_screen.as_ref() == Some(&shell.terminal_id);
            if i > 0 {
                row = row.child(div().flex_none().w(px(1.)).h_full().bg(p.hairline.hsla()));
            }
            let body: gpui::AnyElement = match (active, self.shell_panes.get(&shell.terminal_id)) {
                (true, _) => self.terminal.clone().into_any_element(),
                (false, Some(pane)) => {
                    let mut dim = p.pane_dim.hsla();
                    dim.a = crate::theme::PANE_DIM_ALPHA;
                    div()
                        .relative()
                        .size_full()
                        .py(PANE_PAD_Y)
                        .px(PANE_PAD_X)
                        .child(pane.clone())
                        .child(div().absolute().inset_0().bg(dim))
                        .into_any_element()
                }
                (false, None) => div().into_any_element(),
            };
            let id = shell.terminal_id.clone();
            let remote = self.remote.clone();
            let header = h_flex()
                .flex_none()
                .h(PANE_HEADER)
                .px(px(12.))
                .gap_2()
                .border_b_1()
                .border_color(p.hairline.hsla())
                .text_size(px(12.))
                .when(active, |h| {
                    h.bg(p.surface_raised.hsla())
                        .text_color(p.ink.hsla())
                        .font_weight(FontWeight::MEDIUM)
                })
                .when(!active, |h| h.text_color(p.muted.hsla()))
                .child(icons::icon(
                    crate::assets::icon::TERMINAL,
                    px(11.),
                    if active { p.ink } else { p.muted },
                ))
                .child(shell.title.to_lowercase());
            row = row.child(
                v_flex()
                    .id(("remote-worker-shell-pane", i))
                    .debug_selector(move || format!("remote-worker-shell-pane-{i}"))
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .when(!active, |pane| {
                        pane.cursor_pointer()
                            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                                remote.update(cx, |model, cx| {
                                    model.select_worker_shell(&id);
                                    cx.notify();
                                })
                            })
                    })
                    .child(header)
                    .child(div().flex_1().min_h_0().w_full().child(body)),
            );
        }
        row.into_any_element()
    }

    /// The SSH shell, full width; with Reconnect once it ended, or why it
    /// could not start.
    fn shell_area(&self, p: &Palette, cx: &Context<Self>) -> gpui::AnyElement {
        let target = &self.shell_target;
        let reconnect = |label: &'static str| {
            div()
                .id("remote-shell-reconnect")
                .debug_selector(|| "remote-shell-reconnect".into())
                .flex_none()
                .px_3()
                .py_1()
                .rounded(px(6.))
                .bg(p.button_primary_bg.hsla())
                .text_color(p.button_primary_ink.hsla())
                .cursor_pointer()
                .text_size(px(12.))
                .child(label)
                .on_click(cx.listener(|this, _, _, cx| this.reconnect_shell(cx)))
        };
        let Some(shell) = &self.shell else {
            let error = self.shell_error.clone().unwrap_or_default();
            return v_flex()
                .flex_1()
                .size_full()
                .items_center()
                .justify_center()
                .gap_2()
                .child(notice_card(p, "The SSH shell did not start.", &error))
                .child(reconnect("Try again"))
                .into_any_element();
        };
        let ended = shell.read(cx).has_exited().then(|| {
            h_flex()
                .debug_selector(|| "remote-shell-ended".into())
                .flex_none()
                .px(px(14.))
                .py(px(8.))
                .gap_3()
                .border_t_1()
                .border_color(p.hairline.hsla())
                .bg(p.surface_window.hsla())
                .text_size(px(12.))
                .text_color(p.muted.hsla())
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(format!("The SSH session to {target} ended.")),
                )
                .child(reconnect("Reconnect"))
        });
        v_flex()
            .flex_1()
            .min_h_0()
            .size_full()
            .child(div().flex_1().min_h_0().child(shell.clone()))
            .children(ended)
            .into_any_element()
    }
}

impl Render for RemoteWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = *Palette::global(cx);
        self.ensure_shell(cx);
        self.sync_picker(window, cx);
        self.sync_rename(window, cx);
        self.sync_upload(window, cx);
        self.sync_title(window, cx);
        self.shown_browser = self.sync_browsers(window, cx);
        self.sync_shell_panes(cx);
        let mission = self.mission_shown(cx);
        let model = self.remote.read(cx);
        if model.close_requested() {
            window.remove_window();
        }
        let ws = model.workspace();
        let overlay = model.overlay();
        let mode = model.mode();
        let has_terminal = model.active_terminal().is_some();
        let shell_active = model.shell_active();
        // Either shell's tab hides the project's sidebar and git strip.
        let off_project = model.tab() != Tab::Project;
        let link_down = model.ended().is_some() || !ws.ready;
        let skew = model.skew_warning();
        let picker = self.picker.clone().filter(|_| model.ended().is_none());
        // Hidden, not closed, while its Choose… has the picker up.
        let upload = self.upload.clone().filter(|_| !model.choosing_upload_dir());
        let takes_drops = model.supports_upload();
        let browser_rows: Vec<crate::views::sidebar::BrowserRow> = ws
            .selected_session()
            .map(|session| {
                model
                    .browsers()
                    .of_session(&session.session_id)
                    .map(|tab| crate::views::sidebar::BrowserRow {
                        id: tab.id,
                        label: crate::browser::tabs::label(tab),
                        on_screen: self.shown_browser == Some(tab.id),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let data = SidebarData {
            rows: views::agent_rows(ws),
            focused_child: views::focused_child(ws),
            isolated: false,
            host_marker: views::host_marker(ws),
            browsers: browser_rows,
            browser_on_screen: self.shown_browser.is_some(),
            offers_browser: ws.is_worker()
                && ws.protocol_version >= flightdeck::web::protocol::BROWSER_TUNNEL_MIN_VERSION
                && crate::browser::unavailable().is_none(),
        };
        let strip = views::git_strip(ws);
        let context = OverlayContext {
            project: ws.selected_project().map(|p| p.name.clone()),
            agent: ws.selected_session().map(|s| s.name.clone()),
        };
        let overlay_open = overlay.is_some();
        self.layer.update(cx, |layer, cx| {
            layer.set_context(context, cx);
            layer.set_view(overlay, window, cx);
        });
        // The picker's fields take the keyboard where they are clicked, and
        // Rename Remote's field keeps it while the dialog is up. The upload
        // panel keeps it while it is up — back from Choose… too.
        if let Some(panel) = upload
            .as_ref()
            .filter(|_| !overlay_open && self.rename.is_none())
        {
            if !panel.read(cx).has_focus(window, cx) {
                let focus = panel.read(cx).focus_handle(cx);
                window.focus(&focus, cx);
            }
        } else if !overlay_open
            && picker.is_none()
            && self.rename.is_none()
            // A browser on screen keeps whatever has the keyboard: its
            // address field, or the page itself (a native view).
            && self.shown_browser.is_none()
        {
            let shell_focus = self
                .shell
                .as_ref()
                .filter(|_| shell_active)
                .map(|shell| shell.read(cx).focus_handle().clone());
            let want = if let Some(handle) = shell_focus {
                handle
            } else if mode == InputMode::Terminal && has_terminal {
                self.terminal.read(cx).focus_handle().clone()
            } else {
                self.app_focus.clone()
            };
            if !want.is_focused(window) {
                window.focus(&want, cx);
            }
        }

        // The web views are native and drawn over everything, so the one on
        // screen is hidden whenever anything of GPUI's covers the main area
        // (B13), and every other is hidden always.
        let covered = overlay_open
            || self.rename.is_some()
            || upload.is_some()
            || picker.is_some()
            || mission
            || off_project
            || link_down;
        self.set_browsers_shown(if covered { None } else { self.shown_browser }, cx);

        let surface = Surface::Remote(self.remote.clone());
        let app_remote = self.remote.clone();
        let side = sidebar(&data, &surface, &p)
            .key_context(flightdeck::app::keymap::Context::App.name())
            .track_focus(&self.app_focus)
            .on_key_down(move |event, _, cx| {
                if let Some(entry) = app_key_down(keymap(), event) {
                    perform_remote_entry(entry, &app_remote, cx);
                    cx.stop_propagation();
                }
            });
        let main = v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .bg(p.surface_terminal.hsla())
            .when(!off_project, |main| {
                main.child(git_strip(&strip, &surface, &p))
            })
            .child(self.main_area(&p, cx));
        let titlebar = self.titlebar(&p, cx).into_any_element();
        let status = self.status_bar(&p, cx).into_any_element();
        let rename = self
            .rename
            .clone()
            .map(|input| self.rename_dialog(&input, &p, cx).into_any_element());

        v_flex()
            .key_context(flightdeck::app::keymap::Context::Global.name())
            .on_action(cx.listener(Self::on_keymap_action))
            .relative()
            .size_full()
            .bg(p.surface_window.hsla())
            .text_color(p.ink.hsla())
            .font_family(crate::fonts::UI_FAMILY)
            .child(titlebar)
            .children(skew.map(|text| skew_banner(&p, text)))
            .child(match picker {
                Some(picker) => div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .child(picker)
                    .into_any_element(),
                // Straight into the column, as in the local window: Mission
                // control's own `flex_1` row then takes the window's height.
                // Wrapped in a top-aligned row it stopped at its content.
                None if mission => self.mission.clone().into_any_element(),
                None => h_flex()
                    .flex_1()
                    .min_h_0()
                    .items_start()
                    .when(!off_project, |row| row.child(side))
                    .child(main)
                    .into_any_element(),
            })
            .child(status)
            .children(upload)
            .children(rename)
            .child(self.layer.clone())
            .when(takes_drops, |root| root.child(drop_target(&p, cx)))
    }
}

/// Where files dropped on a worker's window land: the whole window, on top
/// of everything but taking no clicks (its hitbox does not block), tinted
/// with an accent frame while files hover over it.
fn drop_target(p: &Palette, cx: &mut Context<RemoteWindow>) -> impl IntoElement {
    let mut tint = p.accent.hsla();
    tint.a = 0.08;
    let frame = p.accent.hsla();
    div()
        .id("remote-drop-target")
        .debug_selector(|| "remote-drop-target".into())
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .drag_over::<ExternalPaths>(move |style, _, _, _| {
            style.bg(tint).border_2().border_color(frame)
        })
        .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| this.drop_files(paths, cx)))
}

/// A worker's Transfer files…, in the titlebar (W34): the Transfer Files
/// panel, on the tab it was last on.
fn transfer_files_button(p: &Palette, remote: &Entity<RemoteModel>) -> impl IntoElement {
    let remote = remote.clone();
    h_flex()
        .id("remote-transfer-files")
        .debug_selector(|| "remote-transfer-files".into())
        .flex_none()
        .h(px(30.))
        .px(px(10.))
        .rounded(px(7.))
        .border_1()
        .border_color(p.border.hsla())
        .text_size(px(12.5))
        .text_color(p.ink_2.hsla())
        .cursor_pointer()
        .hover(|s| s.bg(p.surface_raised.hsla()))
        .child("Transfer files…")
        .on_click(move |_, _, cx| {
            remote.update(cx, |model, cx| {
                model.open_transfer(None);
                cx.notify();
            })
        })
}

/// The SSH shell's tab, first in the row, before the project tabs.
fn shell_tab(active: bool, p: &Palette, remote: &Entity<RemoteModel>) -> impl IntoElement {
    let remote = remote.clone();
    h_flex()
        .id("remote-shell-tab")
        .debug_selector(|| "remote-shell-tab".into())
        .flex_none()
        .h(px(30.))
        .px(px(10.))
        .gap(px(8.))
        .rounded(px(7.))
        .cursor_pointer()
        .child(icons::icon(
            crate::assets::icon::TERMINAL,
            px(13.),
            if active { p.ink } else { p.muted },
        ))
        .child("SSH shell")
        .when_else(
            active,
            |t| {
                t.border_1()
                    .border_color(p.border.hsla())
                    .bg(p.surface_raised.hsla())
                    .text_color(p.ink.hsla())
                    .font_weight(FontWeight::MEDIUM)
            },
            |t| {
                t.text_color(p.muted.hsla())
                    .hover(|s| s.bg(p.surface_raised.hsla()))
            },
        )
        .on_click(move |_, _, cx| {
            remote.update(cx, |model, cx| {
                model.show_shell();
                cx.notify();
            })
        })
}

/// A worker's own shell's tab, first in the row, where other hosts have the
/// SSH shell's.
fn worker_shell_tab(active: bool, p: &Palette, remote: &Entity<RemoteModel>) -> impl IntoElement {
    let remote = remote.clone();
    h_flex()
        .id("remote-worker-shell-tab")
        .debug_selector(|| "remote-worker-shell-tab".into())
        .flex_none()
        .h(px(30.))
        .px(px(10.))
        .gap(px(8.))
        .rounded(px(7.))
        .cursor_pointer()
        .child(icons::icon(
            crate::assets::icon::TERMINAL,
            px(13.),
            if active { p.ink } else { p.muted },
        ))
        .child("Worker")
        .when_else(
            active,
            |t| {
                t.border_1()
                    .border_color(p.border.hsla())
                    .bg(p.surface_raised.hsla())
                    .text_color(p.ink.hsla())
                    .font_weight(FontWeight::MEDIUM)
            },
            |t| {
                t.text_color(p.muted.hsla())
                    .hover(|s| s.bg(p.surface_raised.hsla()))
            },
        )
        .on_click(move |_, _, cx| {
            remote.update(cx, |model, cx| {
                model.show_worker_shell();
                cx.notify();
            })
        })
}

/// The Worker tab's row of shells (W35): one tab per shell, its **×**, and
/// **+** for another. A worker older than protocol v10 has one shell and
/// neither button.
fn worker_shell_row(
    model: &RemoteModel,
    p: &Palette,
    remote: &Entity<RemoteModel>,
) -> gpui::AnyElement {
    let manage = model.opens_worker_shells();
    let on_screen = model.worker_shell().map(|s| s.terminal_id.clone());
    let tabs = model.worker_shells().iter().enumerate().map(|(i, shell)| {
        let active = on_screen.as_ref() == Some(&shell.terminal_id);
        let id = shell.terminal_id.clone();
        let select = remote.clone();
        let close = {
            let remote = remote.clone();
            let id = shell.terminal_id.clone();
            div()
                .id(("remote-worker-shell-close", i))
                .debug_selector(move || format!("remote-worker-shell-close-{i}"))
                .flex_none()
                .rounded(px(4.))
                .p(px(2.))
                .cursor_pointer()
                .hover(|s| s.bg(p.surface_window.hsla()))
                .child(icons::icon(crate::assets::icon::CLOSE, px(10.), p.muted))
                .on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    remote.update(cx, |model, cx| {
                        model.close_worker_shell(&id);
                        cx.notify();
                    })
                })
        };
        h_flex()
            .id(("remote-worker-shell", i))
            .debug_selector(move || format!("remote-worker-shell-{i}"))
            .flex_none()
            .h(px(24.))
            .pl(px(9.))
            .pr(px(if manage { 5. } else { 9. }))
            .gap(px(6.))
            .rounded(px(6.))
            .cursor_pointer()
            .child(icons::icon(
                crate::assets::icon::TERMINAL,
                px(11.),
                if active { p.ink } else { p.muted },
            ))
            .child(shell.title.clone())
            .when(!shell.alive, |t| t.opacity(0.6))
            .when(manage, |t| t.child(close))
            .when_else(
                active,
                |t| {
                    t.border_1()
                        .border_color(p.border.hsla())
                        .bg(p.surface_raised.hsla())
                        .text_color(p.ink.hsla())
                        .font_weight(FontWeight::MEDIUM)
                },
                |t| {
                    t.text_color(p.muted.hsla())
                        .hover(|s| s.bg(p.surface_raised.hsla()))
                },
            )
            .on_click(move |_, _, cx| {
                select.update(cx, |model, cx| {
                    model.select_worker_shell(&id);
                    cx.notify();
                })
            })
    });
    // Side by side, once there is more than one shell to show (W35).
    let split = (model.worker_shells().len() > 1).then(|| {
        let remote = remote.clone();
        let on = model.worker_split();
        let hint = match keycap("ToggleSplitView") {
            Some(cap) => format!("Side by side ({cap})"),
            None => "Side by side".to_string(),
        };
        div()
            .id("remote-worker-shell-split")
            .debug_selector(|| "remote-worker-shell-split".into())
            .flex_none()
            .h(px(24.))
            .px(px(8.))
            .flex()
            .items_center()
            .rounded(px(6.))
            .cursor_pointer()
            .text_size(px(11.5))
            .when(on, |b| {
                b.bg(p.surface_raised.hsla()).text_color(p.ink.hsla())
            })
            .when(!on, |b| {
                b.text_color(p.muted.hsla())
                    .hover(|s| s.bg(p.surface_raised.hsla()))
            })
            .child(if on { "One at a time" } else { "Side by side" })
            .tooltip(move |window, cx| {
                gpui_component::tooltip::Tooltip::new(hint.clone()).build(window, cx)
            })
            .on_click(move |_, _, cx| {
                remote.update(cx, |model, cx| {
                    model.set_worker_split(!on);
                    cx.notify();
                })
            })
    });
    let add = manage.then(|| {
        let remote = remote.clone();
        let hint = match keycap("NewChildTerminal") {
            Some(cap) => format!("New shell ({cap})"),
            None => "New shell".to_string(),
        };
        div()
            .id("remote-worker-shell-new")
            .debug_selector(|| "remote-worker-shell-new".into())
            .flex_none()
            .h(px(24.))
            .w(px(24.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(6.))
            .cursor_pointer()
            .hover(|s| s.bg(p.surface_raised.hsla()))
            .child(icons::icon(crate::assets::icon::PLUS, px(12.), p.muted))
            .tooltip(move |window, cx| {
                gpui_component::tooltip::Tooltip::new(hint.clone()).build(window, cx)
            })
            .on_click(move |_, _, cx| {
                remote.update(cx, |model, cx| {
                    model.open_worker_shell();
                    cx.notify();
                })
            })
    });
    h_flex()
        .id("remote-worker-shells")
        .flex_none()
        .w_full()
        .px(px(10.))
        .py(px(5.))
        .gap(px(4.))
        .overflow_x_scroll()
        .border_b_1()
        .border_color(p.hairline.hsla())
        .bg(p.surface_window.hsla())
        .text_size(px(12.))
        .children(tabs)
        .children(add)
        .child(div().flex_1())
        .children(split)
        .into_any_element()
}

/// **Open a shell**, from the Worker tab with every shell closed.
fn open_shell_button(p: &Palette, remote: &Entity<RemoteModel>) -> impl IntoElement {
    let remote = remote.clone();
    div()
        .id("remote-worker-shell-open")
        .debug_selector(|| "remote-worker-shell-open".into())
        .flex_none()
        .px_3()
        .py_1()
        .rounded(px(6.))
        .bg(p.button_primary_bg.hsla())
        .text_color(p.button_primary_ink.hsla())
        .cursor_pointer()
        .text_size(px(12.))
        .child("Open a shell")
        .on_click(move |_, _, cx| {
            remote.update(cx, |model, cx| {
                model.open_worker_shell();
                cx.notify();
            })
        })
}

/// A worker's Open Project, from its empty state.
fn open_project_button(p: &Palette, remote: &Entity<RemoteModel>) -> impl IntoElement {
    let remote = remote.clone();
    div()
        .id("remote-open-project")
        .debug_selector(|| "remote-open-project".into())
        .flex_none()
        .px_3()
        .py_1()
        .rounded(px(6.))
        .bg(p.button_primary_bg.hsla())
        .text_color(p.button_primary_ink.hsla())
        .cursor_pointer()
        .text_size(px(12.))
        .child("Open a project…")
        .on_click(move |_, _, cx| {
            remote.update(cx, |model, cx| {
                model.open_picker();
                cx.notify();
            })
        })
}

/// The soft warning that the worker's FlightDeck is not this app's.
fn skew_banner(p: &Palette, text: String) -> impl IntoElement {
    div()
        .debug_selector(|| "remote-version-skew".into())
        .flex_shrink_0()
        .w_full()
        .px(px(14.))
        .py(px(6.))
        .border_b_1()
        .border_color(p.status_attention.hsla())
        .bg(p.surface_raised.hsla())
        .text_size(px(12.))
        .text_color(p.ink_2.hsla())
        .child(text)
}

/// One clickable hint: its keycap and word.
/// The OS window title of a remote window named `label`.
pub fn window_title(label: &str) -> String {
    format!("FlightDeck — {label}")
}

fn hint(p: &Palette, entry: &'static str, label: &'static str) -> impl IntoElement {
    h_flex()
        .flex_none()
        .gap_1()
        .child(
            div()
                .font_family(MONO_FAMILY)
                .text_color(p.ink_2.hsla())
                .child(keycap(entry).unwrap_or_default()),
        )
        .child(label)
}

fn notice_card(p: &Palette, title: &str, detail: &str) -> impl IntoElement {
    v_flex()
        .flex_1()
        .size_full()
        .items_center()
        .justify_center()
        .gap_1()
        .child(
            div()
                .text_sm()
                .text_color(p.ink_2.hsla())
                .child(title.to_string()),
        )
        .when(!detail.is_empty(), |d| {
            d.child(
                div()
                    .text_size(px(12.))
                    .text_color(p.muted.hsla())
                    .child(detail.to_string()),
            )
        })
}

/// The link ended for good: say why, and offer to close the window — or to
/// reconnect, where trying again can work.
fn ended(
    p: &Palette,
    end: &LinkEnd,
    model: &RemoteModel,
    remote: &Entity<RemoteModel>,
) -> impl IntoElement {
    let worker = model.is_worker();
    let taken_over;
    let mismatch;
    let (title, detail): (&str, &str) = match end {
        LinkEnd::TakenOver { by } => {
            taken_over = format!("Taken over by {by}");
            (
                taken_over.as_str(),
                "A worker serves one window at a time. Reconnect to take it back.",
            )
        }
        LinkEnd::HostQuit if worker => (
            "The worker stopped.",
            "Start it again on the box with `flightdeck worker start`, then reconnect.",
        ),
        LinkEnd::VersionMismatch(reason) if worker => {
            mismatch = super::messages::mismatch_advice(
                reason,
                &model.workspace().host_version,
                flightdeck::VERSION,
            );
            (
                "The worker runs a different FlightDeck version.",
                mismatch.as_str(),
            )
        }
        _ => ended_words(end),
    };
    let can_reconnect = matches!(
        end,
        LinkEnd::TakenOver { .. }
            | LinkEnd::HostQuit
            | LinkEnd::ServerStopped
            | LinkEnd::VersionMismatch(_)
    );
    let reconnect = can_reconnect.then(|| {
        let remote = remote.clone();
        div()
            .id("remote-reconnect")
            .debug_selector(|| "remote-reconnect".into())
            .px_3()
            .py_1()
            .rounded(px(6.))
            .bg(p.button_primary_bg.hsla())
            .text_color(p.button_primary_ink.hsla())
            .cursor_pointer()
            .text_size(px(12.))
            .child("Reconnect")
            .on_click(move |_, _, cx| {
                remote.update(cx, |model, cx| {
                    model.reconnect();
                    cx.notify();
                })
            })
    });
    ended_panel(p, end, title, detail, reconnect, remote)
}

/// What an ended link says on any host.
fn ended_words(end: &LinkEnd) -> (&'static str, &'static str) {
    match end {
        LinkEnd::Revoked => (
            "The host withdrew this app's access.",
            "Pair again with a new code from the host's web access overlay.",
        ),
        LinkEnd::Unauthorized => (
            "The host does not know this app any more.",
            "Pair again with a new code from the host's web access overlay.",
        ),
        LinkEnd::HostQuit => (
            "The host quit FlightDeck.",
            "Its agents stopped with it. Reconnect once it is running again.",
        ),
        LinkEnd::ServerStopped => (
            "The host stopped its web interface.",
            "FlightDeck is still running there; start the web interface again to reconnect.",
        ),
        LinkEnd::VersionMismatch(_) => (
            "The host runs a different FlightDeck version.",
            "Update one side so both speak the same web protocol.",
        ),
        LinkEnd::TakenOver { .. } => (
            "Another window took this worker over.",
            "Reconnect to take it back.",
        ),
        LinkEnd::Stopped => ("Disconnected.", ""),
    }
}

fn ended_panel(
    p: &Palette,
    end: &LinkEnd,
    title: &str,
    detail: &str,
    reconnect: Option<impl IntoElement>,
    remote: &Entity<RemoteModel>,
) -> impl IntoElement {
    let title = title.to_string();
    let detail = detail.to_string();
    let pair_again = matches!(end, LinkEnd::Revoked | LinkEnd::Unauthorized).then(|| {
        let remote = remote.clone();
        div()
            .id("remote-pair-again")
            .debug_selector(|| "remote-pair-again".into())
            .px_3()
            .py_1()
            .rounded(px(6.))
            .bg(p.button_primary_bg.hsla())
            .text_color(p.button_primary_ink.hsla())
            .cursor_pointer()
            .text_size(px(12.))
            .child("Pair again…")
            .on_click(move |_, _, cx| {
                let address = remote.read(cx).address().to_string();
                remote.update(cx, |model, cx| {
                    model.request_close();
                    cx.notify();
                });
                super::connect::open_connect_window(Some(address), cx);
            })
    });
    let remote = remote.clone();
    v_flex()
        .debug_selector(|| "remote-ended".into())
        .flex_1()
        .size_full()
        .items_center()
        .justify_center()
        .gap_2()
        .child(div().text_sm().text_color(p.ink.hsla()).child(title))
        .when(!detail.is_empty(), |d| {
            d.child(
                div()
                    .text_size(px(12.))
                    .text_color(p.muted.hsla())
                    .child(detail),
            )
        })
        .children(pair_again)
        .children(reconnect)
        .child(
            div()
                .id("remote-close-window")
                .debug_selector(|| "remote-close-window".into())
                .mt_2()
                .px_3()
                .py_1()
                .rounded(px(6.))
                .border_1()
                .border_color(p.border.hsla())
                .cursor_pointer()
                .hover(|s| s.bg(p.surface_raised.hsla()))
                .text_size(px(12.))
                .child("Close window")
                .on_click(move |_, _, cx| {
                    remote.update(cx, |model, cx| {
                        model.request_close();
                        cx.notify();
                    })
                }),
        )
}
