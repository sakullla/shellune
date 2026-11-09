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
use std::time::Instant;

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionRange, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term, TermMode, point_to_viewport, viewport_to_point};
use alacritty_terminal::vte::ansi::{CursorShape, Processor, Rgb, StdSyncHandler};
use regex::{Regex, RegexBuilder};
use shellune_core::TerminalMode;

use crate::blocks::{
    BlockBook, BlockFilter, GridAnchor, IntegrationEvent, IntegrationSplitter, Segment,
};
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
    /// How many command blocks ended because the shell said so, during this chunk.
    ///
    /// Only an explicit `D` marker in the shell integration counts: a prompt that
    /// arrived without one, a process that exited, or a block closed by an
    /// integration failure is not a reported end. The count is all that crosses this
    /// boundary — no command line and no output — so a caller that marks a background
    /// tab cannot repeat what ran (R7).
    pub commands_finished: usize,
}

impl TerminalEffects {
    /// Whether nothing was requested.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.writes.is_empty()
            && self.title.is_none()
            && self.clipboard.is_none()
            && !self.bell
            && self.commands_finished == 0
    }

    /// Whether a background event happened that a tab may mark, without any of its text.
    #[must_use]
    pub fn has_background_notice(&self) -> bool {
        self.bell || self.commands_finished > 0
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

/// One line of real scrollback that contains a search query.
///
/// The line index is the grid's own index, including negative history lines, and
/// `text` is that line with its trailing line break removed. A line that holds the
/// query twice is two matches, so the number a find bar shows is the number of
/// places a person can step through.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryMatch {
    /// Grid line. Negative values are above the live screen.
    pub line: i32,
    /// Byte index of the query inside [`HistoryMatch::text`].
    pub column: usize,
    /// Length of this match in bytes, so repeated matches in one line stay apart.
    pub length: usize,
    /// The line as the grid stores it.
    pub text: String,
}

/// How a query is compared with the terminal's text (R1).
///
/// The scrollback and the command blocks read the same options, so one query means
/// the same thing in both places. Literal text is the default; the two switches
/// only narrow or widen that same comparison.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SearchOptions {
    /// Whether letters must also match in case.
    ///
    /// Off by default: a search usually starts from text the user half remembers,
    /// and prompts and output do not agree on case.
    pub case_sensitive: bool,
    /// Whether the query is a regular expression instead of literal text.
    pub regex: bool,
}

impl SearchOptions {
    /// Literal, case-insensitive matching: what a query means before any switch.
    #[must_use]
    pub const fn literal() -> Self {
        Self {
            case_sensitive: false,
            regex: false,
        }
    }
}

/// Why a search could not run at all.
///
/// An invalid expression is not an empty result: the caller has to be able to say
/// "this pattern is wrong" instead of "nothing matched", and neither may be
/// reported as success.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchFailure {
    /// The query that could not be compiled.
    pub query: String,
    /// What the expression engine said about it, in one line.
    pub reason: String,
}

impl std::fmt::Display for SearchFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.query, self.reason)
    }
}

impl std::error::Error for SearchFailure {}

/// One query, compiled once and then run over every line or block of a search.
///
/// A literal query is escaped rather than special-cased, so the escaping rules and
/// the case folding are the engine's, not a second implementation's that could
/// disagree with it.
pub(crate) struct Matcher {
    pattern: Regex,
}

impl Matcher {
    pub(crate) fn compile(query: &str, options: SearchOptions) -> Result<Self, SearchFailure> {
        let source = if options.regex {
            query.to_owned()
        } else {
            regex::escape(query)
        };
        RegexBuilder::new(&source)
            .case_insensitive(!options.case_sensitive)
            .build()
            .map(|pattern| Self { pattern })
            .map_err(|error| SearchFailure {
                query: query.to_owned(),
                reason: one_line(&error),
            })
    }

    /// Every non-empty match in `text`, as byte ranges.
    ///
    /// An empty match is dropped: a pattern such as `a*` matches the gap before
    /// every character, and counting those would report places with nothing in
    /// them as results.
    pub(crate) fn find_all(&self, text: &str) -> Vec<(usize, usize)> {
        self.pattern
            .find_iter(text)
            .filter(|found| !found.is_empty())
            .map(|found| (found.start(), found.end()))
            .collect()
    }

