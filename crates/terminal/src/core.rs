//! The terminal core: grid, parser, colours, selection, scrollback (R2, R8).
//!
//! One instance owns the state for one terminal, whether the bytes come from a
//! local PTY or, later, a remote SSH channel. It knows nothing about windows: it
//! turns bytes into grid state and grid state into a snapshot a view can draw.
//! That separation is what lets the parser, colours, cursor placement, selection
//! and scrollback be tested without a display.
//!
//! Three rules hold here:
//!
//! * bytes in, effects out — a parser can ask the application to write something
//!   back (a device-attribute reply), and that request is returned rather than
//!   silently dropped, because otherwise an application waiting for the reply
//!   appears to hang;
//! * the display offset never changes what input means — scrolling moves the
//!   viewport, it does not replay or rewrite anything;
//! * a snapshot is a faithful picture of the grid: hidden cells stay hidden, wide
//!   characters occupy two columns, and a selection is reported per cell.

use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionRange, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term, TermMode, point_to_viewport, viewport_to_point};
use alacritty_terminal::vte::ansi::{CursorShape, Processor, Rgb, StdSyncHandler};

use crate::metrics::GridSize;
use crate::palette;

/// Lines of history kept above the screen.
pub const DEFAULT_SCROLLBACK: usize = 10_000;

/// Something the terminal needs the application to do after parsing bytes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TerminalEffects {
    /// Bytes the terminal wants written back to the session.
    ///
    /// Device attribute queries and window size reports arrive here; dropping them
    /// leaves a program that asked for an answer waiting forever.
    pub writes: Vec<u8>,
    /// The window title the program set, if it set one.
    pub title: Option<String>,
    /// Text the program asked to place on the clipboard (OSC 52).
    pub clipboard: Option<String>,
    /// Whether the program rang the bell.
    pub bell: bool,
}

impl TerminalEffects {
    /// Whether nothing was requested.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.writes.is_empty() && self.title.is_none() && self.clipboard.is_none() && !self.bell
    }
}

#[derive(Default)]
struct SinkState {
    effects: TerminalEffects,
}

/// Event sink that records what the terminal asked for.
#[derive(Clone, Default)]
struct EffectSink {
    state: Arc<Mutex<SinkState>>,
}

impl EffectSink {
    fn take_effects(&self) -> TerminalEffects {
        let mut state = self.state.lock().expect("the effect sink is not poisoned");
        std::mem::take(&mut state.effects)
    }
}

impl EventListener for EffectSink {
    fn send_event(&self, event: Event) {
        let mut state = self.state.lock().expect("the effect sink is not poisoned");
        match event {
            Event::PtyWrite(text) => state.effects.writes.extend_from_slice(text.as_bytes()),
            Event::Title(title) => state.effects.title = Some(title),
            Event::ClipboardStore(_, text) => state.effects.clipboard = Some(text),
            Event::Bell => state.effects.bell = true,
            // Everything else is either a hint for the windowing layer (cursor
            // blinking, mouse shape) or an exit notice the PTY supervisor reports
            // with a real exit status, which is more accurate than this event.
            _ => {}
        }
    }
}

/// Visual attributes of one cell.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CellAttributes {
    /// Bold.
    pub bold: bool,
    /// Dim.
    pub dim: bool,
    /// Italic.
    pub italic: bool,
    /// Underlined.
    pub underline: bool,
    /// Struck through.
    pub strikethrough: bool,
}

/// One drawn cell.
#[derive(Clone, Debug, PartialEq)]
pub struct RenderedCell {
    /// The character, with any combining characters appended.
    pub text: String,
    /// Resolved foreground.
    pub foreground: Rgb,
    /// Resolved background.
    pub background: Rgb,
    /// Bold, dim, italic, underline, strike.
    pub attributes: CellAttributes,
    /// Whether the cell is part of the current selection.
    pub selected: bool,
    /// Columns the cell occupies: 2 for a wide character, 1 otherwise.
    ///
    /// The row deliberately contains no entry for the trailing spacer of a wide
    /// character, so a renderer must use this width to keep the columns aligned.
    pub width: usize,
}

