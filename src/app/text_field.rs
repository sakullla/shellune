//! A single-line text field with selection, the clipboard and input-method support.
//!
//! The shell path, the host form, the connection form and the searches are the places
//! a user types free text before a terminal exists. They behave like every other text
//! box on the platform: a caret that can be placed with the mouse, a selection made by
//! dragging, Shift+arrows or a double click, and cut, copy, paste and select-all from
//! the keyboard or the right-click menu. An input method composes first and commits
//! once, instead of sending a letter per key.
//!
//! The editing rules live in [`LineModel`], which knows nothing about drawing, so they
//! are tested without a window; [`TextField`] draws the model and routes input to it.
//! It is deliberately not an editor: one line, no undo, no wrapping.
//!
//! A field that keeps line breaks ([`TextField::set_keeps_line_breaks`]) stores them and
//! draws each one as `\n`, so the break is visible in the same single line rather than
//! becoming a space the user cannot see. The snippet body uses this: a saved command may
//! be a short block, and the text a user typed has to be the text that is saved.

use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};

use gpui::prelude::*;
use gpui::{
    App, Bounds, BoxShadow, ClipboardItem, Context, Element, ElementId, ElementInputHandler,
    Entity, EntityInputHandler, FocusHandle, GlobalElementId, KeyDownEvent, LayoutId, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point, Render, ShapedLine,
    SharedString, Style, TextRun, UTF16Selection, UnderlineStyle, Window, div, fill, point, px,
    relative, size,
};

use crate::app::copy::Locale;
use crate::app::icons::{self, Icon};
use crate::app::nav::Platform;
use crate::app::theme::{self, Metrics, palette, radius, spacing};

/// The character drawn in place of a masked character.
const MASK: char = '•';

/// What is drawn in place of a kept line break.
///
/// Two ASCII characters rather than a corner glyph: every font draws them, and a reader
/// can tell a break in the middle of a saved command from a space that was typed there.
const BREAK: &str = "\\n";

/// Width of the caret.
const CARET_WIDTH: f32 = 1.5;

/// What a path field's browse button chooses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Browse {
    /// One file, such as a private key or a file to upload.
    File,
    /// One folder, such as where a download goes.
    Directory,
}

/// What the field does with the text the user submitted.
///
/// The window comes with it because a submit usually replaces the field with
/// whatever the text started, and moving keyboard focus needs the window. The
/// answer says whether the text became something real: if it did, the field
/// empties itself, so pressing Enter again does not repeat the same submit.
type SubmitHandler = Box<dyn Fn(String, &mut Window, &mut App) -> bool + 'static>;

// ---------------------------------------------------------------------------
// The editing model
// ---------------------------------------------------------------------------

/// One line of text with a selection, and every edit a text box offers.
///
/// Offsets are byte indices into `text`, always on a character boundary. An empty
/// selection is the caret. `reversed` says which end the caret is at: a selection made
/// leftwards keeps the caret at its start, so Shift+Right shrinks it from there.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LineModel {
    text: String,
    selection: Range<usize>,
    reversed: bool,
    /// Whether a line break is text this field keeps, as a snippet body does.
    keeps_line_breaks: bool,
}

impl LineModel {
    /// A model holding `text`, with the caret at its end.
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        let end = text.len();
        Self {
            text,
            selection: end..end,
            reversed: false,
            keeps_line_breaks: false,
        }
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    #[must_use]
    pub fn selection(&self) -> Range<usize> {
        self.selection.clone()
    }

    /// Where the caret is: the moving end of the selection.
    #[must_use]
    pub fn caret(&self) -> usize {
        if self.reversed {
            self.selection.start
        } else {
            self.selection.end
        }
    }

    /// The selected text, when anything is selected.
    #[must_use]
    pub fn selected_text(&self) -> Option<&str> {
        (!self.selection.is_empty()).then(|| &self.text[self.selection.clone()])
    }

    /// Replaces everything, with the caret at the end.
    ///
    /// What the field does with line breaks is a property of the field, not of the text
    /// it holds, so a new value does not change it.
    pub fn set_text(&mut self, text: impl Into<String>) {
        let keeps_line_breaks = self.keeps_line_breaks;
        *self = Self::new(text);
        self.keeps_line_breaks = keeps_line_breaks;
    }

    /// Whether this field stores line breaks instead of flattening them.
    #[must_use]
    pub fn keeps_line_breaks(&self) -> bool {
        self.keeps_line_breaks
    }

    /// Sets whether line breaks are text or become spaces.
    pub fn set_keeps_line_breaks(&mut self, keeps: bool) {
        self.keeps_line_breaks = keeps;
    }

    /// Puts the caret at `offset`, dropping the selection.
    pub fn move_to(&mut self, offset: usize) {
        let offset = self.clamp(offset);
        self.selection = offset..offset;
        self.reversed = false;
    }

    /// Moves the caret to `offset` and keeps the other end of the selection where it is.
    pub fn select_to(&mut self, offset: usize) {
        let offset = self.clamp(offset);
        let anchor = if self.reversed {
            self.selection.end
        } else {
            self.selection.start
        };
        if offset < anchor {
            self.selection = offset..anchor;
            self.reversed = true;
        } else {
            self.selection = anchor..offset;
            self.reversed = false;
        }
    }

    pub fn select_all(&mut self) {
        self.selection = 0..self.text.len();
        self.reversed = false;
    }

