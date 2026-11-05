//! A single-line text field with input-method support (R2, R8).
//!
//! The shell path is the one place a user types free text before a terminal
//! exists, so it needs the same input path every other text entry uses: an input
//! method composes first and commits once, instead of sending a letter per key.
//!
//! The model is deliberately small — one line, append and delete at the end, Enter
//! submits, Escape clears — because a full editor is work the product does not need
//! yet; a host form with richer editing arrives with the host management step.

use std::ops::Range;

use gpui::prelude::*;
use gpui::{
    App, Bounds, Context, ElementInputHandler, EntityInputHandler, FocusHandle, KeyDownEvent,
    Pixels, Render, SharedString, UTF16Selection, Window, canvas, div, px,
};

use crate::app::theme::{self, Metrics, palette, radius, spacing};

/// What the field does with the text the user submitted.
type SubmitHandler = Box<dyn Fn(String, &mut App) + 'static>;

/// A one-line text input.
pub struct TextField {
    text: String,
    marked: Option<String>,
    placeholder: SharedString,
    focus: FocusHandle,
    submit: Option<SubmitHandler>,
}

impl TextField {
    /// Builds an empty field with a placeholder.
    pub fn new(placeholder: impl Into<SharedString>, cx: &mut Context<Self>) -> Self {
        Self {
            text: String::new(),
            marked: None,
            placeholder: placeholder.into(),
            focus: cx.focus_handle(),
            submit: None,
        }
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
    pub fn on_submit(&mut self, submit: impl Fn(String, &mut App) + 'static) {
        self.submit = Some(Box::new(submit));
    }

    fn clear(&mut self, cx: &mut Context<Self>) {
        self.text.clear();
        self.marked = None;
        cx.notify();
    }

    fn handle_key(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "enter" => {
                if let Some(submit) = &self.submit {
                    submit(self.text.clone(), cx);
                }
                cx.stop_propagation();
            }
            "escape" => {
                self.clear(cx);
                cx.stop_propagation();
            }
            "backspace" => {
                if self.text.pop().is_some() {
                    cx.notify();
                }
                cx.stop_propagation();
            }
            _ => {}
        }
    }
}

impl Render for TextField {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let metrics = Metrics::default();
        let entity = cx.entity();
        let focus = self.focus.clone();
        let display = if self.text.is_empty() {
            SharedString::from(self.placeholder.to_string())
        } else {
            SharedString::from(self.text.clone())
        };

        div()
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
            .border_color(theme::swatch(palette::BORDER_STRONG))
            .text_size(metrics.label_size())
            .text_color(theme::swatch(if self.text.is_empty() {
                palette::TEXT_MUTED
            } else {
                palette::TEXT_PRIMARY
            }))
            .track_focus(&self.focus)
            .debug_selector(|| "shell.path".to_owned())
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
        let end = self.text.encode_utf16().count();
        Some(UTF16Selection {
            range: end..end,
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
        self.marked = None;
        self.text.push_str(text);
        cx.notify();
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
