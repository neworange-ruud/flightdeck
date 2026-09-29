//! The overlays' one text field (beads `remote-control-bmej.4.9`): a caret,
//! a selection and an IME composition, fed by the platform's input handler.
//!
//! ## Why not `gpui-component`'s `Input`
//!
//! `InputState` owns its value and is written to from outside with
//! `set_value`, which resets the caret and any composition. The overlays are
//! the opposite: the HOST owns the text (parity with the TUI, whose prompt and
//! palette filter are the one source of truth) and the view re-reads it after
//! every turn. Making `InputState` follow a value it does not own means
//! diffing on each render and restoring the caret by hand, and it still
//! commits composition through its own state. This field is small enough to
//! own exactly the contract the overlays need, so it is written directly on
//! [`EntityInputHandler`], the way the terminal's input is
//! ([`crate::keys::ImeState`]).
//!
//! ## The contract
//!
//! - The field keeps a *local* copy of the text with a caret, a selection and a
//!   marked (composing) range. The layer refreshes it from the host's view
//!   model ([`TextField::set_spec`]) after every host turn; a value that
//!   already matches leaves the caret where the user put it.
//! - Every *committed* edit emits the full new text through the layer's
//!   [`Emit`] as [`OverlayInput::SetText`] (dialogs, the configuration
//!   manager's inline editor) or [`OverlayInput::PaletteFilter`] (the palette).
//!   Text still being composed is drawn underlined but never emitted, so the
//!   host only ever sees "日本", never "n", "に", "にh", ….
//! - Keys the field owns (caret movement, Backspace, Delete, select all,
//!   paste) are handled in [`TextField::key_down`]; printable text and dead
//!   keys are left to propagate so the platform's input method delivers them to
//!   [`EntityInputHandler::replace_text_in_range`] /
//!   [`EntityInputHandler::replace_and_mark_text_in_range`]. Enter, Esc, Tab and
//!   the arrows a form gives the host stay the layer's.
//!
//! Like every emit, the callback must not update the layer synchronously (see
//! [`Emit`]): the field is mid-update when it calls it.

use std::cell::Cell;
use std::ops::Range;
use std::rc::Rc;

use flightdeck::host::{HostEvent, OverlayInput};
use flightdeck::tui::platform::IS_MACOS;
use gpui::{
    canvas, div, fill, prelude::FluentBuilder as _, px, Bounds, ClipboardItem, Context,
    ElementInputHandler, Entity, EntityInputHandler, FocusHandle, IntoElement, KeyDownEvent,
    ParentElement, Pixels, Point, Render, SharedString, Styled, UTF16Selection, Window,
};

use super::{overlay, Emit, MONO};
use crate::theme::Palette;

/// Where a committed edit goes in the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sink {
    /// A dialog's input, or the configuration manager's inline edit.
    SetText,
    /// The command palette's filter.
    PaletteFilter,
}

impl Sink {
    fn event(self, text: String) -> HostEvent {
        overlay(match self {
            Sink::SetText => OverlayInput::SetText(text),
            Sink::PaletteFilter => OverlayInput::PaletteFilter(text),
        })
    }
}

/// How the field is drawn: in its own box (a dialog's field), or bare inside
/// a container that draws the box (the palette's row, a configuration row).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Look {
    /// A bordered input.
    Boxed { mono: bool },
    /// Just the text and caret, at `size`.
    Bare { size: f32, mono: bool },
}

impl Look {
    fn caret_height(self) -> f32 {
        match self {
            Look::Boxed { .. } => 16.,
            Look::Bare { size, .. } => (size * 1.2).round(),
        }
    }
}

/// What the layer wants the field to show this turn, read from the view model.
#[derive(Debug, Clone, PartialEq)]
pub struct FieldSpec {
    /// The host's current text.
    pub text: String,
    /// Faint text shown while the field is empty.
    pub placeholder: &'static str,
    pub look: Look,
    pub sink: Sink,
    /// Whether Left/Right belong to the field. A form whose arrows mean
    /// something to the host (the folder browser's `←`/`→`) says no.
    pub caret_keys: bool,
}

/// The overlays' text field. See the module docs.
pub struct TextField {
    /// The handle the platform's input handler is attached to: whichever one
    /// holds the keyboard for the open overlay.
    focus: FocusHandle,
    emit: Emit,
    sink: Sink,
    look: Look,
    placeholder: SharedString,
    /// Whether the open overlay has a field at all.
    active: bool,
    /// The local text, composition included.
    text: String,
    /// The last value the host is known to hold: what was synced or emitted.
    committed: String,
    /// The selection is `anchor..cursor` in either order; byte offsets.
    anchor: usize,
    cursor: usize,
    /// The composing range, in bytes.
    marked: Option<Range<usize>>,
    /// Where the caret was last painted, for the input method's candidate
    /// window.
    caret_bounds: Rc<Cell<Bounds<Pixels>>>,
}