    /// Selects the word around `offset`, or the run of spaces or punctuation there.
    pub fn select_word_at(&mut self, offset: usize) {
        let offset = self.clamp(offset);
        let class_at = |index: usize| self.text[index..].chars().next().map(char_class);
        // A double click just past the end of a word selects that word.
        let probe = if offset == self.text.len() || class_at(offset) == Some(CharClass::Space) {
            self.previous_char(offset)
        } else {
            offset
        };
        let Some(class) = class_at(probe) else {
            return;
        };
        let mut start = probe;
        while start > 0 {
            let before = self.previous_char(start);
            if class_at(before) != Some(class) {
                break;
            }
            start = before;
        }
        let mut end = probe;
        while end < self.text.len() && class_at(end) == Some(class) {
            end = self.next_char(end);
        }
        self.selection = start..end;
        self.reversed = false;
    }

    /// Moves left: to the start of a selection, or one character (or word) back.
    pub fn left(&mut self, by_word: bool, extend: bool) {
        if !extend && !self.selection.is_empty() {
            self.move_to(self.selection.start);
            return;
        }
        let target = if by_word {
            self.previous_word(self.caret())
        } else {
            self.previous_char(self.caret())
        };
        self.go(target, extend);
    }

    /// Moves right: to the end of a selection, or one character (or word) on.
    pub fn right(&mut self, by_word: bool, extend: bool) {
        if !extend && !self.selection.is_empty() {
            self.move_to(self.selection.end);
            return;
        }
        let target = if by_word {
            self.next_word(self.caret())
        } else {
            self.next_char(self.caret())
        };
        self.go(target, extend);
    }

    pub fn home(&mut self, extend: bool) {
        self.go(0, extend);
    }

    pub fn end(&mut self, extend: bool) {
        self.go(self.text.len(), extend);
    }

    /// Replaces the selection with `text`, leaving the caret after it.
    ///
    /// The field is one line, so line breaks in pasted text become spaces instead of
    /// being dropped silently: a path or a command copied with its newline still
    /// arrives whole. A field that keeps breaks keeps them, normalised to `\n`, so a
    /// body copied out of a script stays the block that was copied.
    pub fn insert(&mut self, text: &str) {
        let text = if self.keeps_line_breaks {
            kept_breaks(text)
        } else {
            single_line(text)
        };
        let range = self.selection.clone();
        self.text.replace_range(range.clone(), &text);
        self.move_to(range.start + text.len());
    }

    /// Deletes the selection, or the character (or word) before the caret.
    pub fn backspace(&mut self, by_word: bool) {
        if self.selection.is_empty() {
            let start = if by_word {
                self.previous_word(self.caret())
            } else {
                self.previous_char(self.caret())
            };
            self.selection = start..self.caret();
        }
        self.insert("");
    }

    /// Deletes the selection, or the character (or word) after the caret.
    pub fn delete(&mut self, by_word: bool) {
        if self.selection.is_empty() {
            let end = if by_word {
                self.next_word(self.caret())
            } else {
                self.next_char(self.caret())
            };
            self.selection = self.caret()..end;
        }
        self.insert("");
    }

    /// Removes the selection and returns it, for cut.
    pub fn take_selection(&mut self) -> Option<String> {
        let taken = self.selected_text()?.to_owned();
        self.insert("");
        Some(taken)
    }

    fn go(&mut self, offset: usize, extend: bool) {
        if extend {
            self.select_to(offset);
        } else {
            self.move_to(offset);
        }
    }

    fn clamp(&self, offset: usize) -> usize {
        let mut offset = offset.min(self.text.len());
        while !self.text.is_char_boundary(offset) {
            offset -= 1;
        }
        offset
    }

    fn previous_char(&self, offset: usize) -> usize {
        self.text[..offset]
            .char_indices()
            .next_back()
            .map_or(0, |(index, _)| index)
    }

    fn next_char(&self, offset: usize) -> usize {
        self.text[offset..]
            .chars()
            .next()
            .map_or(offset, |character| offset + character.len_utf8())
    }

    /// The start of the word before `offset`, skipping the spaces in between.
    fn previous_word(&self, offset: usize) -> usize {
        let mut index = offset;
        while index > 0 && self.class_before(index) == Some(CharClass::Space) {
            index = self.previous_char(index);
        }
        let Some(class) = self.class_before(index) else {
            return index;
        };
        while index > 0 && self.class_before(index) == Some(class) {
            index = self.previous_char(index);
        }
        index
    }

    /// The end of the word after `offset`, skipping the spaces before it.
    fn next_word(&self, offset: usize) -> usize {
        let class_at = |index: usize| self.text[index..].chars().next().map(char_class);
        let mut index = offset;
        while class_at(index) == Some(CharClass::Space) {
            index = self.next_char(index);
        }
        let Some(class) = class_at(index) else {
            return index;
        };
        while class_at(index) == Some(class) {
            index = self.next_char(index);
        }
        index
    }

    fn class_before(&self, offset: usize) -> Option<CharClass> {
        self.text[..offset].chars().next_back().map(char_class)
    }
}

/// What a word is made of, for word moves and double clicks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CharClass {
    Space,
    Word,
    Punctuation,
}

fn char_class(character: char) -> CharClass {
    if character.is_whitespace() {
        CharClass::Space
    } else if character.is_alphanumeric() || character == '_' {
        CharClass::Word
    } else {
        CharClass::Punctuation
    }
}

/// Text made fit for one line: every line break is a space.
fn single_line(text: &str) -> String {
    text.replace("\r\n", " ").replace(['\r', '\n'], " ")
}

