//! A multi-line text box: wrapped lines, a caret that moves by visual line, selection,
//! the clipboard and input methods.
//!
//! The editing rules are [`LineModel`]'s, with line breaks kept, so the area and the
//! one-line field behave the same way on every key they share. What this adds is the
//! second dimension: the text wraps at the area's width, Up and Down move between drawn
//! lines keeping the column the caret came from, Home and End stay on the caret's own
//! line, Enter inserts a break, and the box scrolls so the caret is always in view.
//!
//! The area does not submit anything. Enter is a line break here; a dialog that holds an
//! area answers Command/Ctrl+Enter and Escape itself, before the area sees them.

use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use gpui::prelude::*;
use gpui::{
    App, AvailableSpace, Bounds, ClipboardItem, Context, Element, ElementId, ElementInputHandler,
    Entity, EntityInputHandler, FocusHandle, GlobalElementId, KeyDownEvent, LayoutId, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, Render, ScrollHandle,
    SharedString, Size, Style, TextAlign, TextRun, UTF16Selection, Window, WrappedLine, div, fill,
    point, px, relative, size,
};

use crate::app::nav::Platform;
use crate::app::text_field::LineModel;
use crate::app::theme::{self, palette, radius, spacing};

/// Width of the caret.
const CARET_WIDTH: f32 = 1.5;

/// The least width a selected line break is drawn with, so a selection that runs over
/// an empty line still shows that line is in it.
const BREAK_MARK_WIDTH: f32 = 4.0;

/// A multi-line text input.
pub struct TextArea {
    id: SharedString,
    model: LineModel,
    /// What an input method is composing, shown at the caret until it commits.
    marked: Option<String>,
    focus: FocusHandle,
    /// Whether the left button is down after a press in the area: dragging selects.
    selecting: bool,
    /// The column Up and Down aim for, kept across a run of vertical moves so a short
    /// line on the way does not pull the caret left for good.
    goal_x: Option<Pixels>,
    /// Whether the caret moved and the box should scroll to show it.
    reveal: bool,
    scroll: ScrollHandle,
    /// The tallest the box grows before it scrolls.
    max_height: Pixels,
    /// The lines as they were last drawn, to map the pointer and the arrows.
    layout: Rc<RefCell<Option<AreaLayout>>>,
}

/// One drawn frame of the text: each logical line, shaped and wrapped.
#[derive(Clone)]
struct AreaLayout {
    lines: Vec<DrawnLine>,
    line_height: Pixels,
    /// The element's own bounds when it was painted, in window coordinates.
    bounds: Bounds<Pixels>,
}

#[derive(Clone)]
struct DrawnLine {
    /// Where the line starts in the drawn text (the text with the composition in it).
    start: usize,
    /// How far down the element the line starts.
    top: Pixels,
    shaped: WrappedLine,
}

impl AreaLayout {
    /// The point of a drawn-text offset, relative to the element's top-left corner.
    fn position(&self, offset: usize) -> Point<Pixels> {
        let line = self
            .lines
            .iter()
            .rev()
            .find(|line| line.start <= offset)
            .or_else(|| self.lines.first());
        let Some(line) = line else {
            return Point::default();
        };
        let local = (offset - line.start).min(line.shaped.len());
        let at = line
            .shaped
            .position_for_index(local, self.line_height)
            .unwrap_or_default();
        point(at.x, line.top + at.y)
    }

    /// The drawn-text offset closest to a point relative to the element.
    fn offset(&self, position: Point<Pixels>) -> usize {
        let Some(line) = self
            .lines
            .iter()
            .rev()
            .find(|line| line.top <= position.y)
            .or_else(|| self.lines.first())
        else {
            return 0;
        };
        let local = point(
            position.x.max(px(0.0)),
            (position.y - line.top).max(px(0.0)),
        );
        let index = match line
            .shaped
            .closest_index_for_position(local, self.line_height)
        {
            Ok(index) | Err(index) => index,
        };
        line.start + index.min(line.shaped.len())
    }
}