/// One drawn row of cells.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RenderedRow {
    /// Cells from left to right.
    pub cells: Vec<RenderedCell>,
}

/// Where the cursor is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RenderedCursor {
    /// Row from the top of the visible grid.
    pub row: usize,
    /// Column from the left.
    pub column: usize,
    /// Shape the program asked for.
    pub shape: CursorShape,
}

/// A complete picture of what the terminal shows.
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    /// Visible rows, top first.
    pub rows: Vec<RenderedRow>,
    /// Cursor position, when it is inside the visible area.
    pub cursor: Option<RenderedCursor>,
    /// Grid size the snapshot was taken at.
    pub grid: GridSize,
    /// Lines the viewport is scrolled back by.
    pub display_offset: usize,
    /// Lines of history available.
    pub history: usize,
    /// Terminal modes currently active.
    pub mode: TermMode,
}

impl Snapshot {
    /// The text of one row, trailing blanks removed.
    #[must_use]
    pub fn row_text(&self, row: usize) -> Option<String> {
        let row = self.rows.get(row)?;
        let mut text: String = row.cells.iter().map(|cell| cell.text.as_str()).collect();
        while text.ends_with(' ') {
            text.pop();
        }
        Some(text)
    }

    /// Every row as text, used by tests to assert what the user would read.
    #[must_use]
    pub fn text(&self) -> Vec<String> {
        (0..self.rows.len())
            .filter_map(|row| self.row_text(row))
            .collect()
    }
}

/// Terminal state for one session.
pub struct TerminalCore {
    term: Term<EffectSink>,
    parser: Processor<StdSyncHandler>,
    sink: EffectSink,
    grid: GridSize,
    title: Option<String>,
}

impl TerminalCore {
    /// An empty terminal of the given size.
    #[must_use]
    pub fn new(cols: usize, rows: usize, scrollback: usize) -> Self {
        let grid = GridSize::new(cols, rows);
        let sink = EffectSink::default();
        let config = Config {
            scrolling_history: scrollback,
            ..Config::default()
        };
        let term = Term::new(config, &grid, sink.clone());
        Self {
            term,
            parser: Processor::new(),
            sink,
            grid,
            title: None,
        }
    }

    /// Parses a chunk of output and returns what the application must do about it.
    pub fn feed(&mut self, bytes: &[u8]) -> TerminalEffects {
        self.parser.advance(&mut self.term, bytes);
        let effects = self.sink.take_effects();
        if let Some(title) = &effects.title {
            self.title = Some(title.clone());
        }
        effects
    }

    /// Resizes the grid, keeping the history.
    pub fn resize(&mut self, grid: GridSize) {
        let grid = grid.clamped();
        if grid == self.grid {
            return;
        }
        self.term.resize(grid);
        self.grid = grid;
    }

    /// Current grid size.
    #[must_use]
    pub fn grid(&self) -> GridSize {
        self.grid
    }

    /// Active terminal modes.
    #[must_use]
    pub fn mode(&self) -> TermMode {
        *self.term.mode()
    }

    /// The title the program last set.
    #[must_use]
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// Lines the viewport is scrolled back by.
    #[must_use]
    pub fn display_offset(&self) -> usize {
        self.term.grid().display_offset()
    }

    /// Lines of history above the screen.
    #[must_use]
    pub fn history(&self) -> usize {
        self.term.grid().history_size()
    }

    /// Moves the viewport through the history.
    ///
    /// Only the viewport moves: input, the cursor and the shell's own scroll
    /// position are untouched, which is what makes looking back safe.
    pub fn scroll(&mut self, lines: i32) {
        if lines == 0 {
            return;
        }
        self.term.scroll_display(Scroll::Delta(lines));
    }

