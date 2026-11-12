//! The Transfer Files panel's **Download** tab (protocol v9,
//! `remote-control-pz6d`): files from a folder on the worker into a folder
//! on this machine.
//!
//! ```text
//! +-------------------------------------------------------------+
//! | Transfer files · <worker>          [ Upload | Download ]    |
//! | [Up] [Home]  ~/src/app                     [ ] Show hidden  |
//! |   ▸ docs                                                    |
//! |   [x] notes.md ...................................  4.0 KB  |
//! |   [ ] data.csv ...................................  1.2 MB  |
//! | Select all a        2 files · 1.2 MB selected               |
//! | SAVE TO [~/Downloads                             ] Choose…  |
//! +-------------------------------------------------------------+
//! | Cancel esc                                  Download ⏎      |
//! +-------------------------------------------------------------+
//! ```
//!
//! then, as the core's [`DownloadPanel`] moves on: the names already in the
//! destination (**Replace** / **Keep both** / **Cancel**, asked before the
//! worker is), each file's progress and the total (**Cancel**), and how it
//! ended — what landed where and why the rest did not — with **Show in
//! folder**, **Download more** and **Close**.
//!
//! Keys, while the panel has the keyboard (not the Save to field): ↑/↓ move
//! over the folders and files, Space ticks a file, Enter (or →) opens a
//! folder — on a file, or with nothing under the cursor, Enter downloads;
//! ← or Backspace goes up, `h` home, `.` shows hidden files, `a` ticks all
//! (or none), `s` chooses where to save, Esc closes. Asked about names
//! already here: `r` Replace, `k` or Enter Keep both, Esc Cancel. While
//! downloading, Esc twice cancels. Done: Enter or Esc closes, `d` downloads
//! more, `o` shows the files in the platform's file manager.

use flightdeck::host::ButtonRole;
use flightdeck::web::client::download::{tilde, DownloadItem, DownloadPhase, ItemState};
use flightdeck::web::client::upload::Resolution;
use flightdeck::web::client::DownloadPanel;
use flightdeck_desktop::overlays::footer;
use gpui::prelude::FluentBuilder;
use gpui::{
    div, px, Context, Div, FontWeight, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, Window,
};
use gpui_component::input::Input;
use gpui_component::{h_flex, v_flex};

use super::transfer::{
    bar, files_word, fraction, human_size, mono, panel_button, section, Parts, TransferPanelView,
};
use crate::fonts::MONO_FAMILY;
use crate::theme::Palette;

/// What one row of the folder list is.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Row {
    /// A folder, by its path.
    Folder(String),
    /// A file, by its name.
    File(String),
}

/// The folder list's rows: folders first, then files, as listed.
fn rows(panel: &DownloadPanel) -> Vec<Row> {
    panel
        .folders
        .iter()
        .map(|f| Row::Folder(f.path.clone()))
        .chain(panel.files.iter().map(|f| Row::File(f.name.clone())))
        .collect()
}

/// What one fetched file's row says on its right.
pub fn item_status(item: &DownloadItem) -> String {
    match &item.state {
        ItemState::Waiting => "Waiting".to_string(),
        ItemState::Receiving => format!(
            "{} of {}",
            human_size(item.received),
            human_size(item.total)
        ),
        ItemState::Saved(_) => "Saved".to_string(),
        ItemState::Failed(why) => why.clone(),
    }
}

