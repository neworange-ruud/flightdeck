//! A worker's project picker (`specs/WORKER_PRD.md` W9, W16): what Open
//! Project means in a worker's window, which has no folder dialog to show.
//!
//! ```text
//! +----------------------------------------------------------+
//! | Open a project on <worker>                        Close   |
//! | error line (the worker's words, verbatim)                 |
//! | PROJECTS IN ~/flightdeck   repo rows ............ Open    |
//! | BROWSE   [path bar             ] Go  Up  Open this folder |
//! |          folder rows (git ones with Open) ..............  |
//! | CLONE    [URL                  ] [name (optional)] Clone  |
//! |          progress line, or git's error                    |
//! +----------------------------------------------------------+
//! ```
//!
//! The state is the core's [`flightdeck::web::client::ProjectPicker`], held
//! by the [`RemoteModel`] (which sends the commands and feeds it the
//! worker's answers); this view only draws it and turns clicks and Enter
//! into the model's `picker_*` calls. The text fields are its own.

use flightdeck::web::client::ProjectPicker;
use gpui::prelude::FluentBuilder;
use gpui::{
    div, px, AppContext, Context, Entity, FontWeight, InteractiveElement, IntoElement,
    ParentElement, Render, SharedString, StatefulInteractiveElement, Styled, Window,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{h_flex, v_flex};

use super::RemoteModel;
use crate::fonts::MONO_FAMILY;
use crate::theme::Palette;

pub struct ProjectPickerView {
    remote: Entity<RemoteModel>,
    path: Entity<InputState>,
    url: Entity<InputState>,
    name: Entity<InputState>,
    /// The folder the path bar was last set to, so a new listing moves it
    /// and typing in it does not get overwritten.
    shown_cwd: Option<String>,
}

impl ProjectPickerView {
    pub fn new(remote: Entity<RemoteModel>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.observe(&remote, |_, _, cx| cx.notify()).detach();
        let path = cx.new(|cx| InputState::new(window, cx).placeholder("~/src/my-repo"));
        let url =
            cx.new(|cx| InputState::new(window, cx).placeholder("git@github.com:me/my-repo.git"));
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("folder name (optional)"));
        cx.subscribe_in(&path, window, |this, _, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.go(cx);
            }
        })
        .detach();
        for field in [&url, &name] {
            cx.subscribe_in(field, window, |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.clone_url(cx);
                }
            })
            .detach();
        }
        Self {
            remote,
            path,
            url,
            name,
            shown_cwd: None,
        }
    }

    /// Type into the clone fields, as a person would (tests).
    #[cfg(test)]
    pub fn fill_clone(
        &mut self,
        url: &str,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.url
            .update(cx, |s, cx| s.set_value(url.to_string(), window, cx));
        self.name
            .update(cx, |s, cx| s.set_value(name.to_string(), window, cx));
    }

    /// List the folder typed in the path bar.
    fn go(&mut self, cx: &mut Context<Self>) {
        let path = self.path.read(cx).value().to_string();
        self.remote.update(cx, |model, cx| {
            model.picker_browse(Some(&path));
            cx.notify();
        });
    }

    /// Clone the URL typed in, into the name typed in (if any).
    fn clone_url(&mut self, cx: &mut Context<Self>) {
        let url = self.url.read(cx).value().to_string();
        let name = self.name.read(cx).value().to_string();
        self.remote.update(cx, |model, cx| {
            model.picker_clone(&url, &name);
            cx.notify();
        });
    }

    /// Move the path bar to the folder listed, when a new listing arrived.
    fn follow_cwd(&mut self, picker: &ProjectPicker, window: &mut Window, cx: &mut Context<Self>) {
        if picker.cwd.is_some() && picker.cwd != self.shown_cwd {
            self.shown_cwd = picker.cwd.clone();
            let text = self.shown_cwd.clone().unwrap_or_default();
            self.path.update(cx, |s, cx| s.set_value(text, window, cx));
        }
    }
}

/// A button of the picker's family: `primary` filled, else outlined.
fn button(
    p: &Palette,
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    primary: bool,
) -> gpui::Stateful<gpui::Div> {
    let id: SharedString = id.into();
    let selector = id.to_string();
    div()
        .id(gpui::ElementId::Name(id))
        .debug_selector(move || selector.clone())
        .flex_none()
        .px_3()
        .py_1()
        .rounded(px(6.))
        .text_size(px(12.))
        .cursor_pointer()
        .when_else(
            primary,
            |b| {
                b.bg(p.button_primary_bg.hsla())
                    .text_color(p.button_primary_ink.hsla())
            },
            |b| {
                b.border_1()
                    .border_color(p.border.hsla())
                    .hover(|s| s.bg(p.surface_raised.hsla()))
            },
        )
        .child(label.into())
}

