//! A worker's Transfer Files panel: its **Upload** tab (`specs/WORKER_PRD.md`
//! W34, protocol v8) sends files from this machine into one folder on the
//! worker; its **Download** tab (protocol v9, [`super::download`]) fetches
//! files from a folder on the worker into a folder here. A worker below v9
//! has no Download tab, and the panel is the upload panel it always was.
//!
//! ```text
//! +-------------------------------------------------------------+
//! | Transfer files · <worker>          [ Upload | Download ]    |
//! | FILES  3 · 1.2 MB                                           |
//! |   notes.md ............................ 4.0 KB   ×          |
//! |   (or: drop files here)                                     |
//! |   Add files…                                                |
//! | FOLDER [~/src/app/.flightdeck/worktrees/x      ] Choose…    |
//! | the panel's note (a refusal, a skipped folder)              |
//! +-------------------------------------------------------------+
//! | Cancel esc                                    Upload ⏎      |
//! +-------------------------------------------------------------+
//! ```
//!
//! then, as the core's [`UploadPanel`] moves on: the names already taken
//! (**Replace** / **Keep both** / **Cancel**), each file's progress and the
//! total (**Cancel**), and how it ended (**Upload more** / **Close**).
//!
//! The state is the core's reducers, held by the [`RemoteModel`]'s session,
//! which asks the worker and runs the transfers; this view only draws them
//! and turns clicks and keys into the model's transfer calls. Upload's
//! Choose… is the project picker in its choose-a-folder mode
//! ([`super::projects`]); the panel is hidden while it is up and comes back
//! with the folder set.
//!
//! Keys, while the panel has the keyboard (not a field): Tab (or Shift-Tab)
//! switches tabs, except while a transfer is asked for or moving bytes.
//! Upload: Enter uploads, Esc closes; ↑/↓ and Delete edit the list; `a` adds
//! files, `f` chooses the folder. Asked about taken names: `r` Replace, `k`
//! or Enter Keep both, Esc Cancel. While uploading, Esc twice cancels — one
//! stray press never cuts a long upload off. Done: Enter or Esc closes, `u`
//! uploads more. The Download tab's keys are in [`super::download`].

use flightdeck::host::{ButtonRole, DialogButton};
use flightdeck::web::client::upload::{ItemState, Resolution, UploadItem, UploadPhase};
use flightdeck::web::client::UploadPanel;
use flightdeck_desktop::overlays::{dialog_button_with, dialog_card, footer};
use gpui::prelude::FluentBuilder;
use gpui::{
    div, px, relative, AppContext, Context, Div, Entity, FocusHandle, Focusable, FontWeight,
    InteractiveElement, IntoElement, KeyDownEvent, MouseButton, ParentElement, Render, Stateful,
    StatefulInteractiveElement, Styled, Window,
};
use gpui_component::input::{Escape, Input, InputEvent, InputState};
use gpui_component::{h_flex, v_flex};

use super::{RemoteModel, TransferTab};
use crate::fonts::MONO_FAMILY;
use crate::theme::{Hex, Palette, SCRIM_ALPHA};
use crate::views::icons;

/// The card's width.
const WIDTH: f32 = 560.;

/// A tab's body, its footer and the note under the body.
pub(super) type Parts = (Vec<gpui::AnyElement>, Div, Option<String>);

/// The panel's key context.
const KEY_CONTEXT: &str = "TransferPanel";

/// Set once the panel's own key bindings are in.
struct KeysBound;

impl gpui::Global for KeysBound {}

/// Tab and Shift-Tab switch the panel's tabs, but gpui-component's `Root`
/// binds them for focus traversal, and a binding wins over the focused
/// view's key handler. Disabled under the panel's (deeper) context, they
/// reach [`TransferPanelView`]'s handler — the terminal's
/// [`flightdeck_desktop::keys::TERMINAL_KEEPS`] fix. Bound once per app.
fn bind_keys(cx: &mut gpui::App) {
    if cx.has_global::<KeysBound>() {
        return;
    }
    cx.bind_keys(
        ["tab", "shift-tab"]
            .into_iter()
            .map(|key| gpui::KeyBinding::new(key, gpui::NoAction {}, Some(KEY_CONTEXT))),
    );
    cx.set_global(KeysBound);
}