/// Text whose line breaks are kept, with every break written as `\n`.
fn kept_breaks(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// How long `text` is once drawn, counting a kept break as the marker that replaces it.
fn drawn_len(text: &str, keeps_line_breaks: bool) -> usize {
    if keeps_line_breaks {
        text.chars()
            .map(|character| {
                if character == '\n' {
                    BREAK.len()
                } else {
                    character.len_utf8()
                }
            })
            .sum()
    } else {
        text.len()
    }
}

/// The byte offset in `text` of a drawn position, for a field that shows breaks as
/// markers. A point inside the marker is the break it stands for.
fn text_offset_in_drawn(text: &str, keeps_line_breaks: bool, drawn: usize) -> usize {
    if !keeps_line_breaks {
        return drawn.min(text.len());
    }
    let mut width = 0;
    for (index, character) in text.char_indices() {
        let piece = if character == '\n' {
            BREAK.len()
        } else {
            character.len_utf8()
        };
        if drawn < width + piece {
            return index;
        }
        width += piece;
    }
    text.len()
}

// ---------------------------------------------------------------------------
// The view
// ---------------------------------------------------------------------------

/// A one-line text input.
pub struct TextField {
    /// Element id, unique per field, so interactivity and click handling work.
    id: SharedString,
    model: LineModel,
    /// What an input method is composing, shown at the caret until it commits.
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
    /// Whether a masked field offers the button that shows its text.
    revealable: bool,
    /// Whether a masked field is showing its text for now.
    revealed: bool,
    /// Whether the field holds a path, and the system chooser can fill it.
    browse: Option<Browse>,
    /// A quiet icon before the text, such as the magnifier of a search field.
    leading: Option<Icon>,
    /// Whether the left button is down after a press in the field: dragging selects.
    selecting: bool,
    /// How far the line is scrolled left so the caret stays in view.
    scroll: Pixels,
    /// Where the right-click menu is open, in window coordinates.
    menu: Option<Point<Pixels>>,
    /// The line as it was last drawn, to map the pointer to a character.
    last_layout: Option<ShapedLine>,
    last_bounds: Option<Bounds<Pixels>>,
}

impl TextField {
    /// Replaces the placeholder, for a field whose language changed.
    pub fn set_placeholder(
        &mut self,
        placeholder: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
        let placeholder = placeholder.into();
        if self.placeholder != placeholder {
            self.placeholder = placeholder;
            cx.notify();
        }
    }

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
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let id = SharedString::from(format!(
            "text-field.{}",
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        Self {
            id,
            model: LineModel::new(text),
            marked: None,
            placeholder: placeholder.into(),
            focus: cx.focus_handle(),
            submit: None,
            masked: false,
            revealable: false,
            revealed: false,
            browse: None,
            leading: None,
            selecting: false,
            scroll: px(0.0),
            menu: None,
            last_layout: None,
            last_bounds: None,
        }
    }

    /// Replaces the text, and puts the caret at its end.
    ///
    /// A field that is reused for another attempt has to start from what that
    /// attempt needs, not from what was left in it.
    pub fn set_text(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        self.model.set_text(text);
        self.marked = None;
        cx.notify();
    }

    /// Draws the line as bullets instead of characters.
    pub fn set_masked(&mut self, masked: bool, cx: &mut Context<Self>) {
        if self.masked != masked {
            self.masked = masked;
            self.revealed = false;
            cx.notify();
        }
    }

    /// Offers a button that shows a masked field's text while the user wants it.
    pub fn set_revealable(&mut self, revealable: bool, cx: &mut Context<Self>) {
        if self.revealable != revealable {
            self.revealable = revealable;
            cx.notify();
        }
    }

    /// Draws a quiet icon before the text, such as a search field's magnifier.
    pub fn set_leading_icon(&mut self, icon: Icon, cx: &mut Context<Self>) {
        self.leading = Some(icon);
        cx.notify();
    }

    /// Makes the field a path field: a button at its end opens the system chooser, and
    /// the path picked replaces the text.
    pub fn set_browse(&mut self, browse: Browse, cx: &mut Context<Self>) {
        self.browse = Some(browse);
        cx.notify();
    }

    /// Lets the field hold line breaks, drawn as `\n` and inserted with Shift+Enter.
    ///
    /// The saved command body is the one field that needs this: a shortcut command can
    /// be a short block, and the text that is saved has to be the text that was typed.
    /// Enter still submits, so the dialog keeps its single keyboard path to save.
    pub fn set_keeps_line_breaks(&mut self, keeps: bool, cx: &mut Context<Self>) {
        if self.model.keeps_line_breaks() != keeps {
            self.model.set_keeps_line_breaks(keeps);
            cx.notify();
        }
    }

    /// Opens the system chooser; the path picked, if any, becomes the text.
    fn browse(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(kind) = self.browse else {
            return;
        };
        let answer = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: kind == Browse::File,
            directories: kind == Browse::Directory,
            multiple: false,
            prompt: None,
        });
        let focus = self.focus.clone();
        cx.spawn_in(window, async move |this, cx| {
            // A cancelled chooser, or one the system could not open, changes nothing.
            let Ok(Ok(Some(paths))) = answer.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let _ = this.update_in(cx, |field, window, cx| {
                field.marked = None;
                field.model.set_text(path.display().to_string());
                window.focus(&focus);
                cx.notify();
            });
        })
        .detach();
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
        self.model.text()
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

    /// Whether the text is hidden on screen right now.
    fn hidden(&self) -> bool {
        self.masked && !self.revealed
    }

    fn edit(&mut self, cx: &mut Context<Self>, change: impl FnOnce(&mut LineModel)) {
        change(&mut self.model);
        cx.notify();
    }

    /// Copies the selection; a hidden password is never put on the clipboard.
    fn copy(&mut self, cx: &mut Context<Self>) {
        if self.masked {
            return;
        }
        if let Some(text) = self.model.selected_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(text.to_owned()));
        }
    }

    fn cut(&mut self, cx: &mut Context<Self>) {
        if self.masked {
            return;
        }
        if let Some(text) = self.model.take_selection() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            cx.notify();
        }
    }

    fn paste(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.marked = None;
            self.edit(cx, |model| model.insert(&text));
        }
    }

    fn handle_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let modifiers = &event.keystroke.modifiers;
        let mac = Platform::current() == Platform::MacOs;
        // The command key is Ctrl on Windows and Linux; a word move is Option on macOS.
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
            // A field that keeps line breaks takes Shift+Enter as a break; Enter alone is
            // still the submit that saves what was typed.
            "enter" if shift && self.model.keeps_line_breaks() => {
                self.edit(cx, |model| model.insert("\n"));
            }
            "enter" => {
                let text = self.model.text().to_owned();
                let consumed = match &self.submit {
                    Some(submit) => submit(text, window, cx),
                    None => false,
                };
                if consumed {
                    self.model.set_text("");
                    cx.notify();
                }
            }
            // Escape first drops a selection; on a bare caret it empties the field, the
            // quick way to start a search or a path again.
            "escape" if !self.model.selection().is_empty() => {
                let caret = self.model.caret();
                self.edit(cx, |model| model.move_to(caret));
            }
            "escape" => self.edit(cx, |model| model.set_text("")),
            "backspace" => self.edit(cx, |model| model.backspace(by_word)),
            "delete" if shift && !mac => self.cut(cx),
            "delete" => self.edit(cx, |model| model.delete(by_word)),
            "left" if mac && command => self.edit(cx, |model| model.home(shift)),
            "right" if mac && command => self.edit(cx, |model| model.end(shift)),
            "left" => self.edit(cx, |model| model.left(by_word, shift)),
            "right" => self.edit(cx, |model| model.right(by_word, shift)),
            "home" => self.edit(cx, |model| model.home(shift)),
            "end" => self.edit(cx, |model| model.end(shift)),
            "a" if command => self.edit(cx, LineModel::select_all),
            "c" if command => self.copy(cx),
            "x" if command => self.cut(cx),
            "v" if command => self.paste(cx),
            "insert" if modifiers.control => self.copy(cx),
            "insert" if shift => self.paste(cx),
            _ => return,
        }
        cx.stop_propagation();
    }

    // -- Mapping between the text and the drawn line --------------------------------

    /// The line as drawn: the text or bullets, with the composition at the caret.
    ///
    /// A field that keeps line breaks draws each one as [`BREAK`], inside the same
    /// single line, so the break is visible where it is.
    fn display_text(&self) -> String {
        let caret = self.model.caret();
        let text = self.model.text();
        let mut shown = if self.hidden() {
            text[..caret].chars().map(|_| MASK).collect::<String>()
        } else {
            text[..caret].to_owned()
        };
        if let Some(marked) = &self.marked {
            shown.push_str(marked);
        }
        if self.hidden() {
            shown.extend(text[caret..].chars().map(|_| MASK));
        } else {
            shown.push_str(&text[caret..]);
        }
        if self.model.keeps_line_breaks() && !self.hidden() {
            return shown.replace('\n', BREAK);
        }
        shown
    }

    /// Where a text offset is in the drawn line.
    fn display_offset(&self, offset: usize) -> usize {
        let text = self.model.text();
        let offset = offset.min(text.len());
        let base = if self.hidden() {
            text[..offset].chars().count() * MASK.len_utf8()
        } else {
            drawn_len(&text[..offset], self.model.keeps_line_breaks())
        };
        match &self.marked {
            Some(marked) if offset >= self.model.caret() && !marked.is_empty() => {
                base + marked.len()
            }
            _ => base,
        }
    }

    /// The text offset of a position in the drawn line.
    fn text_offset(&self, display: usize) -> usize {
        let text = self.model.text();
        // The composition is not text yet: a point inside it is the caret.
        let caret_display = self.display_offset(self.model.caret());
        let marked = self.marked.as_ref().map_or(0, String::len);
        let display = if display >= caret_display.saturating_sub(marked) && display < caret_display
        {
            return self.model.caret();
        } else if display >= caret_display {
            display - marked
        } else {
            display
        };
        if self.hidden() {
            let characters = display / MASK.len_utf8();
            text.char_indices()
                .nth(characters)
                .map_or(text.len(), |(index, _)| index)
        } else {
            text_offset_in_drawn(text, self.model.keeps_line_breaks(), display)
        }
    }

    /// The text offset under a pointer position, from the last drawn line.
    fn offset_for_point(&self, position: Point<Pixels>) -> usize {
        let (Some(layout), Some(bounds)) = (self.last_layout.as_ref(), self.last_bounds) else {
            return self.model.text().len();
        };
        if self.model.text().is_empty() {
            return 0;
        }
        let x = position.x - bounds.left() + self.scroll;
        self.text_offset(layout.closest_index_for_x(x))
    }

    // -- Pointer --------------------------------------------------------------------

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus);
        self.menu = None;
        let offset = self.offset_for_point(event.position);
        match event.click_count {
            2 => self.model.select_word_at(offset),
            count if count >= 3 => self.model.select_all(),
            _ if event.modifiers.shift => self.model.select_to(offset),
            _ => self.model.move_to(offset),
        }
        self.selecting = event.click_count < 2;
        cx.notify();
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if self.selecting && event.pressed_button == Some(MouseButton::Left) {
            let offset = self.offset_for_point(event.position);
            if offset != self.model.caret() {
                self.model.select_to(offset);
                cx.notify();
            }
        }
    }

    fn on_mouse_up(&mut self, _event: &MouseUpEvent, _cx: &mut Context<Self>) {
        self.selecting = false;
    }

    fn open_menu(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus);
        // A right click outside the selection moves the caret there first, as other
        // text boxes do; inside it, the selection is what the menu acts on.
        let offset = self.offset_for_point(event.position);
        if !self.model.selection().contains(&offset) {
            self.model.move_to(offset);
        }
        self.menu = Some(event.position);
        cx.notify();
    }

    fn close_menu(&mut self, cx: &mut Context<Self>) {
        if self.menu.take().is_some() {
            cx.notify();
        }
    }

    /// The right-click menu: cut, copy, paste and select all.
    fn menu(&self, window: &Window, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let position = self.menu?;
        let metrics = Metrics::default();
        let strings = Locale::current().strings();
        let viewport = window.viewport_size();
        let has_selection = !self.model.selection().is_empty();
        let can_copy = has_selection && !self.masked;
        let can_paste = cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .is_some_and(|text| !text.is_empty());
        let can_select_all = !self.model.text().is_empty();
        let platform = Platform::current();
        let key = |letter: &str| match platform {
            Platform::MacOs => format!("\u{2318}{letter}"),
            Platform::Windows | Platform::Linux => format!("Ctrl+{letter}"),
        };

        let item = |label: &'static str, shortcut: String, enabled: bool, id: &'static str| {
            let hover = theme::swatch(palette::SURFACE_HOVER);
            div()
                .id(id)
                .debug_selector(move || id.to_owned())
                .flex()
                .items_center()
                .justify_between()
                .gap(metrics.scale(spacing::LOOSE))
                .h(metrics.scale(28.0))
                .px(metrics.scale(spacing::TIGHT + 2.0))
                .rounded(px(radius::MEDIUM))
                .text_size(metrics.label_size())
                .text_color(theme::swatch(if enabled {
                    palette::TEXT_PRIMARY
                } else {
                    palette::TEXT_MUTED
                }))
                .when(enabled, |item| {
                    item.cursor_pointer().hover(move |style| style.bg(hover))
                })
                .child(label)
                .child(
                    div()
                        .text_size(metrics.caption_size())
                        .text_color(theme::swatch(palette::TEXT_MUTED))
                        .child(SharedString::from(shortcut)),
                )
        };
        let menu = div()
            .flex()
            .flex_col()
            .min_w(metrics.scale(180.0))
            .p(metrics.scale(spacing::HAIRLINE))
            .rounded(px(radius::LARGE))
            .bg(theme::swatch(palette::SURFACE_RAISED))
            .border_1()
            .border_color(theme::swatch(palette::BORDER_STRONG))
            .shadow_lg()
            .debug_selector(|| "field.menu".to_owned())
            .child(
                item(strings.field_cut, key("X"), can_copy, "field.menu.cut").when(
                    can_copy,
                    |item| {
                        item.on_click(cx.listener(|this, _event, _window, cx| {
                            this.cut(cx);
                            this.close_menu(cx);
                        }))
                    },
                ),
            )
            .child(
                item(strings.field_copy, key("C"), can_copy, "field.menu.copy").when(
                    can_copy,
                    |item| {
                        item.on_click(cx.listener(|this, _event, _window, cx| {
                            this.copy(cx);
                            this.close_menu(cx);
                        }))
                    },
                ),
            )
            .child(
                item(strings.field_paste, key("V"), can_paste, "field.menu.paste").when(
                    can_paste,
                    |item| {
                        item.on_click(cx.listener(|this, _event, _window, cx| {
                            this.paste(cx);
                            this.close_menu(cx);
                        }))
                    },
                ),
            )
            .child(
                div()
                    .h(px(1.0))
                    .mx(metrics.scale(spacing::HAIRLINE))
                    .my(metrics.scale(2.0))
                    .bg(theme::swatch(palette::BORDER)),
            )
            .child(
                item(
                    strings.field_select_all,
                    key("A"),
                    can_select_all,
                    "field.menu.select_all",
                )
                .when(can_select_all, |item| {
                    item.on_click(cx.listener(|this, _event, _window, cx| {
                        this.model.select_all();
                        this.close_menu(cx);
                    }))
                }),
            );

        Some(
            gpui::deferred(
                gpui::anchored().position(Point::default()).child(
                    div()
                        .relative()
                        .w(viewport.width)
                        .h(viewport.height)
                        .child(
                            div()
                                .id("field.menu.backdrop")
                                .absolute()
                                .inset_0()
                                .occlude()
                                .on_any_mouse_down(cx.listener(|this, _event, _window, cx| {
                                    this.close_menu(cx);
                                })),
                        )
                        .child(
                            gpui::anchored()
                                .position(position)
                                .snap_to_window_with_margin(metrics.scale(spacing::TIGHT))
                                .child(div().occlude().child(menu)),
                        ),
                ),
            )
            .with_priority(3),
        )
    }
}