impl TextArea {
    /// An area holding `text`, with the caret at its end.
    pub fn new(text: impl Into<String>, max_height: Pixels, cx: &mut Context<Self>) -> Self {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let mut model = LineModel::new(String::new());
        model.set_keeps_line_breaks(true);
        model.insert(&text.into());
        Self {
            id: SharedString::from(format!(
                "text-area.{}",
                SEQUENCE.fetch_add(1, Ordering::Relaxed)
            )),
            model,
            marked: None,
            focus: cx.focus_handle(),
            selecting: false,
            goal_x: None,
            reveal: true,
            scroll: ScrollHandle::new(),
            max_height,
            layout: Rc::new(RefCell::new(None)),
        }
    }

    /// The text as it is now.
    #[must_use]
    pub fn text(&self) -> &str {
        self.model.text()
    }

    /// The handle that holds the keyboard while the area is being typed in.
    #[must_use]
    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    /// Replaces everything, with the caret at the end.
    pub fn replace_all(&mut self, text: &str, cx: &mut Context<Self>) {
        self.marked = None;
        self.edit(cx, |model| {
            model.select_all();
            model.insert(text);
        });
    }

    /// Replaces the text, for a test.
    #[cfg(test)]
    pub fn set_text_for_test(&mut self, text: &str, cx: &mut Context<Self>) {
        self.replace_all(text, cx);
    }

    fn edit(&mut self, cx: &mut Context<Self>, change: impl FnOnce(&mut LineModel)) {
        change(&mut self.model);
        self.goal_x = None;
        self.reveal = true;
        cx.notify();
    }