impl TextField {
    /// An empty, inactive field committing through `emit`.
    pub fn new(emit: Emit, focus: FocusHandle) -> Self {
        Self {
            focus,
            emit,
            sink: Sink::SetText,
            look: Look::Boxed { mono: false },
            placeholder: SharedString::default(),
            active: false,
            text: String::new(),
            committed: String::new(),
            anchor: 0,
            cursor: 0,
            marked: None,
            caret_bounds: Rc::default(),
        }
    }

    /// The local text, composition included.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The caret, as a byte offset into [`TextField::text`].
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// The selection as byte offsets; empty when it is only a caret.
    pub fn selection(&self) -> Range<usize> {
        self.anchor.min(self.cursor)..self.anchor.max(self.cursor)
    }

    /// Whether an input method is composing text in the field.
    pub fn composing(&self) -> bool {
        self.marked.is_some()
    }

    /// Follow the host: show `spec` (or nothing) with `focus` as the keyboard
    /// holder. A text that differs from the local one replaces it with the
    /// caret at the end, unless a composition is under way (the host's value
    /// does not include it, and would wipe it out).
    pub fn set_spec(
        &mut self,
        spec: Option<FieldSpec>,
        focus: &FocusHandle,
        cx: &mut Context<Self>,
    ) {
        let Some(spec) = spec else {
            if self.active {
                self.active = false;
                self.marked = None;
                cx.notify();
            }
            return;
        };
        let was_active = std::mem::replace(&mut self.active, true);
        self.focus = focus.clone();
        self.sink = spec.sink;
        self.look = spec.look;
        self.placeholder = spec.placeholder.into();
        self.committed.clone_from(&spec.text);
        if self.marked.is_none() && (!was_active || self.text != spec.text) {
            self.text = spec.text;
            self.anchor = self.text.len();
            self.cursor = self.text.len();
        }
        cx.notify();
    }

    /// A key press the field may own; whether it did (the layer stops the
    /// event when so). Anything that is not caret movement or deletion is
    /// left for the input method, and everything is left to it while a
    /// composition is under way.
    pub fn key_down(
        &mut self,
        event: &KeyDownEvent,
        caret_keys: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.active || self.marked.is_some() {
            return false;
        }
        let ks = &event.keystroke;
        let m = &ks.modifiers;
        let extend = m.shift;
        // The platform's "command" key: Cmd on macOS, Ctrl elsewhere.
        let command = if IS_MACOS { m.platform } else { m.control };
        if m.alt || (m.platform && m.control) {
            return false;
        }
        match ks.key.as_str() {
            "a" if command => {
                self.anchor = 0;
                self.cursor = self.text.len();
            }
            "v" if command => {
                if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    self.insert(&paste_text(&text), window, cx);
                }
            }
            // Cmd (Ctrl elsewhere) with an arrow is line start or end; a plain
            // arrow moves a character, or collapses a selection to its edge.
            "left" if caret_keys => {
                let to = if command {
                    0
                } else if self.anchor != self.cursor && !extend {
                    self.selection().start
                } else {
                    prev_boundary(&self.text, self.cursor)
                };
                self.move_to(to, extend);
            }
            "right" if caret_keys => {
                let to = if command {
                    self.text.len()
                } else if self.anchor != self.cursor && !extend {
                    self.selection().end
                } else {
                    next_boundary(&self.text, self.cursor)
                };
                self.move_to(to, extend);
            }
            "home" => self.move_to(0, extend),
            "end" => self.move_to(self.text.len(), extend),
            "backspace" if !command => {
                let mut range = self.selection();
                if range.is_empty() {
                    range.start = prev_boundary(&self.text, self.cursor);
                }
                self.delete(range, window, cx);
            }
            "delete" if !command => {
                let mut range = self.selection();
                if range.is_empty() {
                    range.end = next_boundary(&self.text, self.cursor);
                }
                self.delete(range, window, cx);
            }
            _ => return false,
        }
        cx.notify();
        true
    }

    fn move_to(&mut self, at: usize, extend: bool) {
        self.cursor = at;
        if !extend {
            self.anchor = at;
        }
    }

    fn delete(&mut self, range: Range<usize>, window: &mut Window, cx: &mut Context<Self>) {
        self.text.replace_range(range.clone(), "");
        self.anchor = range.start;
        self.cursor = range.start;
        self.settle(window, cx);
    }

    /// Replace the selection with `text` and commit.
    fn insert(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let range = self.selection();
        self.text.replace_range(range.clone(), text);
        let at = range.start + text.len();
        self.anchor = at;
        self.cursor = at;
        self.settle(window, cx);
    }

    /// Tell the host the text, if it is settled (no composition) and differs
    /// from what the host holds.
    fn settle(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.marked.is_some() || self.text == self.committed {
            return;
        }
        self.committed.clone_from(&self.text);
        (self.emit)(self.sink.event(self.text.clone()), window, cx);
    }

    /// The bytes a platform range names, or the composition, or the selection.
    fn edit_range(&self, given: Option<Range<usize>>) -> Range<usize> {
        match given {
            Some(r) => utf16_to_byte(&self.text, r.start)..utf16_to_byte(&self.text, r.end),
            None => self.marked.clone().unwrap_or_else(|| self.selection()),
        }
    }
}