fn section(p: &Palette, title: String) -> impl IntoElement {
    div()
        .pt_2()
        .text_size(px(11.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(p.muted.hsla())
        .child(title)
}

fn muted(p: &Palette, text: String) -> impl IntoElement {
    div()
        .text_size(px(12.))
        .text_color(p.muted.hsla())
        .child(text)
}

/// The clone's line: what git is doing, or how it ended.
pub fn clone_line(picker: &ProjectPicker) -> Option<(String, bool)> {
    let clone = picker.clone.as_ref()?;
    Some(match &clone.done {
        None => (
            match clone.percent {
                Some(percent) => format!("Cloning {} — {} {percent}%", clone.url, clone.phase),
                None => format!("Cloning {} — {}", clone.url, clone.phase),
            },
            false,
        ),
        Some(Ok(path)) => (format!("Cloned into {path}."), false),
        // Git's own words (authentication, host key, network, name in use).
        Some(Err(why)) => (why.clone(), true),
    })
}

impl Render for ProjectPickerView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = *Palette::global(cx);
        let Some(picker) = self.remote.read(cx).picker().cloned() else {
            return div().id("remote-picker-closed");
        };
        self.follow_cwd(&picker, window, cx);
        let label = self.remote.read(cx).label().to_string();
        let remote = self.remote.clone();
        let this = cx.entity();

        let header = h_flex()
            .w_full()
            .gap_3()
            .child(
                div()
                    .flex_1()
                    .text_size(px(16.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(format!("Open a project on {label}")),
            )
            .when(picker.is_busy(), |h| {
                h.child(muted(&p, "Working…".to_string()))
            })
            .child({
                let remote = remote.clone();
                button(&p, "remote-picker-close", "Close", false).on_click(move |_, _, cx| {
                    remote.update(cx, |model, cx| {
                        model.close_picker();
                        cx.notify();
                    })
                })
            });

        let error = picker.error.clone().map(|why| {
            div()
                .debug_selector(|| "remote-picker-error".into())
                .text_size(px(12.))
                .text_color(p.danger.hsla())
                .child(why)
        });

        // Scanned: the projects root's repositories.
        let open_row = |id: SharedString, name: String, path: String, open: bool| {
            let remote = remote.clone();
            let open_path = path.clone();
            h_flex()
                .w_full()
                .gap_3()
                .px_2()
                .py(px(3.))
                .rounded(px(6.))
                .text_size(px(12.5))
                .child(div().font_weight(FontWeight::MEDIUM).child(name))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .font_family(MONO_FAMILY)
                        .text_size(px(11.5))
                        .text_color(p.muted.hsla())
                        .child(path),
                )
                .child(if open {
                    muted(&p, "open".to_string()).into_any_element()
                } else {
                    button(&p, id, "Open", false)
                        .on_click(move |_, _, cx| {
                            remote.update(cx, |model, cx| {
                                model.picker_open(&open_path);
                                cx.notify();
                            })
                        })
                        .into_any_element()
                })
        };
        let scanned = match &picker.scanned {
            None => vec![muted(&p, "Scanning…".to_string()).into_any_element()],
            Some(scan) if !scan.root_exists => vec![muted(
                &p,
                format!(
                    "{} does not exist yet: clone a repository to create it.",
                    scan.root
                ),
            )
            .into_any_element()],
            Some(scan) if scan.repos.is_empty() => {
                vec![muted(&p, format!("No repositories in {}.", scan.root)).into_any_element()]
            }
            Some(scan) => scan
                .repos
                .iter()
                .enumerate()
                .map(|(i, repo)| {
                    open_row(
                        format!("remote-picker-scanned-{i}").into(),
                        repo.name.clone(),
                        repo.path.clone(),
                        repo.is_open_project,
                    )
                    .into_any_element()
                })
                .collect(),
        };
        let scan_title = match &picker.scanned {
            Some(scan) => format!("PROJECTS IN {}", scan.root),
            None => "PROJECTS".to_string(),
        };

        // Browse: the path bar, then the folder's subfolders.
        let bar = h_flex()
            .w_full()
            .gap_2()
            .child(div().flex_1().min_w_0().child(Input::new(&self.path)))
            .child({
                let this = this.clone();
                button(&p, "remote-picker-go", "Go", false)
                    .on_click(move |_, _, cx| this.update(cx, |view, cx| view.go(cx)))
            })
            .when(picker.parent.is_some(), |bar| {
                let remote = remote.clone();
                bar.child(
                    button(&p, "remote-picker-up", "Up", false).on_click(move |_, _, cx| {
                        remote.update(cx, |model, cx| {
                            model.picker_up();
                            cx.notify();
                        })
                    }),
                )
            })
            .when_some(picker.cwd.clone(), |bar, cwd| {
                let remote = remote.clone();
                bar.child(
                    button(&p, "remote-picker-open-here", "Open this folder", false).on_click(
                        move |_, _, cx| {
                            remote.update(cx, |model, cx| {
                                model.picker_open(&cwd);
                                cx.notify();
                            })
                        },
                    ),
                )
            })
            .child({
                let remote = remote.clone();
                button(
                    &p,
                    "remote-picker-hidden",
                    if picker.show_hidden {
                        "Hide hidden"
                    } else {
                        "Show hidden"
                    },
                    false,
                )
                .on_click(move |_, _, cx| {
                    remote.update(cx, |model, cx| {
                        model.picker_toggle_hidden();
                        cx.notify();
                    })
                })
            });
        let entries = picker.entries.iter().enumerate().map(|(i, entry)| {
            let into = {
                let remote = remote.clone();
                let path = entry.path.clone();
                move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut gpui::App| {
                    remote.update(cx, |model, cx| {
                        model.picker_browse(Some(&path));
                        cx.notify();
                    })
                }
            };
            let open = (entry.is_git_repo && !entry.is_open_project).then(|| {
                let remote = remote.clone();
                let path = entry.path.clone();
                button(&p, format!("remote-picker-open-{i}"), "Open", true).on_click(
                    move |_, _, cx| {
                        cx.stop_propagation();
                        remote.update(cx, |model, cx| {
                            model.picker_open(&path);
                            cx.notify();
                        })
                    },
                )
            });
            h_flex()
                .id(("remote-picker-entry", i))
                .debug_selector(move || format!("remote-picker-entry-{i}"))
                .w_full()
                .gap_3()
                .px_2()
                .py(px(3.))
                .rounded(px(6.))
                .cursor_pointer()
                .hover(|s| s.bg(p.surface_raised.hsla()))
                .text_size(px(12.5))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(format!("{}/", entry.name)),
                )
                .when(entry.is_git_repo, |row| {
                    row.child(muted(
                        &p,
                        if entry.is_open_project {
                            "git · open".to_string()
                        } else {
                            "git".to_string()
                        },
                    ))
                })
                .children(open)
                .on_click(into)
        });
        let browse_note = if picker.cwd.is_some() && picker.entries.is_empty() {
            Some(muted(&p, "No folders here.".to_string()))
        } else if picker.truncated {
            Some(muted(
                &p,
                "Only the first folders are listed: type a path to go further.".to_string(),
            ))
        } else {
            None
        };

        // Clone: the URL, an optional folder name, and how it is going.
        let clone_form = h_flex()
            .w_full()
            .gap_2()
            .child(div().flex_1().min_w_0().child(Input::new(&self.url)))
            .child(div().w(px(180.)).child(Input::new(&self.name)))
            .child({
                let this = this.clone();
                button(&p, "remote-picker-clone", "Clone", true)
                    .on_click(move |_, _, cx| this.update(cx, |view, cx| view.clone_url(cx)))
            });
        let clone_status = clone_line(&picker).map(|(text, failed)| {
            div()
                .debug_selector(move || {
                    if failed {
                        "remote-picker-clone-error".into()
                    } else {
                        "remote-picker-clone-progress".into()
                    }
                })
                .text_size(px(12.))
                .font_family(MONO_FAMILY)
                .text_color(if failed { p.danger } else { p.ink_2 }.hsla())
                .child(text)
        });

        div().id("remote-picker").size_full().child(
            v_flex()
                .debug_selector(|| "remote-picker".into())
                .id("remote-picker-scroll")
                .size_full()
                .overflow_y_scroll()
                .p_6()
                .gap_2()
                .bg(p.surface_window.hsla())
                .text_color(p.ink.hsla())
                .child(header)
                .children(error)
                .children(picker.status.clone().map(|s| muted(&p, s)))
                .child(section(&p, scan_title))
                .children(scanned)
                .child(section(&p, "BROWSE".to_string()))
                .child(bar)
                .children(entries)
                .children(browse_note)
                .child(section(&p, "CLONE".to_string()))
                .child(clone_form)
                .children(clone_status),
        )
    }
}