    fn copy(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = self.model.selected_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(text.to_owned()));
        }
    }

    fn cut(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = self.model.take_selection() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            self.reveal = true;
            cx.notify();
        }
    }

    fn paste(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.marked = None;
            self.edit(cx, |model| model.insert(&text));
        }
    }

    /// The start and end of the logical line the caret is on.
    fn line_bounds(&self) -> Range<usize> {
        let text = self.model.text();
        let caret = self.model.caret();
        let start = text[..caret].rfind('\n').map_or(0, |index| index + 1);
        let end = text[caret..]
            .find('\n')
            .map_or(text.len(), |index| caret + index);
        start..end
    }

    /// Moves to the drawn line above (`-1`) or below (`1`), keeping the goal column.
    fn vertical(&mut self, direction: i32, extend: bool, cx: &mut Context<Self>) {
        let Some(layout) = self.layout.borrow().clone() else {
            return;
        };
        if !extend && !self.model.selection().is_empty() {
            let edge = if direction < 0 {
                self.model.selection().start
            } else {
                self.model.selection().end
            };
            self.model.move_to(edge);
        }
        let caret = layout.position(self.display_offset(self.model.caret()));
        let goal = *self.goal_x.get_or_insert(caret.x);
        let y = caret.y + layout.line_height * (direction as f32) + layout.line_height / 2.0;
        let target = if y < px(0.0) {
            0
        } else {
            let total = layout.lines.last().map_or(px(0.0), |line| {
                line.top + line.shaped.size(layout.line_height).height
            });
            if y >= total {
                self.model.text().len()
            } else {
                self.text_offset(layout.offset(point(goal, y)))
            }
        };
        if extend {
            self.model.select_to(target);
        } else {
            self.model.move_to(target);
        }
        self.reveal = true;
        cx.notify();
    }

    fn handle_key(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let modifiers = &event.keystroke.modifiers;
        let mac = Platform::current() == Platform::MacOs;
        let command = if mac {
            modifiers.platform
        } else {
            modifiers.control
        };
        let by_word = if mac {
            modifiers.alt
        } else {
            modifiers.control
        };
        let shift = modifiers.shift;
        match event.keystroke.key.as_str() {
            // A modified Enter and Escape belong to the dialog around the area.
            "enter" if command || modifiers.control || modifiers.platform => return,
            "escape" if self.model.selection().is_empty() => return,
            "escape" => {
                let caret = self.model.caret();
                self.edit(cx, |model| model.move_to(caret));
            }
            "enter" => self.edit(cx, |model| model.insert("\n")),
            "tab" if !command && !modifiers.alt => self.edit(cx, |model| model.insert("\t")),
            "backspace" => self.edit(cx, |model| model.backspace(by_word)),
            "delete" => self.edit(cx, |model| model.delete(by_word)),
            "up" if mac && command => self.edit(cx, |model| model.home(shift)),
            "down" if mac && command => self.edit(cx, |model| model.end(shift)),
            "up" => self.vertical(-1, shift, cx),
            "down" => self.vertical(1, shift, cx),
            "left" if mac && command => {
                let start = self.line_bounds().start;
                self.edit(cx, |model| line_go(model, start, shift));
            }
            "right" if mac && command => {
                let end = self.line_bounds().end;
                self.edit(cx, |model| line_go(model, end, shift));
            }
            "left" => self.edit(cx, |model| model.left(by_word, shift)),
            "right" => self.edit(cx, |model| model.right(by_word, shift)),
            "home" if modifiers.control => self.edit(cx, |model| model.home(shift)),
            "end" if modifiers.control => self.edit(cx, |model| model.end(shift)),
            "home" => {
                let start = self.line_bounds().start;
                self.edit(cx, |model| line_go(model, start, shift));
            }
            "end" => {
                let end = self.line_bounds().end;
                self.edit(cx, |model| line_go(model, end, shift));
            }
            "a" if command => self.edit(cx, LineModel::select_all),
            "c" if command => self.copy(cx),
            "x" if command => self.cut(cx),
            "v" if command => self.paste(cx),
            _ => return,
        }
        cx.stop_propagation();
    }

    // -- Mapping between the text and the drawn text -------------------------------

    /// The text as drawn: the composition sits at the caret.
    fn display_text(&self) -> String {
        let caret = self.model.caret();
        let text = self.model.text();
        let mut shown = text[..caret].to_owned();
        if let Some(marked) = &self.marked {
            shown.push_str(marked);
        }
        shown.push_str(&text[caret..]);
        shown
    }

    fn display_offset(&self, offset: usize) -> usize {
        let offset = offset.min(self.model.text().len());
        match &self.marked {
            Some(marked) if offset >= self.model.caret() => offset + marked.len(),
            _ => offset,
        }
    }

    fn text_offset(&self, display: usize) -> usize {
        let caret = self.model.caret();
        let marked = self.marked.as_ref().map_or(0, String::len);
        if display < caret {
            display
        } else if display < caret + marked {
            caret
        } else {
            (display - marked).min(self.model.text().len())
        }
    }

    fn offset_for_point(&self, position: Point<Pixels>) -> usize {
        let layout = self.layout.borrow();
        let Some(layout) = layout.as_ref() else {
            return self.model.text().len();
        };
        let local = position - layout.bounds.origin;
        self.text_offset(layout.offset(local))
    }

    // -- Pointer ---------------------------------------------------------------------

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus);
        let offset = self.offset_for_point(event.position);
        match event.click_count {
            2 => self.model.select_word_at(offset),
            count if count >= 3 => self.model.select_all(),
            _ if event.modifiers.shift => self.model.select_to(offset),
            _ => self.model.move_to(offset),
        }
        self.goal_x = None;
        self.selecting = event.click_count < 2;
        cx.notify();
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if self.selecting && event.pressed_button == Some(MouseButton::Left) {
            let offset = self.offset_for_point(event.position);
            if offset != self.model.caret() {
                self.model.select_to(offset);
                self.reveal = true;
                cx.notify();
            }
        }
    }
}

/// Moves the caret to `offset`, extending the selection when `extend`.
fn line_go(model: &mut LineModel, offset: usize, extend: bool) {
    if extend {
        model.select_to(offset);
    } else {
        model.move_to(offset);
    }
}

impl Render for TextArea {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let metrics = theme::metrics();
        let focused = self.focus.is_focused(window);
        let accent = theme::swatch(palette::ACCENT);
        div()
            .id(self.id.clone())
            .w_full()
            .min_w_0()
            .max_h(self.max_height)
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .rounded(px(radius::MEDIUM))
            .border_1()
            .border_color(if focused {
                accent
            } else {
                theme::swatch(palette::BORDER_STRONG)
            })
            .when(!focused, |area| {
                let hover = theme::swatch(palette::TEXT_MUTED);
                area.hover(move |style| style.border_color(hover))
            })
            .bg(theme::swatch(palette::CANVAS))
            .px(metrics.scale(spacing::TIGHT))
            .py(metrics.scale(spacing::HALF + 2.0))
            .text_color(theme::swatch(palette::TEXT_PRIMARY))
            .track_focus(&self.focus)
            .cursor_text()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    this.on_mouse_down(event, window, cx);
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _window, cx| {
                this.on_mouse_move(event, cx);
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _event: &MouseUpEvent, _window, _cx| {
                    this.selecting = false;
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _event: &MouseUpEvent, _window, _cx| {
                    this.selecting = false;
                }),
            )
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                this.handle_key(event, window, cx);
            }))
            .child(AreaElement { area: cx.entity() })
    }
}

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------