impl EntityInputHandler for TextField {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let start = utf16_to_byte(&self.text, range.start);
        let end = utf16_to_byte(&self.text, range.end).max(start);
        *adjusted_range = Some(byte_to_utf16(&self.text, start)..byte_to_utf16(&self.text, end));
        Some(self.text[start..end].to_string())
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let r = self.selection();
        Some(UTF16Selection {
            range: byte_to_utf16(&self.text, r.start)..byte_to_utf16(&self.text, r.end),
            reversed: self.anchor > self.cursor,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked
            .as_ref()
            .map(|r| byte_to_utf16(&self.text, r.start)..byte_to_utf16(&self.text, r.end))
    }

    /// The input method accepts what it was composing as ordinary text.
    fn unmark_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.marked = None;
        self.settle(window, cx);
        cx.notify();
    }

    fn paste(&mut self, item: ClipboardItem, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = item.text() {
            self.insert(&paste_text(&text), window, cx);
            cx.notify();
        }
    }

    /// Committed text (a typed character, or the IME's final choice).
    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = self.edit_range(range);
        let text = printable(text);
        self.text.replace_range(range.clone(), &text);
        let at = range.start + text.len();
        self.anchor = at;
        self.cursor = at;
        self.marked = None;
        self.settle(window, cx);
        cx.notify();
    }

    /// Text still being composed: shown, underlined, not emitted.
    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        new_text: &str,
        new_selected_range: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = self.edit_range(range);
        let text = printable(new_text);
        self.text.replace_range(range.clone(), &text);
        self.marked = (!text.is_empty()).then(|| range.start..range.start + text.len());
        // The method's own selection is relative to the new text.
        let (from, to) = match new_selected_range {
            Some(r) => (utf16_to_byte(&text, r.start), utf16_to_byte(&text, r.end)),
            None => (text.len(), text.len()),
        };
        self.anchor = range.start + from;
        self.cursor = range.start + to;
        // An emptied composition is over: commit whatever it left behind.
        self.settle(window, cx);
        cx.notify();
    }

    fn set_selected_text_range(
        &mut self,
        range_utf16: Range<usize>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.anchor = utf16_to_byte(&self.text, range_utf16.start);
        self.cursor = utf16_to_byte(&self.text, range_utf16.end).max(self.anchor);
        cx.notify();
    }

    /// Where the input method's candidate window goes: at the caret, which is
    /// the end of what is being composed.
    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let caret = self.caret_bounds.get();
        Some(if caret.size.width > px(0.) {
            caret
        } else {
            element_bounds
        })
    }

    /// Clicks do not place the caret (yet), so there is no hit-testing to do.
    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

impl Render for TextField {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = *Palette::global(cx);
        let entity: Entity<Self> = cx.entity();
        let focus = self.focus.clone();
        let collapsed = self.anchor == self.cursor;
        let accent = p.accent.hsla();

        // The caret is a canvas so its painted bounds can be remembered for the
        // input method's candidate window.
        let cell = self.caret_bounds.clone();
        let mut caret = Some(
            canvas(
                move |bounds, _, _| cell.set(bounds),
                move |bounds, (), window, _| {
                    if collapsed {
                        window.paint_quad(fill(bounds, accent));
                    }
                },
            )
            .w(px(1.5))
            .h(px(self.look.caret_height()))
            .flex_shrink_0(),
        );

        let sel = self.selection();
        let mut cuts = vec![0, self.text.len(), self.cursor, sel.start, sel.end];
        if let Some(m) = &self.marked {
            cuts.extend([m.start, m.end]);
        }
        cuts.sort_unstable();
        cuts.dedup();

