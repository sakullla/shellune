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

use std::hash::{DefaultHasher, Hasher};
use std::ops::Range;
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
    WorkingDirectory,
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
    /// The working directory the shell last stated with OSC 7 during this chunk (R15).
    ///
    /// It is the shell's own report and nothing guessed from output: a chunk that
    /// printed a path without an OSC 7 leaves this empty. Only the last report of the
    /// chunk is kept, because each one replaces the previous state of the same shell.
    pub directory: Option<String>,
    /// Desktop notifications the program asked for with OSC 9 / OSC 777 (R2).
    ///
    /// Unlike the counts above these carry text, so they are kept apart from the
    /// tab marks (which must never repeat what ran) and only reach the system
    /// notification path, which the user can switch off.
    pub notices: Vec<crate::blocks::ProgramNotice>,
    /// A command that ran at least the long-command threshold ended in this chunk
    /// (R2). Only set when a threshold was given with
    /// [`TerminalCore::set_long_command_threshold`].
    pub finished_command: Option<FinishedCommand>,
}

/// A command the shell reported as ended after running for a long time (R2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FinishedCommand {
    /// The command line, cut to [`MAX_FINISHED_COMMAND`] characters.
    pub command: String,
    /// How it ended, as the shell reported it.
    pub status: crate::blocks::BlockStatus,
    /// From output start to the end marker.
    pub duration: std::time::Duration,
}

/// Longest command text a finished-command notice carries, in characters.
pub const MAX_FINISHED_COMMAND: usize = 256;

impl TerminalEffects {
    /// Whether nothing was requested.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.writes.is_empty()
            && self.title.is_none()
            && self.clipboard.is_none()
            && !self.bell
            && self.commands_finished == 0
            && self.notices.is_empty()
            && self.finished_command.is_none()
    }

    /// Whether a background event happened that a tab may mark, without any of its text.
    #[must_use]
    pub fn has_background_notice(&self) -> bool {
        self.bell || self.commands_finished > 0
    }
}

/// A colour query (OSC 4, 10, 11 or 12) a program asked during one chunk.
///
/// The answer is written in place, among the chunk's other replies: a program that
/// sends OSC 11 and then a device-attributes query uses the second answer to know the
/// first will never come, so the order the questions were asked in is kept.
struct ColorQuery {
    /// Where in the chunk's replies the answer goes.
    offset: usize,
    /// The palette slot asked about: 0-255, or 256+ for the named colours.
    index: usize,
    /// Turns the colour into the escape sequence the program expects.
    format: Arc<dyn Fn(Rgb) -> String + Send + Sync>,
}

#[derive(Default)]
struct SinkState {
    effects: TerminalEffects,
    queries: Vec<ColorQuery>,
}

/// Event sink that records what the terminal asked for.
#[derive(Clone, Default)]
struct EffectSink {
    state: Arc<Mutex<SinkState>>,
}

impl EffectSink {
    fn take_effects(&self) -> (TerminalEffects, Vec<ColorQuery>) {
        let mut state = self.state.lock().expect("the effect sink is not poisoned");
        (
            std::mem::take(&mut state.effects),
            std::mem::take(&mut state.queries),
        )
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
            // Answered once the chunk is parsed, from the colours really on screen;
            // a program left without an answer guesses a dark background (R8).
            Event::ColorRequest(index, format) => {
                let offset = state.effects.writes.len();
                state.queries.push(ColorQuery {
                    offset,
                    index,
                    format,
                });
            }
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
    /// OSC 8 hyperlinks a program attached to cells of this row, left to right (R1).
    ///
    /// Kept per row as column spans rather than on every cell: most rows carry none,
    /// and a span is what both hovering and opening need.
    pub links: Vec<RowLink>,
}

/// Total decoded image memory one terminal keeps, in bytes (R4). Past it the
/// oldest images are dropped first.
pub const MAX_IMAGE_BYTES: usize = 64 * 1024 * 1024;

/// An image on screen, for the view to draw (R4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VisibleImage {
    /// Stable for the image's life, so a renderer can keep what it made of it.
    pub id: u64,
    /// Viewport row of the image's top edge. Negative when the top has scrolled
    /// above the viewport and only the lower part shows.
    pub row: i32,
    /// Grid column of its left edge.
    pub column: usize,
    /// The pixels.
    pub image: Arc<crate::sixel::SixelImage>,
}

/// An image a program placed on the grid (R4).
#[derive(Debug)]
struct PlacedImage {
    id: u64,
    /// Stable line of the image's top row; see [`TerminalCore::stable_line`].
    stable: i64,
    column: usize,
    /// Rows it covered when it was placed.
    rows: usize,
    image: Arc<crate::sixel::SixelImage>,
    /// Placed on the alternate screen, which it is shown on and dies with.
    alternate: bool,
    /// The text of each covered row when it was placed: a row that changes means
    /// text was written over the image, which then goes, as in other terminals.
    covered: Vec<u64>,
}

impl PlacedImage {
    fn bytes(&self) -> usize {
        self.image.rgba.len()
    }
}

fn text_hash(text: &str) -> u64 {
    let mut hash = DefaultHasher::new();
    hash.write(text.trim_end().as_bytes());
    hash.finish()
}

/// A run of cells on one row that a program linked with OSC 8 (R1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowLink {
    /// Grid column span `[start..end)` covered by the link on this row.
    pub columns: Range<usize>,
    /// The link's identity: its OSC 8 `id`, or one the parser gave it.
    ///
    /// Two spans with the same identity and target are the same link, even on
    /// different rows, so a link wrapped over two lines is hovered as one.
    pub id: Arc<str>,
    /// The target exactly as the program stated it.
    pub uri: Arc<str>,
}

/// Where a link under the pointer came from (R1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkKind {
    /// Text that looks like an `http(s)://` address.
    Text,
    /// A link the program attached to the cells with OSC 8.
    Osc8,
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

/// A grid row that still names the same content after lines scroll into history.
///
/// `stable` is `history_size + line + lines_dropped`, the same quantity a command
/// block stores. A raw line number is not that quantity: while the viewport is
/// parked in history, new output moves every existing row further up the grid.
/// Once the scrollback is full, that move no longer changes `history_size`;
/// `lines_dropped` counts the rows rotated out instead, so the sum still names
/// the same row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineAnchor {
    stable: i64,
    /// Byte column of a search hit. A viewport row leaves this at zero.
    column: usize,
}

impl LineAnchor {
    /// Byte column captured with the row.
    #[must_use]
    pub fn column(self) -> usize {
        self.column
    }
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
    /// Images that reach into the viewport, oldest first (R4).
    pub images: Vec<VisibleImage>,
}

/// A link found at one visible position: recognized `http(s)://` text, or an
/// OSC 8 hyperlink.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DetectedLink {
    /// Viewport row index from the top.
    pub row: usize,
    /// Grid column span `[start..end)` covered by the link on that row.
    pub columns: Range<usize>,
    /// The target: matched URL text with trailing sentence punctuation stripped, or
    /// the OSC 8 target as stated.
    pub url: String,
    /// Text or OSC 8.
    pub kind: LinkKind,
    /// Every visible `(row, columns)` span of the same link, the found one included.
    ///
    /// An OSC 8 link can wrap over several rows; recognized text is one span.
    pub spans: Vec<(usize, Range<usize>)>,
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

    /// The OSC 8 hyperlink covering `(row, column)`, with every visible span of it.
    ///
    /// The program declared it, so unlike recognized text it is offered on the
    /// alternate screen too. Whether the target may be opened is not decided here.
    #[must_use]
    pub fn hyperlink_at(&self, row: usize, column: usize) -> Option<DetectedLink> {
        let found = self
            .rows
            .get(row)?
            .links
            .iter()
            .find(|link| link.columns.contains(&column))?;
        let spans = self
            .rows
            .iter()
            .enumerate()
            .flat_map(|(index, rendered)| {
                rendered
                    .links
                    .iter()
                    .filter(|link| link.id == found.id && link.uri == found.uri)
                    .map(move |link| (index, link.columns.clone()))
            })
            .collect();
        Some(DetectedLink {
            row,
            columns: found.columns.clone(),
            url: found.uri.to_string(),
            kind: LinkKind::Osc8,
            spans,
        })
    }

    /// The link at `(row, column)`: an OSC 8 hyperlink first, then recognized
    /// `http(s)://` text on the primary screen.
    #[must_use]
    pub fn link_at(&self, row: usize, column: usize) -> Option<DetectedLink> {
        self.hyperlink_at(row, column)
            .or_else(|| self.http_link_at(row, column))
    }

    /// Returns the openable `http(s)://` link covering `(row, column)` when not on the
    /// alternate screen.
    #[must_use]
    pub fn http_link_at(&self, row: usize, column: usize) -> Option<DetectedLink> {
        if self.mode.contains(TermMode::ALT_SCREEN) {
            return None;
        }
        let rendered = self.rows.get(row)?;
        http_link_in_cells(row, column, &rendered.cells)
    }
}

/// Adds one linked cell to a row's spans, extending the last span when the cell
/// continues it.
fn push_link_span(
    links: &mut Vec<RowLink>,
    column: usize,
    width: usize,
    link: &alacritty_terminal::term::cell::Hyperlink,
) {
    if let Some(last) = links.last_mut()
        && last.columns.end == column
        && &*last.id == link.id()
        && &*last.uri == link.uri()
    {
        last.columns.end = column + width;
        return;
    }
    links.push(RowLink {
        columns: column..column + width,
        id: Arc::from(link.id()),
        uri: Arc::from(link.uri()),
    });
}

fn http_link_in_cells(row: usize, column: usize, cells: &[RenderedCell]) -> Option<DetectedLink> {
    let mut line = String::new();
    let mut byte_to_cell = Vec::new();
    let mut cell_columns = Vec::with_capacity(cells.len());
    let mut col = 0usize;
    for (index, cell) in cells.iter().enumerate() {
        let width = cell.width.max(1);
        cell_columns.push((col, col + width));
        byte_to_cell.extend(std::iter::repeat_n(index, cell.text.len()));
        line.push_str(&cell.text);
        col += width;
    }
    for (range, kind) in crate::highlight::spans(&line) {
        if kind != crate::highlight::Kind::Link || range.is_empty() {
            continue;
        }
        let candidate = &line[range.clone()];
        if !crate::highlight::is_openable_http_url(candidate) {
            continue;
        }
        let Some(&first_cell) = byte_to_cell.get(range.start) else {
            continue;
        };
        let Some(&last_cell) = byte_to_cell.get(range.end - 1) else {
            continue;
        };
        let start_col = cell_columns[first_cell].0;
        let end_col = cell_columns[last_cell].1;
        if (start_col..end_col).contains(&column) {
            return Some(DetectedLink {
                row,
                columns: start_col..end_col,
                url: candidate.to_owned(),
                kind: LinkKind::Text,
                spans: vec![(row, start_col..end_col)],
            });
        }
    }
    None
}

// The shell echoes edits asynchronously, especially over SSH. Only fingerprints
// are kept while waiting: completion never records a stream of user keystrokes.
#[derive(Default)]
enum CompletionEcho {
    #[default]
    Ready,
    Appended(DefaultHasher),
    Changed(u64),
    NextPrompt,
}