impl Render for TextField {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let metrics = Metrics::default();
        let focused = self.focus.is_focused(window);
        let accent = theme::swatch(palette::ACCENT);
        let strings = Locale::current().strings();
        let reveal = self.masked && self.revealable;
        let revealed = self.revealed;
        let trailing = reveal || self.browse.is_some();

        div()
            .id(self.id.clone())
            .relative()
            .flex()
            .items_center()
            .gap(metrics.scale(spacing::HAIRLINE))
            .flex_1()
            .min_w(px(0.0))
            .h(metrics.control_height())
            .pl(metrics.scale(spacing::SNUG - 2.0))
            .pr(metrics.scale(if trailing {
                spacing::HAIRLINE
            } else {
                spacing::SNUG - 2.0
            }))
            .rounded(px(radius::MEDIUM))
            .bg(theme::swatch(palette::CANVAS))
            .border_1()
            .border_color(if focused {
                accent
            } else {
                theme::swatch(palette::BORDER_STRONG)
            })
            // A soft ring outside the border marks the field that has the keyboard,
            // so it reads at a glance without the border alone carrying it.
            .when(focused, |field| {
                field.shadow(vec![BoxShadow {
                    color: theme::faded(accent, 0.22).into(),
                    offset: point(px(0.0), px(0.0)),
                    blur_radius: px(0.0),
                    spread_radius: px(3.0),
                }])
            })
            .when(!focused, |field| {
                let hover = theme::swatch(palette::TEXT_MUTED);
                field.hover(move |style| style.border_color(hover))
            })
            .text_size(metrics.label_size())
            .text_color(theme::swatch(palette::TEXT_PRIMARY))
            .track_focus(&self.focus)
            .cursor_text()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    this.on_mouse_down(event, window, cx);
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.open_menu(event, window, cx);
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _window, cx| {
                this.on_mouse_move(event, cx);
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, event: &MouseUpEvent, _window, cx| {
                    this.on_mouse_up(event, cx);
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, event: &MouseUpEvent, _window, cx| {
                    this.on_mouse_up(event, cx);
                }),
            )
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                this.handle_key(event, window, cx);
            }))
            .children(self.leading.map(|icon| {
                icons::icon(
                    icon,
                    metrics.small_icon_size(),
                    theme::swatch(palette::TEXT_MUTED),
                )
                .flex_none()
                .mr(metrics.scale(2.0))
            }))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .overflow_hidden()
                    .child(LineElement { field: cx.entity() }),
            )
            .when(reveal, |field| {
                let hover = theme::swatch(palette::SURFACE_HOVER);
                field.child(
                    div()
                        .id("field.reveal")
                        .debug_selector(|| "field.reveal".to_owned())
                        .flex()
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .size(metrics.hit_target())
                        .rounded(px(radius::SMALL))
                        .cursor_pointer()
                        .hover(move |style| style.bg(hover))
                        .tooltip(crate::app::components::tooltip(if revealed {
                            strings.field_conceal
                        } else {
                            strings.field_reveal
                        }))
                        .on_mouse_down(MouseButton::Left, |_event, _window, cx| {
                            cx.stop_propagation();
                        })
                        .on_click(cx.listener(|this, _event, _window, cx| {
                            this.revealed = !this.revealed;
                            cx.notify();
                        }))
                        .child(icons::icon(
                            if revealed { Icon::EyeOff } else { Icon::Eye },
                            metrics.small_icon_size(),
                            theme::swatch(palette::TEXT_MUTED),
                        )),
                )
            })
            .when_some(self.browse, |field, kind| {
                let hover = theme::swatch(palette::SURFACE_HOVER);
                let hover_text = theme::swatch(palette::TEXT_PRIMARY);
                field.child(
                    div()
                        .id("field.browse")
                        .debug_selector(|| "field.browse".to_owned())
                        .group("field-browse")
                        .flex()
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .size(metrics.hit_target())
                        .rounded(px(radius::SMALL))
                        .cursor_pointer()
                        .hover(move |style| style.bg(hover))
                        .tooltip(crate::app::components::tooltip(strings.field_browse))
                        .on_mouse_down(MouseButton::Left, |_event, _window, cx| {
                            cx.stop_propagation();
                        })
                        .on_click(cx.listener(|this, _event, window, cx| {
                            this.browse(window, cx);
                        }))
                        .child(
                            icons::icon(
                                match kind {
                                    Browse::File => Icon::FolderOpen,
                                    Browse::Directory => Icon::Folder,
                                },
                                metrics.small_icon_size(),
                                theme::swatch(palette::TEXT_MUTED),
                            )
                            .group_hover("field-browse", move |style| style.text_color(hover_text)),
                        ),
                )
            })
            .children(self.menu(window, cx))
    }
}