        let mut row = div()
            .flex()
            .flex_row()
            .items_center()
            .overflow_hidden()
            .whitespace_nowrap()
            .text_color(p.ink.hsla());
        for (i, &at) in cuts.iter().enumerate() {
            if at == self.cursor {
                row = row.children(caret.take());
            }
            let Some(&next) = cuts.get(i + 1) else {
                continue;
            };
            let composing = self
                .marked
                .as_ref()
                .is_some_and(|m| m.start <= at && next <= m.end);
            let selected = sel.start <= at && next <= sel.end;
            row = row.child(
                div()
                    .child(self.text[at..next].to_string())
                    .when(composing, |d| d.underline())
                    .when(selected, |d| d.bg(p.terminal_selection.hsla())),
            );
        }
        if self.text.is_empty() {
            row = row.child(
                div()
                    .pl(px(2.))
                    .text_color(p.faint.hsla())
                    .child(self.placeholder.clone()),
            );
        }

        // The platform's input handler is attached where the field is painted,
        // and only while its focus handle holds the keyboard.
        let handler = canvas(
            |_, _, _| {},
            move |bounds, (), window, cx| {
                window.handle_input(&focus, ElementInputHandler::new(bounds, entity), cx);
            },
        )
        .absolute()
        .size_full();

        let root = div().relative().child(row).child(handler);
        match self.look {
            Look::Boxed { mono } => root
                .flex()
                .flex_row()
                .items_center()
                .h(px(34.))
                .px(px(10.))
                .rounded(px(7.))
                .border_1()
                .border_color(p.border_strong.hsla())
                .bg(p.surface_input.hsla())
                .when(mono, |d| d.font_family(MONO).text_size(px(12.5))),
            Look::Bare { size, mono } => root
                .flex_1()
                .min_w_0()
                .text_size(px(size))
                .when(mono, |d| d.font_family(MONO)),
        }
    }
}

/// `text` without control characters: the host's text fields take printable
/// characters only, so a stray tab or newline is dropped here.
fn printable(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).collect()
}

/// A pasted text as one line: line breaks become a space (a trailing one is
/// dropped), other control characters go.
fn paste_text(text: &str) -> String {
    let joined = text
        .trim_end_matches(['\r', '\n'])
        .replace("\r\n", " ")
        .replace(['\r', '\n'], " ");
    printable(&joined)
}

/// The boundary before `at`, or 0.
fn prev_boundary(text: &str, at: usize) -> usize {
    text[..at].char_indices().next_back().map_or(0, |(i, _)| i)
}

/// The boundary after `at`, or `at` at the end.
fn next_boundary(text: &str, at: usize) -> usize {
    text[at..].chars().next().map_or(at, |c| at + c.len_utf8())
}

/// UTF-16 offset of byte `at`.
fn byte_to_utf16(text: &str, at: usize) -> usize {
    text[..at.min(text.len())].encode_utf16().count()
}

/// Byte offset of UTF-16 offset `at` (rounded up to a character, clamped to
/// the text).
fn utf16_to_byte(text: &str, at: usize) -> usize {
    let mut units = 0;
    for (i, c) in text.char_indices() {
        if units >= at {
            return i;
        }
        units += c.len_utf16();
    }
    text.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_offsets_convert_both_ways() {
        // "a", a two-unit astral character, a two-byte character, "z".
        let text = "a😀éz";
        assert_eq!(byte_to_utf16(text, 0), 0);
        assert_eq!(byte_to_utf16(text, 1), 1);
        assert_eq!(byte_to_utf16(text, 5), 3);
        assert_eq!(byte_to_utf16(text, 7), 4);
        assert_eq!(utf16_to_byte(text, 0), 0);
        assert_eq!(utf16_to_byte(text, 1), 1);
        assert_eq!(utf16_to_byte(text, 3), 5);
        assert_eq!(utf16_to_byte(text, 4), 7);
        assert_eq!(utf16_to_byte(text, 99), text.len());
        // Half of a surrogate pair rounds up to the next character.
        assert_eq!(utf16_to_byte(text, 2), 5);
    }

    #[test]
    fn the_caret_steps_by_character() {
        let text = "a😀é";
        assert_eq!(next_boundary(text, 0), 1);
        assert_eq!(next_boundary(text, 1), 5);
        assert_eq!(next_boundary(text, text.len()), text.len());
        assert_eq!(prev_boundary(text, text.len()), 5);
        assert_eq!(prev_boundary(text, 5), 1);
        assert_eq!(prev_boundary(text, 0), 0);
    }

    #[test]
    fn a_paste_lands_as_one_line() {
        assert_eq!(paste_text("ed\n"), "ed");
        assert_eq!(paste_text("a\nb"), "a b");
        assert_eq!(paste_text("a\r\nb\tc"), "a bc");
    }
}