struct AreaElement {
    area: Entity<TextArea>,
}

impl IntoElement for AreaElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

/// Shapes every logical line of `text` at `width`.
fn shape(text: &str, width: Option<Pixels>, window: &Window) -> (Vec<DrawnLine>, Pixels) {
    let style = window.text_style();
    let font_size = style.font_size.to_pixels(window.rem_size());
    let line_height = window.line_height();
    let mut lines = Vec::new();
    let mut top = px(0.0);
    let mut start = 0;
    for piece in text.split('\n') {
        let run = TextRun {
            len: piece.len(),
            font: style.font(),
            color: style.color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let shaped = window
            .text_system()
            .shape_text(
                SharedString::from(piece.to_owned()),
                font_size,
                &[run],
                width,
                None,
            )
            .ok()
            .and_then(|mut lines| (!lines.is_empty()).then(|| lines.remove(0)))
            .unwrap_or_default();
        let height = shaped.size(line_height).height.max(line_height);
        lines.push(DrawnLine { start, top, shaped });
        top += height;
        start += piece.len() + 1;
    }
    (lines, top)
}

impl Element for AreaElement {
    type RequestLayoutState = ();
    type PrepaintState = Option<AreaLayout>;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let text = self.area.read(cx).display_text();
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        let layout = window.request_measured_layout(
            style,
            move |known: Size<Option<Pixels>>, available: Size<AvailableSpace>, window, _cx| {
                let width = known.width.or(match available.width {
                    AvailableSpace::Definite(width) => Some(width),
                    _ => None,
                });
                let (_, height) = shape(&text, width, window);
                size(width.unwrap_or(px(0.0)), height)
            },
        );
        (layout, ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let area = self.area.read(cx);
        let (lines, _) = shape(&area.display_text(), Some(bounds.size.width), window);
        let layout = AreaLayout {
            lines,
            line_height: window.line_height(),
            bounds,
        };
        // Keep the caret inside the box after it moved: the box scrolls by the least
        // that shows the caret's line.
        if area.reveal {
            let caret = layout.position(area.display_offset(area.model.caret()));
            let scroll = area.scroll.clone();
            let viewport = scroll.bounds();
            let offset = scroll.offset();
            if viewport.size.height > px(0.0) {
                // Positions inside the content, from the top of the scrolled content.
                let content_top = bounds.top() - offset.y - viewport.top();
                let caret_top = content_top + caret.y;
                let caret_bottom = caret_top + layout.line_height;
                let pad = layout.line_height / 2.0;
                let mut y = -offset.y;
                if caret_top - pad < y {
                    y = (caret_top - pad).max(px(0.0));
                } else if caret_bottom + pad > y + viewport.size.height {
                    y = caret_bottom + pad - viewport.size.height;
                }
                if y != -offset.y {
                    scroll.set_offset(point(offset.x, -y));
                }
            }
            self.area.update(cx, |area, _cx| area.reveal = false);
        }
        Some(layout)
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some(layout) = prepaint.take() else {
            return;
        };
        let (focus, selection, caret) = {
            let area = self.area.read(cx);
            let selection = area.model.selection();
            (
                area.focus.clone(),
                area.display_offset(selection.start)..area.display_offset(selection.end),
                area.display_offset(area.model.caret()),
            )
        };
        window.handle_input(
            &focus,
            ElementInputHandler::new(bounds, self.area.clone()),
            cx,
        );
        let focused = focus.is_focused(window);
        let line_height = layout.line_height;
        let tint = theme::faded(
            theme::swatch(palette::ACCENT),
            if focused { 0.32 } else { 0.14 },
        );
        // The selection, one band per drawn row it covers.
        if !selection.is_empty() {
            let start = layout.position(selection.start);
            let end = layout.position(selection.end);
            let mut y = start.y;
            while y <= end.y {
                let from = if y == start.y { start.x } else { px(0.0) };
                let to = if y == end.y { end.x } else { bounds.size.width };
                if to > from || y != end.y {
                    window.paint_quad(fill(
                        Bounds::from_corners(
                            point(bounds.left() + from, bounds.top() + y),
                            point(
                                bounds.left() + to.max(from + px(BREAK_MARK_WIDTH)),
                                bounds.top() + y + line_height,
                            ),
                        ),
                        tint,
                    ));
                }
                y += line_height;
            }
        }
        for line in &layout.lines {
            let _ = line.shaped.paint(
                point(bounds.left(), bounds.top() + line.top),
                line_height,
                TextAlign::Left,
                Some(bounds),
                window,
                cx,
            );
        }
        if focused && selection.is_empty() {
            let at = layout.position(caret);
            window.paint_quad(fill(
                Bounds::new(
                    point(bounds.left() + at.x, bounds.top() + at.y),
                    size(px(CARET_WIDTH), line_height),
                ),
                theme::swatch(palette::ACCENT),
            ));
        }
        *self.area.read(cx).layout.borrow_mut() = Some(layout);
    }
}

// ---------------------------------------------------------------------------
// Input methods
// ---------------------------------------------------------------------------

fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

fn offset_from_utf16(text: &str, utf16: usize) -> usize {
    let mut counted = 0;
    for (index, character) in text.char_indices() {
        if counted >= utf16 {
            return index;
        }
        counted += character.len_utf16();
    }
    text.len()
}

impl EntityInputHandler for TextArea {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let text = self.model.text();
        let start = offset_from_utf16(text, range_utf16.start);
        let end = offset_from_utf16(text, range_utf16.end);
        *adjusted_range = Some(utf16_len(&text[..start])..utf16_len(&text[..end]));
        Some(text[start..end].to_owned())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let text = self.model.text();
        let selection = self.model.selection();
        Some(UTF16Selection {
            range: utf16_len(&text[..selection.start])..utf16_len(&text[..selection.end]),
            reversed: self.model.reversed,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        let text = self.model.text();
        let caret = utf16_len(&text[..self.model.caret()]);
        self.marked
            .as_ref()
            .map(|marked| caret..caret + utf16_len(marked))
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.marked.take().is_some() {
            cx.notify();
        }
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.marked.take().is_none()
            && let Some(range) = range_utf16
        {
            let current = self.model.text().to_owned();
            let start = offset_from_utf16(&current, range.start);
            let end = offset_from_utf16(&current, range.end);
            self.model.move_to(start);
            self.model.select_to(end);
        }
        self.edit(cx, |model| model.insert(text));
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        new_text: &str,
        _new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.model.selection().is_empty() {
            self.model.insert("");
        }
        self.marked = (!new_text.is_empty()).then(|| new_text.to_owned());
        self.reveal = true;
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let layout = self.layout.borrow();
        let Some(layout) = layout.as_ref() else {
            return Some(element_bounds);
        };
        let text = self.model.text();
        let at = layout.position(self.display_offset(offset_from_utf16(text, range_utf16.start)));
        let origin = element_bounds.origin + at;
        Some(Bounds::new(
            origin,
            size(px(CARET_WIDTH), layout.line_height),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let offset = self.offset_for_point(point);
        Some(utf16_len(&self.model.text()[..offset]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn the_area_keeps_line_breaks_and_tabs(cx: &mut gpui::TestAppContext) {
        let area = cx.new(|cx| TextArea::new("a\r\nb\tc", px(200.0), cx));
        area.read_with(cx, |area, _| {
            assert_eq!(area.text(), "a\nb\tc", "CRLF is kept as one break");
        });
    }

    #[gpui::test]
    fn home_and_end_stay_on_the_caret_line(cx: &mut gpui::TestAppContext) {
        let area = cx.new(|cx| TextArea::new("first\nsecond\nthird", px(200.0), cx));
        area.update(cx, |area, _| {
            area.model.move_to(8);
            assert_eq!(area.line_bounds(), 6..12);
            area.model.move_to(0);
            assert_eq!(area.line_bounds(), 0..5);
            area.model.move_to(area.model.text().len());
            assert_eq!(area.line_bounds(), 13..18);
        });
    }
}