    /// Whether `text` holds at least one non-empty match.
    pub(crate) fn is_match(&self, text: &str) -> bool {
        self.pattern.find_iter(text).any(|found| !found.is_empty())
    }
}

/// The engine's complaint, as the one line an overlay can draw.
///
/// A syntax error is drawn with the offending pattern and a caret above it; the
/// useful sentence is the last line, not the banner.
fn one_line(error: &regex::Error) -> String {
    let text = error.to_string();
    text.lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .map_or_else(
            || text.clone(),
            |line| line.strip_prefix("error: ").unwrap_or(line).to_owned(),
        )
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
    /// OSC 133 / 633 markers, held when a chunk splits one.
    splitter: IntegrationSplitter,
    /// Command blocks built from those markers. Not from prompt text.
    blocks: BlockBook,
    /// Classic, or command blocks when the shell actually reports them.
    presentation: TerminalMode,
    /// Lines dropped from the front of history, so anchors survive a full scrollback.
    lines_dropped: i64,
    /// History size after the previous chunk, to see how many lines were dropped.
    last_history: usize,
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
            splitter: IntegrationSplitter::new(),
            blocks: BlockBook::new(),
            presentation: TerminalMode::Classic,
            lines_dropped: 0,
            last_history: 0,
        }
    }

    /// Parses a chunk of output and returns what the application must do about it.
    ///
    /// OSC 133 and OSC 633 command markers are taken out of the stream and turned
    /// into command blocks. Every other byte, including other OSC sequences, is
    /// parsed into the grid as before. The grid is never cleared because a marker
    /// was missing or the presentation changed.
    pub fn feed(&mut self, bytes: &[u8]) -> TerminalEffects {
        let segments = self.splitter.push(bytes);
        let mut effects = TerminalEffects::default();
        for segment in segments {
            match segment {
                Segment::Bytes(chunk) => {
                    if chunk.is_empty() {
                        continue;
                    }
                    self.parser.advance(&mut self.term, &chunk);
                    let next = self.sink.take_effects();
                    if let Some(title) = &next.title {
                        self.title = Some(title.clone());
                    }
                    merge_effects(&mut effects, next);
                    self.note_history();
                    self.refresh_running_output();
                }
                Segment::Event(event) => {
                    self.note_history();
                    // A block can only finish because the shell said it did; the
                    // count is a background notice, never a command line (R7).
                    if self.apply_integration(event) {
                        effects.commands_finished += 1;
                    }
                }
            }
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
        self.note_history();
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

    /// Chooses classic or command-block presentation.
    ///
    /// Switching does not clear the grid, the scrollback, or blocks already
    /// recorded. Choosing command blocks again clears an integration-failure flag
    /// so a later real marker can be shown; it does not replay or rewrite output.
    pub fn set_presentation(&mut self, mode: TerminalMode) {
        self.presentation = mode;
        if mode == TerminalMode::Integrated {
            self.blocks.clear_failure();
        }
    }

    /// The presentation the user asked for, not the one on screen.
    ///
    /// The screen falls back to the classic grid while the alternate screen is
    /// active, while no shell integration has been seen, or after integration fails.
    #[must_use]
    pub fn presentation(&self) -> TerminalMode {
        self.presentation
    }

    /// Whether command blocks may be drawn over this terminal.
    ///
    /// False on the alternate screen, in classic mode, when the shell has not
    /// reported a block, and after integration has failed. The grid is unaffected.
    #[must_use]
    pub fn shows_command_blocks(&self) -> bool {
        self.presentation == TerminalMode::Integrated
            && !self.mode().contains(TermMode::ALT_SCREEN)
            && !self.blocks.failed()
            && self.blocks.has_blocks()
    }

    /// Whether the screen is the classic grid rather than command blocks.
    #[must_use]
    pub fn shows_classic_grid(&self) -> bool {
        !self.shows_command_blocks()
    }

    /// Whether shell integration was seen and then stopped being trustworthy.
    ///
    /// A missing exit code is not a failure of the integration: the block stays
    /// unknown. An unreadable exit field, or [`TerminalCore::invalidate_integration`],
    /// is.
    #[must_use]
    pub fn integration_failed(&self) -> bool {
        self.blocks.failed()
    }

    /// Drops back to the classic grid because integration can no longer be trusted.
    ///
    /// The grid and the recorded blocks stay. A running command becomes unknown,
    /// never success.
    pub fn invalidate_integration(&mut self) {
        self.blocks.fail();
    }

    /// Finished commands, plus the one still running once input or output started.
    ///
    /// A prompt marker alone is not a block. The order is the order the shell
    /// started them.
    #[must_use]
    pub fn command_blocks(&self) -> Vec<crate::blocks::CommandBlock> {
        self.blocks.blocks()
    }

    /// Blocks whose command or output matches `query`, limited by `filter`.
    ///
    /// An empty query matches every block the filter allows: the block list is the
    /// record of what ran, so it is shown whole until a query narrows it. The
    /// matching itself is the same one the scrollback uses, so a query means the
    /// same thing in both places.
    pub fn search_blocks(
        &self,
        query: &str,
        filter: BlockFilter,
        options: SearchOptions,
    ) -> Result<Vec<crate::blocks::CommandBlock>, SearchFailure> {
        self.blocks.search(query, filter, options)
    }

    /// Command and output of one block, for the clipboard.
    #[must_use]
    pub fn copy_block_text(&self, id: crate::blocks::CommandBlockId) -> Option<String> {
        self.blocks.get(id).map(|block| block.copy_text())
    }

    /// The command text to send for a rerun.
    ///
    /// Reading it does not change the block. The caller writes it as a new
    /// execution; the shell's next markers, if any, become a new block.
    #[must_use]
    pub fn rerun_command(&self, id: crate::blocks::CommandBlockId) -> Option<String> {
        self.blocks.get(id).and_then(|block| {
            if block.command.is_empty() {
                None
            } else {
                Some(block.command)
            }
        })
    }

    /// Lines of the real grid, including scrollback, that match `query`.
    ///
    /// The search does not consult a side log: every match carries the grid line,
    /// the byte column inside that line and the line's text, so a match is also the
    /// place to scroll to. Every occurrence in a line is its own match.
    ///
    /// An empty query matches nothing: there is no "everything" to step through,
    /// and the caller keeps its panel open for typing. An unusable expression is an
    /// error rather than an empty list, so the two are never confused.
    pub fn search_history(
        &self,
        query: &str,
        options: SearchOptions,
    ) -> Result<Vec<HistoryMatch>, SearchFailure> {
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let matcher = Matcher::compile(query, options)?;
        let top = self.term.grid().topmost_line().0;
        let bottom = self.term.grid().bottommost_line().0;
        let mut matches = Vec::new();
        for line in top..=bottom {
            let text = self.line_text(Line(line));
            for (column, end) in matcher.find_all(&text) {
                matches.push(HistoryMatch {
                    line,
                    column,
                    length: end - column,
                    text: text.clone(),
                });
            }
        }
        Ok(matches)
    }

    /// Scrolls so `line` is the top of the viewport, without moving the cursor.
    pub fn reveal_history(&mut self, line: i32) {
        let offset = usize::try_from(-line).unwrap_or(0).min(self.history());
        let current = i32::try_from(self.display_offset()).unwrap_or(i32::MAX);
        let target = i32::try_from(offset).unwrap_or(i32::MAX);
        self.scroll(target.saturating_sub(current));
    }

    fn grid_point(&self, row: usize, column: usize) -> Point {
        let column = column.min(self.grid.cols.saturating_sub(1));
        let row = row.min(self.grid.rows.saturating_sub(1));
        viewport_to_point(self.display_offset(), Point::new(row, Column(column)))
    }

    /// Applies one shell-integration marker.
    ///
    /// Returns whether the marker ended a command block, which is the only case the
    /// caller reports as a background notice (R7). An end marker with no block open,
    /// a prompt, and an input or output start all return `false`.
    fn apply_integration(&mut self, event: IntegrationEvent) -> bool {
        let now = Instant::now();
        match event {
            IntegrationEvent::PromptStart => {
                // The alternate grid is a different screen. Scraping it would store
                // that screen as the command that was running on the primary grid.
                if !self.on_alternate_screen() {
                    if self.blocks.in_input() {
                        let scraped = self
                            .blocks
                            .input_anchor()
                            .and_then(|anchor| self.text_between(anchor, self.anchor_now()));
                        self.blocks.set_scraped_command(scraped);
                    }
                    if self.blocks.in_output() {
                        self.refresh_running_output();
                    }
                }
                self.blocks.prompt_start();
                false
            }
            IntegrationEvent::InputStart => {
                self.blocks.input_start(self.anchor_now());
                false
            }
            IntegrationEvent::CommandText(command) => {
                self.blocks.set_explicit_command(command);
                false
            }
            IntegrationEvent::OutputStart => {
                let scraped = if self.on_alternate_screen() {
                    None
                } else {
                    self.blocks
                        .input_anchor()
                        .and_then(|anchor| self.text_between(anchor, self.anchor_now()))
                };
                self.blocks.output_start(scraped, self.anchor_now(), now);
                false
            }
            IntegrationEvent::CommandFinished { exit } => {
                let output = if self.on_alternate_screen() {
                    None
                } else {
                    self.blocks
                        .output_anchor()
                        .and_then(|anchor| self.text_between(anchor, self.anchor_now()))
                };
                self.blocks.finish(output, exit, now)
            }
        }
    }

    fn on_alternate_screen(&self) -> bool {
        self.mode().contains(TermMode::ALT_SCREEN)
    }

    fn refresh_running_output(&mut self) {
        if self.on_alternate_screen() {
            return;
        }
        let Some(anchor) = self.blocks.output_anchor() else {
            return;
        };
        let text = self.text_between(anchor, self.anchor_now());
        self.blocks.set_running_output(text);
    }

    fn anchor_now(&self) -> GridAnchor {
        let point = self.term.grid().cursor.point;
        let history = self.term.grid().history_size() as i64;
        GridAnchor {
            stable: history + i64::from(point.line.0) + self.lines_dropped,
            column: point.column.0,
        }
    }

    fn point_of(&self, anchor: GridAnchor) -> Option<Point> {
        let history = self.term.grid().history_size() as i64;
        let line = anchor.stable - self.lines_dropped - history;
        let top = -history;
        let bottom = self.term.grid().screen_lines() as i64 - 1;
        if line < top || line > bottom {
            return None;
        }
        let line = i32::try_from(line).ok()?;
        let column = anchor.column.min(self.grid.cols.saturating_sub(1));
        Some(Point::new(Line(line), Column(column)))
    }

    fn text_between(&self, from: GridAnchor, to: GridAnchor) -> Option<String> {
        let start = self.point_of(from)?;
        let end = self.point_of(to)?;
        if (end.line, end.column) < (start.line, start.column) {
            return Some(String::new());
        }
        Some(self.term.bounds_to_string(start, end))
    }

    fn line_text(&self, line: Line) -> String {
        let start = Point::new(line, Column(0));
        let end = Point::new(line, self.term.grid().last_column());
        self.term
            .bounds_to_string(start, end)
            .trim_end_matches(['\n', '\r'])
            .to_owned()
    }

    fn note_history(&mut self) {
        // The alternate grid reports no scrollback. That zero is not the primary
        // grid losing lines, so it must not move anchors that still point at it.
        if self.on_alternate_screen() {
            return;
        }
        let history = self.term.grid().history_size();
        if history < self.last_history {
            self.lines_dropped += (self.last_history - history) as i64;
        }
        self.last_history = history;
    }
}

fn merge_effects(into: &mut TerminalEffects, next: TerminalEffects) {
    into.writes.extend(next.writes);
    if next.title.is_some() {
        into.title = next.title;
    }
    if next.clipboard.is_some() {
        into.clipboard = next.clipboard;
    }
    into.bell |= next.bell;
    into.commands_finished += next.commands_finished;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blocks::{BlockStatus, CommandBlockId};

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
    fn a_ring_and_a_reported_command_end_are_the_only_background_notices() {
        let mut terminal = core();

        // A bell is a real bell byte, and it is not also a command end.
        let effects = terminal.feed(b"\x07");
        assert!(effects.bell);
        assert_eq!(effects.commands_finished, 0);
        assert!(effects.has_background_notice());

        // Plain output says nothing about a command ending.
        let effects = terminal.feed(b"just output\r\n");
        assert!(!effects.has_background_notice());

        // The chunk that carries the end marker reports the finished command.
        terminal.feed(&osc("133;A"));
        terminal.feed(&osc("133;B"));
        terminal.feed(&osc("133;C"));
        let effects = terminal.feed(&osc("133;D;0"));
        assert_eq!(effects.commands_finished, 1);
        assert!(!effects.bell, "an end marker is not a bell byte");

        // An end marker without an exit code is still a reported end, not success.
        terminal.feed(&osc("133;A"));
        terminal.feed(&osc("133;B"));
        terminal.feed(&osc("133;C"));
        let effects = terminal.feed(&osc("133;D"));
        assert_eq!(effects.commands_finished, 1);
        assert_eq!(
            terminal.command_blocks().last().map(|block| block.status),
            Some(BlockStatus::Unknown)
        );
    }

    #[test]
    fn a_prompt_without_an_end_marker_is_not_a_finished_command() {
        let mut terminal = core();
        terminal.feed(&osc("133;A"));
        terminal.feed(&osc("133;B"));
        terminal.feed(&osc("133;C"));
        // No shell integration marker ended the command: a new prompt is not one.
        let effects = terminal.feed(&osc("133;A"));
        assert_eq!(
            effects.commands_finished, 0,
            "an end the shell never reported must not be invented"
        );
        assert!(!effects.has_background_notice());

        // An end marker with nothing running invents nothing either.
        let effects = terminal.feed(&osc("133;D;0"));
        assert_eq!(effects.commands_finished, 0);
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

    fn osc(payload: &str) -> Vec<u8> {
        let mut bytes = b"\x1b]".to_vec();
        bytes.extend_from_slice(payload.as_bytes());
        bytes.push(0x07);
        bytes
    }

    fn feed_block(terminal: &mut TerminalCore, command: &str, output: &str, end: &str) {
        terminal.feed(&osc("133;A"));
        terminal.feed(b"$ ");
        terminal.feed(&osc("133;B"));
        terminal.feed(command.as_bytes());
        terminal.feed(&osc("133;C"));
        terminal.feed(b"\r\n");
        terminal.feed(output.as_bytes());
        terminal.feed(b"\r\n");
        std::thread::sleep(std::time::Duration::from_millis(20));
        terminal.feed(&osc(end));
    }

    #[test]
    fn shell_integration_markers_record_command_output_exit_and_duration() {
        let mut terminal = core();
        terminal.set_presentation(TerminalMode::Integrated);
        let before = terminal.snapshot().text();
        feed_block(&mut terminal, "echo hello", "hello", "133;D;0");

        let blocks = terminal.command_blocks();
        assert_eq!(blocks.len(), 1, "one command, one block");
        let block = blocks[0].clone();
        assert_eq!(block.command, "echo hello");
        assert_eq!(block.output, "hello");
        assert_eq!(block.status, BlockStatus::Exited(0));
        assert!(block.status.is_success());
        let duration = block.duration.expect("the command has a measured duration");
        assert!(
            duration.as_millis() >= 1,
            "duration comes from the markers' timestamps, got {duration:?}"
        );
        assert!(terminal.shows_command_blocks());
        assert_ne!(
            terminal.snapshot().text(),
            before,
            "the grid keeps the real output"
        );

        let copied = terminal
            .copy_block_text(block.id)
            .expect("the block can be copied");
        assert_eq!(copied, "echo hello\nhello");
        let found = terminal
            .search_blocks("hello", BlockFilter::ExitedZero, SearchOptions::literal())
            .expect("a literal query compiles");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, block.id);
        let history = terminal
            .search_history("hello", SearchOptions::literal())
            .expect("a literal query compiles");
        assert!(
            history.iter().any(|hit| hit.text.contains("hello")),
            "history search reads the grid, got {history:?}"
        );
        assert!(
            history.iter().any(|hit| hit.text == block.output),
            "the block output is the grid line, got {history:?}"
        );

        let rerun = terminal
            .rerun_command(block.id)
            .expect("the command can be rerun");
        assert_eq!(rerun, "echo hello");
        assert_eq!(
            terminal.command_blocks()[0],
            block,
            "a rerun reads the command and does not change the recorded result"
        );
    }

    #[test]
    fn a_missing_exit_code_is_not_success_and_a_rerun_is_a_new_block() {
        let mut terminal = core();
        terminal.set_presentation(TerminalMode::Integrated);
        feed_block(&mut terminal, "false", "nope", "133;D");
        let first = terminal.command_blocks()[0].clone();
        assert_eq!(first.status, BlockStatus::Unknown);
        assert!(
            !first.status.is_success(),
            "an ended command with no exit code is not success"
        );
        assert!(
            first.duration.is_some(),
            "the end marker still has a duration"
        );
        assert!(
            terminal
                .search_blocks("false", BlockFilter::ExitedZero, SearchOptions::literal())
                .expect("a literal query compiles")
                .is_empty(),
            "the success filter does not keep an unknown exit"
        );
        assert_eq!(
            terminal
                .search_blocks("", BlockFilter::Unknown, SearchOptions::literal())
                .expect("an empty query compiles")
                .len(),
            1
        );

        let command = terminal.rerun_command(first.id).expect("rerun text");
        assert_eq!(command, "false");
        feed_block(&mut terminal, "false", "again", "133;D;2");
        let blocks = terminal.command_blocks();
        assert_eq!(blocks.len(), 2, "the rerun is a second execution");
        assert_eq!(blocks[0], first, "the original result is unchanged");
        assert_eq!(blocks[1].status, BlockStatus::Exited(2));
        assert!(!blocks[1].status.is_success());
        assert_eq!(blocks[1].output, "again");
    }

    #[test]
    fn an_explicit_command_marker_is_the_command_text() {
        let mut terminal = core();
        terminal.feed(&osc("133;A"));
        terminal.feed(&osc("133;E;make test"));
        terminal.feed(&osc("133;C"));
        terminal.feed(b"\r\nok\r\n");
        terminal.feed(&osc("133;D;0"));
        let block = &terminal.command_blocks()[0];
        assert_eq!(block.command, "make test");
        assert_eq!(block.output, "ok");
        assert!(block.status.is_success());
    }

    #[test]
    fn a_running_command_is_not_success_and_scrollback_search_reveals_it() {
        let mut terminal = TerminalCore::new(20, 4, 100);
        terminal.set_presentation(TerminalMode::Integrated);
        terminal.feed(&osc("133;B"));
        terminal.feed(b"sleep 5");
        terminal.feed(&osc("133;C"));
        terminal.feed(b"\r\nstill going\r\n");
        let block = &terminal.command_blocks()[0];
        assert_eq!(block.status, BlockStatus::Running);
        assert!(!block.status.is_success());
        assert!(
            block.duration.is_none(),
            "a running command has no final duration"
        );
        assert_eq!(block.command, "sleep 5");
        assert_eq!(block.output, "still going");

        for line in 0..10 {
            terminal.feed(format!("older {line}\r\n").as_bytes());
        }
        let hits = terminal
            .search_history("older 0", SearchOptions::literal())
            .expect("a literal query compiles");
        assert!(!hits.is_empty(), "the scrolled line is still in history");
        terminal.reveal_history(hits[0].line);
        let shown = terminal.snapshot().text().join("\n");
        assert!(
            shown.contains("older 0"),
            "revealing a history match scrolls the real grid, got {shown}"
        );
    }

    #[test]
    fn every_occurrence_in_a_line_is_a_match_and_the_switches_narrow_it() {
        let mut terminal = core();
        terminal.feed("needle and Needle and needle\r\n".as_bytes());

        let folded = terminal
            .search_history("needle", SearchOptions::literal())
            .expect("a literal query compiles");
        assert_eq!(
            folded.iter().map(|hit| hit.column).collect::<Vec<_>>(),
            vec![0, "needle and ".len(), "needle and Needle and ".len()],
            "every occurrence counts and the capitals fold in: {folded:?}"
        );
        assert_eq!(folded[0].length, "needle".len());

        let exact = terminal
            .search_history(
                "needle",
                SearchOptions {
                    case_sensitive: true,
                    regex: false,
                },
            )
            .expect("a literal query compiles");
        assert_eq!(
            exact.iter().map(|hit| hit.column).collect::<Vec<_>>(),
            vec![0, "needle and Needle and ".len()],
            "the capitalised one is excluded: {exact:?}"
        );

        let expression = terminal
            .search_history(
                "N.edle",
                SearchOptions {
                    case_sensitive: true,
                    regex: true,
                },
            )
            .expect("a valid expression compiles");
        assert_eq!(expression.len(), 1, "only the capitalised one matches");
        assert_eq!(expression[0].column, "needle and ".len());

        let both = terminal
            .search_history(
                "(?i)needle",
                SearchOptions {
                    case_sensitive: true,
                    regex: true,
                },
            )
            .expect("a valid expression compiles");
        assert_eq!(
            both.len(),
            3,
            "the expression's own case flag is respected: {both:?}"
        );
    }

    #[test]
    fn an_empty_query_is_not_a_result_and_a_broken_expression_is_an_error() {
        let mut terminal = core();
        terminal.set_presentation(TerminalMode::Integrated);
        feed_block(&mut terminal, "echo Hello", "Hello there", "133;D;0");

        assert!(
            terminal
                .search_history("", SearchOptions::literal())
                .expect("an empty query compiles")
                .is_empty(),
            "an empty query steps through nothing"
        );
        assert!(
            terminal
                .search_history("absent", SearchOptions::literal())
                .expect("a literal query compiles")
                .is_empty(),
            "a query nothing matches is an empty list, not an error"
        );

        let failure = terminal
            .search_history(
                "Hel(lo",
                SearchOptions {
                    case_sensitive: false,
                    regex: true,
                },
            )
            .expect_err("an unclosed group cannot be compiled");
        assert_eq!(failure.query, "Hel(lo");
        assert!(
            !failure.reason.is_empty() && !failure.reason.contains('\n'),
            "the reason is one readable line, got {:?}",
            failure.reason
        );
        assert!(
            terminal
                .search_history("Hel(lo", SearchOptions::literal())
                .expect("the same text is literal when the switch is off")
                .is_empty(),
            "literal text is not parsed as an expression"
        );

        // The blocks read the same options as the scrollback.
        let insensitive = terminal
            .search_blocks("hello", BlockFilter::All, SearchOptions::literal())
            .expect("a literal query compiles");
        assert_eq!(insensitive.len(), 1, "case folds in the block search too");
        let sensitive = terminal
            .search_blocks(
                "hello",
                BlockFilter::All,
                SearchOptions {
                    case_sensitive: true,
                    regex: false,
                },
            )
            .expect("a literal query compiles");
        assert!(
            sensitive.is_empty(),
            "a case-sensitive query does not match Hello"
        );
        let expression = terminal
            .search_blocks(
                "H.llo there$",
                BlockFilter::All,
                SearchOptions {
                    case_sensitive: true,
                    regex: true,
                },
            )
            .expect("a valid expression compiles");
        assert_eq!(expression.len(), 1, "the output is searched as a line");
        assert!(
            terminal
                .search_blocks(
                    "Hel(lo",
                    BlockFilter::All,
                    SearchOptions {
                        case_sensitive: false,
                        regex: true,
                    },
                )
                .is_err(),
            "a broken expression is an error for the block list as well"
        );
    }

    #[test]
    fn a_pattern_that_matches_nothing_reports_no_places() {
        let mut terminal = core();
        terminal.feed(b"abc\r\n");
        let hits = terminal
            .search_history(
                "x*",
                SearchOptions {
                    case_sensitive: false,
                    regex: true,
                },
            )
            .expect("the expression compiles");
        assert!(
            hits.is_empty(),
            "zero-width matches are not places to step through: {hits:?}"
        );
    }

    #[test]
    fn alternate_screen_is_the_classic_grid_and_the_primary_screen_returns() {
        let mut terminal = core();
        terminal.set_presentation(TerminalMode::Integrated);
        feed_block(&mut terminal, "echo before", "before", "133;D;0");
        let primary = terminal.snapshot().text();
        let block = terminal.command_blocks()[0].clone();
        assert!(terminal.shows_command_blocks());

        terminal.feed(b"\x1b[?1049h\x1b[H");
        assert!(terminal.mode().contains(TermMode::ALT_SCREEN));
        assert!(
            terminal.shows_classic_grid(),
            "a full-screen program is the classic grid"
        );
        terminal.feed(b"inside");
        assert_eq!(terminal.snapshot().row_text(0).as_deref(), Some("inside"));

        terminal.feed(b"\x1b[?1049l");
        assert!(!terminal.mode().contains(TermMode::ALT_SCREEN));
        assert_eq!(
            terminal.snapshot().text(),
            primary,
            "leaving the alternate screen restores the previous grid"
        );
        assert_eq!(
            terminal.command_blocks()[0],
            block,
            "the full-screen program does not rewrite the recorded command"
        );
        assert!(terminal.shows_command_blocks());
    }

    #[test]
    fn classic_mode_an_unsupported_shell_and_a_failed_integration_keep_the_grid() {
        let mut terminal = core();
        terminal.feed(b"plain prompt\r\n");
        terminal.set_presentation(TerminalMode::Integrated);
        let text = terminal.snapshot().text();
        assert!(
            terminal.shows_classic_grid(),
            "no integration markers means the classic grid"
        );
        assert!(terminal.command_blocks().is_empty());
        assert_eq!(terminal.snapshot().text(), text);

        feed_block(&mut terminal, "echo kept", "kept", "133;D;0");
        let with_block = terminal.snapshot().text();
        terminal.set_presentation(TerminalMode::Classic);
        assert!(terminal.shows_classic_grid());
        assert_eq!(
            terminal.snapshot().text(),
            with_block,
            "turning command blocks off does not rewrite the grid"
        );
        assert_eq!(terminal.command_blocks()[0].command, "echo kept");

        terminal.set_presentation(TerminalMode::Integrated);
        let id: CommandBlockId = terminal.command_blocks()[0].id;
        terminal.feed(&osc("133;B"));
        terminal.feed(b"broken");
        terminal.feed(&osc("133;C"));
        terminal.feed(b"\r\nbad\r\n");
        terminal.feed(&osc("133;D;not-a-code"));
        assert!(terminal.integration_failed());
        assert!(
            terminal.shows_classic_grid(),
            "an unreadable exit field falls back to the classic grid"
        );
        let broken = terminal
            .command_blocks()
            .into_iter()
            .find(|block| block.id != id)
            .expect("the unreadable end is its own block");
        assert_eq!(broken.status, BlockStatus::Unknown);
        assert!(!broken.status.is_success());
        let again = terminal.snapshot().text();
        terminal.invalidate_integration();
        assert_eq!(
            terminal.snapshot().text(),
            again,
            "falling back does not rewrite the grid"
        );
        assert!(
            terminal
                .command_blocks()
                .iter()
                .all(|block| !block.status.is_success() || block.id == id)
        );
    }

    #[test]
    fn a_command_across_the_alternate_screen_does_not_copy_scrollback() {
        let mut terminal = TerminalCore::new(40, 8, 100);
        terminal.set_presentation(TerminalMode::Integrated);
        for line in 0..30 {
            terminal.feed(format!("older {line}\r\n").as_bytes());
        }
        assert_eq!(
            terminal.history(),
            23,
            "thirty lines on an eight-row grid leave a real scrollback"
        );

        terminal.feed(&osc("133;A"));
        terminal.feed(&osc("133;B"));
        terminal.feed(b"vim file");
        terminal.feed(&osc("133;C"));
        terminal.feed(b"\r\n");
        let primary = terminal.snapshot().text();

        terminal.feed(b"\x1b[?1049h");
        assert!(terminal.mode().contains(TermMode::ALT_SCREEN));
        assert!(
            terminal.shows_classic_grid(),
            "the full-screen program is the classic grid"
        );
        terminal.feed(b"fullscreen-not-output");
        let during = terminal.command_blocks();
        assert_eq!(during.len(), 1);
        assert_eq!(during[0].command, "vim file");
        assert!(
            !during[0].output.contains("older") && !during[0].output.contains("fullscreen"),
            "the alternate screen is not this command's output: {}",
            during[0].output
        );

        terminal.feed(b"\x1b[?1049l");
        assert!(!terminal.mode().contains(TermMode::ALT_SCREEN));
        assert_eq!(
            terminal.snapshot().text(),
            primary,
            "leaving the alternate screen restores the primary grid"
        );
        terminal.feed(&osc("133;D;0"));
        let finished = terminal.command_blocks();
        assert_eq!(finished[0].status, BlockStatus::Exited(0));
        assert!(finished[0].status.is_success());
        assert_eq!(finished[0].command, "vim file");
        assert!(
            !finished[0].output.contains("older") && !finished[0].output.contains("fullscreen"),
            "scrollback must not be copied into the command: {}",
            finished[0].output
        );
        let vim = finished[0].clone();

        terminal.feed(&osc("133;A"));
        terminal.feed(&osc("133;B"));
        terminal.feed(b"echo after");
        terminal.feed(&osc("133;C"));
        terminal.feed(b"\r\nafter-out\r\n");
        terminal.feed(&osc("133;D;0"));
        let blocks = terminal.command_blocks();
        assert_eq!(
            blocks[0], vim,
            "the earlier command stays as it was recorded"
        );
        assert_eq!(blocks[1].command, "echo after");
        assert_eq!(blocks[1].output, "after-out");
        assert!(blocks[1].status.is_success());
    }
}