pub struct TransferPanelView {
    pub(super) remote: Entity<RemoteModel>,
    pub(super) focus: FocusHandle,
    /// The upload's target folder, editable.
    folder: Entity<InputState>,
    /// The folder the field was last set to from the panel, so a folder
    /// chosen elsewhere moves it and typing in it is not overwritten.
    shown_dir: Option<String>,
    /// The upload file row the keyboard is on.
    selected: Option<usize>,
    /// Esc was pressed once while moving bytes: the next one cancels.
    pub(super) cancel_armed: bool,
    /// The download's destination here, editable.
    pub(super) dest: Entity<InputState>,
    /// The destination the field was last set to from the panel.
    pub(super) shown_dest: Option<String>,
    /// The download row (folders, then files) the keyboard is on.
    pub(super) cursor: Option<usize>,
}

impl TransferPanelView {
    pub fn new(remote: Entity<RemoteModel>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        bind_keys(cx);
        cx.observe(&remote, |_, _, cx| cx.notify()).detach();
        let folder = cx.new(|cx| InputState::new(window, cx).placeholder("~/folder"));
        let dest = cx.new(|cx| InputState::new(window, cx).placeholder("~/Downloads"));
        cx.subscribe_in(
            &dest,
            window,
            |this, input, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    let typed = input.read(cx).value().to_string();
                    this.shown_dest = Some(typed.clone());
                    this.remote
                        .update(cx, |model, _| model.set_download_dest(&typed));
                }
                InputEvent::PressEnter { .. } => window.focus(&this.focus, cx),
                _ => {}
            },
        )
        .detach();
        cx.subscribe_in(
            &folder,
            window,
            |this, input, event: &InputEvent, window, cx| {
                match event {
                    InputEvent::Change => {
                        let typed = input.read(cx).value().to_string();
                        this.shown_dir = Some(typed.clone());
                        this.remote
                            .update(cx, |model, _| model.set_upload_dir(&typed));
                    }
                    // Enter in the field hands the keyboard back to the panel:
                    // the next Enter uploads.
                    InputEvent::PressEnter { .. } => window.focus(&this.focus, cx),
                    _ => {}
                }
            },
        )
        .detach();
        Self {
            remote,
            focus: cx.focus_handle(),
            folder,
            shown_dir: None,
            selected: None,
            cancel_armed: false,
            dest,
            shown_dest: None,
            cursor: None,
        }
    }

    /// Whether the panel (or its folder field) has the keyboard.
    pub fn has_focus(&self, window: &Window, cx: &gpui::App) -> bool {
        self.focus.contains_focused(window, cx)
    }

    /// Empty the folder field and give it the keyboard, as a person
    /// selecting its text would (tests then type into it).
    #[cfg(test)]
    pub fn clear_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.folder.update(cx, |s, cx| {
            s.set_value("", window, cx);
            s.focus(window, cx);
        });
    }

    /// Move the folder field to the panel's folder when it changed elsewhere
    /// (opened, or chosen in the picker).
    fn follow_dir(&mut self, panel: &UploadPanel, window: &mut Window, cx: &mut Context<Self>) {
        if self.shown_dir.as_deref() != Some(panel.dir.as_str()) {
            self.shown_dir = Some(panel.dir.clone());
            let text = panel.dir.clone();
            self.folder
                .update(cx, |s, cx| s.set_value(text, window, cx));
        }
    }

    pub(super) fn model(
        &self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut RemoteModel, &mut Context<RemoteModel>),
    ) {
        self.remote.update(cx, |model, cx| {
            f(model, cx);
            cx.notify();
        });
    }

    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        // The folder field types its own keys.
        if !self.focus.is_focused(window) {
            return;
        }
        let m = &event.keystroke.modifiers;
        if m.control || m.alt || m.platform || m.function {
            return;
        }
        let key = event.keystroke.key.as_str();
        let tab = self.remote.read(cx).transfer_tab();
        if key == "tab" {
            let other = match tab {
                Some(TransferTab::Upload) => TransferTab::Download,
                _ => TransferTab::Upload,
            };
            self.switch_tab(other, cx);
            cx.stop_propagation();
            return;
        }
        if tab == Some(TransferTab::Download) {
            if self.download_key(key, cx) {
                cx.stop_propagation();
            }
            return;
        }
        let Some(panel) = self.remote.read(cx).upload().cloned() else {
            return;
        };
        let handled = match &panel.phase {
            UploadPhase::Choosing => self.choosing_key(key, &panel, cx),
            UploadPhase::Requested { .. } => match key {
                "escape" => {
                    self.model(cx, |m, _| m.cancel_upload());
                    true
                }
                _ => false,
            },
            UploadPhase::Asking { .. } => {
                let resolution = match key {
                    "r" => Some(Resolution::Replace),
                    "k" | "enter" => Some(Resolution::KeepBoth),
                    "escape" | "c" => Some(Resolution::Cancel),
                    _ => None,
                };
                resolution
                    .map(|r| self.model(cx, |m, _| m.resolve_upload(r)))
                    .is_some()
            }
            UploadPhase::Uploading { .. } => match key {
                "escape" if self.cancel_armed => {
                    self.cancel_armed = false;
                    self.model(cx, |m, _| m.cancel_upload());
                    true
                }
                "escape" => {
                    self.cancel_armed = true;
                    cx.notify();
                    true
                }
                _ => false,
            },
            UploadPhase::Done { .. } | UploadPhase::Cancelled => match key {
                "enter" | "escape" => {
                    self.model(cx, |m, _| m.close_upload());
                    true
                }
                "u" => {
                    self.upload_more(cx);
                    true
                }
                _ => false,
            },
        };
        if handled {
            cx.stop_propagation();
        }
    }

    fn choosing_key(&mut self, key: &str, panel: &UploadPanel, cx: &mut Context<Self>) -> bool {
        let count = panel.files.len();
        match key {
            "enter" => self.model(cx, |m, _| m.start_upload()),
            "escape" => self.model(cx, |m, _| m.close_upload()),
            "a" => self.model(cx, |m, cx| m.pick_upload_files(cx)),
            "f" => self.model(cx, |m, _| m.choose_upload_dir()),
            "up" | "down" if count > 0 => {
                self.selected = Some(match (self.selected, key) {
                    (None, "up") => count - 1,
                    (None, _) => 0,
                    (Some(i), "up") => i.saturating_sub(1),
                    (Some(i), _) => (i + 1).min(count - 1),
                });
                cx.notify();
            }
            "backspace" | "delete" => {
                let Some(index) = self.selected.filter(|i| *i < count) else {
                    return false;
                };
                self.remove(index, cx);
            }
            _ => return false,
        }
        true
    }

    fn remove(&mut self, index: usize, cx: &mut Context<Self>) {
        self.model(cx, |m, _| m.remove_upload_file(index));
        let left = self.remote.read(cx).upload().map_or(0, |p| p.files.len());
        self.selected = self.selected.filter(|_| left > 0).map(|i| i.min(left - 1));
    }

    fn upload_more(&mut self, cx: &mut Context<Self>) {
        self.selected = None;
        self.model(cx, |m, _| m.upload_more());
    }

    /// Switch to `tab`, unless a transfer is asked for or moving bytes (or
    /// the worker gives no downloads).
    pub(super) fn switch_tab(&mut self, tab: TransferTab, cx: &mut Context<Self>) {
        let switched = self
            .remote
            .update(cx, |model, _| model.set_transfer_tab(tab));
        if switched {
            self.cancel_armed = false;
            cx.notify();
        }
    }
}

