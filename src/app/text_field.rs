//! A single-line text field with input-method support (R2, R3, R8).
//!
//! The shell path and the host form are the places a user types free text before a
//! terminal exists, so they need the same input path every other text entry uses:
//! an input method composes first and commits once, instead of sending a letter per
//! key.
//!
//! The model is one line with a caret: typing inserts at the caret, Backspace and
//! Delete remove before and after it, and the arrow, Home and End keys move it.
//! That is what makes a saved host editable without retyping the whole record, and
//! it is deliberately not an editor: no selection, no undo, no wrapping.

use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};

use gpui::prelude::*;
use gpui::{
    App, Bounds, Context, ElementInputHandler, EntityInputHandler, FocusHandle, KeyDownEvent,
    MouseButton, Pixels, Render, SharedString, UTF16Selection, Window, canvas, div, px,
};

use crate::app::theme::{self, Metrics, palette, radius, spacing};

/// The character drawn where the caret is.
///
/// A typed bar rather than a measured one: the field is one line of plain text, and
/// a real text layout for a caret would be worth building when the field grows
/// selection. The tests read the model, not this character.
const CARET: &str = "│";

/// The character drawn in place of a masked character.
const MASK: char = '•';

/// What the field does with the text the user submitted.
///
/// The window comes with it because a submit usually replaces the field with
/// whatever the text started, and moving keyboard focus needs the window. The
/// answer says whether the text became something real: if it did, the field
/// empties itself, so pressing Enter again does not repeat the same submit.
type SubmitHandler = Box<dyn Fn(String, &mut Window, &mut App) -> bool + 'static>;

/// A one-line text input.
pub struct TextField {
    /// Element id, unique per field, so interactivity and click handling work.
    id: SharedString,
    text: String,
    /// Byte index into `text`, always on a character boundary.
    caret: usize,
    marked: Option<String>,
    placeholder: SharedString,
    focus: FocusHandle,
    submit: Option<SubmitHandler>,
    /// Whether the line is drawn as bullets.
    ///
    /// A password or a passphrase is typed into a field like any other, but what is
    /// on screen must not be readable over a shoulder. The model keeps the real text:
    /// only the drawing changes, so the input method path stays exactly the same.
    masked: bool,
}

impl TextField {
    /// Builds an empty field with a placeholder.
    pub fn new(placeholder: impl Into<SharedString>, cx: &mut Context<Self>) -> Self {
        Self::with_text(placeholder, String::new(), cx)
    }

    /// Builds a field that already holds text, with the caret at the end.
    pub fn with_text(
        placeholder: impl Into<SharedString>,
        text: impl Into<String>,
        cx: &mut Context<Self>,
    ) -> Self {
        let text = text.into();
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let id = SharedString::from(format!(
            "text-field.{}",
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        Self {
            id,
            caret: text.len(),
            text,
            marked: None,
            placeholder: placeholder.into(),
            focus: cx.focus_handle(),
            submit: None,
            masked: false,
        }
    }

    /// Replaces the text, and puts the caret at its end.
    ///
    /// A field that is reused for another attempt has to start from what that
    /// attempt needs, not from what was left in it.
    pub fn set_text(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        self.text = text.into();
        self.caret = self.text.len();
        self.marked = None;
        cx.notify();
    }

    /// Draws the line as bullets instead of characters.
    pub fn set_masked(&mut self, masked: bool, cx: &mut Context<Self>) {
        if self.masked != masked {
            self.masked = masked;
            cx.notify();
        }
    }

    /// Whether the line is drawn as bullets.
    #[cfg(test)]
    #[must_use]
    pub fn is_masked(&self) -> bool {
        self.masked
    }

    /// The current text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The focus handle the caller focuses to type here.
    #[must_use]
    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    /// Called with the text when the user presses Enter.
    pub fn on_submit(&mut self, submit: impl Fn(String, &mut Window, &mut App) -> bool + 'static) {
        self.submit = Some(Box::new(submit));
    }

    fn clear(&mut self, cx: &mut Context<Self>) {
        self.text.clear();
        self.caret = 0;
        self.marked = None;
        cx.notify();
    }

    /// Inserts text at the caret.
    fn insert(&mut self, text: &str, cx: &mut Context<Self>) {
        self.text.insert_str(self.caret, text);
        self.caret += text.len();
        cx.notify();
    }

    fn backspace(&mut self, cx: &mut Context<Self>) {
        let Some((index, _)) = self.text[..self.caret].char_indices().next_back() else {
            return;
        };
        self.text.replace_range(index..self.caret, "");
        self.caret = index;
        cx.notify();
    }

    fn delete_forward(&mut self, cx: &mut Context<Self>) {
        let Some(character) = self.text[self.caret..].chars().next() else {
            return;
        };
        let end = self.caret + character.len_utf8();
        self.text.replace_range(self.caret..end, "");
        cx.notify();
    }

    fn move_caret(&mut self, caret: usize, cx: &mut Context<Self>) {
        if self.caret != caret {
            self.caret = caret;
            cx.notify();
        }
    }

    fn caret_left(&mut self, cx: &mut Context<Self>) {
        if let Some((index, _)) = self.text[..self.caret].char_indices().next_back() {
            self.move_caret(index, cx);
        }
    }

    fn caret_right(&mut self, cx: &mut Context<Self>) {
        if let Some(character) = self.text[self.caret..].chars().next() {
            let caret = self.caret + character.len_utf8();
            self.move_caret(caret, cx);
        }
    }

    fn handle_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "enter" => {
                let text = self.text.clone();
                let consumed = match &self.submit {
                    Some(submit) => submit(text, window, cx),
                    None => false,
                };
                if consumed {
                    self.clear(cx);
                }
                cx.stop_propagation();
            }
            "escape" => {
                self.clear(cx);
                cx.stop_propagation();
            }
            "backspace" => {
                self.backspace(cx);
                cx.stop_propagation();
            }
            "delete" => {
                self.delete_forward(cx);
                cx.stop_propagation();
            }
            "left" => {
                self.caret_left(cx);
                cx.stop_propagation();
            }
            "right" => {
                self.caret_right(cx);
                cx.stop_propagation();
            }
            "home" => {
                self.move_caret(0, cx);
                cx.stop_propagation();
            }
            "end" => {
                self.move_caret(self.text.len(), cx);
                cx.stop_propagation();
            }
            _ => {}
        }
    }