    /// Jumps back to the live output.
    pub fn scroll_to_bottom(&mut self) {
        self.term.scroll_display(Scroll::Bottom);
    }

    /// Starts a mouse selection at a viewport row and column.
    pub fn begin_selection(&mut self, row: usize, column: usize, kind: SelectionType) {
        let point = self.grid_point(row, column);
        self.term.selection = Some(Selection::new(kind, point, Side::Left));
    }

    /// Extends the selection to a viewport row and column.
    pub fn extend_selection(&mut self, row: usize, column: usize) {
        let point = self.grid_point(row, column);
        if let Some(selection) = self.term.selection.as_mut() {
            selection.update(point, Side::Right);
        }
    }

    /// Removes the selection.
    pub fn clear_selection(&mut self) {
        self.term.selection = None;
    }

    /// Whether a non-empty selection exists.
    #[must_use]
    pub fn has_selection(&self) -> bool {
        self.selection_range().is_some()
    }

    /// The grid range of the current selection.
    #[must_use]
    pub fn selection_range(&self) -> Option<SelectionRange> {
        self.term
            .selection
            .as_ref()
            .and_then(|s| s.to_range(&self.term))
    }

    /// The selected text, ready for the clipboard.
    #[must_use]
    pub fn selection_text(&self) -> Option<String> {
        self.term.selection_to_string()
    }

    /// Selects a whole row, which is what a triple click does in other terminals.
    pub fn select_row(&mut self, row: usize) {
        let start = self.grid_point(row, 0);
        let end = self.grid_point(row, self.grid.cols.saturating_sub(1));
        let mut selection = Selection::new(SelectionType::Lines, start, Side::Left);
        selection.update(end, Side::Left);
        self.term.selection = Some(selection);
    }