fn prefix_hash(text: &str) -> DefaultHasher {
    let mut hash = DefaultHasher::new();
    for byte in text.bytes() {
        hash.write_u8(byte);
    }
    hash
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
    /// The working directory the shell last stated with OSC 7 (R15).
    ///
    /// Kept beside the blocks because it is the same kind of fact: something the
    /// shell said about itself, not something read off the grid. A new terminal
    /// starts without one, which is what a reconnect gets.
    directory: Option<WorkingDirectory>,
    /// Command blocks built from those markers. Not from prompt text.
    blocks: BlockBook,
    completion_echo: CompletionEcho,
    /// Classic, or command blocks when the shell actually reports them.
    presentation: TerminalMode,
    /// Whether plain output has its addresses, links and failures coloured
    /// (see [`crate::highlight`]).
    highlight: bool,
    /// Lines dropped from the front of history, so anchors survive a full scrollback.
    ///
    /// A shrink of `history_size` is one way lines leave. A full ring also rotates
    /// a row out on every extra scroll without `history_size` changing; both add
    /// here, or a parked row's anchor slips one line per new row.
    lines_dropped: i64,
    /// History size after the previous chunk, to see how many lines were dropped.
    last_history: usize,
    /// `scrolling_history` from construction. The grid stops growing at this size
    /// and then rotates.
    scrollback: usize,
    row_addresses: Vec<usize>,
    /// How a palette colour is shown, for the answers to colour queries.
    ///
    /// The window may draw the grid in another palette than the one cells are
    /// resolved in (a light terminal maps every default slot); a program asking
    /// for the background must hear the colour the user sees.
    shown: fn(Rgb) -> Rgb,
    /// How long a command must run for its end to be reported with its text (R2).
    long_command: Option<std::time::Duration>,
    /// Images programs placed, oldest first (R4).
    images: Vec<PlacedImage>,
    next_image: u64,
    /// The size of one cell in the pixels images are measured in (R4).
    cell_pixels: (f32, f32),
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
            directory: None,
            blocks: BlockBook::new(),
            completion_echo: CompletionEcho::Ready,
            presentation: TerminalMode::Classic,
            highlight: true,
            lines_dropped: 0,
            last_history: 0,
            scrollback,
            row_addresses: Vec::new(),
            shown: |color| color,
            long_command: None,
            images: Vec::new(),
            next_image: 1,
            cell_pixels: (8.0, 16.0),
        }
    }

    /// Sets how a palette colour is shown, so colour queries are answered with what
    /// is on screen rather than the dark defaults.
    pub fn set_shown_colors(&mut self, shown: fn(Rgb) -> Rgb) {
        self.shown = shown;
    }

    /// The colour a program asking about palette slot `index` is told.
    ///
    /// A colour the program set itself (OSC 4/10/11/12) is returned as set; any other
    /// is the default as the window draws it.
    fn queried_color(&self, index: usize) -> Option<Rgb> {
        use alacritty_terminal::vte::ansi::NamedColor;
        let colors = self.term.colors();
        if let Some(set) = colors[index] {
            return Some(set);
        }
        let default = match index {
            0..=255 => palette::indexed(index as u8),
            i if i == NamedColor::Foreground as usize => palette::FOREGROUND,
            i if i == NamedColor::Background as usize => palette::BACKGROUND,
            i if i == NamedColor::Cursor as usize => palette::CURSOR,
            _ => return None,
        };
        Some((self.shown)(default))
    }

    /// Parses a chunk of output and returns what the application must do about it.
    ///
    /// OSC 133 and OSC 633 command markers are taken out of the stream and turned
    /// into command blocks. Every other byte, including other OSC sequences, is
    /// parsed into the grid as before. The grid is never cleared because a marker
    /// was missing or the presentation changed.
    pub fn feed(&mut self, bytes: &[u8]) -> TerminalEffects {
        if bytes.is_empty() {
            return TerminalEffects::default();
        }
        if self.images.is_empty() {
            return self.feed_segments(bytes);
        }
        // Clearing the screen or resetting the terminal takes the images with it
        // (R4). Erasing blank rows leaves no trace in the text, so the sequences
        // are looked for here rather than read back from the grid.
        let reset = contains(bytes, b"\x1bc");
        let scrollback = reset || contains(bytes, b"\x1b[3J");
        let screen = scrollback || contains(bytes, b"\x1b[2J");
        if screen {
            self.clear_images(scrollback);
        }
        let was_alternate = self.on_alternate_screen();
        let effects = self.feed_segments(bytes);
        if was_alternate != self.on_alternate_screen() {
            // Entering or leaving the alternate screen starts it fresh.
            self.images.retain(|image| !image.alternate);
        }
        self.drop_stale_images();
        effects
    }

    fn feed_segments(&mut self, bytes: &[u8]) -> TerminalEffects {
        if self.splitter.can_pass_through(bytes) {
            let plain_ascii = bytes.len() >= 256 && bytes.is_ascii() && !bytes.contains(&0x1b);
            return self.feed_grid(bytes, plain_ascii);
        }
        let segments = self.splitter.push(bytes);
        let mut effects = TerminalEffects::default();
        for segment in segments {
            match segment {
                Segment::Bytes(chunk) => {
                    merge_effects(&mut effects, self.feed_grid(&chunk, false));
                }
                Segment::Event(event) => {
                    self.note_history();
                    // A block can only finish because the shell said it did; the
                    // count is a background notice, never a command line (R7).
                    if self.apply_integration(event) {
                        effects.commands_finished += 1;
                        if let Some(finished) = self.long_command_finished() {
                            effects.finished_command = Some(finished);
                        }
                    }
                }
                Segment::Notice(notice) => {
                    // Taken out of the stream like a directory report: the grid has
                    // received nothing for these bytes (R2).
                    effects.notices.push(notice);
                }
                Segment::Sixel { params, data } => {
                    self.note_history();
                    merge_effects(&mut effects, self.place_sixel(&params, &data));
                }
                // Too large to keep: nothing is drawn and the grid is untouched (R4).
                Segment::SixelDropped => {}
                Segment::Directory(directory) => {
                    // The shell stated where it is. The report replaces the previous
                    // one and is never drawn: the grid has already received nothing
                    // for these bytes (R15).
                    self.note_history();
                    effects.directory = Some(directory.path().to_owned());
                    self.directory = Some(directory);
                }
            }
        }
        effects
    }

    fn feed_grid(&mut self, bytes: &[u8], plain_ascii: bool) -> TerminalEffects {
        self.advance_bytes(bytes, plain_ascii);
        let (mut effects, queries) = self.sink.take_effects();
        // Later answers first, so each earlier offset still points where it did.
        for query in queries.into_iter().rev() {
            if let Some(color) = self.queried_color(query.index) {
                let answer = (query.format)(color);
                let at = query.offset.min(effects.writes.len());
                effects.writes.splice(at..at, answer.into_bytes());
            }
        }
        if let Some(title) = &effects.title {
            self.title = Some(title.clone());
        }
        // The grid parser answers primary device attributes as a VT102; this
        // terminal also draws Sixel images, which programs only send to a terminal
        // that says so (R4).
        if !effects.writes.is_empty() {
            replace_all(&mut effects.writes, PARSER_DA1, SIXEL_DA1);
        }
        self.note_history();
        self.refresh_running_output();
        effects
    }

    /// Tells the terminal how large one cell is, in the pixels images are measured
    /// in, so an image covers the right number of rows and columns (R4).
    pub fn set_cell_pixels(&mut self, width: f32, height: f32) {
        if width.is_finite() && height.is_finite() && width >= 1.0 && height >= 1.0 {
            self.cell_pixels = (width, height);
        }
    }

    /// Decodes and places one Sixel image at the cursor, then moves the cursor to
    /// the row under it, as xterm does with Sixel scrolling on (R4).
    fn place_sixel(&mut self, params: &[u8], data: &[u8]) -> TerminalEffects {
        let background = palette::BACKGROUND;
        let Ok(image) =
            crate::sixel::decode(params, data, [background.r, background.g, background.b])
        else {
            // Malformed or too large: dropped, and the grid is as it was.
            return TerminalEffects::default();
        };
        let (_, cell_height) = self.cell_pixels;
        let rows = ((image.height as f32) / cell_height).ceil().max(1.0) as usize;
        let cursor = self.term.grid().cursor.point;
        let column = cursor.column.0;
        let alternate = self.on_alternate_screen();
        // Make room first: the line feeds scroll the screen when the image is
        // taller than the rows below the cursor, and the image's top row goes with
        // them, so its anchor is taken from where the cursor ends up.
        let effects = self.feed_grid(&vec![b'\n'; rows], false);
        let bottom = self.term.grid().cursor.point.line.0;
        let top = bottom - i32::try_from(rows).unwrap_or(i32::MAX);
        let stable = self.stable_line(top);
        let covered = (0..rows)
            .map(|offset| {
                let line = top + i32::try_from(offset).unwrap_or(0);
                self.line_from_stable(self.stable_line(line))
                    .map_or(0, |line| text_hash(&self.line_text(Line(line))))
            })
            .collect();
        let id = self.next_image;
        self.next_image += 1;
        self.images.push(PlacedImage {
            id,
            stable,
            column,
            rows,
            image: Arc::new(image),
            alternate,
            covered,
        });
        let mut total: usize = self.images.iter().map(PlacedImage::bytes).sum();
        while total > MAX_IMAGE_BYTES && !self.images.is_empty() {
            total -= self.images.remove(0).bytes();
        }
        effects
    }

    /// Drops the images on the current screen (and, with `scrollback`, in its
    /// history too).
    fn clear_images(&mut self, scrollback: bool) {
        let alternate = self.on_alternate_screen();
        let history = self.term.grid().history_size() as i64;
        let first_on_screen = history + self.lines_dropped;
        self.images.retain(|image| {
            if image.alternate != alternate {
                return true;
            }
            !scrollback && image.stable + image.rows as i64 <= first_on_screen
        });
    }

    /// Drops images whose rows left the scrollback or had text written over them.
    fn drop_stale_images(&mut self) {
        let alternate = self.on_alternate_screen();
        let images = std::mem::take(&mut self.images);
        self.images = images
            .into_iter()
            .filter(|image| {
                // Its bottom row rotated out of the kept history: so did the rest.
                let last = image.stable + image.rows as i64 - 1;
                if self.line_from_stable(last).is_none() {
                    return false;
                }
                if image.alternate != alternate {
                    return true;
                }
                image.covered.iter().enumerate().all(|(offset, hash)| {
                    match self.line_from_stable(image.stable + offset as i64) {
                        Some(line) => text_hash(&self.line_text(Line(line))) == *hash,
                        // Rotated out of history at the top: still the same image.
                        None => true,
                    }
                })
            })
            .collect();
    }

    /// How many images this terminal keeps, on either screen.
    #[must_use]
    pub fn image_count(&self) -> usize {
        self.images.len()
    }

    /// Sets how long a command must run before its end is reported as a
    /// [`FinishedCommand`] (R2); `None` reports none.
    pub fn set_long_command_threshold(&mut self, threshold: Option<std::time::Duration>) {
        self.long_command = threshold;
    }

    /// The block that just ended, when it ran at least the threshold.
    fn long_command_finished(&self) -> Option<FinishedCommand> {
        let threshold = self.long_command?;
        let block = self.blocks.last_finished()?;
        let duration = block.duration?;
        if duration < threshold {
            return None;
        }
        Some(FinishedCommand {
            command: block.command.chars().take(MAX_FINISHED_COMMAND).collect(),
            status: block.status,
            duration,
        })
    }

    /// Resizes the grid, keeping the history.
    pub fn resize(&mut self, grid: GridSize) {
        let grid = grid.clamped();
        if grid == self.grid {
            return;
        }
        let rewrapped = grid.cols != self.grid.cols;
        self.term.resize(grid);
        self.grid = grid;
        self.note_history();
        // A new width re-wraps lines, so an image's rows no longer stand where its
        // anchor says: it goes rather than being drawn over other text (R4). A new
        // height keeps them; any row that changed still takes its image with it.
        if rewrapped {
            self.images.clear();
        } else if !self.images.is_empty() {
            self.drop_stale_images();
        }
    }

    /// The working directory the shell last reported with OSC 7 (R15).
    ///
    /// `None` means this shell has not said where it is, which is not the login
    /// directory and not a guess: the file view says so instead of showing a path
    /// nobody stated.
    #[must_use]
    pub fn directory(&self) -> Option<&str> {
        self.directory.as_ref().map(WorkingDirectory::path)
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

    /// Clears the visible primary screen and scrollback history without writing to the PTY.
    ///
    /// Full-screen alternate programs own their screen and are left untouched.
    pub fn clear_screen_and_scrollback(&mut self) {
        // The primary screen's images go with its text (R4).
        self.images.retain(|image| image.alternate);
        if self.on_alternate_screen() {
            return;
        }
        self.parser.advance(&mut self.term, b"\x1b[H\x1b[2J\x1b[3J");
        self.term.scroll_display(Scroll::Bottom);
        self.term.selection = None;
        self.blocks = BlockBook::new();
        self.completion_echo = CompletionEcho::Ready;
        self.lines_dropped = 0;
        self.last_history = 0;
        self.row_addresses.clear();
    }

    /// Returns the openable `http(s)://` link at viewport `(row, column)` when on the
    /// primary screen.
    #[must_use]
    pub fn http_link_at(&self, row: usize, column: usize) -> Option<DetectedLink> {
        if self.on_alternate_screen() || row >= self.grid.rows || column >= self.grid.cols {
            return None;
        }
        self.snapshot().http_link_at(row, column)
    }

    /// The link at viewport `(row, column)`: an OSC 8 hyperlink on either screen,
    /// else recognized `http(s)://` text on the primary screen (R1).
    #[must_use]
    pub fn link_at(&self, row: usize, column: usize) -> Option<DetectedLink> {
        if row >= self.grid.rows || column >= self.grid.cols {
            return None;
        }
        self.snapshot().link_at(row, column)
    }

    /// Starts a mouse selection at a viewport row and column.
    ///
    /// `side` is the half of the cell the pointer is in. A selection's edges are
    /// between cells, as in other terminals: a drag that starts on the left half of
    /// a character takes it, one that starts on its right half leaves it out, in
    /// whichever direction the drag then goes.
    pub fn begin_selection(&mut self, row: usize, column: usize, side: Side, kind: SelectionType) {
        let point = self.grid_point(row, column);
        self.term.selection = Some(Selection::new(kind, point, side));
    }

    /// Starts a mouse selection at a grid line and column.
    ///
    /// Out-of-range lines clamp to the oldest scrollback row or the live bottom.
    pub fn begin_selection_at_line(
        &mut self,
        line: i32,
        column: usize,
        side: Side,
        kind: SelectionType,
    ) {
        let history = self.history() as i64;
        let top = -history;
        let bottom = self.term.grid().screen_lines().saturating_sub(1) as i64;
        let clamped = i32::try_from(i64::from(line).clamp(top, bottom)).unwrap_or(0);
        let column = column.min(self.grid.cols.saturating_sub(1));
        let point = Point::new(Line(clamped), Column(column));
        self.term.selection = Some(Selection::new(kind, point, side));
    }

    /// Starts a mouse selection at an anchored grid line and column.
    ///
    /// If the anchored row has rotated out of scrollback or past the live bottom,
    /// it clamps to the nearest row still in the grid.
    pub fn begin_selection_at_anchor(
        &mut self,
        anchor: LineAnchor,
        column: usize,
        side: Side,
        kind: SelectionType,
    ) {
        let line = self.line_of(anchor).unwrap_or_else(|| {
            let history = self.history() as i64;
            let top = -history;
            let bottom = self.term.grid().screen_lines().saturating_sub(1) as i64;
            let raw = anchor.stable - self.lines_dropped - history;
            i32::try_from(raw.clamp(top, bottom)).unwrap_or(0)
        });
        self.begin_selection_at_line(line, column, side, kind);
    }

    /// Starts a selection at the cursor's cell, on its left edge.
    ///
    /// It is what Shift+click extends when nothing is selected yet: the text
    /// between the cursor and the click.
    pub fn begin_selection_at_cursor(&mut self) {
        let point = self.term.grid().cursor.point;
        self.term.selection = Some(Selection::new(SelectionType::Simple, point, Side::Left));
    }

    /// Moves the free end of the selection to a viewport row and column, on the
    /// `side` half of that cell.
    pub fn extend_selection(&mut self, row: usize, column: usize, side: Side) {
        let point = self.grid_point(row, column);
        if let Some(selection) = self.term.selection.as_mut() {
            selection.update(point, side);
        }
    }

    /// The kind of the selection being kept, even one that covers nothing yet.
    #[must_use]
    pub fn selection_kind(&self) -> Option<SelectionType> {
        self.term.selection.as_ref().map(|selection| selection.ty)
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

    /// Selects every line from the top of scrollback through the bottom of the screen.
    pub fn select_all(&mut self) {
        let grid = self.term.grid();
        let cols = grid.columns();
        if cols == 0 {
            return;
        }
        let start = Point::new(grid.topmost_line(), Column(0));
        let end = Point::new(grid.bottommost_line(), Column(cols - 1));
        let mut selection = Selection::new(SelectionType::Lines, start, Side::Left);
        selection.update(end, Side::Right);
        self.term.selection = Some(selection);
    }

    /// A picture of the visible grid.
    #[must_use]
    pub fn snapshot(&self) -> Snapshot {
        let content = self.term.renderable_content();
        let display_offset = content.display_offset;
        let selection = content.selection;
        let mut rows: Vec<_> = (0..self.grid.rows)
            .map(|_| RenderedRow {
                cells: Vec::with_capacity(self.grid.cols),
                links: Vec::new(),
            })
            .collect();
        // Which cells the program left in the default ink: only those may be
        // highlighted, so a program's own colours always win.
        let mut plain: Vec<Vec<bool>> = vec![Vec::new(); self.grid.rows];
        let highlight = self.highlight && !content.mode.contains(TermMode::ALT_SCREEN);

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

            if let Some(link) = cell.hyperlink() {
                let width = if cell.flags.contains(Flags::WIDE_CHAR) {
                    2
                } else {
                    1
                };
                push_link_span(&mut row.links, point.column.0, width, &link);
            }

            if highlight && let Some(flags) = plain.get_mut(viewport.line) {
                flags.push(
                    matches!(
                        cell.fg,
                        alacritty_terminal::vte::ansi::Color::Named(
                            alacritty_terminal::vte::ansi::NamedColor::Foreground
                        )
                    ) && !cell.flags.contains(Flags::INVERSE),
                );
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
                // A tab is the marker the grid keeps for the stop it covers: the
                // cells after it are already blank, so it is drawn as the blank it
                // stands for. Drawn as a tab, its width is not a cell's — a tab
                // inside a run of text is not one advance wide — and every column
                // after it in the line drifts. "ls" pads its columns with tabs.
                text.push(if cell.c == '\t' { ' ' } else { cell.c });
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

        if highlight {
            for (row, plain) in rows.iter_mut().zip(&plain) {
                highlight_row(row, plain);
            }
        }

        let images = self.visible_images(display_offset);
        Snapshot {
            rows,
            cursor: self.viewport_cursor(display_offset, content.cursor),
            grid: self.grid,
            display_offset,
            history: self.term.grid().history_size(),
            mode: content.mode,
            images,
        }
    }

    /// The images of the screen on show that reach into the viewport (R4).
    fn visible_images(&self, display_offset: usize) -> Vec<VisibleImage> {
        if self.images.is_empty() {
            return Vec::new();
        }
        let alternate = self.on_alternate_screen();
        let viewport_rows = self.grid.rows as i64;
        let history = self.term.grid().history_size() as i64;
        self.images
            .iter()
            .filter(|image| image.alternate == alternate)
            .filter_map(|image| {
                // Stable line -> grid line -> viewport row.
                let line = image.stable - self.lines_dropped - history;
                let row = line + display_offset as i64;
                let bottom = row + image.rows as i64;
                if bottom <= 0 || row >= viewport_rows {
                    return None;
                }
                Some(VisibleImage {
                    id: image.id,
                    row: i32::try_from(row).ok()?,
                    column: image.column,
                    image: image.image.clone(),
                })
            })
            .collect()
    }

    /// Cursor geometry for input methods that only need its screen position.
    #[must_use]
    pub fn visible_cursor(&self) -> Option<RenderedCursor> {
        let content = self.term.renderable_content();
        self.viewport_cursor(content.display_offset, content.cursor)
    }

    fn viewport_cursor(
        &self,
        display_offset: usize,
        cursor: alacritty_terminal::term::RenderableCursor,
    ) -> Option<RenderedCursor> {
        point_to_viewport(display_offset, cursor.point)
            .filter(|point| point.line < self.grid.rows)
            .map(|point| RenderedCursor {
                row: point.line,
                column: point.column.0,
                shape: cursor.shape,
            })
    }

    /// Turns the colouring of addresses, links and failures in plain output on or off.
    pub fn set_highlight(&mut self, on: bool) {
        self.highlight = on;
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

    /// Anchors `line` so it can be found again after the grid scrolls.
    #[must_use]
    pub fn anchor_line(&self, line: i32, column: usize) -> LineAnchor {
        LineAnchor {
            stable: self.stable_line(line),
            column,
        }
    }

    /// Anchors a viewport row so it can be found again after streaming output
    /// scrolls the grid.
    #[must_use]
    pub fn anchor_viewport_row(&self, row: usize) -> LineAnchor {
        let point = self.grid_point(row, 0);
        self.anchor_line(point.line.0, 0)
    }

    /// Top row of a viewport parked in history.
    ///
    /// `None` means the viewport is pinned to the live bottom. That pin is not a
    /// row: new output leaves it there, and anchoring line 0 would later scroll
    /// back to an old row.
    #[must_use]
    pub fn parked_viewport(&self) -> Option<LineAnchor> {
        let offset = self.display_offset();
        if offset == 0 {
            return None;
        }
        let line = -i32::try_from(offset).unwrap_or(i32::MAX);
        Some(self.anchor_line(line, 0))
    }

    /// Grid line of `anchor`, if that row is still in the grid.
    #[must_use]
    pub fn line_of(&self, anchor: LineAnchor) -> Option<i32> {
        self.line_from_stable(anchor.stable)
    }

    /// Display offset that puts `anchor` at the top of the viewport.
    ///
    /// A row that has fallen out of the scrollback clamps to the oldest row still
    /// kept. A row on the live screen clamps to the bottom.
    #[must_use]
    pub fn offset_of(&self, anchor: LineAnchor) -> usize {
        let history = self.history();
        let history_i = i64::try_from(history).unwrap_or(i64::MAX);
        let line = anchor.stable - self.lines_dropped - history_i;
        if line >= 0 {
            return 0;
        }
        usize::try_from(-line).unwrap_or(history).min(history)
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
                            .and_then(|anchor| self.command_between(anchor, self.anchor_now()));
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
                self.completion_echo = CompletionEcho::Ready;
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
                        .and_then(|anchor| self.command_between(anchor, self.anchor_now()))
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
                        .and_then(|anchor| self.output_between(anchor, self.anchor_now()))
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
        let text = self.output_between(anchor, self.anchor_now());
        self.blocks.set_running_output(text);
    }

    fn anchor_now(&self) -> GridAnchor {
        let point = self.term.grid().cursor.point;
        GridAnchor {
            stable: self.stable_line(point.line.0),
            column: point.column.0,
        }
    }

    /// Visible, echoed text from the shell's explicit input marker to the end
    /// of the edited line. Leave multiline editors and ambiguous grids alone.
    fn completion_prefix(&self) -> Option<String> {
        if !self.blocks.in_input()
            || self.blocks.failed()
            || self.on_alternate_screen()
            || self.display_offset() != 0
            || !self.mode().contains(TermMode::SHOW_CURSOR)
        {
            return None;
        }
        let anchor = self.blocks.input_anchor()?;
        let grid = self.term.grid();
        let cursor = &grid.cursor;
        let point = cursor.point;
        if anchor.stable != self.stable_line(point.line.0)
            || anchor.column > point.column.0
            || cursor.input_needs_wrap
        {
            return None;
        }
        let row = &grid[point.line];
        // An arrow key in the middle of a command belongs to the line editor.
        if (point.column.0..self.grid.cols).any(|column| {
            let cell = &row[Column(column)];
            cell.c != ' ' || cell.zerowidth().is_some()
        }) {
            return None;
        }
        let mut prefix = String::new();
        for column in anchor.column..point.column.0 {
            let cell = &row[Column(column)];
            if cell.flags.contains(Flags::HIDDEN) || cell.c.is_control() {
                return None;
            }
            if !cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                prefix.push(cell.c);
                if let Some(combining) = cell.zerowidth() {
                    prefix.extend(combining.iter());
                }
            }
        }
        Some(prefix)
    }

    /// Printable suffix the user can accept at the current shell prompt.
    #[must_use]
    pub fn history_completion(&self) -> Option<&str> {
        let prefix = self.completion_prefix()?;
        if prefix.trim().is_empty() {
            return None;
        }
        let seen = prefix_hash(&prefix).finish();
        match &self.completion_echo {
            CompletionEcho::Appended(expected) if expected.finish() != seen => None,
            CompletionEcho::Changed(previous) if *previous == seen => None,
            CompletionEcho::NextPrompt => None,
            _ => self.blocks.history_suffix(&prefix),
        }
    }

    /// Hold a hint until the PTY echoes this edit. A submission closes the
    /// input period immediately even if an SSH round trip is still pending.
    pub fn note_command_input(&mut self, bytes: &[u8]) {
        if !self.blocks.in_input() || matches!(self.completion_echo, CompletionEcho::NextPrompt) {
            return;
        }
        if bytes.iter().any(|byte| matches!(byte, b'\r' | b'\n' | 3)) {
            self.completion_echo = CompletionEcho::NextPrompt;
            return;
        }
        let Some(prefix) = self.completion_prefix() else {
            return;
        };
        if std::str::from_utf8(bytes).is_ok_and(|text| !text.chars().any(char::is_control)) {
            let mut hash = match std::mem::take(&mut self.completion_echo) {
                CompletionEcho::Appended(previous) => previous,
                _ => prefix_hash(&prefix),
            };
            for &byte in bytes {
                hash.write_u8(byte);
            }
            self.completion_echo = CompletionEcho::Appended(hash);
        } else {
            self.completion_echo = CompletionEcho::Changed(prefix_hash(&prefix).finish());
        }
    }

    /// Dismiss the current suggestion without sending Escape to the shell.
    pub fn dismiss_history_completion(&mut self) {
        if let Some(prefix) = self.completion_prefix() {
            self.completion_echo = CompletionEcho::Changed(prefix_hash(&prefix).finish());
        }
    }

    /// `history_size + line + lines_dropped`. Command blocks and search anchors
    /// both use this, so a row that scrolled into history stays the same row.
    fn stable_line(&self, line: i32) -> i64 {
        let history = self.term.grid().history_size() as i64;
        history + i64::from(line) + self.lines_dropped
    }

    fn line_from_stable(&self, stable: i64) -> Option<i32> {
        let history = self.term.grid().history_size() as i64;
        let line = stable - self.lines_dropped - history;
        let top = -history;
        let bottom = self.term.grid().screen_lines() as i64 - 1;
        if !(top..=bottom).contains(&line) {
            return None;
        }
        i32::try_from(line).ok()
    }

    fn point_of(&self, anchor: GridAnchor) -> Option<Point> {
        let line = self.line_from_stable(anchor.stable)?;
        let column = anchor.column.min(self.grid.cols.saturating_sub(1));
        Some(Point::new(Line(line), Column(column)))
    }

    // Block previews retain a bounded tail. The classic grid keeps its configured
    // scrollback independently. Avoid rescanning the entire command on every read.
    fn output_between(&self, from: GridAnchor, to: GridAnchor) -> Option<String> {
        let top = self.stable_line(-(self.history() as i32));
        let earliest = top.max(to.stable.saturating_sub(128));
        let clipped = from.stable < earliest;
        let start = if clipped {
            GridAnchor {
                stable: earliest,
                column: 0,
            }
        } else {
            from
        };
        let text = self.text_between(start, to)?;
        Some(if clipped {
            format!("[…]\n{text}")
        } else {
            text
        })
    }

    fn command_between(&self, from: GridAnchor, to: GridAnchor) -> Option<String> {
        let start = self.point_of(from)?;
        let end = self.point_of(to)?;
        let grid = self.term.grid();
        for line in start.line.0..=end.line.0 {
            let left = if line == start.line.0 {
                start.column.0
            } else {
                0
            };
            let right = if line == end.line.0 {
                end.column.0
            } else {
                grid.last_column().0
            };
            if (left..=right).any(|column| {
                grid[Line(line)][Column(column)]
                    .flags
                    .contains(Flags::HIDDEN)
            }) {
                return None;
            }
        }
        self.text_between(from, to)
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

    fn screen_rows(&self) -> usize {
        self.grid.rows.max(1)
    }

    /// Parses `bytes`, counting rows rotated out of a full scrollback.
    ///
    /// Below the limit the grid grows and line numbers move with `history_size`,
    /// so anchors stay put without touching `lines_dropped`. At the limit the
    /// ring rotates instead: the row now at the top is the row that used to sit
    /// `dropped` lines below it, and that distance is what `lines_dropped` gains.
    fn advance_bytes(&mut self, mut bytes: &[u8], plain_ascii: bool) {
        if bytes.is_empty() {
            return;
        }
        if self.scrollback == 0 {
            self.parser.advance(&mut self.term, bytes);
            return;
        }
        // An escape-free ASCII read cannot introduce a new VT sequence. Its
        // scroll bound can be counted in bulk instead of testing it per byte.
        // The whole-read bound also covers every remaining suffix of the read.
        let ascii_depth = plain_ascii.then(|| self.scroll_bound(bytes));
        while !bytes.is_empty() {
            if self.on_alternate_screen() {
                // One byte at a time so a sequence that leaves the alternate grid
                // does not swallow the primary output that follows it.
                let n = Self::alt_step(bytes).clamp(1, bytes.len());
                self.parser.advance(&mut self.term, &bytes[..n]);
                bytes = &bytes[n..];
                if !self.on_alternate_screen() {
                    self.note_history();
                }
                continue;
            }
            let history = self.history();
            let rows = self.screen_rows();
            if history < self.scrollback {
                let room = self.scrollback - history;
                // One byte scrolls at most one screen, so this many bytes cannot
                // rotate anything out yet.
                if room >= rows {
                    let n = (room / rows).min(bytes.len()).max(1);
                    self.parser.advance(&mut self.term, &bytes[..n]);
                    bytes = &bytes[n..];
                    self.note_history();
                    continue;
                }
            }
            let (n, depth) = match ascii_depth {
                Some(depth) if depth < history.saturating_add(rows) => (bytes.len(), depth),
                _ => self.measured_step(bytes),
            };
            self.advance_measured(&mut bytes, n, depth);
        }
    }

    /// Bytes that can start a control sequence, as opposed to plain text.
    fn is_hazard(byte: u8) -> bool {
        byte == 0x1b || (0x80..=0x9f).contains(&byte)
    }

    /// Keep continuation bytes of a complete printable UTF-8 character together.
    /// An isolated C1 byte or a partial character still takes the control-aware path.
    fn text_unit_len(bytes: &[u8]) -> usize {
        match bytes[0] {
            0xc2..=0xf4 => {
                let width = bytes[0].leading_ones() as usize;
                if let Some(unit) = bytes.get(..width)
                    && !(unit[0] == 0xc2 && unit[1] < 0xa0)
                    && std::str::from_utf8(unit).is_ok()
                {
                    return width;
                }
                1
            }
            _ => 1,
        }
    }

    /// How many leading bytes to parse while the alternate grid is showing.
    ///
    /// A sequence is taken whole. Any other byte is taken alone: it may be the
    /// final byte of a sequence started earlier, including the one that returns
    /// to the primary grid, and the bytes after it must be counted there.
    fn alt_step(bytes: &[u8]) -> usize {
        if bytes.is_empty() {
            return 0;
        }
        if Self::is_hazard(bytes[0]) {
            return sequence_len(bytes);
        }
        1
    }

    /// A slice whose scroll is bounded, and how many rows that bound covers.
    ///
    /// The depth stays inside the active grid so the top row after the slice is
    /// still one of the rows snapshotted before it. A larger scroll would wrap
    /// the ring and the same address would name a different row.
    fn measured_step(&self, bytes: &[u8]) -> (usize, usize) {
        let rows = self.screen_rows();
        let active = self.history().saturating_add(rows).max(1);
        let limit = active - 1;
        let cols = self.grid.cols.max(1);
        let mut n = 0;
        let mut newlines = 0usize;
        while n < bytes.len() {
            let rest = &bytes[n..];
            let width = if !Self::is_hazard(rest[0]) {
                Self::text_unit_len(rest)
            } else if let Some(len) = sgr_len(rest) {
                // A complete rendition change cannot scroll or switch grids.
                // Include it in the same history measurement as the text it styles.
                len
            } else if n == 0 {
                let len = sequence_len(rest).clamp(1, rest.len());
                let depth = self.scroll_bound(&rest[..len]).min(limit).max(1);
                return (len, depth);
            } else {
                break;
            };
            let mut next_newlines = newlines;
            if matches!(bytes[n], b'\n' | 0x0b | 0x0c) {
                next_newlines += 1;
            }
            let next = n + width;
            // `rows` covers one control action already in progress at the start
            // of this slice. After that, only newlines and wraps scroll.
            let bound = rows + next_newlines + next / cols;
            if n > 0 && bound > limit {
                break;
            }
            n = next;
            newlines = next_newlines;
            if bound > limit {
                break;
            }
        }
        let n = n.clamp(1, bytes.len());
        let depth = (rows + newlines + n / cols).min(limit).max(1);
        (n, depth)
    }

    /// Upper bound on how many rows `bytes` can scroll into history.
    fn scroll_bound(&self, bytes: &[u8]) -> usize {
        let rows = self.screen_rows();
        let cols = self.grid.cols.max(1);
        let newlines = bytes
            .iter()
            .filter(|byte| matches!(byte, b'\n' | 0x0b | 0x0c))
            .count();
        rows.saturating_add(newlines)
            .saturating_add(bytes.len() / cols)
            .max(1)
    }

    fn advance_measured(&mut self, bytes: &mut &[u8], n: usize, depth: usize) {
        let n = n.clamp(1, bytes.len());
        let before_history = self.history();
        let before_alt = self.on_alternate_screen();
        let mut rows = std::mem::take(&mut self.row_addresses);
        self.line_addrs(depth, &mut rows);
        self.parser.advance(&mut self.term, &bytes[..n]);
        *bytes = &bytes[n..];
        if self.on_alternate_screen() || self.on_alternate_screen() != before_alt {
            self.row_addresses = rows;
            if !self.on_alternate_screen() {
                self.note_history();
            }
            return;
        }
        let history = self.history();
        if history < before_history {
            self.lines_dropped += (before_history - history) as i64;
        } else if before_history == self.scrollback || history == self.scrollback {
            self.lines_dropped += self.dropped_in_rotation(&rows) as i64;
        }
        self.last_history = history;
        self.row_addresses = rows;
    }

    /// Addresses of the rows a scroll of at most `depth` can move onto the top.
    fn line_addrs(&self, depth: usize, addresses: &mut Vec<usize>) {
        addresses.clear();
        let top = -i32::try_from(self.history()).unwrap_or(i32::MAX);
        let bottom = self.term.grid().bottommost_line().0;
        let depth = i32::try_from(depth).unwrap_or(i32::MAX);
        let end = top.saturating_add(depth).min(bottom);
        if end < top {
            return;
        }
        let grid = self.term.grid();
        addresses.extend((top..=end).map(|line| std::ptr::from_ref(&grid[Line(line)]).addr()));
    }

    /// How many rows rotated off the top, from a [`Self::line_addrs`] snapshot.
    ///
    /// Growing into free history leaves the old top row as the new top, so this
    /// is zero. A full ring (or a scroll that also crosses the limit) puts an
    /// older row there; its index in the snapshot is the number of rows that left.
    fn dropped_in_rotation(&self, before: &[usize]) -> usize {
        if before.is_empty() {
            return 0;
        }
        let top = -i32::try_from(self.history()).unwrap_or(i32::MAX);
        let addr = std::ptr::from_ref(&self.term.grid()[Line(top)]).addr();
        before
            .iter()
            .position(|candidate| *candidate == addr)
            .unwrap_or(before.len())
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

/// Only complete, numeric SGRs can share a measurement with printable text.
/// Leave partial sequences, private modes and embedded controls to the VT parser
/// in a separate step: those may scroll, clear history or switch screens.
fn sgr_len(bytes: &[u8]) -> Option<usize> {
    let params = bytes.strip_prefix(b"\x1b[")?;
    for (index, byte) in params.iter().take(65).enumerate() {
        match byte {
            b'0'..=b'9' | b';' | b':' => continue,
            b'm' => return Some(index + 3),
            _ => break,
        }
    }
    None
}

/// Leading control sequence, or one byte when `bytes` does not start one.
///
/// A CSI or OSC is one terminal action (one scroll, one clear, one mode change),
/// so it is parsed in one step. The scan stops at the final byte; it does not
/// interpret the sequence.
fn sequence_len(bytes: &[u8]) -> usize {
    if bytes.is_empty() {
        return 0;
    }
    if bytes[0] == 0x9b {
        return csi_len(&bytes[1..]).saturating_add(1).min(bytes.len());
    }
    if bytes[0] != 0x1b {
        return 1;
    }
    if bytes.len() == 1 {
        return 1;
    }
    match bytes[1] {
        b'[' => csi_len(&bytes[2..]).saturating_add(2).min(bytes.len()),
        b']' => osc_len(&bytes[2..]).saturating_add(2).min(bytes.len()),
        _ => 2,
    }
}

/// Bytes of a CSI body, including its final byte when one is present.
fn csi_len(bytes: &[u8]) -> usize {
    for (index, byte) in bytes.iter().copied().enumerate() {
        if (0x40..=0x7e).contains(&byte) {
            return index + 1;
        }
        // A new introducer or a C0 control aborts the sequence. Leave that byte
        // for the next step. Parameter bytes are bounded so a broken sequence
        // cannot swallow the rest of the chunk.
        if byte == 0x1b || byte < 0x20 || index >= 64 {
            return index;
        }
    }
    bytes.len()
}

/// Bytes of an OSC body, including its BEL or ST terminator when present.
fn osc_len(bytes: &[u8]) -> usize {
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == 0x07 {
            return index + 1;
        }
        if bytes[index] == 0x1b && index + 1 < bytes.len() && bytes[index + 1] == b'\\' {
            return index + 2;
        }
        if bytes[index] == 0x1b {
            return index;
        }
        index += 1;
    }
    bytes.len()
}

/// What the grid parser answers to primary device attributes.
const PARSER_DA1: &[u8] = b"\x1b[?6c";
/// What this terminal answers instead: VT220 class, with Sixel (4) and ANSI
/// colour (22) (R4).
pub const SIXEL_DA1: &[u8] = b"\x1b[?62;4;22c";

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn replace_all(bytes: &mut Vec<u8>, from: &[u8], to: &[u8]) {
    if !contains(bytes, from) {
        return;
    }
    let mut out = Vec::with_capacity(bytes.len() + to.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at..].starts_with(from) {
            out.extend_from_slice(to);
            at += from.len();
        } else {
            out.push(bytes[at]);
            at += 1;
        }
    }
    *bytes = out;
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
    into.notices.extend(next.notices);
    if next.finished_command.is_some() {
        into.finished_command = next.finished_command;
    }
}

/// Colours the highlighted spans of one drawn row, in the cells left in default ink.
fn highlight_row(row: &mut RenderedRow, plain: &[bool]) {
    if !plain.iter().any(|plain| *plain) {
        return;
    }
    let line: String = row.cells.iter().map(|cell| cell.text.as_str()).collect();
    let spans = crate::highlight::spans(&line);
    if spans.is_empty() {
        return;
    }
    let owner: Vec<_> = row
        .cells
        .iter()
        .enumerate()
        .flat_map(|(index, cell)| std::iter::repeat_n(index, cell.text.len()))
        .collect();
    for (range, kind) in spans {
        let colour = kind.colour();
        let first = owner.get(range.start).copied();
        let last = owner.get(range.end.saturating_sub(1)).copied();
        let (Some(first), Some(last)) = (first, last) else {
            continue;
        };
        // A span the program coloured even in part is the program's.
        if !(first..=last).all(|index| plain.get(index).copied().unwrap_or(false)) {
            continue;
        }
        for cell in &mut row.cells[first..=last] {
            cell.foreground = colour;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blocks::{BlockStatus, CommandBlockId};

    fn core() -> TerminalCore {
        TerminalCore::new(40, 10, 100)
    }

    /// The foreground of each drawn cell of row 0 whose text is in `word`.
    fn colours_of(core: &TerminalCore, word: &str) -> Vec<Rgb> {
        let snapshot = core.snapshot();
        let row = &snapshot.rows[0];
        let line: String = row.cells.iter().map(|cell| cell.text.as_str()).collect();
        let start = line.find(word).expect("the word is on the row");
        row.cells[start..start + word.len()]
            .iter()
            .map(|cell| cell.foreground)
            .collect()
    }

    #[test]
    fn an_address_in_plain_output_is_drawn_in_the_address_colour() {
        let mut core = core();
        core.feed(b"from 10.0.0.5 ok");
        assert!(
            colours_of(&core, "10.0.0.5")
                .iter()
                .all(|colour| *colour == crate::highlight::Kind::Address.colour())
        );
        assert!(
            colours_of(&core, "from")
                .iter()
                .all(|colour| *colour == palette::FOREGROUND),
            "the words around it keep the default ink"
        );
    }

    #[test]
    fn a_program_that_coloured_its_own_output_keeps_its_colours() {
        let mut core = core();
        core.feed(b"\x1b[32m10.0.0.5\x1b[0m");
        assert!(
            colours_of(&core, "10.0.0.5")
                .iter()
                .all(|colour| *colour == palette::ANSI[2])
        );
    }

    #[test]
    fn highlighting_can_be_turned_off_and_skips_the_alternate_screen() {
        let mut core = core();
        core.set_highlight(false);
        core.feed(b"10.0.0.5");
        assert!(
            colours_of(&core, "10.0.0.5")
                .iter()
                .all(|colour| *colour == palette::FOREGROUND)
        );
        let mut full_screen = self::core();
        full_screen.feed(b"\x1b[?1049h10.0.0.5");
        assert!(
            colours_of(&full_screen, "10.0.0.5")
                .iter()
                .all(|colour| *colour == palette::FOREGROUND),
            "an editor's or a pager's screen is the program's"
        );
    }

    #[test]
    fn a_shell_directory_report_never_reaches_the_grid_and_survives_a_split_chunk() {
        let mut terminal = core();
        let first = terminal.feed(b"\x1b]7;file://host/var");
        assert_eq!(
            first.directory, None,
            "an unfinished report states nothing yet"
        );
        assert_eq!(terminal.directory(), None);

        let second = terminal.feed(b"/log\x07$ ");
        assert_eq!(second.directory.as_deref(), Some("/var/log"));
        assert_eq!(terminal.directory(), Some("/var/log"));
        // Only the prompt the shell printed after the report is on the grid.
        let text = terminal.snapshot().text();
        assert_eq!(text[0].trim_end(), "$");
        assert!(
            !text
                .iter()
                .any(|row| row.contains("file://") || row.contains("7;")),
            "the report is not terminal text: {text:?}"
        );
    }

    #[test]
    fn the_last_directory_of_a_chunk_is_the_one_the_shell_is_in() {
        let mut terminal = core();
        let effects = terminal
            .feed(b"\x1b]7;file://host/home/dev\x07\x1b]7;file://host/home/dev/project\x1b\\");
        assert_eq!(effects.directory.as_deref(), Some("/home/dev/project"));
        assert_eq!(terminal.directory(), Some("/home/dev/project"));
    }

    #[test]
    fn a_rejected_report_leaves_the_previous_directory_and_the_grid_alone() {
        let mut terminal = core();
        terminal.feed(b"\x1b]7;file://host/var/log\x07");
        // A relative path says nothing about where the shell is.
        let effects = terminal.feed(b"\x1b]7;file://hostrelative\x07ok");
        assert_eq!(effects.directory, None);
        assert_eq!(terminal.directory(), Some("/var/log"));
        assert_eq!(terminal.snapshot().row_text(0).as_deref(), Some("ok"));
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
    fn a_tab_is_drawn_as_the_blanks_it_stands_for() {
        // "ls -C" pads its columns with tabs, so each row writes a different number
        // of them; the grid keeps the tab as a marker on the first cell of the stop
        // and leaves the rest blank. Drawn as a character, every column after it in
        // that row would drift by its own width, and the columns would not line up.
        let mut terminal = core();
        terminal.feed(b"ab\t--\t\t::\r\nabcde\t--\t\t::");
        let snapshot = terminal.snapshot();
        let short = snapshot.row_text(0).expect("row 0");
        let long = snapshot.row_text(1).expect("row 1");

        assert!(
            !short.contains('\t'),
            "a tab is drawn as a blank: {short:?}"
        );
        assert_eq!(
            short.find("--"),
            long.find("--"),
            "the column after the tabs lines up: {short:?} / {long:?}"
        );
        assert_eq!(
            short.find("::"),
            long.find("::"),
            "the second column lines up too: {short:?} / {long:?}"
        );
        assert_eq!(
            long.find("--"),
            Some(8),
            "a tab stops at the next multiple of 8"
        );
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
    fn a_chinese_character_split_across_reads_stays_one_character() {
        // A read can end anywhere, including in the middle of a three byte
        // character. The bytes that arrive later must complete it, and the half
        // character must not be shown in the meantime (R15).
        let bytes = "中文".as_bytes();
        for split in 1..bytes.len() {
            let mut terminal = core();
            terminal.feed(&bytes[..split]);
            assert!(
                !terminal
                    .snapshot()
                    .row_text(0)
                    .is_some_and(|text| text.contains('\u{fffd}')),
                "a half character was shown as a replacement before the rest arrived \
                 (split at {split})"
            );
            terminal.feed(&bytes[split..]);
            let snapshot = terminal.snapshot();
            assert_eq!(
                snapshot.row_text(0).as_deref(),
                Some("中文"),
                "the character did not survive a read boundary at {split}"
            );
            assert_eq!(
                snapshot.cursor.expect("cursor").column,
                4,
                "the cursor lost the two columns of a wide character (split at {split})"
            );
        }
    }

    #[test]
    fn a_chinese_character_arriving_one_byte_at_a_time_is_still_whole() {
        let mut terminal = core();
        for byte in "中文测试".as_bytes() {
            terminal.feed(&[*byte]);
        }
        let snapshot = terminal.snapshot();
        assert_eq!(snapshot.row_text(0).as_deref(), Some("中文测试"));
        assert_eq!(snapshot.cursor.expect("cursor").column, 8);
    }

    #[test]
    fn invalid_bytes_show_a_replacement_character_and_the_grid_keeps_working() {
        // A remote program can send bytes that are not UTF-8 at all. The terminal
        // shows a replacement and keeps parsing; nothing is dropped silently and
        // nothing panics (R15).
        let mut terminal = core();
        terminal.feed(b"before\xff\xfeafter");
        let text = terminal.snapshot().row_text(0).expect("the row holds text");
        assert!(
            text.starts_with("before") && text.ends_with("after"),
            "the readable text around an invalid byte was lost: {text:?}"
        );
        assert!(
            text.contains('\u{fffd}'),
            "an invalid byte must be visible as a replacement: {text:?}"
        );
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
    fn a_parked_viewport_and_a_match_keep_their_rows_when_output_arrives() {
        let mut terminal = TerminalCore::new(20, 5, 100);
        for line in 0..12 {
            terminal.feed(format!("line {line}\r\n").as_bytes());
        }
        assert!(terminal.parked_viewport().is_none());
        terminal.scroll(5);
        let shown = terminal.snapshot().text();
        let parked = terminal
            .parked_viewport()
            .expect("the viewport is in history");
        let hits = terminal
            .search_history("line 3", SearchOptions::literal())
            .expect("a literal query compiles");
        let mark = terminal.anchor_line(hits[0].line, hits[0].column);
        let saved_offset = terminal.display_offset();

        terminal.feed(b"extra\r\n");
        assert!(
            terminal.display_offset() > saved_offset,
            "a parked viewport's raw offset grows so the same rows stay put"
        );
        assert_eq!(terminal.offset_of(parked), terminal.display_offset());
        assert_eq!(
            terminal.snapshot().text(),
            shown,
            "the rows on screen are the ones that were parked"
        );
        let line = terminal
            .line_of(mark)
            .expect("the match is still in the grid");
        let again = terminal
            .search_history("line 3", SearchOptions::literal())
            .expect("a literal query compiles");
        assert_eq!(again[0].line, line);
        assert!(
            again[0].line < hits[0].line,
            "the raw line number moved, the anchor did not"
        );

        // Restoring the number captured before the new row lands lower.
        terminal.scroll_to_bottom();
        let current = i32::try_from(terminal.display_offset()).unwrap_or(0);
        terminal.scroll(i32::try_from(saved_offset).unwrap_or(0) - current);
        assert_ne!(terminal.snapshot().text(), shown);
        let restored = i32::try_from(terminal.offset_of(parked)).unwrap_or(0);
        let current = i32::try_from(terminal.display_offset()).unwrap_or(0);
        terminal.scroll(restored - current);
        assert_eq!(terminal.snapshot().text(), shown);
    }

    /// The review probe: scrollback 8, screen 5, viewport parked at offset 4.
    #[test]
    fn a_full_scrollback_keeps_the_parked_row_and_the_match_when_output_arrives() {
        let mut terminal = TerminalCore::new(40, 5, 8);
        for line in 0..24 {
            terminal.feed(format!("mark-{line:04}\r\n").as_bytes());
        }
        assert_eq!(terminal.history(), 8, "the scrollback is full");
        terminal.scroll(4);
        assert_eq!(terminal.display_offset(), 4);
        let shown = terminal.snapshot().text();
        let top = shown[0].clone();
        assert!(
            top.starts_with("mark-"),
            "the parked row is one of the retained marks, not a blank, got {top:?}"
        );
        let parked = terminal
            .parked_viewport()
            .expect("the viewport is in history");
        let hits = terminal
            .search_history(&top, SearchOptions::literal())
            .expect("a literal query compiles");
        let hit = hits
            .iter()
            .find(|hit| hit.text == top)
            .expect("the visible row is a match");
        let mark = terminal.anchor_line(hit.line, hit.column);
        let saved_line = hit.line;

        terminal.feed(b"extra\r\n");
        assert_eq!(
            terminal.display_offset(),
            5,
            "a parked viewport's raw offset grows so the same rows stay put"
        );
        assert_eq!(terminal.offset_of(parked), terminal.display_offset());
        assert_eq!(
            terminal.snapshot().text(),
            shown,
            "the rows on screen are the ones that were parked"
        );
        let line = terminal
            .line_of(mark)
            .expect("the match is still in the grid");
        assert!(
            line < saved_line,
            "the raw line number moved up, the anchor did not"
        );
        let again = terminal
            .search_history(&top, SearchOptions::literal())
            .expect("a literal query compiles");
        let again = again
            .iter()
            .find(|hit| hit.text == top)
            .expect("the same mark is still a match");
        assert_eq!(again.line, line);
        assert_eq!(again.column, mark.column());
        assert_eq!(again.text, top);

        // Closing uses the anchor, not the offset captured when search opened.
        terminal.scroll_to_bottom();
        let stale = 4i32;
        let current = i32::try_from(terminal.display_offset()).unwrap_or(0);
        terminal.scroll(stale - current);
        assert_ne!(
            terminal.snapshot().text(),
            shown,
            "the offset from before the new row now shows a later screen"
        );
        let restored = i32::try_from(terminal.offset_of(parked)).unwrap_or(0);
        let current = i32::try_from(terminal.display_offset()).unwrap_or(0);
        terminal.scroll(restored - current);
        assert_eq!(terminal.snapshot().text(), shown);
    }

    #[test]
    fn a_parked_row_that_scrolls_out_of_a_full_history_clamps_to_the_oldest_line() {
        let mut terminal = TerminalCore::new(40, 5, 8);
        for line in 0..24 {
            terminal.feed(format!("mark-{line:04}\r\n").as_bytes());
        }
        assert_eq!(terminal.history(), 8);
        terminal.scroll(4);
        let shown = terminal.snapshot().text();
        let parked_text = shown[0].clone();
        let parked = terminal.parked_viewport().expect("parked");

        // One chunk both keeps the row (the first lines) and then rotates it out.
        terminal.feed(b"one\r\ntwo\r\nthree\r\nfour\r\nfive\r\n");
        assert!(
            terminal.line_of(parked).is_none(),
            "the parked row is no longer in the grid"
        );
        assert_eq!(
            terminal.offset_of(parked),
            terminal.history(),
            "closing clamps to the oldest retained row, not the offset from open"
        );
        assert_ne!(terminal.offset_of(parked), 4);

        terminal.scroll_to_bottom();
        let stale = 4i32;
        let current = i32::try_from(terminal.display_offset()).unwrap_or(0);
        terminal.scroll(stale - current);
        assert_ne!(
            terminal.snapshot().text(),
            shown,
            "the offset saved at open now shows a later screen"
        );
        let target = i32::try_from(terminal.offset_of(parked)).unwrap_or(0);
        let current = i32::try_from(terminal.display_offset()).unwrap_or(0);
        terminal.scroll(target - current);
        assert_eq!(terminal.display_offset(), terminal.history());
        let oldest = terminal.snapshot().row_text(0).expect("oldest row");
        assert_ne!(oldest, parked_text);
        assert!(
            oldest.starts_with("mark-") || oldest == "one" || oldest == "two" || oldest == "three",
            "the oldest retained row is real output, got {oldest:?}"
        );
    }

    #[test]
    fn a_control_scroll_in_a_full_history_moves_the_anchor_with_the_rows() {
        let mut terminal = TerminalCore::new(40, 5, 8);
        for line in 0..24 {
            terminal.feed(format!("mark-{line:04}\r\n").as_bytes());
        }
        terminal.scroll(4);
        let shown = terminal.snapshot().text();
        let parked = terminal.parked_viewport().expect("parked");
        // SU scrolls the whole screen up by two rows and into the full history.
        terminal.feed(b"\x1b[2S");
        assert_eq!(terminal.offset_of(parked), terminal.display_offset());
        assert_eq!(terminal.snapshot().text(), shown);
    }

    #[test]
    fn identical_rows_in_a_full_scrollback_stay_parked_when_another_arrives() {
        let mut terminal = TerminalCore::new(20, 5, 8);
        for _ in 0..30 {
            terminal.feed(b"same\r\n");
        }
        assert_eq!(terminal.history(), 8);
        terminal.scroll(4);
        let shown = terminal.snapshot().text();
        let parked = terminal.parked_viewport().expect("parked");
        terminal.feed(b"same\r\n");
        assert_eq!(terminal.offset_of(parked), terminal.display_offset());
        assert_eq!(terminal.snapshot().text(), shown);
    }

    #[test]
    fn mixed_utf8_keeps_the_same_history_anchors_across_read_boundaries() {
        let mut data = "中한あ🙂e\u{301}\r\n".repeat(200).into_bytes();
        data.extend_from_slice(b"\x1b[32m\xe4\x9b\x1b[0m\r\n\x85\x9b");
        data.extend_from_slice("★終端🙂\r\n".repeat(30).as_bytes());
        let mut reference = TerminalCore::new(9, 4, 20);
        for byte in data.chunks(1) {
            reference.feed(byte);
        }
        for batch in [2, 7, 64, data.len()] {
            let mut terminal = TerminalCore::new(9, 4, 20);
            for chunk in data.chunks(batch) {
                terminal.feed(chunk);
            }
            assert_eq!(terminal.anchor_line(0, 0), reference.anchor_line(0, 0));
            terminal.scroll(20);
            reference.scroll(20);
            assert_eq!(terminal.snapshot().text(), reference.snapshot().text());
        }
    }

    #[test]
    fn large_ascii_reads_keep_history_anchors_across_wraps_and_line_controls() {
        let text = b"abcdefghijklmnopqrst\r\n123456\x0b789\x0cend\r\n".repeat(16);
        for history in [3, 128] {
            let mut reference = TerminalCore::new(9, 5, history);
            let mut batched = TerminalCore::new(9, 5, history);
            let preload = b"same\r\n".repeat(history + 5);
            for term in [&mut reference, &mut batched] {
                term.feed(&preload);
                term.scroll(2);
            }
            for byte in text.chunks(1) {
                reference.feed(byte);
            }
            batched.feed(&text);
            assert_eq!(batched.anchor_line(0, 0), reference.anchor_line(0, 0));
            assert_eq!(batched.snapshot(), reference.snapshot());
            assert_eq!(batched.visible_cursor(), reference.snapshot().cursor);
            batched.scroll_to_bottom();
            reference.scroll_to_bottom();
            assert_eq!(batched.snapshot(), reference.snapshot());
            assert_eq!(batched.visible_cursor(), reference.snapshot().cursor);
        }
    }

    #[test]
    fn batched_rendition_changes_preserve_scrollback_styles_and_control_replies() {
        let colored = b"\x1b[38;5;172mred\x1b[48;2;1;2;3m green\x1b[m!\r\n".repeat(30);
        for batch in [2, 7, 64, colored.len()] {
            let mut terminal = TerminalCore::new(8, 4, 9);
            let mut reference = TerminalCore::new(8, 4, 9);
            for phase in [
                colored.as_slice(),
                b"\x1b[32m\x1b[3S\x1b[1mB\x1b[6n",
                b"\x1b[?1049h\x1b[4:1malt\x1b[6n",
                b"\x1b[?1049l\x1b[0mhome\x1b[6n",
                b"\x1b[3J\x1b[38;5;11mend",
            ] {
                let expected: Vec<_> = phase
                    .chunks(1)
                    .flat_map(|b| reference.feed(b).writes)
                    .collect();
                let actual: Vec<_> = phase
                    .chunks(batch)
                    .flat_map(|b| terminal.feed(b).writes)
                    .collect();
                assert_eq!(actual, expected);
                assert_eq!(terminal.snapshot(), reference.snapshot());
                assert_eq!(terminal.anchor_line(0, 0), reference.anchor_line(0, 0));
            }
        }
    }

    #[test]
    fn the_last_free_history_slot_does_not_count_as_a_dropped_row() {
        let mut terminal = TerminalCore::new(40, 5, 8);
        let mut n = 0;
        while terminal.history() < 7 {
            terminal.feed(format!("row-{n:04}\r\n").as_bytes());
            n += 1;
            assert!(n < 100, "history never reached one below the limit");
        }
        terminal.scroll(3);
        let shown = terminal.snapshot().text();
        let parked = terminal.parked_viewport().expect("parked");
        let saved = terminal.display_offset();
        terminal.feed(b"last-free\r\n");
        assert!(terminal.history() <= 8);
        assert_eq!(terminal.offset_of(parked), terminal.display_offset());
        assert_eq!(terminal.snapshot().text(), shown);
        assert!(terminal.display_offset() > saved);
    }

    #[test]
    fn crossing_the_scrollback_limit_in_one_chunk_keeps_the_anchor() {
        let mut terminal = TerminalCore::new(40, 5, 8);
        let mut n = 0;
        while terminal.history() < 7 {
            terminal.feed(format!("row-{n:04}\r\n").as_bytes());
            n += 1;
            assert!(n < 100, "history never reached one below the limit");
        }
        terminal.scroll(3);
        let shown = terminal.snapshot().text();
        let parked = terminal.parked_viewport().expect("parked");
        // One newline fills the last free slot and the next rotates a row out.
        terminal.feed(b"cross-a\r\ncross-b\r\n");
        assert_eq!(terminal.history(), 8);
        assert_eq!(terminal.offset_of(parked), terminal.display_offset());
        assert_eq!(
            terminal.snapshot().text(),
            shown,
            "the parked rows stay put across the limit"
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
        terminal.begin_selection(0, 0, Side::Left, SelectionType::Simple);
        terminal.extend_selection(2, 2, Side::Right);
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
        terminal.begin_selection(0, 0, Side::Left, SelectionType::Simple);
        terminal.extend_selection(0, 2, Side::Right);
        let snapshot = terminal.snapshot();
        assert!(snapshot.rows[0].cells[0].selected);
        assert!(snapshot.rows[0].cells[2].selected);
        assert!(!snapshot.rows[0].cells[3].selected);
    }

    /// The edges of a selection are the halves of cells the pointer was in, in
    /// either direction (R4).
    #[test]
    fn a_selection_takes_the_cells_between_its_edges() {
        let mut terminal = core();
        terminal.feed(b"cd test-c9685ba2 now");

        // Leftwards from the left half of the space after `2` to the left half of `t`.
        terminal.begin_selection(0, 16, Side::Left, SelectionType::Simple);
        terminal.extend_selection(0, 3, Side::Left);
        assert_eq!(terminal.selection_text().as_deref(), Some("test-c9685ba2"));

        // From the right half of `t`: the `t` is left out.
        terminal.begin_selection(0, 3, Side::Right, SelectionType::Simple);
        terminal.extend_selection(0, 15, Side::Right);
        assert_eq!(terminal.selection_text().as_deref(), Some("est-c9685ba2"));

        // From the right half of one cell to the left half of the next covers nothing.
        terminal.begin_selection(0, 3, Side::Right, SelectionType::Simple);
        terminal.extend_selection(0, 4, Side::Left);
        assert!(!terminal.has_selection());
        assert_eq!(terminal.selection_kind(), Some(SelectionType::Simple));
    }

    /// A block selection takes the same columns from every row it spans.
    #[test]
    fn a_block_selection_takes_a_rectangle() {
        let mut terminal = core();
        terminal.feed(b"abcd\r\nefgh\r\nijkl");
        terminal.begin_selection(0, 1, Side::Left, SelectionType::Block);
        terminal.extend_selection(2, 2, Side::Right);
        assert_eq!(terminal.selection_text().as_deref(), Some("bc\nfg\njk"));
    }

    /// With nothing selected, a selection can start at the cursor (Shift+click).
    #[test]
    fn a_selection_can_start_at_the_cursor() {
        let mut terminal = core();
        terminal.feed(b"hello world");
        terminal.begin_selection_at_cursor();
        terminal.extend_selection(0, 6, Side::Left);
        assert_eq!(terminal.selection_text().as_deref(), Some("world"));
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
    fn a_background_query_is_answered_with_the_colour_on_screen() {
        let mut terminal = core();
        let effects = terminal.feed(b"\x1b]11;?\x1b\\");
        assert_eq!(
            String::from_utf8_lossy(&effects.writes),
            "\x1b]11;rgb:0e0e/1010/1414\x1b\\",
            "the dark default background is reported"
        );

        terminal.set_shown_colors(|_| Rgb {
            r: 0xfa,
            g: 0xfb,
            b: 0xfc,
        });
        let effects = terminal.feed(b"\x1b]11;?\x07");
        assert_eq!(
            String::from_utf8_lossy(&effects.writes),
            "\x1b]11;rgb:fafa/fbfb/fcfc\x07",
            "a light window reports its light background"
        );
    }

    #[test]
    fn colour_answers_keep_their_place_among_other_replies() {
        let mut terminal = core();
        let effects = terminal.feed(b"\x1b]10;?\x07\x1b[c\x1b]4;1;?\x07");
        let reply = String::from_utf8_lossy(&effects.writes).into_owned();
        let foreground = reply.find("\x1b]10;").expect("foreground answered");
        let attributes = reply.find("\x1b[?").expect("attributes answered");
        let slot = reply.find("\x1b]4;1;").expect("palette slot answered");
        assert!(
            foreground < attributes && attributes < slot,
            "answers follow the order of the questions: {reply:?}"
        );
    }

    #[test]
    fn a_colour_the_program_set_is_reported_as_set() {
        let mut terminal = core();
        terminal.set_shown_colors(|_| Rgb { r: 1, g: 1, b: 1 });
        let effects = terminal.feed(b"\x1b]11;rgb:12/34/56\x07\x1b]11;?\x07");
        assert_eq!(
            String::from_utf8_lossy(&effects.writes),
            "\x1b]11;rgb:1212/3434/5656\x07"
        );
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
    fn history_completion_needs_a_live_prompt_and_echoed_input() {
        let mut terminal = TerminalCore::new(80, 8, 100);
        terminal.feed(
            b"\x1b]133;A\x07$ \x1b]133;B\x07echo hello\x1b]133;C\x07\r\nhello\r\n\x1b]133;D;0\x07",
        );
        terminal.feed(b"$ ech");
        assert_eq!(terminal.history_completion(), None);
        terminal.feed(b"\r\n\x1b]133;A\x07$ \x1b]133;B\x07");
        terminal.note_command_input(b"e");
        terminal.note_command_input(b"c");
        terminal.feed(b"e");
        assert_eq!(terminal.history_completion(), None);
        terminal.feed(b"c");
        assert_eq!(terminal.history_completion(), Some("ho hello"));
        terminal.dismiss_history_completion();
        assert_eq!(terminal.history_completion(), None);
        terminal.note_command_input(b"h");
        terminal.feed(b"h");
        assert_eq!(terminal.history_completion(), Some("o hello"));
        terminal.feed(b"\x1b[D");
        assert_eq!(terminal.history_completion(), None);
        terminal.feed(b"\x1b[C");
        terminal.note_command_input(b"o hello");
        assert_eq!(terminal.history_completion(), None);
        terminal.feed(b"o hello");
        assert_eq!(terminal.history_completion(), None);
        terminal.feed(b"\x08 \x08");
        assert_eq!(
            terminal.history_completion(),
            None,
            "unexpected edits invalidate the expected echo"
        );
        terminal.note_command_input(b"\r");
        terminal.feed(b"\x1b]133;C\x07\r\nPassword: ");
        assert_eq!(terminal.history_completion(), None);
        terminal.feed(b"\x1b]133;A\x07\r\n\x1b]133;B\x07ech");
        assert_eq!(terminal.history_completion(), Some("o hello"));
        terminal.feed(b"\x1b[?1049h");
        assert_eq!(terminal.history_completion(), None);
        let mut other = TerminalCore::new(80, 8, 100);
        other.feed(b"\x1b]133;B\x07ech");
        assert_eq!(other.history_completion(), None);
    }

    #[test]
    fn history_completion_excludes_concealed_and_multiline_commands() {
        let mut terminal = TerminalCore::new(80, 8, 100);
        terminal.feed(b"\x1b]133;B\x07echo \x1b[8msecret\x1b[0m\x1b]133;C\x07\x1b]133;D;0\x07\r\n");
        terminal.feed(
            b"\x1b]133;B\x07\x1b]633;E;echo first\\x0asecond\x07\x1b]133;C\x07\x1b]133;D;0\x07\r\n",
        );
        terminal.feed(b"\x1b]133;B\x07echo");
        assert_eq!(terminal.history_completion(), None);
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

    #[test]
    fn select_all_covers_scrollback_and_visible_rows() {
        let mut terminal = TerminalCore::new(32, 4, 100);
        for index in 0..10 {
            terminal.feed(format!("row-{index}\r\n").as_bytes());
        }
        terminal.feed(b"last-row");
        assert!(terminal.history() > 0, "earlier rows entered scrollback");
        assert!(!terminal.has_selection());

        terminal.select_all();
        assert!(terminal.has_selection());
        let selected = terminal.selection_text().expect("selection text");
        assert!(
            selected.contains("row-0") && selected.contains("last-row"),
            "select_all spans from oldest scrollback row to live bottom: {selected:?}"
        );
    }

    #[test]
    fn anchored_selection_survives_streaming_scroll_and_history_clamp() {
        let mut terminal = TerminalCore::new(32, 4, 6);
        for index in 0..4 {
            terminal.feed(format!("row-{index:02}\r\n").as_bytes());
        }
        // Viewport row 1 currently holds "row-02".
        let anchor = terminal.anchor_viewport_row(1);
        let before = terminal.snapshot();
        assert!(before.text().iter().any(|line| line.contains("row-02")));

        // Stream 3 more lines so "row-02" scrolls up into scrollback.
        for index in 4..7 {
            terminal.feed(format!("row-{index:02}\r\n").as_bytes());
        }
        terminal.begin_selection_at_anchor(anchor, 0, Side::Left, SelectionType::Simple);
        terminal.extend_selection(2, 5, Side::Right);
        let selected = terminal.selection_text().expect("anchored selection text");
        assert!(
            selected.starts_with("row-02"),
            "selection must stay anchored to row-02 after streaming scroll, got: {selected:?}"
        );

        // Stream enough lines to evict the anchor past scrollback and ensure clamping works.
        for index in 7..30 {
            terminal.feed(format!("row-{index:02}\r\n").as_bytes());
        }
        assert!(terminal.line_of(anchor).is_none());
        terminal.begin_selection_at_anchor(anchor, 0, Side::Left, SelectionType::Simple);
        terminal.extend_selection(0, 5, Side::Right);
        assert!(terminal.has_selection());
    }

    #[test]
    fn http_link_hit_testing_aligns_with_wide_cells_and_skips_alternate_or_non_http() {
        let mut terminal = TerminalCore::new(60, 6, 50);
        terminal.feed("访问 https://example.com/docs, or ssh://git@host/repo.".as_bytes());
        // "访问 " is 2 + 2 + 1 = 5 columns; "https://example.com/docs" spans columns 5..29.
        assert_eq!(terminal.http_link_at(0, 4), None);
        let link = terminal.http_link_at(0, 5).expect("http link at start col");
        assert_eq!(link.url, "https://example.com/docs");
        assert_eq!(link.columns, 5..29);
        assert_eq!(terminal.http_link_at(0, 28), Some(link));
        assert_eq!(
            terminal.http_link_at(0, 29),
            None,
            "trailing comma excluded"
        );
        // ssh:// is highlighted as text but never openable via external URL launch.
        for col in 30..59 {
            assert_eq!(terminal.http_link_at(0, col), None);
        }

        // Alternate screen never exposes clickable links.
        terminal.feed(b"\x1b[?1049h\x1b[Hhttps://example.com/alt");
        assert_eq!(terminal.http_link_at(0, 2), None);
    }

    #[test]
    fn clear_screen_and_scrollback_wipes_history_and_screen_only_on_primary_grid() {
        let mut terminal = TerminalCore::new(40, 4, 50);
        for index in 0..12 {
            terminal.feed(format!("line-{index}\r\n").as_bytes());
        }
        terminal.scroll(4);
        assert!(terminal.history() > 0);
        assert!(terminal.display_offset() > 0);

        terminal.clear_screen_and_scrollback();
        assert_eq!(terminal.history(), 0);
        assert_eq!(terminal.display_offset(), 0);
        let snap = terminal.snapshot();
        assert!(snap.text().iter().all(String::is_empty));
        assert_eq!(
            snap.cursor.map(|c| (c.row, c.column)),
            Some((0, 0)),
            "cursor returns to origin"
        );
    }

    fn osc8(id: Option<&str>, uri: &str, text: &str) -> Vec<u8> {
        let params = id.map_or(String::new(), |id| format!("id={id}"));
        format!("\x1b]8;{params};{uri}\x1b\\{text}\x1b]8;;\x1b\\").into_bytes()
    }

    #[test]
    fn an_osc8_link_covers_exactly_the_cells_it_was_attached_to() {
        let mut terminal = core();
        terminal.feed(b"see ");
        terminal.feed(&osc8(None, "https://example.com", "text"));
        terminal.feed(b" after");

        let snapshot = terminal.snapshot();
        assert_eq!(snapshot.row_text(0).as_deref(), Some("see text after"));
        let links = &snapshot.rows[0].links;
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].columns, 4..8);
        assert_eq!(&*links[0].uri, "https://example.com");

        let link = terminal.link_at(0, 5).expect("the linked text has a link");
        assert_eq!(link.kind, LinkKind::Osc8);
        assert_eq!(link.url, "https://example.com");
        assert_eq!(link.columns, 4..8);
        assert_eq!(terminal.link_at(0, 3), None);
        assert_eq!(terminal.link_at(0, 8), None);
    }

    #[test]
    fn an_osc8_link_wins_over_text_that_looks_like_another_address() {
        let mut terminal = core();
        terminal.feed(&osc8(None, "https://real.example", "https://shown.example"));
        let link = terminal.link_at(0, 2).expect("a link");
        assert_eq!(link.kind, LinkKind::Osc8);
        assert_eq!(link.url, "https://real.example");
    }

    #[test]
    fn a_wrapped_osc8_link_is_one_link_on_every_row_it_covers() {
        let mut terminal = TerminalCore::new(10, 4, 100);
        terminal.feed(&osc8(
            Some("w"),
            "https://example.com/long",
            "abcdefghijklmn",
        ));

        let link = terminal.link_at(1, 1).expect("the second row is linked");
        assert_eq!(link.spans, vec![(0, 0..10), (1, 0..4)]);
        assert_eq!(link.row, 1);
        assert_eq!(link.columns, 0..4);
    }

    #[test]
    fn two_osc8_links_side_by_side_stay_two_links() {
        let mut terminal = core();
        terminal.feed(&osc8(None, "https://a.example", "aa"));
        terminal.feed(&osc8(None, "https://b.example", "bb"));
        let links = terminal.snapshot().rows[0].links.clone();
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].columns, 0..2);
        assert_eq!(links[1].columns, 2..4);
        assert_eq!(terminal.link_at(0, 3).expect("b").url, "https://b.example");
    }

    #[test]
    fn a_wide_character_inside_an_osc8_link_counts_two_columns() {
        let mut terminal = core();
        terminal.feed(&osc8(None, "https://example.com", "中a"));
        let links = terminal.snapshot().rows[0].links.clone();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].columns, 0..3);
    }

    #[test]
    fn an_osc8_link_follows_its_text_into_history_and_back() {
        let mut terminal = TerminalCore::new(20, 3, 100);
        terminal.feed(&osc8(None, "https://example.com", "link"));
        terminal.feed(b"\r\n1\r\n2\r\n3\r\n4");
        // The linked row has scrolled into history: nothing on screen is linked.
        assert!(
            terminal
                .snapshot()
                .rows
                .iter()
                .all(|row| row.links.is_empty())
        );

        terminal.scroll(10);
        let snapshot = terminal.snapshot();
        assert_eq!(snapshot.row_text(0).as_deref(), Some("link"));
        assert_eq!(snapshot.rows[0].links[0].columns, 0..4);
        assert_eq!(
            terminal.link_at(0, 1).map(|link| link.url),
            Some("https://example.com".into())
        );
    }

    #[test]
    fn clearing_or_resizing_leaves_no_link_on_cells_without_one() {
        let mut terminal = core();
        terminal.feed(&osc8(None, "https://example.com", "link"));
        terminal.clear_screen_and_scrollback();
        terminal.feed(b"plain");
        assert!(
            terminal
                .snapshot()
                .rows
                .iter()
                .all(|row| row.links.is_empty())
        );

        let mut terminal = core();
        terminal.feed(b"\x1b[2;1H");
        terminal.feed(&osc8(None, "https://example.com", "link"));
        terminal.resize(GridSize { cols: 20, rows: 5 });
        let snapshot = terminal.snapshot();
        let linked: Vec<_> = snapshot
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| !row.links.is_empty())
            .map(|(index, row)| (index, row.links[0].columns.clone()))
            .collect();
        assert_eq!(linked.len(), 1);
        let (row, columns) = &linked[0];
        assert_eq!(
            &snapshot.row_text(*row).expect("row")[columns.clone()],
            "link"
        );
    }

    #[test]
    fn an_osc8_link_is_offered_on_the_alternate_screen_but_text_is_not() {
        let mut terminal = core();
        terminal.feed(b"\x1b[?1049h");
        terminal.feed(&osc8(None, "https://example.com", "link"));
        terminal.feed(b" https://text.example");
        assert_eq!(
            terminal.link_at(0, 1).map(|link| link.kind),
            Some(LinkKind::Osc8)
        );
        assert_eq!(terminal.link_at(0, 8), None);
    }

    /// Runs one integrated command that takes about 20 ms and returns the effects of
    /// the chunk that ended it.
    fn run_command(terminal: &mut TerminalCore, command: &str, end: &str) -> TerminalEffects {
        terminal.feed(&osc("133;A"));
        terminal.feed(b"$ ");
        terminal.feed(&osc("133;B"));
        terminal.feed(command.as_bytes());
        terminal.feed(&osc("133;C"));
        terminal.feed(b"\r\nout\r\n");
        std::thread::sleep(std::time::Duration::from_millis(20));
        terminal.feed(&osc(end))
    }

    #[test]
    fn a_command_over_the_threshold_reports_its_text_status_and_duration() {
        let mut terminal = core();
        terminal.set_long_command_threshold(Some(std::time::Duration::from_millis(10)));
        let effects = run_command(&mut terminal, "make build", "133;D;2");
        assert_eq!(
            effects.commands_finished, 1,
            "the count means the same as before"
        );
        let finished = effects
            .finished_command
            .clone()
            .expect("a command over the threshold is reported");
        assert_eq!(finished.command, "make build");
        assert_eq!(finished.status, BlockStatus::Exited(2));
        assert!(finished.duration >= std::time::Duration::from_millis(10));
        assert!(effects.has_background_notice());
    }

    #[test]
    fn a_short_command_or_no_threshold_reports_only_the_count() {
        let mut terminal = core();
        terminal.set_long_command_threshold(Some(std::time::Duration::from_secs(10)));
        let effects = run_command(&mut terminal, "ls", "133;D;0");
        assert_eq!(effects.commands_finished, 1);
        assert_eq!(effects.finished_command, None);

        let mut terminal = core();
        let effects = run_command(&mut terminal, "sleep 1", "133;D;0");
        assert_eq!(effects.commands_finished, 1);
        assert_eq!(effects.finished_command, None, "no threshold, no text");
    }

    #[test]
    fn a_reported_command_line_is_cut_to_its_limit() {
        let mut terminal = core();
        terminal.set_long_command_threshold(Some(std::time::Duration::from_millis(1)));
        let long = "e".repeat(MAX_FINISHED_COMMAND + 40);
        terminal.feed(&osc("133;A"));
        terminal.feed(&osc("133;B"));
        terminal.feed(&osc(&format!("633;E;{long}")));
        terminal.feed(&osc("133;C"));
        std::thread::sleep(std::time::Duration::from_millis(5));
        let effects = terminal.feed(&osc("133;D;0"));
        let finished = effects.finished_command.expect("reported");
        assert_eq!(finished.command.chars().count(), MAX_FINISHED_COMMAND);
    }

    #[test]
    fn a_program_notice_is_an_effect_and_leaves_the_grid_untouched() {
        let mut terminal = core();
        let effects = terminal.feed(b"before\x1b]9;done\x07after");
        assert_eq!(
            effects.notices,
            vec![crate::blocks::ProgramNotice {
                title: None,
                body: "done".into()
            }]
        );
        assert!(!effects.is_empty());
        assert!(
            !effects.has_background_notice(),
            "a notice is not a tab mark; the window decides about it separately"
        );
        assert_eq!(
            terminal.snapshot().row_text(0).as_deref(),
            Some("beforeafter")
        );
    }

    /// A Sixel image `width` x `height` pixels of one colour, as an encoder writes it.
    fn sixel(width: usize, height: usize) -> Vec<u8> {
        let mut out = format!("\x1bP0;1q\"1;1;{width};{height}#1;2;100;0;0#1").into_bytes();
        let bands = height.div_ceil(6);
        for band in 0..bands {
            out.extend_from_slice(format!("!{width}~").as_bytes());
            if band + 1 < bands {
                out.push(b'-');
            }
        }
        out.extend_from_slice(b"\x1b\\");
        out
    }

    fn core_with_cells() -> TerminalCore {
        let mut terminal = TerminalCore::new(40, 10, 100);
        terminal.set_cell_pixels(8.0, 16.0);
        terminal
    }

    #[test]
    fn an_image_sits_at_the_cursor_and_text_continues_below_it() {
        let mut terminal = core_with_cells();
        terminal.feed(b"top\r\n  ");
        // 32 px tall at 16 px a row: two rows.
        terminal.feed(&sixel(24, 32));
        terminal.feed(b"after");
        let snapshot = terminal.snapshot();
        assert_eq!(snapshot.images.len(), 1);
        let image = &snapshot.images[0];
        assert_eq!((image.row, image.column), (1, 2));
        assert_eq!((image.image.width, image.image.height), (24, 32));
        assert_eq!(snapshot.row_text(3).as_deref(), Some("  after"));
        assert!(
            snapshot.text().iter().all(|row| !row.contains('~')),
            "the image data never reaches the grid"
        );
    }

    #[test]
    fn an_image_cut_between_reads_is_one_image() {
        let mut terminal = core_with_cells();
        let bytes = sixel(16, 12);
        for chunk in bytes.chunks(3) {
            terminal.feed(chunk);
        }
        assert_eq!(terminal.snapshot().images.len(), 1);
        assert_eq!(terminal.snapshot().row_text(0).as_deref(), Some(""));
    }

    #[test]
    fn an_image_scrolls_with_its_rows_and_goes_when_they_leave_the_history() {
        let mut terminal = TerminalCore::new(20, 4, 5);
        terminal.set_cell_pixels(8.0, 16.0);
        terminal.feed(&sixel(8, 16));
        assert_eq!(terminal.snapshot().images[0].row, 0);
        terminal.feed(b"a\r\nb\r\nc\r\n");
        let images = terminal.snapshot().images;
        assert!(
            images.is_empty() || images[0].row < 0,
            "it moved up: {images:?}"
        );
        terminal.scroll(10);
        let images = terminal.snapshot().images;
        assert_eq!(images.len(), 1, "scrolled back, it is on screen again");
        assert!(images[0].row >= 0);
        terminal.scroll_to_bottom();
        for _ in 0..20 {
            terminal.feed(b"more\r\n");
        }
        assert_eq!(
            terminal.image_count(),
            0,
            "out of the history, it is released"
        );
    }

    #[test]
    fn clearing_resetting_or_switching_screens_takes_images_away() {
        let mut terminal = core_with_cells();
        terminal.feed(&sixel(8, 8));
        terminal.feed(b"\x1b[2J");
        assert_eq!(terminal.image_count(), 0, "ED 2");

        terminal.feed(&sixel(8, 8));
        terminal.feed(b"\x1bc");
        assert_eq!(terminal.image_count(), 0, "RIS");

        terminal.feed(&sixel(8, 8));
        terminal.clear_screen_and_scrollback();
        assert_eq!(terminal.image_count(), 0, "the clear command");

        // An image on the alternate screen is shown there and goes when it is left.
        terminal.feed(&sixel(8, 8));
        terminal.feed(b"\x1b[?1049h");
        assert!(
            terminal.snapshot().images.is_empty(),
            "the primary image is not on the alternate screen"
        );
        terminal.feed(&sixel(8, 8));
        assert_eq!(terminal.snapshot().images.len(), 1);
        terminal.feed(b"\x1b[?1049l");
        assert_eq!(
            terminal.snapshot().images.len(),
            1,
            "the primary image is back"
        );
        assert_eq!(terminal.image_count(), 1, "the alternate one is gone");
    }

    #[test]
    fn text_written_over_an_image_removes_it() {
        let mut terminal = core_with_cells();
        terminal.feed(&sixel(16, 32));
        assert_eq!(terminal.image_count(), 1);
        terminal.feed(b"\x1b[1;1Hover");
        assert_eq!(terminal.image_count(), 0);
    }

    #[test]
    fn a_new_width_removes_images_and_a_new_height_keeps_them() {
        let mut terminal = core_with_cells();
        terminal.feed(&sixel(8, 8));
        terminal.resize(GridSize::new(40, 12));
        assert_eq!(terminal.image_count(), 1);
        terminal.resize(GridSize::new(30, 12));
        assert_eq!(terminal.image_count(), 0);
    }

    #[test]
    fn broken_or_oversized_images_are_dropped_and_the_screen_stays_right() {
        let mut terminal = core_with_cells();
        terminal.feed(b"before ");
        terminal.feed(b"\x1bP0;1q\x01\x02garbage\x1b\\");
        terminal.feed(b"\x1bP0;1q\"1;1;9000;9000#1~\x1b\\");
        terminal.feed(b"after");
        assert_eq!(terminal.image_count(), 0);
        assert_eq!(
            terminal.snapshot().row_text(0).as_deref(),
            Some("before after")
        );
    }

    #[test]
    fn images_past_the_memory_limit_push_out_the_oldest() {
        let mut terminal = TerminalCore::new(40, 10, 10_000);
        terminal.set_cell_pixels(8.0, 16.0);
        // 2048 x 1024 RGBA is 8 MiB: nine of them exceed 64 MiB.
        for _ in 0..9 {
            terminal.feed(&sixel(2048, 1024));
        }
        let kept: usize = terminal.images.iter().map(PlacedImage::bytes).sum();
        assert!(kept <= MAX_IMAGE_BYTES, "{kept}");
        assert_eq!(terminal.image_count(), 8);
        assert_eq!(terminal.images[0].id, 2, "the first image went first");
    }

    #[test]
    fn device_attributes_say_this_terminal_draws_sixel() {
        let mut terminal = core();
        let effects = terminal.feed(b"\x1b[c");
        assert_eq!(effects.writes, SIXEL_DA1);
        let effects = terminal.feed(b"\x1b[0c");
        assert_eq!(effects.writes, SIXEL_DA1);
    }

    #[test]
    fn other_device_control_strings_still_reach_the_grid_parser() {
        let mut terminal = core();
        // DECRQSS for the cursor style: answered by the parser as before.
        let effects = terminal.feed(b"\x1bP$q q\x1b\\");
        assert!(terminal.snapshot().text().iter().all(String::is_empty));
        let _ = effects;
        terminal.feed(b"x");
        assert_eq!(terminal.snapshot().row_text(0).as_deref(), Some("x"));
    }
}