    /// The line to draw, including the caret where it currently is.
    fn display(&self, focused: bool) -> String {
        if self.masked {
            let bullets: String = self
                .text
                .chars()
                .map(|_| MASK)
                .chain(
                    self.marked
                        .as_deref()
                        .unwrap_or_default()
                        .chars()
                        .map(|_| MASK),
                )
                .collect();
            let before = self.text[..self.caret].chars().count();
            return if focused && self.text.is_empty() && self.marked.is_none() {
                format!("{CARET}{}", self.placeholder)
            } else if focused {
                let (head, tail) = bullets.split_at(
                    bullets
                        .char_indices()
                        .nth(before)
                        .map_or(bullets.len(), |(index, _)| index),
                );
                format!("{head}{CARET}{tail}")
            } else {
                bullets
            };
        }
        if self.text.is_empty() {
            let placeholder = self.placeholder.to_string();
            return if focused {
                format!("{CARET}{placeholder}")
            } else {
                placeholder
            };
        }
        match self.marked.as_deref() {
            Some(marked) => format!(
                "{}{marked}{}",
                &self.text[..self.caret],
                &self.text[self.caret..]
            ),
            None if focused => format!(
                "{}{CARET}{}",
                &self.text[..self.caret],
                &self.text[self.caret..]
            ),
            None => self.text.clone(),
        }
    }
}

impl Render for TextField {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let metrics = Metrics::default();
        let entity = cx.entity();
        let focus = self.focus.clone();
        let focused = self.focus.is_focused(window);
        let empty = self.text.is_empty();
        let display = SharedString::from(self.display(focused));

        div()
            .id(self.id.clone())
            .relative()
            .flex()
            .items_center()
            .flex_1()
            .min_w(px(0.0))
            .h(metrics.scale(26.0))
            .px(metrics.scale(spacing::TIGHT))
            .rounded(px(radius::SMALL))
            .bg(theme::swatch(palette::SURFACE_RAISED))
            .border_1()
            .border_color(theme::swatch(if focused {
                palette::ACCENT
            } else {
                palette::BORDER_STRONG
            }))
            .text_size(metrics.label_size())
            .text_color(theme::swatch(if empty {
                palette::TEXT_MUTED
            } else {
                palette::TEXT_PRIMARY
            }))
            .track_focus(&self.focus)
            .cursor_text()
            // Clicking a field puts the caret in it, which is what a form field has
            // to do: a user who clicks the address of a saved host expects to type
            // there, not to have to Tab into it.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _event, window, cx| {
                    window.focus(&this.focus);
                    cx.notify();
                }),
            )
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                this.handle_key(event, window, cx);
            }))
            .child(div().truncate().child(display))
            .child(div().absolute().inset_0().child(canvas(
                move |bounds, _window, _cx| bounds,
                move |bounds, _prepaint, window, cx| {
                    window.handle_input(
                        &focus,
                        ElementInputHandler::new(bounds, entity.clone()),
                        cx,
                    );
                },
            )))
    }
}

impl EntityInputHandler for TextField {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        *adjusted_range = Some(range_utf16);
        Some(self.text.clone())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let caret = self.text[..self.caret].encode_utf16().count();
        Some(UTF16Selection {
            range: caret..caret,
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.marked
            .as_ref()
            .map(|marked| 0..marked.encode_utf16().count())
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.marked.take().is_some() {
            cx.notify();
        }
    }

    fn replace_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The composition was never inserted, so committing it only has to insert
        // the final text once, at the caret.
        self.marked = None;
        self.insert(text, cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        new_text: &str,
        _new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The composition is shown in the field but is only added to the text when
        // the input method commits it.
        self.marked = if new_text.is_empty() {
            None
        } else {
            Some(new_text.to_owned())
        };
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        Some(element_bounds)
    }

    fn character_index_for_point(
        &mut self,
        _point: gpui::Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}