impl Focusable for TransferPanelView {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

/// A file size, the way a file manager says it.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if value < 10.0 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.0} {}", UNITS[unit])
    }
}

/// "1 file" / "3 files".
pub(super) fn files_word(n: usize) -> String {
    if n == 1 {
        "1 file".to_string()
    } else {
        format!("{n} files")
    }
}

/// How far an item is, `0.0..=1.0`. An empty file is done when written.
pub(super) fn fraction(sent: u64, total: u64) -> f32 {
    if total == 0 {
        return 0.0;
    }
    (sent as f64 / total as f64).clamp(0.0, 1.0) as f32
}

/// What one granted file's row says on its right.
pub fn item_status(item: &UploadItem) -> String {
    match &item.state {
        ItemState::Waiting => "Waiting".to_string(),
        ItemState::Sending => format!("{} of {}", human_size(item.sent), human_size(item.total)),
        ItemState::Written(_) => "Uploaded".to_string(),
        ItemState::Failed(why) => why.clone(),
    }
}

/// A panel button in the dialogs' family: `role` styles it, `default` makes
/// it the solid one, and `keycap` is drawn beside the label.
pub(super) fn panel_button(
    p: &Palette,
    id: &str,
    label: &str,
    role: ButtonRole,
    default: bool,
    keycap: Option<&str>,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> Stateful<Div> {
    let button = DialogButton {
        id: id.to_string(),
        label: label.to_string(),
        role,
        default,
    };
    dialog_button_with(p, &button, keycap.map(str::to_string), on_click)
}

/// A muted, upper-case section heading with an optional detail after it.
pub(super) fn section(p: &Palette, title: &str, detail: Option<String>) -> Div {
    h_flex()
        .gap_2()
        .text_size(px(11.))
        .child(
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(p.muted.hsla())
                .child(title.to_uppercase()),
        )
        .children(detail.map(|d| div().text_color(p.muted.hsla()).child(d)))
}

/// A thin progress bar, `fraction` full, in `colour`.
pub(super) fn bar(p: &Palette, fraction: f32, colour: Hex) -> Div {
    div()
        .h(px(4.))
        .w_full()
        .rounded(px(2.))
        .bg(p.surface_raised_nested.hsla())
        .child(
            div()
                .h_full()
                .w(relative(fraction.clamp(0.0, 1.0)))
                .rounded(px(2.))
                .bg(colour.hsla()),
        )
}

pub(super) fn mono(p: &Palette, text: String) -> Div {
    div()
        .font_family(MONO_FAMILY)
        .text_size(px(11.5))
        .text_color(p.muted.hsla())
        .child(text)
}

/// The tab strip: Upload and Download, the one on screen raised; both dim
/// and inert while a transfer is under way.
fn tab_strip(
    p: &Palette,
    tab: TransferTab,
    busy: bool,
    cx: &mut Context<TransferPanelView>,
) -> impl IntoElement {
    let pill = |id: &'static str, label: &'static str, this: TransferTab| {
        let active = this == tab;
        div()
            .id(id)
            .debug_selector(move || id.to_string())
            .px(px(10.))
            .py(px(3.))
            .rounded(px(6.))
            .text_size(px(12.))
            .font_weight(FontWeight::MEDIUM)
            .text_color(if active { p.ink } else { p.muted }.hsla())
            .when(active, |pill| pill.bg(p.surface_raised.hsla()))
            .when(!active && !busy, |pill| {
                pill.cursor_pointer()
                    .hover(|s| s.bg(p.surface_raised_nested.hsla()))
            })
            .when(busy && !active, |pill| pill.opacity(0.5))
            .child(label)
            .on_click(cx.listener(move |view, _, _, cx| view.switch_tab(this, cx)))
    };
    h_flex()
        .debug_selector(|| "transfer-tabs".into())
        .flex_none()
        .p(px(2.))
        .gap(px(2.))
        .rounded(px(8.))
        .border_1()
        .border_color(p.border.hsla())
        .child(pill("transfer-tab-upload", "Upload", TransferTab::Upload))
        .child(pill(
            "transfer-tab-download",
            "Download",
            TransferTab::Download,
        ))
}

impl TransferPanelView {
    fn choosing_body(
        &self,
        panel: &UploadPanel,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> (Vec<gpui::AnyElement>, Div) {
        let mut body = Vec::new();
        let detail = (!panel.files.is_empty()).then(|| {
            format!(
                "{} · {}",
                files_word(panel.files.len()),
                human_size(panel.total_bytes())
            )
        });
        body.push(section(p, "Files", detail).into_any_element());
        if panel.files.is_empty() {
            body.push(
                div()
                    .debug_selector(|| "upload-empty".into())
                    .w_full()
                    .py_4()
                    .flex()
                    .justify_center()
                    .rounded(px(8.))
                    .border_1()
                    .border_dashed()
                    .border_color(p.border.hsla())
                    .text_size(px(12.))
                    .text_color(p.muted.hsla())
                    .child("Drop files here, or choose Add files…")
                    .into_any_element(),
            );
        } else {
            let rows = panel.files.iter().enumerate().map(|(i, file)| {
                let selected = self.selected == Some(i);
                let remove = div()
                    .id(("upload-remove", i))
                    .debug_selector(move || format!("upload-remove-{i}"))
                    .flex_none()
                    .p(px(3.))
                    .rounded(px(4.))
                    .cursor_pointer()
                    .hover(|s| s.bg(p.surface_raised_nested.hsla()))
                    .child(icons::icon(crate::assets::icon::CLOSE, px(11.), p.muted))
                    .on_click(cx.listener(move |this, _, _, cx| this.remove(i, cx)));
                h_flex()
                    .id(("upload-file", i))
                    .debug_selector(move || format!("upload-file-{i}"))
                    .w_full()
                    .gap_3()
                    .px_2()
                    .py(px(3.))
                    .rounded(px(6.))
                    .text_size(px(12.5))
                    .when(selected, |row| row.bg(p.surface_raised.hsla()))
                    .child(div().flex_1().min_w_0().truncate().child(file.name.clone()))
                    .child(mono(p, human_size(file.size)))
                    .child(remove)
            });
            body.push(
                v_flex()
                    .id("upload-files")
                    .max_h(px(220.))
                    .overflow_y_scroll()
                    .children(rows)
                    .into_any_element(),
            );
        }
        body.push(
            h_flex()
                .child(panel_button(
                    p,
                    "upload-add",
                    "Add files…",
                    ButtonRole::Secondary,
                    false,
                    Some("a"),
                    cx.listener(|this, _, _, cx| this.model(cx, |m, cx| m.pick_upload_files(cx))),
                ))
                .into_any_element(),
        );
        body.push(section(p, "Folder", None).into_any_element());
        body.push(
            h_flex()
                .w_full()
                .gap_2()
                .child(div().flex_1().min_w_0().child(Input::new(&self.folder)))
                .child(panel_button(
                    p,
                    "upload-choose",
                    "Choose…",
                    ButtonRole::Secondary,
                    false,
                    Some("f"),
                    cx.listener(|this, _, _, cx| this.model(cx, |m, _| m.choose_upload_dir())),
                ))
                .into_any_element(),
        );
        let has_files = !panel.files.is_empty();
        let upload = panel_button(
            p,
            "upload-start",
            "Upload",
            ButtonRole::Primary,
            has_files,
            Some("⏎"),
            cx.listener(|this, _, _, cx| this.model(cx, |m, _| m.start_upload())),
        )
        .when(!has_files, |b| b.opacity(0.5).cursor_default());
        let footer = footer(p)
            .justify_end()
            .child(panel_button(
                p,
                "upload-cancel",
                "Cancel",
                ButtonRole::Cancel,
                false,
                Some("esc"),
                cx.listener(|this, _, _, cx| this.model(cx, |m, _| m.close_upload())),
            ))
            .child(upload);
        (body, footer)
    }

    fn asking_body(
        &self,
        dir: &str,
        conflicts: &[String],
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> (Vec<gpui::AnyElement>, Div) {
        let lead = if conflicts.len() == 1 {
            format!("A file with this name is already in {dir}:")
        } else {
            format!(
                "{} files with these names are already in {dir}:",
                conflicts.len()
            )
        };
        let body = vec![
            div()
                .debug_selector(|| "upload-conflicts".into())
                .text_size(px(12.5))
                .child(lead)
                .into_any_element(),
            v_flex()
                .id("upload-conflict-list")
                .max_h(px(180.))
                .overflow_y_scroll()
                .pl_2()
                .children(conflicts.iter().enumerate().map(|(i, name)| {
                    div()
                        .debug_selector(move || format!("upload-conflict-{i}"))
                        .font_family(MONO_FAMILY)
                        .text_size(px(12.))
                        .child(name.clone())
                }))
                .into_any_element(),
            div()
                .text_size(px(12.))
                .text_color(p.muted.hsla())
                .child("Replace overwrites them. Keep both saves the new files as name (1).ext.")
                .into_any_element(),
        ];
        let resolve = |r: Resolution| {
            cx.listener(move |this: &mut Self, _: &gpui::ClickEvent, _, cx| {
                this.model(cx, |m, _| m.resolve_upload(r))
            })
        };
        let footer = footer(p)
            .justify_end()
            .child(panel_button(
                p,
                "upload-conflict-cancel",
                "Cancel",
                ButtonRole::Cancel,
                false,
                Some("esc"),
                resolve(Resolution::Cancel),
            ))
            .child(panel_button(
                p,
                "upload-replace",
                "Replace",
                ButtonRole::Destructive,
                false,
                Some("r"),
                resolve(Resolution::Replace),
            ))
            .child(panel_button(
                p,
                "upload-keep-both",
                "Keep both",
                ButtonRole::Primary,
                true,
                Some("k"),
                resolve(Resolution::KeepBoth),
            ));
        (body, footer)
    }

    fn uploading_body(
        &self,
        dir: &str,
        items: &[UploadItem],
        panel: &UploadPanel,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> (Vec<gpui::AnyElement>, Div) {
        let (sent, total) = panel.progress_bytes().unwrap_or((0, 0));
        let done = items
            .iter()
            .filter(|i| matches!(i.state, ItemState::Written(_) | ItemState::Failed(_)))
            .count();
        let rows = items.iter().enumerate().map(|(i, item)| {
            let colour = match item.state {
                ItemState::Failed(_) => p.danger,
                ItemState::Written(_) => p.status_done,
                _ => p.accent,
            };
            let renamed =
                (item.final_name != item.name).then(|| mono(p, format!("→ {}", item.final_name)));
            v_flex()
                .debug_selector(move || format!("upload-item-{i}"))
                .w_full()
                .gap_1()
                .py(px(3.))
                .child(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .text_size(px(12.5))
                        .child(div().min_w_0().truncate().child(item.name.clone()))
                        .children(renamed)
                        .child(div().flex_1())
                        .child(
                            div()
                                .debug_selector(move || format!("upload-item-status-{i}"))
                                .flex_none()
                                .text_size(px(11.5))
                                .text_color(
                                    if matches!(item.state, ItemState::Failed(_)) {
                                        p.danger
                                    } else {
                                        p.muted
                                    }
                                    .hsla(),
                                )
                                .child(item_status(item)),
                        ),
                )
                .child(bar(p, fraction(item.sent, item.total), colour))
        });
        let mut body = vec![
            v_flex()
                .id("upload-items")
                .max_h(px(260.))
                .overflow_y_scroll()
                .children(rows)
                .into_any_element(),
            v_flex()
                .debug_selector(|| "upload-total".into())
                .gap_1()
                .pt_2()
                .child(
                    h_flex()
                        .text_size(px(12.))
                        .child(
                            div()
                                .flex_1()
                                .child(format!("{done} of {} into {dir}", files_word(items.len()))),
                        )
                        .child(mono(
                            p,
                            format!("{} of {}", human_size(sent), human_size(total)),
                        )),
                )
                .child(bar(p, fraction(sent, total), p.accent))
                .into_any_element(),
        ];
        if self.cancel_armed {
            body.push(
                div()
                    .debug_selector(|| "upload-cancel-armed".into())
                    .text_size(px(12.))
                    .text_color(p.status_attention.hsla())
                    .child("Press esc again to cancel the rest of the upload.")
                    .into_any_element(),
            );
        }
        let footer = footer(p).justify_end().child(panel_button(
            p,
            "upload-stop",
            "Cancel",
            ButtonRole::Cancel,
            false,
            Some("esc"),
            cx.listener(|this, _, _, cx| {
                this.cancel_armed = false;
                this.model(cx, |m, _| m.cancel_upload())
            }),
        ));
        (body, footer)
    }

    fn ended_body(
        &self,
        summary: Vec<gpui::AnyElement>,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> (Vec<gpui::AnyElement>, Div) {
        let footer = footer(p)
            .justify_end()
            .child(panel_button(
                p,
                "upload-more",
                "Upload more",
                ButtonRole::Secondary,
                false,
                Some("u"),
                cx.listener(|this, _, _, cx| this.upload_more(cx)),
            ))
            .child(panel_button(
                p,
                "upload-close",
                "Close",
                ButtonRole::Primary,
                true,
                Some("⏎"),
                cx.listener(|this, _, _, cx| this.model(cx, |m, _| m.close_upload())),
            ));
        (summary, footer)
    }

    /// The Upload tab's parts.
    fn upload_parts(
        &mut self,
        panel: &UploadPanel,
        label: &str,
        p: &Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Parts {
        let p = *p;
        if !panel.is_uploading() {
            self.cancel_armed = false;
        }
        if self.selected.is_some_and(|i| i >= panel.files.len()) {
            self.selected = None;
        }
        self.follow_dir(panel, window, cx);
        let (body, footer) = match &panel.phase {
            UploadPhase::Choosing => self.choosing_body(panel, &p, cx),
            UploadPhase::Requested { .. } => {
                let body = vec![div()
                    .debug_selector(|| "upload-asking-worker".into())
                    .text_size(px(12.5))
                    .text_color(p.muted.hsla())
                    .child(format!(
                        "Asking {label} to take {}…",
                        files_word(panel.files.len())
                    ))
                    .into_any_element()];
                let footer = footer(&p).justify_end().child(panel_button(
                    &p,
                    "upload-stop",
                    "Cancel",
                    ButtonRole::Cancel,
                    false,
                    Some("esc"),
                    cx.listener(|this, _, _, cx| this.model(cx, |m, _| m.cancel_upload())),
                ));
                (body, footer)
            }
            UploadPhase::Asking { dir, conflicts } => self.asking_body(dir, conflicts, &p, cx),
            UploadPhase::Uploading { dir, items, .. } => {
                self.uploading_body(dir, items, panel, &p, cx)
            }
            UploadPhase::Done {
                dir,
                written,
                failed,
            } => {
                let mut summary = vec![div()
                    .debug_selector(|| "upload-done".into())
                    .text_size(px(12.5))
                    .child(if written.is_empty() {
                        "Nothing was uploaded.".to_string()
                    } else {
                        format!("Uploaded {} to {dir}.", files_word(written.len()))
                    })
                    .into_any_element()];
                summary.extend(failed.iter().enumerate().map(|(i, (name, why))| {
                    h_flex()
                        .debug_selector(move || format!("upload-failed-{i}"))
                        .gap_2()
                        .text_size(px(12.))
                        .child(div().font_family(MONO_FAMILY).child(name.clone()))
                        .child(div().text_color(p.danger.hsla()).child(why.clone()))
                        .into_any_element()
                }));
                self.ended_body(summary, &p, cx)
            }
            UploadPhase::Cancelled => {
                let summary = vec![div()
                    .debug_selector(|| "upload-cancelled".into())
                    .text_size(px(12.5))
                    .child("Upload cancelled. Files already sent stay on the worker.")
                    .into_any_element()];
                self.ended_body(summary, &p, cx)
            }
        };
        (body, footer, panel.note.clone())
    }
}

impl Render for TransferPanelView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = *Palette::global(cx);
        let model = self.remote.read(cx);
        let Some(tab) = model.transfer_tab() else {
            return div().id("transfer-closed");
        };
        let label = model.label().to_string();
        let tabs = model.supports_download();
        let busy = model.transfer_busy();
        let upload = model.upload().cloned();
        let download = model.download().cloned();
        let parts = match (tab, upload, download) {
            (TransferTab::Upload, Some(panel), _) => {
                Some(self.upload_parts(&panel, &label, &p, window, cx))
            }
            (TransferTab::Download, _, Some(panel)) => {
                Some(self.download_parts(&panel, &label, &p, window, cx))
            }
            _ => None,
        };
        let Some((body, footer, note)) = parts else {
            return div().id("transfer-closed");
        };
        let (note_id, title) = match (tab, tabs) {
            (TransferTab::Download, _) => ("download-note", format!("Transfer files · {label}")),
            (TransferTab::Upload, true) => ("upload-note", format!("Transfer files · {label}")),
            (TransferTab::Upload, false) => ("upload-note", format!("Upload files to {label}")),
        };
        let note = note.map(|note| {
            div()
                .debug_selector(move || note_id.into())
                .text_size(px(12.))
                .text_color(p.status_attention.hsla())
                .child(note)
        });
        let heading = h_flex()
            .w_full()
            .gap_3()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(15.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(title),
            )
            .when(tabs, |row| row.child(tab_strip(&p, tab, busy, cx)));

        let mut scrim = p.scrim.hsla();
        scrim.a = SCRIM_ALPHA;
        div()
            .id("transfer-panel")
            .debug_selector(|| "transfer-panel".into())
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .bg(scrim)
            .occlude()
            .flex()
            .items_center()
            .justify_center()
            .child(
                dialog_card(&p, WIDTH)
                    .id("transfer-card")
                    .key_context(KEY_CONTEXT)
                    .track_focus(&self.focus)
                    .on_key_down(cx.listener(Self::on_key))
                    // Esc in the folder field hands the keyboard back.
                    .on_action(
                        cx.listener(|this, _: &Escape, window, cx| window.focus(&this.focus, cx)),
                    )
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        v_flex()
                            .gap_2()
                            .p(px(16.))
                            .child(heading)
                            .children(body)
                            .children(note),
                    )
                    .child(footer),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_like_a_file_manager() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1023), "1023 B");
        assert_eq!(human_size(1024), "1.0 KB");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(20 * 1024), "20 KB");
        assert_eq!(human_size(5 * 1024 * 1024 + 300 * 1024), "5.3 MB");
        assert_eq!(human_size(3 * 1024 * 1024 * 1024), "3.0 GB");
    }

    #[test]
    fn an_items_status_says_how_far_it_is() {
        let mut item = UploadItem {
            name: "a.bin".into(),
            final_name: "a.bin".into(),
            local: "/tmp/a.bin".into(),
            ticket: "t".into(),
            sent: 0,
            total: 2048,
            state: ItemState::Waiting,
        };
        assert_eq!(item_status(&item), "Waiting");
        item.state = ItemState::Sending;
        item.sent = 1024;
        assert_eq!(item_status(&item), "1.0 KB of 2.0 KB");
        assert!((fraction(item.sent, item.total) - 0.5).abs() < f32::EPSILON);
        item.state = ItemState::Failed("The worker is out of space.".into());
        assert_eq!(item_status(&item), "The worker is out of space.");
        assert_eq!(fraction(5, 0), 0.0, "an empty file has no bar to fill");
    }
}