// ---------------------------------------------------------------------------
// Drawing the line
// ---------------------------------------------------------------------------

/// The text, the selection and the caret, drawn from the field's model.
struct LineElement {
    field: Entity<TextField>,
}

struct LinePrepaint {
    line: ShapedLine,
    origin: Point<Pixels>,
    selection: Option<PaintQuad>,
    caret: Option<PaintQuad>,
}

impl IntoElement for LineElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for LineElement {
    type RequestLayoutState = ();
    type PrepaintState = LinePrepaint;

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
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = window.line_height().into();
        (window.request_layout(style, [], cx), ())
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
        let field = self.field.read(cx);
        let style = window.text_style();
        let empty = field.model.text().is_empty() && field.marked.is_none();
        let (shown, color) = if empty {
            (
                field.placeholder.to_string(),
                theme::swatch(palette::TEXT_MUTED).into(),
            )
        } else {
            (field.display_text(), style.color)
        };
        let run = TextRun {
            len: shown.len(),
            font: style.font(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        // The composition is underlined, so it reads as not yet typed.
        let runs = match &field.marked {
            Some(marked) if !empty && !marked.is_empty() => {
                let start = field.display_offset(field.model.caret()) - marked.len();
                let end = start + marked.len();
                [
                    TextRun {
                        len: start,
                        ..run.clone()
                    },
                    TextRun {
                        len: end - start,
                        underline: Some(UnderlineStyle {
                            color: Some(run.color),
                            thickness: px(1.0),
                            wavy: false,
                        }),
                        ..run.clone()
                    },
                    TextRun {
                        len: shown.len() - end,
                        ..run
                    },
                ]
                .into_iter()
                .filter(|run| run.len > 0)
                .collect::<Vec<_>>()
            }
            _ => vec![run],
        };
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line =
            window
                .text_system()
                .shape_line(SharedString::from(shown), font_size, &runs, None);

        let caret_x = if empty {
            px(0.0)
        } else {
            line.x_for_index(field.display_offset(field.model.caret()))
        };
        // Keep the caret inside the visible width, scrolling no further than needed,
        // and never past the end of the text.
        let width = bounds.size.width;
        let mut scroll = field.scroll;
        if caret_x - scroll > width - px(CARET_WIDTH) {
            scroll = caret_x - width + px(CARET_WIDTH);
        }
        if caret_x < scroll {
            scroll = caret_x;
        }
        let overflow = (line.width + px(CARET_WIDTH) - width).max(px(0.0));
        scroll = scroll.min(overflow).max(px(0.0));

        let origin = point(bounds.left() - scroll, bounds.top());
        let selection_range = field.model.selection();
        let focused = field.focus.is_focused(window);
        // A field without the keyboard keeps its selection, dimmed, so the field that
        // has the keyboard is the one whose selection stands out.
        let selection_alpha = if focused { 0.32 } else { 0.14 };
        let selection = (!empty && !selection_range.is_empty()).then(|| {
            let start = line.x_for_index(field.display_offset(selection_range.start));
            let end = line.x_for_index(field.display_offset(selection_range.end));
            fill(
                Bounds::from_corners(
                    point(origin.x + start, bounds.top() + px(2.0)),
                    point(origin.x + end, bounds.bottom() - px(2.0)),
                ),
                theme::faded(theme::swatch(palette::ACCENT), selection_alpha),
            )
        });
        let caret = (focused && selection.is_none()).then(|| {
            fill(
                Bounds::new(
                    point(origin.x + caret_x, bounds.top() + px(2.0)),
                    size(px(CARET_WIDTH), bounds.size.height - px(4.0)),
                ),
                theme::swatch(palette::ACCENT),
            )
        });
        self.field.update(cx, |field, _cx| field.scroll = scroll);
        LinePrepaint {
            line,
            origin,
            selection,
            caret,
        }
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
        let focus = self.field.read(cx).focus.clone();
        window.handle_input(
            &focus,
            ElementInputHandler::new(bounds, self.field.clone()),
            cx,
        );
        let line_height = window.line_height();
        window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
            if let Some(selection) = prepaint.selection.take() {
                window.paint_quad(selection);
            }
            let _ = prepaint
                .line
                .paint(prepaint.origin, line_height, window, cx);
            if let Some(caret) = prepaint.caret.take() {
                window.paint_quad(caret);
            }
        });
        let line = prepaint.line.clone();
        self.field.update(cx, |field, _cx| {
            field.last_layout = Some(line);
            field.last_bounds = Some(bounds);
        });
    }
}