    /// A picture of the visible grid.
    #[must_use]
    pub fn snapshot(&self) -> Snapshot {
        let content = self.term.renderable_content();
        let display_offset = content.display_offset;
        let selection = content.selection;
        let mut rows = vec![RenderedRow::default(); self.grid.rows];

        for indexed in content.display_iter {
            let point = indexed.point;
            let cell = indexed.cell;
            let Some(viewport) = point_to_viewport(display_offset, point) else {
                continue;
            };
            let Some(row) = rows.get_mut(viewport.line) else {
                continue;
            };
            // A wide character already covers the next column, so its spacer is
            // not drawn: drawing it would push the rest of the row one cell right.
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }

            let mut foreground = palette::resolve(cell.fg, content.colors);
            let mut background = palette::resolve(cell.bg, content.colors);
            if cell.flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut foreground, &mut background);
            }
            let hidden = cell.flags.contains(Flags::HIDDEN);

            let mut text = if hidden {
                String::from(" ")
            } else {
                String::new()
            };
            if !hidden {
                text.push(cell.c);
                if let Some(zerowidth) = cell.zerowidth() {
                    for character in zerowidth {
                        text.push(*character);
                    }
                }
            }

            row.cells.push(RenderedCell {
                text,
                foreground,
                background,
                attributes: CellAttributes {
                    bold: cell.flags.contains(Flags::BOLD),
                    dim: cell.flags.contains(Flags::DIM),
                    italic: cell.flags.contains(Flags::ITALIC),
                    underline: cell.flags.contains(Flags::UNDERLINE),
                    strikethrough: cell.flags.contains(Flags::STRIKEOUT),
                },
                width: if cell.flags.contains(Flags::WIDE_CHAR) {
                    2
                } else {
                    1
                },
                selected: selection.is_some_and(|range| {
                    range.contains(point)
                        || (cell.flags.contains(Flags::WIDE_CHAR)
                            && range.contains(Point::new(point.line, point.column + 1)))
                }),
            });
        }

        let cursor = point_to_viewport(display_offset, content.cursor.point)
            // A cursor that is scrolled out of the viewport is not drawn: reporting
            // it anyway would put it at a row the grid does not have.
            .filter(|point| point.line < self.grid.rows)
            .map(|point| RenderedCursor {
                row: point.line,
                column: point.column.0,
                shape: content.cursor.shape,
            });

        Snapshot {
            rows,
            cursor,
            grid: self.grid,
            display_offset,
            history: self.term.grid().history_size(),
            mode: content.mode,
        }
    }

    fn grid_point(&self, row: usize, column: usize) -> Point {
        let column = column.min(self.grid.cols.saturating_sub(1));
        let row = row.min(self.grid.rows.saturating_sub(1));
        viewport_to_point(self.display_offset(), Point::new(row, Column(column)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn core() -> TerminalCore {
        TerminalCore::new(40, 10, 100)
    }

    #[test]
    fn plain_text_lands_on_the_grid() {
        let mut terminal = core();
        terminal.feed(b"hello");
        let snapshot = terminal.snapshot();
        assert_eq!(snapshot.rows.len(), 10);
        assert_eq!(snapshot.row_text(0).as_deref(), Some("hello"));
        assert_eq!(snapshot.cursor.expect("cursor is visible").column, 5);
    }

    #[test]
    fn colours_are_resolved_including_rgb_and_indexed() {
        let mut terminal = core();
        // SGR 31 = red, SGR 38;5;196 = indexed red, SGR 38;2;1;2;3 = direct RGB.
        terminal.feed(b"\x1b[31mred\x1b[38;5;196mindexed\x1b[38;2;1;2;3mrgb\x1b[0m");
        let snapshot = terminal.snapshot();
        let cells = &snapshot.rows[0].cells;

        assert_eq!(cells[0].foreground, palette::ANSI[1]);
        assert_eq!(cells[3].foreground, palette::indexed(196));
        assert_eq!(cells[10].foreground, Rgb { r: 1, g: 2, b: 3 });
    }

    #[test]
    fn inverse_and_hidden_cells_are_reported_truthfully() {
        let mut terminal = core();
        terminal.feed(b"\x1b[7minv\x1b[0m\x1b[8mhid\x1b[0m");
        let snapshot = terminal.snapshot();
        let cells = &snapshot.rows[0].cells;

        // Inverse swaps the colours rather than inventing a third one.
        assert_eq!(cells[0].foreground, palette::BACKGROUND);
        assert_eq!(cells[0].background, palette::FOREGROUND);
        // A hidden cell shows a space, not its character.
        assert_eq!(cells[3].text, " ");
        assert_eq!(cells[4].text, " ");
    }

    #[test]
    fn caret_control_moves_the_cursor() {
        let mut terminal = core();
        terminal.feed(b"abc\r\x1b[K");
        let snapshot = terminal.snapshot();
        assert_eq!(snapshot.row_text(0).as_deref(), Some(""));
        assert_eq!(snapshot.cursor.expect("cursor").column, 0);
    }

    #[test]
    fn a_wide_character_takes_two_columns() {
        let mut terminal = core();
        terminal.feed("中文".as_bytes());
        let snapshot = terminal.snapshot();
        assert_eq!(snapshot.row_text(0).as_deref(), Some("中文"));
        // Two characters occupy four columns, and the trailing spacer of each is
        // not drawn, so the row holds 38 entries for a 40 column grid.
        assert_eq!(snapshot.rows[0].cells.len(), 38);
        assert_eq!(snapshot.rows[0].cells[0].width, 2);
        assert_eq!(snapshot.rows[0].cells[1].text, "文");
        assert_eq!(snapshot.rows[0].cells[2].width, 1);
        assert_eq!(snapshot.cursor.expect("cursor").column, 4);
    }

    #[test]
    fn the_alternate_screen_is_separate_from_the_history() {
        let mut terminal = core();
        terminal.feed(b"before\r\n");
        assert!(!terminal.mode().contains(TermMode::ALT_SCREEN));

        terminal.feed(b"\x1b[?1049h\x1b[H");
        assert!(terminal.mode().contains(TermMode::ALT_SCREEN));
        terminal.feed(b"inside");
        assert_eq!(terminal.snapshot().row_text(0).as_deref(), Some("inside"));

        terminal.feed(b"\x1b[?1049l");
        assert!(!terminal.mode().contains(TermMode::ALT_SCREEN));
        assert_eq!(terminal.snapshot().row_text(0).as_deref(), Some("before"));
    }

    #[test]
    fn scrolling_back_shows_history_without_moving_the_cursor() {
        let mut terminal = TerminalCore::new(20, 5, 100);
        for line in 0..12 {
            terminal.feed(format!("line {line}\r\n").as_bytes());
        }
        let live = terminal.snapshot();
        // Twelve lines plus the newline after the last one, minus the five rows
        // on screen.
        assert_eq!(terminal.history(), 8);
        assert_eq!(terminal.display_offset(), 0);
        assert_eq!(live.row_text(0).as_deref(), Some("line 8"));
        assert_eq!(live.row_text(3).as_deref(), Some("line 11"));

        terminal.scroll(5);
        let scrolled = terminal.snapshot();
        assert_eq!(scrolled.display_offset, 5);
        assert_eq!(scrolled.row_text(0).as_deref(), Some("line 3"));
        assert_eq!(scrolled.row_text(4).as_deref(), Some("line 7"));
        assert_eq!(
            scrolled.cursor, None,
            "the cursor is below the scrolled viewport, so it is not drawn"
        );

        terminal.scroll_to_bottom();
        assert_eq!(terminal.display_offset(), 0);
        assert_eq!(
            terminal.snapshot().row_text(0).as_deref(),
            Some("line 8"),
            "returning to the bottom restores the live output"
        );
    }

    #[test]
    fn resizing_keeps_the_empty_cells_in_step_with_the_grid() {
        let mut terminal = core();
        terminal.feed(b"hello");
        terminal.resize(GridSize::new(100, 30));
        let snapshot = terminal.snapshot();
        assert_eq!(snapshot.grid, GridSize::new(100, 30));
        assert_eq!(snapshot.rows.len(), 30);
        assert_eq!(snapshot.rows[0].cells.len(), 100);
        assert_eq!(snapshot.row_text(0).as_deref(), Some("hello"));
    }

    #[test]
    fn widening_the_terminal_reflows_a_wrapped_line() {
        let mut terminal = TerminalCore::new(4, 5, 100);
        terminal.feed(b"abcdefgh");
        assert_eq!(terminal.snapshot().text()[0], "abcd");
        assert_eq!(terminal.snapshot().text()[1], "efgh");

        terminal.resize(GridSize::new(8, 5));
        let snapshot = terminal.snapshot();
        assert_eq!(snapshot.text()[0], "abcdefgh");
        assert_eq!(
            snapshot.text()[1],
            "",
            "the reflowed line no longer needs two rows"
        );
    }

    #[test]
    fn narrowing_the_terminal_keeps_the_end_of_a_too_long_line_visible() {
        let mut terminal = TerminalCore::new(20, 5, 100);
        terminal.feed(b"abcdefghij");
        terminal.resize(GridSize::new(4, 5));
        let snapshot = terminal.snapshot();
        assert_eq!(snapshot.rows.len(), 5);
        assert_eq!(snapshot.rows[0].cells.len(), 4);
        // The cursor end of the line is what stays visible, so the user keeps
        // seeing the prompt they are typing at instead of the far left of it.
        assert_eq!(snapshot.text()[0], "ij");
    }

    #[test]
    fn a_selection_returns_the_text_under_it() {
        let mut terminal = core();
        terminal.feed(b"one\r\ntwo\r\nthree");
        terminal.begin_selection(0, 0, SelectionType::Simple);
        terminal.extend_selection(2, 2);
        let text = terminal.selection_text().expect("a selection exists");
        assert_eq!(text.trim_end(), "one\ntwo\nthr");
        assert!(terminal.has_selection());
        assert_eq!(
            terminal.selection_range().map(|range| range.start.line.0),
            Some(0)
        );

        terminal.clear_selection();
        assert!(!terminal.has_selection());
        assert_eq!(terminal.selection_text(), None);
    }

    #[test]
    fn a_selection_is_drawn_as_selected_cells() {
        let mut terminal = core();
        terminal.feed(b"abc");
        terminal.begin_selection(0, 0, SelectionType::Simple);
        terminal.extend_selection(0, 2);
        let snapshot = terminal.snapshot();
        assert!(snapshot.rows[0].cells[0].selected);
        assert!(snapshot.rows[0].cells[2].selected);
        assert!(!snapshot.rows[0].cells[3].selected);
    }

    #[test]
    fn a_row_selection_takes_the_whole_line() {
        let mut terminal = core();
        terminal.feed(b"first\r\nsecond");
        terminal.select_row(1);
        let text = terminal.selection_text().expect("a selection exists");
        assert_eq!(text.trim_end(), "second");
    }

    #[test]
    fn the_program_can_set_the_title() {
        let mut terminal = core();
        let effects = terminal.feed(b"\x1b]0;my title\x07");
        assert_eq!(effects.title.as_deref(), Some("my title"));
        assert_eq!(terminal.title(), Some("my title"));
    }

    #[test]
    fn a_device_attribute_query_asks_for_a_reply() {
        let mut terminal = core();
        let effects = terminal.feed(b"\x1b[c");
        assert!(
            !effects.writes.is_empty(),
            "a primary device attribute query must produce a reply"
        );
        assert!(
            effects.writes.starts_with(b"\x1b[?"),
            "the reply is a device attribute response, got {:?}",
            String::from_utf8_lossy(&effects.writes)
        );
        assert!(terminal.feed(b"x").is_empty(), "no reply for plain text");
    }

    #[test]
    fn clipboard_requests_are_reported_not_swallowed() {
        let mut terminal = core();
        let effects = terminal.feed(b"\x1b]52;c;SGVsbG8=\x07");
        assert_eq!(effects.clipboard.as_deref(), Some("Hello"));
    }

    #[test]
    fn a_snapshot_reports_the_modes_a_program_switched_on() {
        let mut terminal = core();
        terminal.feed(b"\x1b[?1049h\x1b[?2004h");
        let snapshot = terminal.snapshot();
        assert!(snapshot.mode.contains(TermMode::ALT_SCREEN));
        assert!(snapshot.mode.contains(TermMode::BRACKETED_PASTE));
    }

    #[test]
    fn the_cursor_shape_follows_the_program() {
        let mut terminal = core();
        terminal.feed(b"\x1b[5 q");
        assert_eq!(
            terminal.snapshot().cursor.map(|cursor| cursor.shape),
            Some(CursorShape::Beam)
        );
        terminal.feed(b"\x1b[?25l");
        assert_eq!(
            terminal.snapshot().cursor.map(|cursor| cursor.shape),
            Some(CursorShape::Hidden)
        );
    }

    #[test]
    fn a_shrunk_terminal_still_reports_a_usable_grid() {
        let mut terminal = core();
        terminal.resize(GridSize::new(0, 0));
        assert!(terminal.grid().cols >= crate::metrics::MIN_COLUMNS);
        assert!(terminal.grid().rows >= crate::metrics::MIN_ROWS);
        assert_eq!(terminal.snapshot().rows.len(), terminal.grid().rows);
    }

    #[test]
    fn history_is_bounded_by_the_configured_scrollback() {
        let mut terminal = TerminalCore::new(10, 5, 3);
        for line in 0..50 {
            terminal.feed(format!("{line}\r\n").as_bytes());
        }
        assert_eq!(terminal.history(), 3);
        terminal.scroll(10);
        assert_eq!(
            terminal.display_offset(),
            3,
            "scrolling stops at the oldest line"
        );
    }
}