/// A file name's last component, for a saved path.
fn file_name(path: &std::path::Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// A small square checkbox, ticked or not.
fn checkbox(p: &Palette, ticked: bool) -> Div {
    div()
        .flex_none()
        .size(px(14.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(3.))
        .border_1()
        .border_color(if ticked { p.accent } else { p.border_strong }.hsla())
        .when(ticked, |b| {
            b.bg(p.accent.hsla())
                .text_color(p.surface_window.hsla())
                .text_size(px(10.))
                .font_weight(FontWeight::BOLD)
                .child("✓")
        })
}

impl TransferPanelView {
    /// A key on the Download tab; whether it was the tab's.
    pub(super) fn download_key(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
        let Some(panel) = self.remote.read(cx).download().cloned() else {
            return false;
        };
        match &panel.phase {
            DownloadPhase::Browsing => self.browsing_key(key, &panel, cx),
            DownloadPhase::Requested { .. } => match key {
                "escape" => {
                    self.model(cx, |m, _| m.cancel_download());
                    true
                }
                _ => false,
            },
            DownloadPhase::Asking { .. } => {
                let resolution = match key {
                    "r" => Some(Resolution::Replace),
                    "k" | "enter" => Some(Resolution::KeepBoth),
                    "escape" | "c" => Some(Resolution::Cancel),
                    _ => None,
                };
                resolution
                    .map(|r| self.model(cx, |m, _| m.resolve_download(r)))
                    .is_some()
            }
            DownloadPhase::Downloading { .. } => match key {
                "escape" if self.cancel_armed => {
                    self.cancel_armed = false;
                    self.model(cx, |m, _| m.cancel_download());
                    true
                }
                "escape" => {
                    self.cancel_armed = true;
                    cx.notify();
                    true
                }
                _ => false,
            },
            DownloadPhase::Done { .. } | DownloadPhase::Cancelled => match key {
                "enter" | "escape" => {
                    self.model(cx, |m, _| m.close_upload());
                    true
                }
                "d" => {
                    self.download_more(cx);
                    true
                }
                "o" => {
                    self.remote.update(cx, |m, cx| m.reveal_download(cx));
                    true
                }
                _ => false,
            },
        }
    }

    fn browsing_key(&mut self, key: &str, panel: &DownloadPanel, cx: &mut Context<Self>) -> bool {
        let rows = rows(panel);
        let count = rows.len();
        let at = self.cursor.and_then(|i| rows.get(i).cloned());
        match key {
            "up" | "down" if count > 0 => {
                self.cursor = Some(match (self.cursor, key) {
                    (None, "up") => count - 1,
                    (None, _) => 0,
                    (Some(i), "up") => i.saturating_sub(1),
                    (Some(i), _) => (i + 1).min(count - 1),
                });
                cx.notify();
            }
            "space" => match at {
                Some(Row::File(name)) => self.model(cx, |m, _| m.download_toggle(&name)),
                _ => return false,
            },
            "enter" | "right" => match at {
                Some(Row::Folder(path)) => self.open_folder(&path, cx),
                _ if key == "enter" => self.model(cx, |m, _| m.start_download()),
                _ => return false,
            },
            "left" | "backspace" => {
                self.cursor = None;
                self.model(cx, |m, _| m.download_up());
            }
            "h" => {
                self.cursor = None;
                self.model(cx, |m, _| m.download_home());
            }
            "." => self.model(cx, |m, _| m.download_toggle_hidden()),
            "a" => self.model(cx, |m, _| m.download_select_all()),
            "s" => self.model(cx, |m, cx| m.choose_download_dest(cx)),
            "escape" => self.model(cx, |m, _| m.close_upload()),
            _ => return false,
        }
        true
    }

    fn open_folder(&mut self, path: &str, cx: &mut Context<Self>) {
        self.cursor = None;
        let path = path.to_string();
        self.model(cx, move |m, _| m.download_browse(Some(&path)));
    }

    fn download_more(&mut self, cx: &mut Context<Self>) {
        self.cursor = None;
        self.model(cx, |m, _| m.download_more());
    }

    /// Move the Save to field to the panel's destination when it changed
    /// elsewhere (opened, or chosen with the platform's chooser).
    fn follow_dest(&mut self, panel: &DownloadPanel, window: &mut Window, cx: &mut Context<Self>) {
        if self.shown_dest.as_deref() != Some(panel.dest.as_str()) {
            self.shown_dest = Some(panel.dest.clone());
            let text = panel.dest.clone();
            self.dest.update(cx, |s, cx| s.set_value(text, window, cx));
        }
    }

    /// Empty the Save to field and give it the keyboard (tests type into it).
    #[cfg(test)]
    pub fn clear_dest(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dest.update(cx, |s, cx| {
            s.set_value("", window, cx);
            s.focus(window, cx);
        });
    }

    /// The Download tab's parts.
    pub(super) fn download_parts(
        &mut self,
        panel: &DownloadPanel,
        label: &str,
        p: &Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Parts {
        let p = *p;
        if !panel.is_downloading() {
            self.cancel_armed = false;
        }
        let count = rows(panel).len();
        if self.cursor.is_some_and(|i| i >= count) {
            self.cursor = None;
        }
        self.follow_dest(panel, window, cx);
        let (body, footer) = match &panel.phase {
            DownloadPhase::Browsing => self.browsing_body(panel, &p, cx),
            DownloadPhase::Requested { planned, .. } => {
                let body = vec![div()
                    .debug_selector(|| "download-asking-worker".into())
                    .text_size(px(12.5))
                    .text_color(p.muted.hsla())
                    .child(format!("Asking {label} for {}…", files_word(planned.len())))
                    .into_any_element()];
                let footer = footer(&p).justify_end().child(panel_button(
                    &p,
                    "download-stop",
                    "Cancel",
                    ButtonRole::Cancel,
                    false,
                    Some("esc"),
                    cx.listener(|this, _, _, cx| this.model(cx, |m, _| m.cancel_download())),
                ));
                (body, footer)
            }
            DownloadPhase::Asking { dest, clashes } => {
                self.clash_body(&dest.display().to_string(), clashes, &p, cx)
            }
            DownloadPhase::Downloading { dest, items, .. } => {
                self.downloading_body(&dest.display().to_string(), items, panel, &p, cx)
            }
            DownloadPhase::Done {
                dest,
                saved,
                failed,
                ..
            } => {
                let shown = dest.display().to_string();
                let mut summary = vec![div()
                    .debug_selector(|| "download-done".into())
                    .text_size(px(12.5))
                    .child(if saved.is_empty() {
                        "Nothing was downloaded.".to_string()
                    } else {
                        format!("Saved {} to {shown}.", files_word(saved.len()))
                    })
                    .into_any_element()];
                summary.extend(saved.iter().enumerate().map(|(i, (name, path))| {
                    let landed = file_name(path);
                    let renamed = (&landed != name).then(|| mono(&p, format!("→ {landed}")));
                    h_flex()
                        .debug_selector(move || format!("download-saved-{i}"))
                        .gap_2()
                        .text_size(px(12.))
                        .child(div().font_family(MONO_FAMILY).child(name.clone()))
                        .children(renamed)
                        .into_any_element()
                }));
                summary.extend(failed.iter().enumerate().map(|(i, (name, why))| {
                    h_flex()
                        .debug_selector(move || format!("download-failed-{i}"))
                        .gap_2()
                        .text_size(px(12.))
                        .child(div().font_family(MONO_FAMILY).child(name.clone()))
                        .child(div().text_color(p.danger.hsla()).child(why.clone()))
                        .into_any_element()
                }));
                self.ended_download(summary, !saved.is_empty(), &p, cx)
            }
            DownloadPhase::Cancelled => {
                let summary = vec![div()
                    .debug_selector(|| "download-cancelled".into())
                    .text_size(px(12.5))
                    .child(
                        "Download cancelled. Files already saved stay on this computer; the \
                         one coming in was removed.",
                    )
                    .into_any_element()];
                self.ended_download(summary, false, &p, cx)
            }
        };
        (body, footer, panel.note.clone())
    }

    fn browsing_body(
        &self,
        panel: &DownloadPanel,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> (Vec<gpui::AnyElement>, Div) {
        let p = *p;
        let mut body = Vec::new();

        // Where on the worker: Up, Home, the folder, show hidden.
        let shown = if panel.listed || !panel.cwd.is_empty() {
            tilde(&panel.cwd, &panel.home)
        } else {
            "~".to_string()
        };
        let hidden = h_flex()
            .id("download-hidden")
            .debug_selector(|| "download-hidden".into())
            .flex_none()
            .gap(px(6.))
            .cursor_pointer()
            .text_size(px(12.))
            .text_color(p.muted.hsla())
            .child(checkbox(&p, panel.show_hidden))
            .child("Show hidden")
            .on_click(
                cx.listener(|this, _, _, cx| this.model(cx, |m, _| m.download_toggle_hidden())),
            );
        body.push(
            h_flex()
                .w_full()
                .gap_2()
                .child(
                    panel_button(
                        &p,
                        "download-up",
                        "Up",
                        ButtonRole::Secondary,
                        false,
                        Some("←"),
                        cx.listener(|this, _, _, cx| {
                            this.cursor = None;
                            this.model(cx, |m, _| m.download_up())
                        }),
                    )
                    .when(panel.parent.is_none(), |b| b.opacity(0.5).cursor_default()),
                )
                .child(panel_button(
                    &p,
                    "download-home",
                    "Home",
                    ButtonRole::Secondary,
                    false,
                    Some("h"),
                    cx.listener(|this, _, _, cx| {
                        this.cursor = None;
                        this.model(cx, |m, _| m.download_home())
                    }),
                ))
                .child(
                    div()
                        .debug_selector(|| "download-path".into())
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .font_family(MONO_FAMILY)
                        .text_size(px(12.))
                        .child(shown),
                )
                .child(hidden)
                .into_any_element(),
        );

        // The folder's folders and files.
        let list = if !panel.listed && panel.loading {
            vec![div()
                .debug_selector(|| "download-loading".into())
                .py_2()
                .text_size(px(12.))
                .text_color(p.muted.hsla())
                .child("Listing…")
                .into_any_element()]
        } else if panel.folders.is_empty() && panel.files.is_empty() {
            vec![div()
                .debug_selector(|| "download-empty".into())
                .py_2()
                .text_size(px(12.))
                .text_color(p.muted.hsla())
                .child(if panel.listed {
                    "Nothing in this folder."
                } else {
                    "This folder could not be listed."
                })
                .into_any_element()]
        } else {
            let folders = panel.folders.iter().enumerate().map(|(i, folder)| {
                let path = folder.path.clone();
                let on_cursor = self.cursor == Some(i);
                h_flex()
                    .id(("download-folder", i))
                    .debug_selector(move || format!("download-folder-{i}"))
                    .w_full()
                    .gap_2()
                    .px_2()
                    .py(px(3.))
                    .rounded(px(6.))
                    .text_size(px(12.5))
                    .cursor_pointer()
                    .hover(|s| s.bg(p.surface_raised_nested.hsla()))
                    .when(on_cursor, |row| row.bg(p.surface_raised.hsla()))
                    .child(div().flex_none().text_color(p.muted.hsla()).child("▸"))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(folder.name.clone()),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.open_folder(&path, cx)))
                    .into_any_element()
            });
            let first_file = panel.folders.len();
            let files = panel.files.iter().enumerate().map(|(i, file)| {
                let name = file.name.clone();
                let ticked = panel.is_selected(&file.name);
                let on_cursor = self.cursor == Some(first_file + i);
                h_flex()
                    .id(("download-file", i))
                    .debug_selector(move || format!("download-file-{i}"))
                    .w_full()
                    .gap_2()
                    .px_2()
                    .py(px(3.))
                    .rounded(px(6.))
                    .text_size(px(12.5))
                    .cursor_pointer()
                    .hover(|s| s.bg(p.surface_raised_nested.hsla()))
                    .when(on_cursor, |row| row.bg(p.surface_raised.hsla()))
                    .child(checkbox(&p, ticked))
                    .child(div().flex_1().min_w_0().truncate().child(file.name.clone()))
                    .child(mono(&p, human_size(file.size)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let name = name.clone();
                        this.model(cx, move |m, _| m.download_toggle(&name))
                    }))
                    .into_any_element()
            });
            folders.chain(files).collect()
        };
        body.push(
            v_flex()
                .id("download-list")
                .w_full()
                .max_h(px(240.))
                .overflow_y_scroll()
                .children(list)
                .into_any_element(),
        );
        if panel.truncated {
            body.push(
                div()
                    .text_size(px(11.5))
                    .text_color(p.muted.hsla())
                    .child("The worker listed only part of this folder.")
                    .into_any_element(),
            );
        }

        // Select all, and what is ticked.
        let detail = (!panel.selected.is_empty()).then(|| {
            format!(
                "{} · {} selected",
                files_word(panel.selected.len()),
                human_size(panel.selected_bytes())
            )
        });
        body.push(
            h_flex()
                .gap_3()
                .child(panel_button(
                    &p,
                    "download-select-all",
                    "Select all",
                    ButtonRole::Secondary,
                    false,
                    Some("a"),
                    cx.listener(|this, _, _, cx| this.model(cx, |m, _| m.download_select_all())),
                ))
                .children(detail.map(|d| {
                    div()
                        .debug_selector(|| "download-selected".into())
                        .text_size(px(12.))
                        .text_color(p.muted.hsla())
                        .child(d)
                }))
                .into_any_element(),
        );

        // Where here.
        body.push(section(&p, "Save to", None).into_any_element());
        body.push(
            h_flex()
                .w_full()
                .gap_2()
                .child(div().flex_1().min_w_0().child(Input::new(&self.dest)))
                .child(panel_button(
                    &p,
                    "download-choose",
                    "Choose…",
                    ButtonRole::Secondary,
                    false,
                    Some("s"),
                    cx.listener(|this, _, _, cx| {
                        this.model(cx, |m, cx| m.choose_download_dest(cx))
                    }),
                ))
                .into_any_element(),
        );

        let ready = !panel.selected.is_empty();
        let download = panel_button(
            &p,
            "download-start",
            "Download",
            ButtonRole::Primary,
            ready,
            Some("⏎"),
            cx.listener(|this, _, _, cx| this.model(cx, |m, _| m.start_download())),
        )
        .when(!ready, |b| b.opacity(0.5).cursor_default());
        let footer = footer(&p)
            .justify_end()
            .child(panel_button(
                &p,
                "download-cancel",
                "Cancel",
                ButtonRole::Cancel,
                false,
                Some("esc"),
                cx.listener(|this, _, _, cx| this.model(cx, |m, _| m.close_upload())),
            ))
            .child(download);
        (body, footer)
    }

    fn clash_body(
        &self,
        dest: &str,
        clashes: &[String],
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> (Vec<gpui::AnyElement>, Div) {
        let lead = if clashes.len() == 1 {
            format!("A file with this name is already in {dest}:")
        } else {
            format!(
                "{} files with these names are already in {dest}:",
                clashes.len()
            )
        };
        let body = vec![
            div()
                .debug_selector(|| "download-clashes".into())
                .text_size(px(12.5))
                .child(lead)
                .into_any_element(),
            v_flex()
                .id("download-clash-list")
                .max_h(px(180.))
                .overflow_y_scroll()
                .pl_2()
                .children(clashes.iter().enumerate().map(|(i, name)| {
                    div()
                        .debug_selector(move || format!("download-clash-{i}"))
                        .font_family(MONO_FAMILY)
                        .text_size(px(12.))
                        .child(name.clone())
                }))
                .into_any_element(),
            div()
                .text_size(px(12.))
                .text_color(p.muted.hsla())
                .child("Replace overwrites them. Keep both saves the downloads as name (1).ext.")
                .into_any_element(),
        ];
        let resolve = |r: Resolution| {
            cx.listener(move |this: &mut Self, _: &gpui::ClickEvent, _, cx| {
                this.model(cx, |m, _| m.resolve_download(r))
            })
        };
        let footer = footer(p)
            .justify_end()
            .child(panel_button(
                p,
                "download-clash-cancel",
                "Cancel",
                ButtonRole::Cancel,
                false,
                Some("esc"),
                resolve(Resolution::Cancel),
            ))
            .child(panel_button(
                p,
                "download-replace",
                "Replace",
                ButtonRole::Destructive,
                false,
                Some("r"),
                resolve(Resolution::Replace),
            ))
            .child(panel_button(
                p,
                "download-keep-both",
                "Keep both",
                ButtonRole::Primary,
                true,
                Some("k"),
                resolve(Resolution::KeepBoth),
            ));
        (body, footer)
    }

    fn downloading_body(
        &self,
        dest: &str,
        items: &[DownloadItem],
        panel: &DownloadPanel,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> (Vec<gpui::AnyElement>, Div) {
        let (got, total) = panel.progress_bytes().unwrap_or((0, 0));
        let done = items
            .iter()
            .filter(|i| matches!(i.state, ItemState::Saved(_) | ItemState::Failed(_)))
            .count();
        let rows = items.iter().enumerate().map(|(i, item)| {
            let colour = match item.state {
                ItemState::Failed(_) => p.danger,
                ItemState::Saved(_) => p.status_done,
                _ => p.accent,
            };
            let renamed =
                (item.final_name != item.name).then(|| mono(p, format!("→ {}", item.final_name)));
            v_flex()
                .debug_selector(move || format!("download-item-{i}"))
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
                                .debug_selector(move || format!("download-item-status-{i}"))
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
                .child(bar(p, fraction(item.received, item.total), colour))
        });
        let mut body =
            vec![
                v_flex()
                    .id("download-items")
                    .max_h(px(260.))
                    .overflow_y_scroll()
                    .children(rows)
                    .into_any_element(),
                v_flex()
                    .debug_selector(|| "download-total".into())
                    .gap_1()
                    .pt_2()
                    .child(
                        h_flex()
                            .text_size(px(12.))
                            .child(div().flex_1().child(format!(
                                "{done} of {} into {dest}",
                                files_word(items.len())
                            )))
                            .child(mono(
                                p,
                                format!("{} of {}", human_size(got), human_size(total)),
                            )),
                    )
                    .child(bar(p, fraction(got, total), p.accent))
                    .into_any_element(),
            ];
        if self.cancel_armed {
            body.push(
                div()
                    .debug_selector(|| "download-cancel-armed".into())
                    .text_size(px(12.))
                    .text_color(p.status_attention.hsla())
                    .child("Press esc again to cancel the rest of the download.")
                    .into_any_element(),
            );
        }
        let footer = footer(p).justify_end().child(panel_button(
            p,
            "download-stop",
            "Cancel",
            ButtonRole::Cancel,
            false,
            Some("esc"),
            cx.listener(|this, _, _, cx| {
                this.cancel_armed = false;
                this.model(cx, |m, _| m.cancel_download())
            }),
        ));
        (body, footer)
    }

    fn ended_download(
        &self,
        summary: Vec<gpui::AnyElement>,
        saved_any: bool,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> (Vec<gpui::AnyElement>, Div) {
        let reveal = saved_any.then(|| {
            panel_button(
                p,
                "download-reveal",
                "Show in folder",
                ButtonRole::Secondary,
                false,
                Some("o"),
                cx.listener(|this, _, _, cx| this.remote.update(cx, |m, cx| m.reveal_download(cx))),
            )
        });
        let footer = footer(p)
            .justify_end()
            .children(reveal)
            .child(panel_button(
                p,
                "download-more",
                "Download more",
                ButtonRole::Secondary,
                false,
                Some("d"),
                cx.listener(|this, _, _, cx| this.download_more(cx)),
            ))
            .child(panel_button(
                p,
                "download-close",
                "Close",
                ButtonRole::Primary,
                true,
                Some("⏎"),
                cx.listener(|this, _, _, cx| this.model(cx, |m, _| m.close_upload())),
            ));
        (summary, footer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_items_status_says_how_far_it_is() {
        let mut item = DownloadItem {
            name: "a.bin".into(),
            final_name: "a.bin".into(),
            replace: false,
            ticket: Some("t".into()),
            received: 0,
            total: 2048,
            state: ItemState::Waiting,
        };
        assert_eq!(item_status(&item), "Waiting");
        item.state = ItemState::Receiving;
        item.received = 1024;
        assert_eq!(item_status(&item), "1.0 KB of 2.0 KB");
        item.state = ItemState::Saved("/tmp/a.bin".into());
        assert_eq!(item_status(&item), "Saved");
        item.state = ItemState::Failed("It changed on the worker.".into());
        assert_eq!(item_status(&item), "It changed on the worker.");
    }

    #[test]
    fn folders_come_before_files_in_the_list() {
        use flightdeck::web::protocol::{DirEntryView, FileEntryView};
        let mut panel = DownloadPanel::new("~", "/home/u", "/tmp");
        panel.folders = vec![DirEntryView {
            name: "src".into(),
            path: "/home/u/src".into(),
            is_git_repo: false,
            is_open_project: false,
        }];
        panel.files = vec![FileEntryView {
            name: "a.txt".into(),
            path: "/home/u/a.txt".into(),
            size: 1,
        }];
        assert_eq!(
            rows(&panel),
            [Row::Folder("/home/u/src".into()), Row::File("a.txt".into())]
        );
    }
}