// ---------------------------------------------------------------------------
// Input methods
// ---------------------------------------------------------------------------

fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// The byte offset of a UTF-16 offset in `text`.
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

impl EntityInputHandler for TextField {
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
        // A composition was never inserted, so committing it only inserts the final
        // text once. An explicit range outside a composition (an input method
        // correcting earlier text) replaces exactly that range.
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
        // The composition is shown at the caret but is only added to the text when the
        // input method commits it; a selection it replaces goes as it starts.
        if !self.model.selection().is_empty() {
            self.model.insert("");
        }
        self.marked = if new_text.is_empty() {
            None
        } else {
            Some(new_text.to_owned())
        };
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        // The candidate window opens under the caret, not under the whole field.
        let Some(layout) = self.last_layout.as_ref() else {
            return Some(element_bounds);
        };
        let text = self.model.text();
        let start = self.display_offset(offset_from_utf16(text, range_utf16.start));
        let x = element_bounds.left() - self.scroll + layout.x_for_index(start);
        Some(Bounds::from_corners(
            point(x, element_bounds.top()),
            point(x + px(1.0), element_bounds.bottom()),
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

    #[test]
    fn typing_replaces_the_selection() {
        let mut model = LineModel::new("hello world");
        model.select_word_at(8);
        assert_eq!(model.selected_text(), Some("world"));
        model.insert("there");
        assert_eq!(model.text(), "hello there");
        assert_eq!(model.caret(), "hello there".len());
    }

    #[test]
    fn shift_arrows_extend_from_the_caret_and_shrink_back() {
        let mut model = LineModel::new("abcd");
        model.home(false);
        model.right(false, true);
        model.right(false, true);
        assert_eq!(model.selected_text(), Some("ab"));
        model.left(false, true);
        assert_eq!(model.selected_text(), Some("a"));
        model.left(false, true);
        model.left(false, true);
        assert_eq!(model.selection(), 0..0, "the caret cannot pass the start");
        model.end(true);
        assert_eq!(model.selected_text(), Some("abcd"));
    }

    #[test]
    fn a_leftward_selection_keeps_the_caret_at_its_start() {
        let mut model = LineModel::new("abcd");
        model.left(false, true);
        model.left(false, true);
        assert_eq!(model.selected_text(), Some("cd"));
        assert_eq!(model.caret(), 2);
        // An arrow without Shift collapses to the side it points at.
        model.left(false, false);
        assert_eq!(model.selection(), 2..2);
    }

    #[test]
    fn word_moves_skip_spaces_and_stop_at_punctuation() {
        let mut model = LineModel::new("cd /srv/app  && ls");
        model.left(true, false);
        assert_eq!(model.caret(), "cd /srv/app  && ".len());
        model.left(true, false);
        assert_eq!(model.caret(), "cd /srv/app  ".len(), "&& is its own word");
        model.left(true, false);
        assert_eq!(model.caret(), "cd /srv/".len());
        model.home(false);
        model.right(true, false);
        assert_eq!(model.caret(), 2);
    }

    #[test]
    fn word_deletes_remove_one_word() {
        let mut model = LineModel::new("ssh deploy@web");
        model.backspace(true);
        assert_eq!(model.text(), "ssh deploy@");
        model.home(false);
        model.delete(true);
        assert_eq!(model.text(), " deploy@");
    }

    #[test]
    fn a_double_click_selects_the_word_under_it_or_just_before_it() {
        let mut model = LineModel::new("root@10.0.0.5");
        model.select_word_at(1);
        assert_eq!(model.selected_text(), Some("root"));
        model.select_word_at("root@10.0.0.5".len());
        assert_eq!(model.selected_text(), Some("5"));
    }

    #[test]
    fn pasted_line_breaks_become_spaces() {
        let mut model = LineModel::new("");
        model.insert("cd /srv\r\nls -la\n");
        assert_eq!(model.text(), "cd /srv ls -la ");
    }

    #[test]
    fn a_field_that_keeps_line_breaks_stores_them_as_bare_breaks() {
        let mut model = LineModel::new("");
        model.set_keeps_line_breaks(true);
        model.insert("cd /srv\r\nls -la\r\n");
        assert_eq!(
            model.text(),
            "cd /srv\nls -la\n",
            "a copied block stays a block, with the platform's breaks normalised"
        );
        model.insert("\n");
        assert_eq!(
            model.text(),
            "cd /srv\nls -la\n\n",
            "Shift+Enter adds a break"
        );
        // A new value keeps the field's rule: what it does with breaks is its own.
        model.set_text("one\ntwo");
        assert!(model.keeps_line_breaks());
        assert_eq!(model.text(), "one\ntwo");
        model.set_keeps_line_breaks(false);
        model.insert("\n");
        assert_eq!(
            model.text(),
            "one\ntwo ",
            "a field that does not keep breaks still flattens them"
        );
    }

    #[test]
    fn a_kept_break_is_drawn_as_its_marker_and_mapped_back() {
        // The three characters of "a\nb" are drawn as the four of "a\\nb".
        let text = "a\nb";
        assert_eq!(drawn_len(text, true), 4);
        assert_eq!(drawn_len(text, false), 3);
        assert_eq!(text_offset_in_drawn(text, true, 0), 0, "before the a");
        assert_eq!(text_offset_in_drawn(text, true, 1), 1, "on the break");
        assert_eq!(
            text_offset_in_drawn(text, true, 2),
            1,
            "inside the marker is the break it stands for"
        );
        assert_eq!(text_offset_in_drawn(text, true, 3), 2, "on the b");
        assert_eq!(
            text_offset_in_drawn(text, true, 99),
            text.len(),
            "past the end is the end"
        );
        assert_eq!(text_offset_in_drawn(text, false, 99), text.len());
    }

    #[test]
    fn cut_takes_exactly_the_selection() {
        let mut model = LineModel::new("abcdef");
        model.move_to(1);
        model.select_to(4);
        assert_eq!(model.take_selection().as_deref(), Some("bcd"));
        assert_eq!(model.text(), "aef");
        assert_eq!(model.caret(), 1);
        assert_eq!(
            model.take_selection(),
            None,
            "nothing selected, nothing taken"
        );
    }

    #[test]
    fn edits_stay_on_character_boundaries() {
        let mut model = LineModel::new("你好世界");
        model.left(false, false);
        assert_eq!(model.caret(), "你好世".len());
        model.backspace(false);
        assert_eq!(model.text(), "你好界");
        model.move_to(1);
        assert_eq!(
            model.caret(),
            0,
            "an offset inside a character is clamped to its start"
        );
        model.select_word_at(0);
        assert_eq!(model.selected_text(), Some("你好界"));
    }

    #[test]
    fn utf16_offsets_map_to_bytes() {
        assert_eq!(offset_from_utf16("a你b", 2), "a你".len());
        assert_eq!(offset_from_utf16("a😀b", 3), "a😀".len());
        assert_eq!(offset_from_utf16("ab", 9), 2);
    }
}
