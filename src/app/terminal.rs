//! The local terminal pane: the grid a shell writes into and everything the user
//! does to it (R2, R8).
//!
//! The pane is a thin GPUI layer over two render-independent pieces: a
//! [`TerminalCore`] that owns the parsed grid and a [`LocalSession`] that owns the
//! real shell process. What lives here is the translation between those and the
//! window — layout, colours, the caret, mouse mapping, clipboard shortcuts and
//! input-method composition.
//!
//! Three behaviours are deliberate:
//!
//! * text arrives through the input-method path, never from key presses, so a
//!   Chinese input method composes before anything is sent and no letter is typed
//!   twice;
//! * a failed write, a closed PTY or an abnormal exit becomes a state the pane
//!   reports, so nothing keeps looking like a live shell after it stopped;
//! * resizing measures the real element bounds and then tells the grid and the PTY
//!   the same character size.

use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    App, Bounds, BoxShadow, ClipboardItem, Context, ElementInputHandler, EntityInputHandler,
    FocusHandle, FontWeight, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Render, ScrollDelta, ScrollWheelEvent, SharedString, Size,
    UTF16Selection, Window, canvas, div, point, px,
};
use std::ops::Range;

use shellune_core::{EventTag, LocalShell, SessionFailure, TerminalMode};
use shellune_ssh::{ChainEvent, ChainSession, RemoteEvent, RemoteSession, X11Status};
use shellune_terminal::core::{LineAnchor, SearchOptions};
use shellune_terminal::shell::ShellEnvironmentNote;
use shellune_terminal::{
    BlockFilter, BlockStatus, CellSize, CommandBlock, CommandBlockId, CursorShape, ExitReport,
    GridSize, HistoryMatch, Key, KeyModifiers, LocalSession, MouseAction,
    MouseButton as TerminalMouseButton, MouseModifiers, PtyEvent, RenderedCell, Rgb, SelectionType,
    Snapshot, TermMode, TerminalCore, TerminalEffects, encode_alternate_scroll, encode_key,
    encode_mouse, encode_paste, encode_text, grid_for, mouse_reporting, palette,
    paste_needs_confirmation, paste_preview, sanitize_font_size, step_font_size,
};

use crate::app::actions::{TerminalCopy, TerminalPaste, TerminalSearch};
use crate::app::components;
use crate::app::copy::Locale;
use crate::app::glyphs;
use crate::app::icons::Icon;
use crate::app::theme::{self, Metrics, Tone, palette as shell_palette, radius, spacing};

/// How often the pane looks for output from the shell.
///
/// Output is coalesced rather than drawn byte by byte: a program that writes
/// thousands of small chunks must not force thousands of frames, and 12ms is below
/// the point where typing feels delayed.
pub const OUTPUT_POLL_INTERVAL: Duration = Duration::from_millis(12);

/// Height of the status line under the grid.
pub const SHELL_BAR_HEIGHT: f32 = 28.0;

/// Width of the caret in logical pixels.
const CARET_WIDTH: f32 = 2.0;

/// Background of a selected cell.
const SELECTION_BACKGROUND: u32 = 0x2c33_5cff;

/// Selection behind the text in the light palette.
const SELECTION_BACKGROUND_LIGHT: u32 = 0xc9d1_ffff;

/// Font family the grid is laid out in.
///
/// A terminal needs one advance width for every cell; each platform ships its own
/// monospace face, and naming the platform's own avoids a fallback that would break
/// the column alignment.
pub fn monospace_family() -> &'static str {
    if cfg!(target_os = "windows") {
        "Cascadia Mono"
    } else if cfg!(target_os = "macos") {
        "Menlo"
    } else {
        "DejaVu Sans Mono"
    }
}

/// What happened to the shell behind a pane.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaneState {
    /// The shell is running.
    Live,
    /// The shell could not be started, or stopped accepting input or output.
    Failed(SessionFailure),
    /// The shell exited.
    Exited {
        /// How it ended.
        report: ExitReport,
    },
}

impl PaneState {
    /// Whether the pane still sends input to a process.
    #[must_use]
    pub fn accepts_input(&self) -> bool {
        matches!(self, Self::Live)
    }

    /// The failure to show, when the session ended badly.
    #[must_use]
    pub fn failure(&self) -> Option<SessionFailure> {
        match self {
            Self::Failed(failure) => Some(failure.clone()),
            Self::Exited { report } => report.session_failure(),
            Self::Live => None,
        }
    }

    /// Short label for the pane bar, in the language the shell is using.
    #[must_use]
    pub fn label(&self, locale: Locale) -> String {
        let strings = locale.strings();
        match self {
            Self::Live => strings.terminal_live.to_owned(),
            Self::Failed(_) => strings.terminal_failed.to_owned(),
            Self::Exited { report } => match report {
                ExitReport::Clean => strings.terminal_exited.to_owned(),
                ExitReport::Code { code } => {
                    format!("{} ({code})", strings.terminal_exited)
                }
                ExitReport::Signal { signal } => {
                    format!("{} ({signal})", strings.terminal_terminated)
                }
            },
        }
    }
}

/// What feeds a pane.
///
/// The grid, the selection, the scrollback and the input encoding are the same for
/// both, which is the point of the shared terminal core: only the byte source
/// differs. A remote session is not a second terminal implementation.
pub enum PaneSource {
    /// A shell process on this machine.
    Local(LocalSession),
    /// A shell on a server, reached over SSH.
    Remote(RemoteSession),
    /// A shell on the target of a jump chain.
    Chain(ChainSession),
}

/// Something a source produced, in the terms the pane thinks in.
pub enum SourceEvent {
    /// Bytes a program wrote.
    Data(Vec<u8>),
    /// The program ended.
    Exited(ExitReport),
    /// The source stopped for a reason worth stating.
    Lost(String),
    /// What X11 forwarding of this session is doing (R11).
    ///
    /// Not output: it says whether a remote program can reach this machine's
    /// display, and is reported to the window the pane belongs to.
    X11(X11Status),
}

impl SourceEvent {
    /// The pane's view of an event a remote session reported.
    ///
    /// The window also uses this for a batch it drained before the pane existed —
    /// a connection notice and the first output often arrive together — so both
    /// paths agree on what output and an exit mean.
    pub(crate) fn from_remote(event: RemoteEvent) -> Option<Self> {
        match event {
            RemoteEvent::Data(bytes) => Some(Self::Data(bytes)),
            RemoteEvent::Exited { code } => Some(Self::Exited(match code {
                None | Some(0) => ExitReport::Clean,
                Some(code) => ExitReport::Code {
                    code: i32::try_from(code).unwrap_or(i32::MAX),
                },
            })),
            // A prompt or a connection notice belongs to the window, which owns the
            // attempt until the terminal exists; reaching a pane with one would mean
            // two owners, so it is ignored rather than drawn as output.
            RemoteEvent::HostKeyPrompt(_) | RemoteEvent::Connected => None,
            RemoteEvent::X11(status) => Some(Self::X11(status)),
            RemoteEvent::Failed(error) => Some(Self::Lost(error.to_string())),
        }
    }
}

impl PaneSource {
    fn write(&mut self, bytes: &[u8]) -> Result<(), String> {
        match self {
            Self::Local(session) => session.write(bytes).map_err(|error| error.to_string()),
            Self::Remote(session) => session.write(bytes),
            Self::Chain(session) => session.write(bytes),
        }
    }

    fn resize(&mut self, grid: GridSize) -> Result<(), String> {
        match self {
            Self::Local(session) => session.resize(grid),
            Self::Remote(session) => session.resize(pty_size(grid)),
            Self::Chain(session) => session.resize(pty_size(grid)),
        }
    }

    fn drain(&mut self) -> Vec<SourceEvent> {
        match self {
            Self::Local(session) => session
                .drain()
                .into_iter()
                .map(|event| match event {
                    PtyEvent::Data(bytes) => SourceEvent::Data(bytes),
                    PtyEvent::Exited(report) => SourceEvent::Exited(report),
                    PtyEvent::ReadFailed(reason) => SourceEvent::Lost(reason),
                })
                .collect(),
            Self::Remote(session) => session
                .drain()
                .into_iter()
                .filter_map(SourceEvent::from_remote)
                .collect(),
            Self::Chain(session) => session
                .drain()
                .into_iter()
                .filter_map(|event| match event {
                    ChainEvent::Data(bytes) => Some(SourceEvent::Data(bytes)),
                    ChainEvent::Exited { code } => Some(SourceEvent::Exited(match code {
                        None | Some(0) => ExitReport::Clean,
                        Some(code) => ExitReport::Code {
                            code: i32::try_from(code).unwrap_or(i32::MAX),
                        },
                    })),
                    // The chain's own text names the hop. Mapping through
                    // session_failure would turn a disconnect into "the server".
                    ChainEvent::Failed(failure) => Some(SourceEvent::Lost(failure.to_string())),
                    ChainEvent::X11 { status, .. } => Some(SourceEvent::X11(status)),
                    ChainEvent::HopStarted(_)
                    | ChainEvent::HostKeyPrompt { .. }
                    | ChainEvent::ProxyConfirmation { .. }
                    | ChainEvent::HopReady(_)
                    | ChainEvent::Connected { .. } => None,
                })
                .collect(),
        }
    }

    fn close(&mut self) {
        match self {
            Self::Local(session) => session.close(),
            Self::Remote(session) => session.close(),
            Self::Chain(session) => session.close(),
        }
    }
}

fn pty_size(grid: GridSize) -> shellune_ssh::PtySize {
    shellune_ssh::PtySize::new(
        u32::try_from(grid.cols).unwrap_or(u32::MAX),
        u32::try_from(grid.rows).unwrap_or(u32::MAX),
    )
}

/// What a pane is running, for labels and for the reason a failure names.
#[derive(Clone, Debug)]
pub enum PaneOrigin {
    /// A program on this machine.
    Local(LocalShell),
    /// A shell on a server.
    Remote {
        /// The address the session talks to, as the user sees it.
        target: String,
    },
}

impl PaneOrigin {
    /// What the pane calls the thing it runs.
    #[must_use]
    pub fn label(&self) -> &str {
        match self {
            Self::Local(shell) => shell.name(),
            Self::Remote { target } => target,
        }
    }

    /// The failure a stopped source implies.
    ///
    /// A local program that started and then stopped needs different advice from an
    /// SSH session that lost its channel, which is why the origin decides.
    #[must_use]
    pub fn lost_failure(&self, reason: String) -> SessionFailure {
        match self {
            Self::Local(shell) => SessionFailure::LocalShellIoFailed {
                shell: shell.program().to_owned(),
                reason,
            },
            Self::Remote { target } => SessionFailure::RemoteIoFailed {
                host: target.clone(),
                reason,
            },
        }
    }
}

/// One terminal view backed by a local shell or a remote session.
/// What a pane asks of the window, for what it cannot do alone.
#[derive(Clone, Debug)]
pub enum PaneRequest {
    /// Files were dropped on a remote terminal: upload them, then type where
    /// they went.
    UploadFiles(Vec<std::path::PathBuf>),
    /// An image was pasted into a remote terminal: the server cannot read this
    /// computer's clipboard, so save it, upload it, then type where it went.
    PasteImage(gpui::Image),
}

impl gpui::EventEmitter<PaneRequest> for TerminalPane {}

/// The background notices one terminal produced, without a trace of its text (R7).
///
/// Exactly two things are noticed: the program rang the bell, and a command block the
/// shell reported moved from running to ended. Neither carries a command line, output
/// or anything else the session printed, so a mark on a tab can never repeat what ran,
/// and a reminder that is switched off changes nothing about the bytes or the blocks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TerminalAttention {
    /// The program rang the bell.
    pub bell: bool,
    /// A command ended, because the shell said so with an end marker.
    pub finished: bool,
}

impl TerminalAttention {
    /// Whether nothing is waiting to be collected.
    #[must_use]
    pub fn is_empty(self) -> bool {
        !self.bell && !self.finished
    }

    /// Adds another set of notices. Nothing is ever cleared by a merge.
    pub fn absorb(&mut self, other: Self) {
        self.bell |= other.bell;
        self.finished |= other.finished;
    }
}

/// What offering a saved command to a pane did (R6).
///
/// The window may only report what really happened: a body held behind the paste prompt
/// has not reached the shell, so it is not "inserted" or "sent" until the prompt is
/// answered, and cancelling it leaves nothing behind but the fact that nothing happened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnippetOffer {
    /// The body is in this pane now, with or without its one line break; nothing waits.
    Written,
    /// The body waits behind the paste prompt. Nothing is written until it is confirmed,
    /// so this is not a completion.
    Waiting,
    /// The body held no command, so nothing was offered and nothing waits.
    Empty,
}

pub struct TerminalPane {
    /// Which session process this pane shows.
    ///
    /// Every event a source produces is checked against it before it is applied, and
    /// the window passes events with the tag it drained them under, so a batch that
    /// belongs to a process this pane does not show is dropped instead of being drawn
    /// into the wrong terminal.
    tag: EventTag,
    core: TerminalCore,
    source: Option<PaneSource>,
    /// What the pane is running: a program here, or a host over SSH.
    origin: PaneOrigin,
    state: PaneState,
    /// Bytes written to the shell, kept so what the user typed can be inspected.
    ///
    /// Only a test reads this back: keeping every byte the user ever typed would
    /// grow for the life of the session, so a release build does not.
    #[cfg(test)]
    sent: Vec<u8>,
    font_size: f32,
    cell: CellSize,
    grid: GridSize,
    viewport: Option<Bounds<Pixels>>,
    focus: FocusHandle,
    dragging: bool,
    /// Cell the left button went down on, while a drag is in progress.
    ///
    /// It is what separates a click from a selection: a release on the cell the
    /// button went down on selected nothing, and copying there would replace the
    /// user's clipboard with a single character they never chose.
    press: Option<(usize, usize)>,
    /// The selection came from a double or triple click (a word or a line), so
    /// releasing on the same cell keeps and copies it.
    press_selects: bool,
    /// The button a program is being told is held, while it asked for the mouse.
    mouse_held: Option<TerminalMouseButton>,
    /// The cell the last motion report named, so moving inside one cell does not
    /// report it again.
    last_report_cell: Option<(usize, usize)>,
    /// Where the right-click menu is open.
    menu: Option<gpui::Point<Pixels>>,
    marked: Option<String>,
    last_copy: Option<String>,
    locale: Locale,
    polling: bool,
    /// A multi-line or high-risk paste that has not been confirmed.
    ///
    /// It is not encoded and not written until [`TerminalPane::confirm_paste`].
    /// Cancel drops it. Program keys are not answers to this prompt.
    pending_paste: Option<PendingPaste>,
    search_open: bool,
    /// When set, typed text edits the search query instead of the shell.
    ///
    /// Ctrl+C, Ctrl+L and the program's own arrow keys were never taken by the
    /// search; Escape belongs to the find bar only while it holds the keyboard, and
    /// then it closes the bar and gives the terminal back (R1, R4).
    search_focused: bool,
    search_query: String,
    /// Why the query could not be compiled, in the expression engine's words.
    ///
    /// An unusable expression is its own answer: it is never drawn as "no matches".
    search_error: Option<String>,
    search_hits: Vec<HistoryMatch>,
    /// Index of the match the viewport was last put on, counted from zero.
    search_at: usize,
    /// The current hit as a stable row, so a recount can find it after the grid
    /// has moved the raw line numbers.
    search_mark: Option<LineAnchor>,
    /// Literal, case and expression switches, shared by the scrollback and the blocks.
    search_options: SearchOptions,
    /// Top row of the viewport when the bar opened, put back when it closes.
    ///
    /// `None` means the viewport was pinned to the live bottom. A display offset
    /// is not stored: while the reader is parked in history, new output increases
    /// that offset so the same rows stay on screen, and the old number would
    /// close onto a lower row.
    search_scroll: Option<LineAnchor>,
    block_filter: BlockFilter,
    /// The shortcuts the pane's own controls answer, as the bindings in force show
    /// them (R5). The view keeps this in step with the settings page.
    shortcuts: TerminalShortcuts,
    /// Background notices the window has not collected yet (R7).
    ///
    /// They are taken by the window, which knows which tab is on screen; this pane only
    /// remembers that something happened, never what it was.
    attention: TerminalAttention,
    /// What X11 forwarding of this session reported last (R11).
    ///
    /// Taken by the window like the background notices are: the pane only holds
    /// what arrived since the last look.
    x11: Option<X11Status>,
    /// What the shell could not be given when it started (R14, R15).
    ///
    /// Read once from the session and kept, because the notice belongs to this pane
    /// for as long as it is on screen: the environment of a running shell cannot
    /// change, and a note that disappeared would leave the user with an unexplained
    /// locale or an unexplained missing program.
    environment_notes: Vec<ShellEnvironmentNote>,
}

/// What a terminal's copy, paste and search controls answer, written the way a
/// person reads a shortcut. `None` means the action is unbound, so the tooltip and the
/// menu say nothing rather than naming a key the window would not take.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TerminalShortcuts {
    /// The search action, `Ctrl+Shift+F` or `⌘F` by default.
    pub search: Option<String>,
    /// The copy action, `Ctrl+Shift+C` or `⌘⇧C` by default.
    pub copy: Option<String>,
    /// The paste action, `Ctrl+Shift+V` or `⌘⇧V` by default.
    pub paste: Option<String>,
}

impl TerminalShortcuts {
    /// The labels of the bindings in force.
    #[must_use]
    pub fn of(keymap: &crate::app::keymap::Keymap) -> Self {
        Self {
            search: keymap.label("terminal.search"),
            copy: keymap.label("terminal.copy"),
            paste: keymap.label("terminal.paste"),
        }
    }

    /// The default catalogue labels, which a pane starts with before the view hands
    /// it the bindings really in force.
    #[must_use]
    pub fn defaults() -> Self {
        Self::of(&crate::app::keymap::Keymap::defaults(
            crate::app::nav::Platform::current(),
        ))
    }
}

/// A paste held until the user confirms the session it names.
struct PendingPaste {
    text: String,
    /// The session label captured when the paste was offered.
    target: String,
    /// Whether the text is a saved command that runs once it is confirmed (R6).
    ///
    /// A confirmation is asked for the same reason either way — the text is multi-line or
    /// high-risk — but what happens after it differs: a paste is only typed, a command is
    /// typed and submitted.
    execute: bool,
}

/// What the find bar says beside the query (R1).
///
/// Exactly one of these is drawn, so "nothing matched" and "this pattern is broken"
/// never look alike, and an empty query claims neither of them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SearchNote {
    /// The query is empty, so no count is claimed.
    Silent,
    /// The place the viewport is at, counted from one, of how many matched.
    Position {
        /// One-based position among the matches.
        position: usize,
        /// How many places matched.
        total: usize,
    },
    /// The query matched nothing.
    Empty,
    /// The expression could not be read; the reason is the engine's own words.
    Invalid(String),
}

/// What the find bar is showing, for tests that assert what a person reads there.
///
/// It is the same state the bar draws from, so a test cannot pass on a number the
/// user would never see.
#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchBarState {
    /// Whether the bar is open.
    pub open: bool,
    /// Whether typed text edits the query rather than reaching the shell.
    pub focused: bool,
    /// The query as typed.
    pub query: String,
    /// Whether the case switch is on.
    pub case_sensitive: bool,
    /// Whether the query is read as an expression.
    pub regex: bool,
    /// What the bar says beside the query.
    pub note: SearchNote,
    /// How many command blocks the list below the bar is drawing.
    pub blocks: usize,
}

impl TerminalPane {
    /// Starts a shell and wraps it in a pane.
    ///
    /// A launch failure is stored as [`PaneState::Failed`] and no process is kept,
    /// so the caller cannot present a terminal that has nothing behind it.
    pub fn start(
        shell: LocalShell,
        tag: EventTag,
        grid: GridSize,
        font_size: f32,
        locale: Locale,
        cx: &mut Context<Self>,
    ) -> Self {
        let font_size = sanitize_font_size(font_size);
        let grid = grid.clamped();
        let core = TerminalCore::new(grid.cols, grid.rows, shellune_terminal::DEFAULT_SCROLLBACK);
        let (source, state, environment_notes) = match LocalSession::spawn(&shell, grid, None) {
            Ok(session) => {
                // Taken before the session moves: what the shell could not be given is
                // said once, at startup, and has to reach the bar the user reads.
                let notes = session.environment().notes().to_vec();
                (Some(PaneSource::Local(session)), PaneState::Live, notes)
            }
            Err(error) => (None, PaneState::Failed(error.session_failure()), Vec::new()),
        };

        let mut pane = Self {
            tag,
            core,
            source,
            origin: PaneOrigin::Local(shell),
            state,
            #[cfg(test)]
            sent: Vec::new(),
            font_size,
            cell: CellSize::for_font_size(font_size),
            grid,
            viewport: None,
            focus: cx.focus_handle(),
            dragging: false,
            press: None,
            press_selects: false,
            mouse_held: None,
            last_report_cell: None,
            menu: None,
            marked: None,
            last_copy: None,
            locale,
            polling: false,
            pending_paste: None,
            search_open: false,
            search_focused: false,
            search_query: String::new(),
            search_error: None,
            search_hits: Vec::new(),
            search_at: 0,
            search_mark: None,
            search_options: SearchOptions::literal(),
            search_scroll: None,
            block_filter: BlockFilter::All,
            shortcuts: TerminalShortcuts::defaults(),
            attention: TerminalAttention::default(),
            x11: None,
            environment_notes,
        };
        pane.start_polling(cx);
        pane
    }

    /// Wraps a live remote session in a pane.
    ///
    /// The session has already reached the remote shell, so the pane starts live:
    /// everything from here on is the same grid, selection, scrollback and input
    /// encoding a local shell uses.
    pub fn start_remote(
        session: RemoteSession,
        tag: EventTag,
        target: impl Into<String>,
        grid: GridSize,
        font_size: f32,
        locale: Locale,
        cx: &mut Context<Self>,
    ) -> Self {
        let font_size = sanitize_font_size(font_size);
        let grid = grid.clamped();
        let target = target.into();
        let core = TerminalCore::new(grid.cols, grid.rows, shellune_terminal::DEFAULT_SCROLLBACK);
        let mut pane = Self {
            tag,
            core,
            source: Some(PaneSource::Remote(session)),
            origin: PaneOrigin::Remote { target },
            state: PaneState::Live,
            #[cfg(test)]
            sent: Vec::new(),
            font_size,
            cell: CellSize::for_font_size(font_size),
            grid,
            viewport: None,
            focus: cx.focus_handle(),
            dragging: false,
            press: None,
            press_selects: false,
            mouse_held: None,
            last_report_cell: None,
            menu: None,
            marked: None,
            last_copy: None,
            locale,
            polling: false,
            pending_paste: None,
            search_open: false,
            search_focused: false,
            search_query: String::new(),
            search_error: None,
            search_hits: Vec::new(),
            search_at: 0,
            search_mark: None,
            search_options: SearchOptions::literal(),
            search_scroll: None,
            block_filter: BlockFilter::All,
            shortcuts: TerminalShortcuts::defaults(),
            attention: TerminalAttention::default(),
            x11: None,
            environment_notes: Vec::new(),
        };
        pane.start_polling(cx);
        pane
    }

    /// Wraps a live jump chain in a pane.
    ///
    /// The target shell is already running. Later output and a later failure use the
    /// same grid as a direct session; a chain failure keeps the hop in its own text.
    pub fn start_chain(
        session: ChainSession,
        tag: EventTag,
        target: impl Into<String>,
        grid: GridSize,
        font_size: f32,
        locale: Locale,
        cx: &mut Context<Self>,
    ) -> Self {
        let font_size = sanitize_font_size(font_size);
        let grid = grid.clamped();
        let target = target.into();
        let core = TerminalCore::new(grid.cols, grid.rows, shellune_terminal::DEFAULT_SCROLLBACK);
        let mut pane = Self {
            tag,
            core,
            source: Some(PaneSource::Chain(session)),
            origin: PaneOrigin::Remote { target },
            state: PaneState::Live,
            #[cfg(test)]
            sent: Vec::new(),
            font_size,
            cell: CellSize::for_font_size(font_size),
            grid,
            viewport: None,
            focus: cx.focus_handle(),
            dragging: false,
            press: None,
            press_selects: false,
            mouse_held: None,
            last_report_cell: None,
            menu: None,
            marked: None,
            last_copy: None,
            locale,
            polling: false,
            pending_paste: None,
            search_open: false,
            search_focused: false,
            search_query: String::new(),
            search_error: None,
            search_hits: Vec::new(),
            search_at: 0,
            search_mark: None,
            search_options: SearchOptions::literal(),
            search_scroll: None,
            block_filter: BlockFilter::All,
            shortcuts: TerminalShortcuts::defaults(),
            attention: TerminalAttention::default(),
            x11: None,
            environment_notes: Vec::new(),
        };
        pane.start_polling(cx);
        pane
    }

    /// Shuts the source down because the user closed it.
    ///
    /// The last screen stays on display: dropping the grid on close would hide what
    /// was on it, and the pane reports that it is no longer live either way.
    pub fn close(&mut self, cx: &mut Context<Self>) {
        if let Some(source) = self.source.as_mut() {
            source.close();
        }
        self.source = None;
        self.polling = false;
        if self.state.accepts_input() {
            self.state = PaneState::Exited {
                report: ExitReport::Clean,
            };
        }
        cx.notify();
    }

    /// A pane with a terminal but no process.
    ///
    /// Tests use this to drive the grid deterministically; the application always
    /// goes through [`TerminalPane::start`].
    #[cfg(test)]
    pub fn without_process(
        shell: LocalShell,
        tag: EventTag,
        grid: GridSize,
        font_size: f32,
        locale: Locale,
        cx: &mut Context<Self>,
    ) -> Self {
        let font_size = sanitize_font_size(font_size);
        let grid = grid.clamped();
        Self {
            tag,
            core: TerminalCore::new(grid.cols, grid.rows, 1000),
            source: None,
            origin: PaneOrigin::Local(shell),
            state: PaneState::Live,
            #[cfg(test)]
            sent: Vec::new(),
            font_size,
            cell: CellSize::for_font_size(font_size),
            grid,
            viewport: None,
            focus: cx.focus_handle(),
            dragging: false,
            press: None,
            press_selects: false,
            mouse_held: None,
            last_report_cell: None,
            menu: None,
            marked: None,
            last_copy: None,
            locale,
            polling: false,
            pending_paste: None,
            search_open: false,
            search_focused: false,
            search_query: String::new(),
            search_error: None,
            search_hits: Vec::new(),
            search_at: 0,
            search_mark: None,
            search_options: SearchOptions::literal(),
            search_scroll: None,
            block_filter: BlockFilter::All,
            shortcuts: TerminalShortcuts::defaults(),
            attention: TerminalAttention::default(),
            x11: None,
            environment_notes: Vec::new(),
        }
    }

    /// Feeds bytes as if the shell had produced them.
    #[cfg(test)]
    pub fn feed_for_test(&mut self, bytes: &[u8], cx: &mut Context<Self>) {
        let was_alternate = self.alternate_screen();
        let effects = self.core.feed(bytes);
        self.apply_effects(effects, cx);
        self.recount_after_output(was_alternate);
        cx.notify();
    }

    /// The session process this pane shows.
    #[must_use]
    pub fn tag(&self) -> EventTag {
        self.tag
    }

    /// What the shell behind this pane is doing.
    #[must_use]
    pub fn state(&self) -> &PaneState {
        &self.state
    }

    /// Current font size.
    #[must_use]
    pub fn font_size(&self) -> f32 {
        self.font_size
    }

    /// Size of one character cell, as the window measured it.
    #[cfg(test)]
    #[must_use]
    pub fn cell_size(&self) -> CellSize {
        self.cell
    }

    /// Switches the language of the labels the pane draws itself.
    pub fn set_locale(&mut self, locale: Locale, cx: &mut Context<Self>) {
        if self.locale != locale {
            self.locale = locale;
            cx.notify();
        }
    }

    /// The grid the pane is showing, for tests that assert the layout.
    #[cfg(test)]
    pub fn grid(&self) -> GridSize {
        self.grid
    }

    /// The visible grid as text, row by row.
    #[cfg(test)]
    pub fn visible_text(&self) -> Vec<String> {
        self.core.snapshot().text()
    }

    /// The last text the pane put on the clipboard.
    #[cfg(test)]
    pub fn last_copy(&self) -> Option<&str> {
        self.last_copy.as_deref()
    }

    /// Bytes the pane sent to the shell.
    #[cfg(test)]
    pub fn sent(&self) -> &[u8] {
        &self.sent
    }

    /// Lines the view is scrolled back by.
    #[cfg(test)]
    pub fn display_offset(&self) -> usize {
        self.core.display_offset()
    }

    /// Lines of scrollback above the screen.
    #[cfg(test)]
    #[must_use]
    pub fn history_len(&self) -> usize {
        self.core.history()
    }

    /// Applies whatever the shell wrote since the last poll.
    ///
    /// The polling task waits on the window's timer, which a test cannot advance, so
    /// a test drives the same call directly.
    #[cfg(test)]
    pub fn pump_once(&mut self, cx: &mut Context<Self>) -> bool {
        self.drain_events(cx)
    }

    /// The focus handle the window gives the terminal.
    #[must_use]
    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    /// Increases or decreases the font size by one step.
    pub fn step_font_size(&mut self, larger: bool, cx: &mut Context<Self>) {
        let size = step_font_size(self.font_size, larger);
        self.set_font_size(size, cx);
    }

    fn start_polling(&mut self, cx: &mut Context<Self>) {
        if self.polling || self.source.is_none() {
            return;
        }
        self.polling = true;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(OUTPUT_POLL_INTERVAL).await;
                let keep_going = this.update(cx, |pane, cx| pane.drain_events(cx));
                match keep_going {
                    Ok(true) => {}
                    _ => break,
                }
            }
        })
        .detach();
    }

    /// Applies events the caller already took from the source.
    ///
    /// The window drains the attempt's event channel before this pane exists, and a
    /// batch where the connection notice and the first output arrived together is
    /// the common case; the output cannot be given back to the channel, so it is
    /// handed over here and applied in the order it arrived. Dropping it would open
    /// the remote terminal with the first screen missing.
    /// Returns whether the batch belonged to this pane: a batch drained under another
    /// session or another process of this session is dropped, because applying it
    /// would put one session's output into another one's screen.
    pub fn adopt_events(
        &mut self,
        tag: EventTag,
        events: Vec<SourceEvent>,
        cx: &mut Context<Self>,
    ) -> bool {
        if tag != self.tag {
            return false;
        }
        if events.is_empty() {
            return true;
        }
        for event in events {
            self.handle_event(event, cx);
        }
        cx.notify();
        true
    }

    /// Applies every event that arrived since the last poll.
    ///
    /// Returns whether polling should continue: a pane whose process ended stops
    /// polling instead of waking up for ever.
    fn drain_events(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(source) = self.source.as_mut() else {
            return false;
        };
        let events = source.drain();
        if events.is_empty() {
            return self.state.accepts_input();
        }
        let mut live = true;
        for event in events {
            live = self.handle_event(event, cx);
        }
        cx.notify();
        live
    }

    fn handle_event(&mut self, event: SourceEvent, cx: &mut Context<Self>) -> bool {
        match event {
            SourceEvent::Data(bytes) => {
                let was_alternate = self.alternate_screen();
                let effects = self.core.feed(&bytes);
                self.apply_effects(effects, cx);
                self.recount_after_output(was_alternate);
                self.state.accepts_input()
            }
            SourceEvent::Exited(report) => {
                self.state = PaneState::Exited { report };
                false
            }
            SourceEvent::Lost(reason) => {
                // The session did start; what failed is reading it. Saying it "could
                // not be started" would send the user to fix a launch that worked.
                self.state = PaneState::Failed(self.origin.lost_failure(reason));
                false
            }
            SourceEvent::X11(status) => {
                // A display that failed does not end the session: the shell keeps
                // working and the status is news for the session bar (R11).
                self.x11 = Some(status);
                self.state.accepts_input()
            }
        }
    }

    fn apply_effects(&mut self, effects: TerminalEffects, cx: &mut Context<Self>) {
        // Read the background flags before the clipboard text is moved out: the notice
        // is two booleans and never anything the terminal printed (R7).
        let notices = TerminalAttention {
            bell: effects.bell,
            finished: effects.commands_finished > 0,
        };
        if !effects.writes.is_empty() {
            // A device-attribute query has to be answered or the program that asked
            // waits for ever; the answer goes back through the same PTY.
            self.write(&effects.writes, cx);
        }
        if let Some(text) = effects.clipboard {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
        if !notices.is_empty() {
            // A bell byte and a command end the shell reported are remembered for the
            // window, which marks the tab only when it is not the one on screen (R7).
            self.attention.absorb(notices);
        }
    }

    /// Takes the background notices this terminal produced since the last call.
    ///
    /// The window collects them, so the same bell is never marked twice. Reading them
    /// does not touch the grid, the blocks or the session state.
    pub fn take_attention(&mut self) -> TerminalAttention {
        std::mem::take(&mut self.attention)
    }

    /// Takes what X11 forwarding reported since the last call (R11).
    ///
    /// The window keeps it beside the session, because the session bar is where a
    /// remote program's access to this machine's display is stated. Reading it does
    /// not touch the grid or the session state.
    pub fn take_x11_status(&mut self) -> Option<X11Status> {
        self.x11.take()
    }

    /// The notices waiting to be collected, for the tests that check a tab mark.
    #[cfg(test)]
    #[must_use]
    pub fn pending_attention_for_test(&self) -> TerminalAttention {
        self.attention
    }

    fn write(&mut self, bytes: &[u8], cx: &mut Context<Self>) {
        if bytes.is_empty() || !self.state.accepts_input() {
            return;
        }
        // Only a test keeps a copy of the byte stream; a release build would grow
        // it for the life of the session.
        #[cfg(test)]
        self.sent.extend_from_slice(bytes);
        let Some(source) = self.source.as_mut() else {
            return;
        };
        if let Err(error) = source.write(bytes) {
            let failure = self.origin.lost_failure(error);
            self.state = PaneState::Failed(failure);
            cx.notify();
        }
    }

    /// Sends what a key press means, or scrolls for the view-only combinations.
    fn handle_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.menu.is_some() && event.keystroke.key == "escape" {
            self.menu = None;
            cx.notify();
            return;
        }
        let keystroke = &event.keystroke;
        let modifiers = KeyModifiers {
            shift: keystroke.modifiers.shift,
            control: keystroke.modifiers.control,
            alt: keystroke.modifiers.alt,
            platform: keystroke.modifiers.platform,
        };
        let plain = !modifiers.control && !modifiers.alt && !modifiers.platform;

        // While the find bar holds the keyboard, the keys that edit and leave it are
        // the bar's: Escape closes it and hands the terminal back, Enter steps to the
        // next match and Shift+Enter to the previous one, Alt+C turns the case switch
        // over and Alt+R the expression switch, so a complete search — including its
        // two switches — can be run without a pointer (R1, R4). None of these reaches
        // the shell. They are the bar's only while it holds the keyboard: the terminal
        // keeps Alt+C and Alt+R whenever the keys are its own.
        if self.search_focused && self.search_open && !self.alternate_screen() {
            // Alt alone, without Ctrl or ⌘, is a bar key rather than a program
            // combination. Shift is left to the program so the pairs other programs
            // read stay intact.
            let bar_alt = modifiers.alt && !modifiers.control && !modifiers.platform;
            match keystroke.key.as_str() {
                "escape" if plain => {
                    self.close_search(window, cx);
                    cx.stop_propagation();
                    return;
                }
                "enter" if plain => {
                    if modifiers.shift {
                        self.previous_search_hit(cx);
                    } else {
                        self.next_search_hit(cx);
                    }
                    cx.stop_propagation();
                    return;
                }
                "backspace" if plain => {
                    self.search_query.pop();
                    self.refresh_search(true);
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                "c" if bar_alt && !modifiers.shift => {
                    self.toggle_case_sensitive(cx);
                    cx.stop_propagation();
                    return;
                }
                "r" if bar_alt && !modifiers.shift => {
                    self.toggle_regex(cx);
                    cx.stop_propagation();
                    return;
                }
                _ => {}
            }
        }

        // The terminal's own clipboard and search keys arrive as actions, so the
        // settings page can rebind or unbind them (R5). These confirm or cancel a
        // paste and step through search hits; they are not Ctrl+L or Ctrl+C, which
        // stay program input below.
        if modifiers.control && modifiers.shift {
            match keystroke.key.as_str() {
                "enter" if self.pending_paste.is_some() => {
                    self.confirm_paste(cx);
                    cx.stop_propagation();
                    return;
                }
                "backspace" if self.pending_paste.is_some() => {
                    self.cancel_paste(cx);
                    cx.stop_propagation();
                    return;
                }
                "g" if self.search_open && !self.alternate_screen() => {
                    self.next_search_hit(cx);
                    cx.stop_propagation();
                    return;
                }
                _ => {}
            }
        }
        if modifiers.platform {
            return;
        }

        if modifiers.shift {
            let page = i32::try_from(self.grid.rows).unwrap_or(24);
            match keystroke.key.as_str() {
                "pageup" => {
                    self.core.scroll(page);
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                "pagedown" => {
                    self.core.scroll(-page);
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                _ => {}
            }
        }

        if let Some(key) = Key::from_name(&keystroke.key)
            && let Some(bytes) = encode_key(key, modifiers, self.core.mode())
        {
            self.write(&bytes, cx);
            // The key was consumed as a terminal key, so the text-input path must not
            // run for it as well: Return would otherwise be sent twice.
            cx.stop_propagation();
            cx.notify();
        }
    }

    /// Sends text to the program as if it had been typed.
    pub fn type_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.write(text.as_bytes(), cx);
    }

    /// Takes the shortcuts in force, so the toolbar describes the keys the window
    /// really answers (R5).
    pub fn set_shortcuts(&mut self, shortcuts: TerminalShortcuts, cx: &mut Context<Self>) {
        if self.shortcuts != shortcuts {
            self.shortcuts = shortcuts;
            cx.notify();
        }
    }

    /// A tooltip line naming the shortcut, or just the action when it is unbound.
    fn with_shortcut(label: &str, shortcut: &Option<String>) -> String {
        match shortcut {
            Some(shortcut) => format!("{label} ({shortcut})"),
            None => label.to_owned(),
        }
    }

    /// Puts the selection on the clipboard.
    fn copy_selection(&mut self, cx: &mut Context<Self>) {
        let Some(text) = self.core.selection_text().filter(|text| !text.is_empty()) else {
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
        self.last_copy = Some(text);
        cx.notify();
    }

    /// Sends pasted text, or holds it when it is multi-line or high-risk.
    ///
    /// Holding it writes nothing. Confirmation encodes with the mode the program
    /// has set at confirm time, and only into this pane.
    ///
    /// While the find bar holds the keyboard, pasted text edits the query instead:
    /// a copy-pasted command must not reach the shell just because the bar was open
    /// (R1).
    fn paste(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = cx.read_from_clipboard() else {
            return;
        };
        if let Some(text) = item.text().filter(|text| !text.is_empty()) {
            if self.search_focused && self.search_open && !self.alternate_screen() {
                // Line breaks have no meaning in a query: a pasted block becomes one
                // line, exactly as a single-line field would take it.
                let query = text.lines().collect::<Vec<_>>().join(" ");
                self.append_search_query(&query, cx);
                return;
            }
            self.stage_paste(&text, cx);
            return;
        }
        // An image. A program here (an AI command line, an editor) reads the
        // clipboard itself when it sees Ctrl+V, as in other terminals; a server
        // cannot, so the window uploads the image and types its path.
        let image = item.into_entries().find_map(|entry| match entry {
            gpui::ClipboardEntry::Image(image) => Some(image),
            _ => None,
        });
        let Some(image) = image else {
            return;
        };
        match self.origin {
            PaneOrigin::Local(_) => self.write(&[0x16], cx),
            PaneOrigin::Remote { .. } => cx.emit(PaneRequest::PasteImage(image)),
        }
    }

    /// Files dropped on the terminal: their paths, quoted, at the prompt here; on
    /// a server, uploaded first.
    fn drop_paths(
        &mut self,
        paths: &[std::path::PathBuf],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if paths.is_empty() {
            return;
        }
        window.focus(&self.focus);
        match self.origin {
            PaneOrigin::Local(_) => {
                let mut text = paths
                    .iter()
                    .map(|path| quote_local_path(&path.to_string_lossy()))
                    .collect::<Vec<_>>()
                    .join(" ");
                text.push(' ');
                self.type_text(&text, cx);
            }
            PaneOrigin::Remote { .. } => cx.emit(PaneRequest::UploadFiles(paths.to_vec())),
        }
    }

    /// The presentation the user chose.
    #[must_use]
    pub fn presentation(&self) -> TerminalMode {
        self.core.presentation()
    }

    /// Applies a saved presentation without notifying. Used while the pane is built.
    pub fn apply_presentation(&mut self, mode: TerminalMode) {
        self.core.set_presentation(mode);
    }

    /// Switches classic and command-block presentation.
    ///
    /// The grid is not cleared. Command blocks appear only when the shell has
    /// reported them and the alternate screen is not up.
    pub fn set_presentation(&mut self, mode: TerminalMode, cx: &mut Context<Self>) {
        self.core.set_presentation(mode);
        cx.notify();
    }

    /// Offers `text` to this pane. A risky paste waits; a plain line is written.
    pub fn stage_paste(&mut self, text: &str, cx: &mut Context<Self>) {
        if text.is_empty() {
            return;
        }
        if paste_needs_confirmation(text) {
            self.pending_paste = Some(PendingPaste {
                text: text.to_owned(),
                target: self.origin.label().to_owned(),
                execute: false,
            });
            cx.notify();
            return;
        }
        self.pending_paste = None;
        self.write_paste(text, cx);
        cx.notify();
    }

    /// Offers a saved command to this pane (R6).
    ///
    /// `execute` is the only difference between the palette's two actions: inserting
    /// types the body and submits nothing, running types it and sends one line break.
    /// The body's own trailing line breaks are dropped first, so a command saved with a
    /// final newline cannot run just because it was inserted; running then adds exactly
    /// one break, so the command reaches the shell once rather than once per stored line
    /// ending.
    ///
    /// A multi-line or otherwise risky body is held and confirmed the way a paste is —
    /// the same prompt, the same pane, the same rule that cancelling writes nothing.
    ///
    /// The answer says whether the bytes are in this pane now: the window reports a
    /// command as inserted or sent only for [`SnippetOffer::Written`], because a body
    /// waiting behind the prompt has not arrived anywhere yet.
    #[must_use]
    pub fn stage_snippet(
        &mut self,
        body: &str,
        execute: bool,
        cx: &mut Context<Self>,
    ) -> SnippetOffer {
        // The body arrives as it is saved; the rule about its trailing line breaks lives in
        // one place so the editor, the list and this pane cannot disagree about what will
        // be typed.
        let trimmed = crate::app::snippets::body_for_terminal(body);
        if trimmed.trim().is_empty() {
            return SnippetOffer::Empty;
        }
        if paste_needs_confirmation(trimmed) {
            self.pending_paste = Some(PendingPaste {
                text: trimmed.to_owned(),
                target: self.origin.label().to_owned(),
                execute,
            });
            cx.notify();
            return SnippetOffer::Waiting;
        }
        self.pending_paste = None;
        self.write_snippet(trimmed, execute, cx);
        cx.notify();
        SnippetOffer::Written
    }

    /// Writes the held paste into this pane only.
    ///
    /// Returns whether a paste was waiting. The bytes go through this pane's
    /// source, so another session cannot receive them.
    pub fn confirm_paste(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(pending) = self.pending_paste.take() else {
            return false;
        };
        if pending.execute {
            self.write_snippet(&pending.text, true, cx);
        } else {
            self.write_paste(&pending.text, cx);
        }
        cx.notify();
        true
    }

    /// Drops a held paste. Nothing is written.
    pub fn cancel_paste(&mut self, cx: &mut Context<Self>) {
        if self.pending_paste.take().is_some() {
            cx.notify();
        }
    }

    /// The session a held paste would be sent to, when one is waiting.
    #[cfg(test)]
    #[must_use]
    pub fn pending_paste_target(&self) -> Option<&str> {
        self.pending_paste
            .as_ref()
            .map(|paste| paste.target.as_str())
    }

    /// Whether a paste or a saved command is waiting for the user's answer.
    ///
    /// The window reads this to keep its own "waiting for confirmation" note honest: the
    /// note leaves as soon as the prompt is answered either way.
    #[must_use]
    pub fn awaits_paste_confirmation(&self) -> bool {
        self.pending_paste.is_some()
    }

    /// The text a held paste would send.
    #[cfg(test)]
    #[must_use]
    pub fn pending_paste_text(&self) -> Option<&str> {
        self.pending_paste.as_ref().map(|paste| paste.text.as_str())
    }

    /// Whether the held paste is a saved command that will run once it is confirmed.
    #[cfg(test)]
    #[must_use]
    pub fn pending_paste_runs_for_test(&self) -> bool {
        self.pending_paste
            .as_ref()
            .is_some_and(|paste| paste.execute)
    }

    /// Command blocks recorded from shell integration, including a running one.
    #[cfg(test)]
    #[must_use]
    pub fn command_blocks(&self) -> Vec<CommandBlock> {
        self.core.command_blocks()
    }

    /// Sends one block's command as a new execution.
    ///
    /// The recorded block is not edited. A later integration marker, if the shell
    /// emits one, becomes a separate block.
    pub fn rerun_block(&mut self, id: CommandBlockId, cx: &mut Context<Self>) {
        let Some(command) = self.core.rerun_command(id) else {
            return;
        };
        let before = self.core.command_blocks();
        let mut bytes = encode_text(&command);
        if !bytes.ends_with(b"\r") {
            bytes.push(b'\r');
        }
        self.write(&bytes, cx);
        debug_assert_eq!(self.core.command_blocks(), before);
        cx.notify();
    }

    fn write_paste(&mut self, text: &str, cx: &mut Context<Self>) {
        let bytes = encode_paste(text, self.core.mode());
        self.write(&bytes, cx);
    }

    /// Types a saved command, and submits it when `execute` says so.
    ///
    /// `text` must already be free of trailing line breaks; the one break this sends is
    /// the submit that running asks for, and nothing else adds another.
    fn write_snippet(&mut self, text: &str, execute: bool, cx: &mut Context<Self>) {
        let mut bytes = encode_paste(text, self.core.mode());
        if execute {
            bytes.push(b'\r');
        }
        self.write(&bytes, cx);
    }

    fn alternate_screen(&self) -> bool {
        self.core.mode().contains(TermMode::ALT_SCREEN)
    }

    /// Whether the alternate screen is up. Tests use this; the view reads the same flag.
    #[cfg(test)]
    #[must_use]
    pub fn alternate_for_test(&self) -> bool {
        self.alternate_screen()
    }

    /// Whether the block list is on screen. The alternate screen and classic mode hide it.
    #[cfg(test)]
    #[must_use]
    pub fn shows_blocks_for_test(&self) -> bool {
        !self.alternate_screen() && (self.search_open || self.core.shows_command_blocks())
    }

    /// How many command blocks this terminal has recorded, for the tests that check a
    /// switched-off reminder leaves the blocks alone (R7).
    #[cfg(test)]
    #[must_use]
    pub fn command_block_count_for_test(&self) -> usize {
        self.core.command_blocks().len()
    }

    /// Opens or closes the find bar (R1).
    ///
    /// A full-screen program owns the screen and its own keys, so an alternate-screen
    /// terminal never opens one: its contents are not this pane's history, and the
    /// keys the bar would take have to reach the program.
    fn toggle_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.search_open {
            self.close_search(window, cx);
        } else if !self.alternate_screen() {
            self.open_search(window, cx);
        }
    }

    fn open_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_open = true;
        self.search_focused = true;
        // The top row, taken before anything scrolls, so closing puts the reader
        // back on the line they were reading rather than at the bottom of a live
        // shell. The anchor, not the display offset: new output while this row is
        // parked in history grows the offset and would otherwise close lower.
        self.search_scroll = self.core.parked_viewport();
        self.refresh_search(true);
        window.focus(&self.focus);
        cx.notify();
    }

    /// Closes the bar, restores the scroll position and gives the keyboard back.
    fn close_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_open = false;
        self.search_focused = false;
        self.search_query.clear();
        self.search_hits.clear();
        self.search_at = 0;
        self.search_mark = None;
        self.search_error = None;
        match self.search_scroll.take() {
            Some(anchor) => self.scroll_to_offset(self.core.offset_of(anchor)),
            None => self.core.scroll_to_bottom(),
        }
        // The terminal is where the keyboard was, and where it goes back to.
        window.focus(&self.focus);
        cx.notify();
    }

    /// Moves the viewport so `offset` lines are above the live screen.
    fn scroll_to_offset(&mut self, offset: usize) {
        let current = i32::try_from(self.core.display_offset()).unwrap_or(i32::MAX);
        let target = i32::try_from(offset).unwrap_or(i32::MAX);
        self.core.scroll(target.saturating_sub(current));
    }

    /// Re-runs the query over the scrollback.
    ///
    /// `reveal` scrolls to the current match. Output that arrives while the bar is
    /// open only recounts, so the numbers stay true without moving the viewport the
    /// reader is looking at.
    fn refresh_search(&mut self, reveal: bool) {
        // Resolved after the grid has moved. Comparing the line number saved
        // before a scroll with the numbers after it skips the hit: those numbers
        // are more negative, so the old line now belongs to a later match.
        let previous = self.search_mark.and_then(|anchor| {
            self.core
                .line_of(anchor)
                .map(|line| (line, anchor.column()))
        });
        self.search_hits.clear();
        self.search_at = 0;
        self.search_error = None;
        if self.search_query.is_empty() {
            self.search_mark = None;
            return;
        }
        match self
            .core
            .search_history(&self.search_query, self.search_options)
        {
            Ok(hits) => {
                self.search_hits = hits;
                let Some(last) = self.search_hits.len().checked_sub(1) else {
                    self.search_mark = None;
                    return;
                };
                // A longer query keeps the reader near the hit they were on: the
                // first match at or below that row, or the last one when the query
                // moved past every remaining match.
                self.search_at = match previous {
                    Some((line, column)) => self
                        .search_hits
                        .iter()
                        .position(|hit| {
                            hit.line > line || (hit.line == line && hit.column >= column)
                        })
                        .unwrap_or(last),
                    None => 0,
                };
                self.note_search_hit();
                if reveal {
                    self.reveal_current();
                }
            }
            // A pattern the engine cannot compile is reported, and the empty list it
            // leaves behind is never drawn as "no matches".
            Err(failure) => {
                self.search_error = Some(failure.reason);
                self.search_mark = None;
            }
        }
    }

    /// Remembers the current hit as a stable row.
    fn note_search_hit(&mut self) {
        self.search_mark = self
            .search_hits
            .get(self.search_at)
            .map(|hit| self.core.anchor_line(hit.line, hit.column));
    }

    fn reveal_current(&mut self) {
        if let Some(hit) = self.search_hits.get(self.search_at) {
            self.core.reveal_history(hit.line);
        }
    }

    /// Adds text to the query, the way typing or pasting into the bar does.
    fn append_search_query(&mut self, text: &str, cx: &mut Context<Self>) {
        if text.is_empty() {
            return;
        }
        self.search_query.push_str(text);
        self.refresh_search(true);
        cx.notify();
    }

    /// Counts the matches again after the grid changed.
    ///
    /// The viewport is left where the reader put it: a search that jumped on every
    /// chunk of output would fight the person reading the screen. A change between
    /// the primary and the full-screen grid is the exception — the matches belong to
    /// the screen that has just come back, so the reader is put back on one.
    fn recount_after_output(&mut self, was_alternate: bool) {
        if !self.search_open {
            return;
        }
        self.refresh_search(self.alternate_screen() != was_alternate);
    }

    /// Counts the matches again after the grid was re-wrapped by a resize.
    ///
    /// The viewport stays where the reader put it: a resize changes where the lines
    /// break, not where the person is looking.
    fn recount_after_layout(&mut self) {
        if !self.search_open {
            return;
        }
        self.refresh_search(false);
    }

    fn next_search_hit(&mut self, cx: &mut Context<Self>) {
        self.step_search(1, cx);
    }

    fn previous_search_hit(&mut self, cx: &mut Context<Self>) {
        self.step_search(-1, cx);
    }

    /// Steps through the matches, wrapping at either end.
    fn step_search(&mut self, step: i32, cx: &mut Context<Self>) {
        let count = i32::try_from(self.search_hits.len()).unwrap_or(0);
        if count == 0 {
            return;
        }
        let current = i32::try_from(self.search_at).unwrap_or(0);
        let next = (current + step).rem_euclid(count);
        self.search_at = usize::try_from(next).unwrap_or(0);
        self.note_search_hit();
        self.reveal_current();
        cx.notify();
    }

    /// Turns the case switch over and shows the matches it leaves.
    fn toggle_case_sensitive(&mut self, cx: &mut Context<Self>) {
        self.search_options.case_sensitive = !self.search_options.case_sensitive;
        self.refresh_search(true);
        cx.notify();
    }

    /// Turns literal text into an expression, or back.
    fn toggle_regex(&mut self, cx: &mut Context<Self>) {
        self.search_options.regex = !self.search_options.regex;
        self.refresh_search(true);
        cx.notify();
    }

    /// The state the find bar draws, for tests that assert what a person sees.
    #[cfg(test)]
    #[must_use]
    pub fn search_state(&self) -> SearchBarState {
        SearchBarState {
            open: self.search_open,
            focused: self.search_focused,
            query: self.search_query.clone(),
            case_sensitive: self.search_options.case_sensitive,
            regex: self.search_options.regex,
            note: self.search_note(),
            blocks: self.listed_blocks().len(),
        }
    }

    /// What the bar says beside the query, which is also what it draws.
    fn search_note(&self) -> SearchNote {
        if let Some(error) = &self.search_error {
            return SearchNote::Invalid(error.clone());
        }
        let total = self.search_hits.len();
        if total > 0 {
            return SearchNote::Position {
                position: self.search_at + 1,
                total,
            };
        }
        if self.search_query.is_empty() {
            SearchNote::Silent
        } else {
            SearchNote::Empty
        }
    }

    fn copy_block(&mut self, id: CommandBlockId, cx: &mut Context<Self>) {
        let Some(text) = self
            .core
            .copy_block_text(id)
            .filter(|text| !text.is_empty())
        else {
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
        self.last_copy = Some(text);
        cx.notify();
    }

    /// Recomputes the cell size from the font the window resolved.
    fn measure_cell(&mut self, window: &Window) -> CellSize {
        let text_system = window.text_system().clone();
        let font_id = text_system.resolve_font(&gpui::font(monospace_family()));
        let advance = text_system
            .em_advance(font_id, px(self.font_size))
            .unwrap_or_else(|_| px(self.font_size * shellune_terminal::MONOSPACE_ADVANCE_RATIO));
        let cell = CellSize {
            width: f32::from(advance),
            height: self.font_size * shellune_terminal::LINE_HEIGHT_RATIO,
        };
        self.cell = cell;
        cell
    }

    /// Applies a measured viewport, resizing the grid and the PTY when it changed.
    fn apply_viewport(&mut self, bounds: Bounds<Pixels>, cx: &mut Context<Self>) {
        if self.viewport == Some(bounds) {
            return;
        }
        self.viewport = Some(bounds);
        self.resize_grid(bounds.size, cx);
    }

    fn resize_grid(&mut self, size: Size<Pixels>, cx: &mut Context<Self>) {
        let cell = self.cell.sanitized();
        let grid = grid_for(f32::from(size.width), f32::from(size.height), cell);
        if grid == self.grid {
            return;
        }
        self.grid = grid;
        self.core.resize(grid);
        // A narrower or wider grid re-wraps the primary screen, so rows the find bar
        // counted are split or joined: the count and the places it steps through are
        // counted again, without moving the reader (R1).
        self.recount_after_layout();
        if let Some(source) = self.source.as_mut()
            && let Err(_reason) = source.resize(grid)
        {
            // A PTY that rejects the size keeps its old size; the grid still
            // changes so the user sees what the program wrote at that width.
        }
        cx.notify();
    }

    pub(crate) fn set_font_size(&mut self, size: f32, cx: &mut Context<Self>) {
        let size = sanitize_font_size(size);
        if (size - self.font_size).abs() < f32::EPSILON {
            return;
        }
        self.font_size = size;
        self.cell = CellSize::for_font_size(size);
        if let Some(bounds) = self.viewport {
            self.resize_grid(bounds.size, cx);
        }
        cx.notify();
    }

    fn cell_at(&self, position: gpui::Point<Pixels>) -> Option<(usize, usize)> {
        let bounds = self.viewport?;
        let cell = self.cell.sanitized();
        let x = f32::from(position.x) - f32::from(bounds.origin.x);
        let y = f32::from(position.y) - f32::from(bounds.origin.y);
        if x < 0.0 || y < 0.0 {
            return None;
        }
        let column = ((x / cell.width) as usize).min(self.grid.cols.saturating_sub(1));
        let row = ((y / cell.height) as usize).min(self.grid.rows.saturating_sub(1));
        Some((row, column))
    }

    /// Tells the program about a pointer event, when it asked for the mouse.
    ///
    /// Returns whether the event belongs to the program. Holding Shift keeps the
    /// pointer for the terminal itself, so text can still be selected and copied
    /// inside a program that took the mouse, as in other terminals.
    fn report_mouse(
        &mut self,
        button: TerminalMouseButton,
        action: MouseAction,
        position: gpui::Point<Pixels>,
        modifiers: &gpui::Modifiers,
        cx: &mut Context<Self>,
    ) -> bool {
        let mode = self.core.mode();
        if !mouse_reporting(mode) || modifiers.shift {
            return false;
        }
        let Some((row, column)) = self.cell_at(position) else {
            return true;
        };
        if action == MouseAction::Motion {
            if self.last_report_cell == Some((row, column)) {
                return true;
            }
            self.last_report_cell = Some((row, column));
        }
        let held = MouseModifiers {
            shift: false,
            alt: modifiers.alt,
            control: modifiers.control,
        };
        if let Some(bytes) = encode_mouse(button, action, row, column, held, mode) {
            self.write(&bytes, cx);
        }
        true
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus);
        self.search_focused = false;
        self.menu = None;
        if self.report_mouse(
            TerminalMouseButton::Left,
            MouseAction::Press,
            event.position,
            &event.modifiers,
            cx,
        ) {
            self.mouse_held = Some(TerminalMouseButton::Left);
            self.last_report_cell = self.cell_at(event.position);
            return;
        }
        let Some((row, column)) = self.cell_at(event.position) else {
            return;
        };
        // A double click selects a word, a triple click the line.
        let kind = match event.click_count {
            2 => SelectionType::Semantic,
            count if count >= 3 => SelectionType::Lines,
            _ => SelectionType::Simple,
        };
        self.dragging = true;
        self.press = Some((row, column));
        self.press_selects = kind != SelectionType::Simple;
        self.core.begin_selection(row, column, kind);
        cx.notify();
    }

    /// The middle button pastes, unless the program took the mouse.
    fn on_middle_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus);
        if self.report_mouse(
            TerminalMouseButton::Middle,
            MouseAction::Press,
            event.position,
            &event.modifiers,
            cx,
        ) {
            self.mouse_held = Some(TerminalMouseButton::Middle);
            return;
        }
        self.paste(window, cx);
    }

    /// The right button opens the terminal's menu, unless the program took the
    /// mouse.
    fn on_right_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus);
        if self.report_mouse(
            TerminalMouseButton::Right,
            MouseAction::Press,
            event.position,
            &event.modifiers,
            cx,
        ) {
            self.mouse_held = Some(TerminalMouseButton::Right);
            return;
        }
        self.menu = Some(event.position);
        cx.notify();
    }

    /// Ends a press the program was told about.
    fn release_reported(
        &mut self,
        button: TerminalMouseButton,
        event: &MouseUpEvent,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.mouse_held != Some(button) {
            return false;
        }
        self.mouse_held = None;
        self.last_report_cell = None;
        // The release is reported even with Shift now held: the program saw the
        // press and must not be left with a button it thinks is down.
        let mut modifiers = event.modifiers;
        modifiers.shift = false;
        self.report_mouse(button, MouseAction::Release, event.position, &modifiers, cx);
        true
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if !self.dragging {
            let button = self.mouse_held.unwrap_or(TerminalMouseButton::None);
            self.report_mouse(
                button,
                MouseAction::Motion,
                event.position,
                &event.modifiers,
                cx,
            );
            return;
        }
        let Some((row, column)) = self.cell_at(event.position) else {
            return;
        };
        self.core.extend_selection(row, column);
        cx.notify();
    }

    fn on_mouse_up(&mut self, event: &MouseUpEvent, cx: &mut Context<Self>) {
        if self.release_reported(TerminalMouseButton::Left, event, cx) {
            return;
        }
        if !self.dragging {
            return;
        }
        self.dragging = false;
        let released = self.cell_at(event.position);
        if let Some((row, column)) = released {
            self.core.extend_selection(row, column);
        }
        let press = self.press.take();
        // A selection needs movement: pressing and releasing on the same cell is a
        // click, and copying it would replace whatever the user had on the
        // clipboard with one character they did not select. A release outside the
        // grid counts as movement, because the pointer really did leave the cell
        // the button went down on. A double or triple click did choose something.
        let moved = match (press, released) {
            (Some(press), Some(released)) => press != released,
            (Some(_), None) => true,
            (None, _) => false,
        };
        if moved || std::mem::take(&mut self.press_selects) {
            // Copying on release is what makes "select with the mouse, paste" work
            // without a second step; the copy button stays for a repeat.
            self.copy_selection(cx);
        } else {
            // A click clears the selection instead of leaving a one-cell highlight
            // that claims the user selected something.
            self.core.clear_selection();
        }
        cx.notify();
    }

    fn on_scroll(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let lines = match event.delta {
            ScrollDelta::Lines(delta) => delta.y,
            ScrollDelta::Pixels(delta) => f32::from(delta.y) / self.cell.sanitized().height,
        };
        let lines = if lines.abs() < 1.0 {
            lines.signum() * 1.0
        } else {
            lines.trunc()
        } as i32;
        if lines == 0 {
            return;
        }
        let mode = self.core.mode();
        // A program that took the mouse gets the wheel as button presses.
        if mouse_reporting(mode) && !event.modifiers.shift {
            let button = if lines > 0 {
                TerminalMouseButton::WheelUp
            } else {
                TerminalMouseButton::WheelDown
            };
            for _ in 0..lines.unsigned_abs().min(10) {
                self.report_mouse(
                    button,
                    MouseAction::Press,
                    event.position,
                    &event.modifiers,
                    cx,
                );
            }
            return;
        }
        // A full-screen program that did not gets it as the arrow keys, so `less`,
        // `man` and editors scroll; the history only scrolls on the main screen.
        if self.core.display_offset() == 0
            && let Some(bytes) = encode_alternate_scroll(lines.clamp(-10, 10), mode)
        {
            self.write(&bytes, cx);
            return;
        }
        self.core.scroll(lines);
        cx.notify();
    }

    /// The right-click menu: copy, paste and find.
    fn context_menu(&self, window: &Window, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let position = self.menu?;
        let metrics = Metrics::default();
        let strings = self.locale.strings();
        let viewport = window.viewport_size();
        let can_copy = self.core.has_selection();
        let can_paste = cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .is_some_and(|text| !text.is_empty());
        // The hint names the binding in force, like the tooltips do: a rebound key, or
        // an unbinding that handed the combination back to the program, is described as
        // it really is (R5).
        let item =
            |label: &'static str, shortcut: &Option<String>, enabled: bool, id: &'static str| {
                let hover = theme::swatch(shell_palette::SURFACE_HOVER);
                div()
                    .id(id)
                    .debug_selector(move || id.to_owned())
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(metrics.scale(spacing::LOOSE))
                    .h(metrics.scale(28.0))
                    .px(metrics.scale(spacing::TIGHT + 2.0))
                    .rounded(px(theme::radius::MEDIUM))
                    .text_size(metrics.label_size())
                    .text_color(theme::swatch(if enabled {
                        shell_palette::TEXT_PRIMARY
                    } else {
                        shell_palette::TEXT_MUTED
                    }))
                    .when(enabled, |item| {
                        item.cursor_pointer().hover(move |style| style.bg(hover))
                    })
                    .child(label)
                    .when_some(shortcut.clone(), |item, shortcut| {
                        item.child(
                            div()
                                .debug_selector(move || format!("{id}.shortcut"))
                                .text_size(metrics.caption_size())
                                .text_color(theme::swatch(shell_palette::TEXT_MUTED))
                                .child(SharedString::from(shortcut)),
                        )
                    })
            };
        let menu = div()
            .flex()
            .flex_col()
            .min_w(metrics.scale(200.0))
            .p(metrics.scale(spacing::HAIRLINE))
            .rounded(px(theme::radius::LARGE))
            .bg(theme::swatch(shell_palette::SURFACE_RAISED))
            .border_1()
            .border_color(theme::swatch(shell_palette::BORDER_STRONG))
            .shadow_lg()
            .debug_selector(|| "terminal.menu".to_owned())
            .child(
                item(
                    strings.field_copy,
                    &self.shortcuts.copy,
                    can_copy,
                    "terminal.menu.copy",
                )
                .when(can_copy, |item| {
                    item.on_click(cx.listener(|this, _event, _window, cx| {
                        this.copy_selection(cx);
                        this.menu = None;
                        cx.notify();
                    }))
                }),
            )
            .child(
                item(
                    strings.field_paste,
                    &self.shortcuts.paste,
                    can_paste,
                    "terminal.menu.paste",
                )
                .when(can_paste, |item| {
                    item.on_click(cx.listener(|this, _event, window, cx| {
                        this.menu = None;
                        this.paste(window, cx);
                        cx.notify();
                    }))
                }),
            )
            .child(
                div()
                    .h(px(1.0))
                    .mx(metrics.scale(spacing::HAIRLINE))
                    .my(metrics.scale(2.0))
                    .bg(theme::swatch(shell_palette::BORDER)),
            )
            .child(
                item(
                    strings.terminal_search,
                    &self.shortcuts.search,
                    true,
                    "terminal.menu.search",
                )
                .on_click(cx.listener(|this, _event, window, cx| {
                    this.menu = None;
                    this.toggle_search(window, cx);
                })),
            );
        Some(
            gpui::deferred(
                gpui::anchored().position(gpui::Point::default()).child(
                    div()
                        .relative()
                        .w(viewport.width)
                        .h(viewport.height)
                        .child(
                            div()
                                .id("terminal.menu.backdrop")
                                .absolute()
                                .inset_0()
                                .occlude()
                                .on_any_mouse_down(cx.listener(|this, _event, _window, cx| {
                                    this.menu = None;
                                    cx.notify();
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

    /// The status line under the grid: the program, its state and the view controls.
    ///
    /// A live process needs no badge to say so - the session bar above already
    /// states the connection - so the state is only spelled out once the program
    /// stopped, in the tone of how it stopped.
    fn bar(&self, metrics: Metrics, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let strings = self.locale.strings();
        let live = self.state.accepts_input();
        let tone = if live {
            Tone::Positive
        } else {
            match self.state.failure() {
                Some(failure) => Tone::for_failure(&failure),
                None => Tone::Neutral,
            }
        };
        let muted = theme::swatch(shell_palette::TEXT_MUTED);
        div()
            .id("terminal.bar")
            .debug_selector(|| "terminal.bar".to_owned())
            .flex()
            .items_center()
            .justify_between()
            .gap(metrics.scale(spacing::TIGHT))
            .h(px(SHELL_BAR_HEIGHT))
            .flex_none()
            .pl(metrics.scale(spacing::BASE - 2.0))
            .pr(metrics.scale(spacing::HAIRLINE))
            .bg(theme::swatch(shell_palette::CANVAS))
            .border_t_1()
            .border_color(theme::faded(theme::swatch(shell_palette::BORDER), 0.8))
            .text_size(metrics.caption_size())
            .text_color(muted)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(metrics.scale(spacing::TIGHT))
                    .min_w(px(0.0))
                    .child(
                        div()
                            .flex_none()
                            .font_family(monospace_family())
                            .debug_selector(|| "terminal.grid.size".to_owned())
                            .child(SharedString::from(format!(
                                "{}\u{00D7}{}",
                                self.grid.cols, self.grid.rows
                            ))),
                    )
                    // The program is named on the session bar and the tab; down here it
                    // is only repeated once it stopped, beside how it stopped.
                    .when(!live, |row| {
                        row.child(
                            div()
                                .truncate()
                                .debug_selector(|| "terminal.shell".to_owned())
                                .child(SharedString::from(self.origin.label().to_owned())),
                        )
                        .child(components::chip(
                            self.state.label(self.locale),
                            tone,
                            metrics,
                        ))
                    })
                    .when_some(self.integration_note(), |row, note| {
                        row.child(
                            div()
                                .truncate()
                                .debug_selector(|| "terminal.integration".to_owned())
                                .child(note),
                        )
                    })
                    // Said under a running shell, not only after it stopped: a shell
                    // started without the login environment or in a replaced locale
                    // still runs, and this is the only place the user learns why a
                    // tool is missing or Chinese looks wrong (R14, R15).
                    .when_some(self.environment_note(), |row, note| {
                        // The whole sentence is in the tooltip: this bar is 28px high,
                        // and a note the user cannot finish reading is not a notice.
                        row.child(
                            div()
                                .id("terminal.environment")
                                .truncate()
                                .debug_selector(|| "terminal.environment".to_owned())
                                .tooltip(components::tooltip(note.clone()))
                                .child(note),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(metrics.scale(1.0))
                    .child(self.mode_button(metrics, strings, cx))
                    .child(self.search_button(metrics, strings, cx))
                    .child(
                        components::icon_only_button(Icon::Copy, metrics)
                            .w(metrics.hit_target())
                            .h(metrics.hit_target())
                            .id("terminal.copy")
                            .debug_selector(|| "terminal.copy".to_owned())
                            .tooltip(components::tooltip(SharedString::from(
                                Self::with_shortcut(strings.terminal_copy, &self.shortcuts.copy),
                            )))
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.copy_selection(cx);
                            })),
                    )
                    .child(
                        components::divider_vertical(metrics)
                            .h(metrics.scale(12.0))
                            .mx(metrics.scale(spacing::HAIRLINE)),
                    )
                    .child(
                        components::icon_button("A\u{2212}", metrics)
                            .w(metrics.hit_target())
                            .h(metrics.hit_target())
                            .id("terminal.font.decrease")
                            .debug_selector(|| "terminal.font.decrease".to_owned())
                            .tooltip(components::tooltip(strings.terminal_smaller))
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.step_font_size(false, cx);
                            })),
                    )
                    .child(
                        div()
                            .min_w(metrics.scale(34.0))
                            .text_center()
                            .font_family(monospace_family())
                            .child(SharedString::from(format!("{}px", self.font_size))),
                    )
                    .child(
                        components::icon_button("A+", metrics)
                            .w(metrics.hit_target())
                            .h(metrics.hit_target())
                            .id("terminal.font.increase")
                            .debug_selector(|| "terminal.font.increase".to_owned())
                            .tooltip(components::tooltip(strings.terminal_larger))
                            .on_click(cx.listener(|this, _event, _window, cx| {
                                this.step_font_size(true, cx);
                            })),
                    ),
            )
    }

    /// One row of the grid, drawn in three layers, every piece at its own column:
    /// the backgrounds, the cursor, then the text and the drawn shapes on top.
    ///
    /// Positions come from the column, never from the width of the glyphs drawn
    /// before: a character from a fallback face (a frame line, an icon, a CJK
    /// character) is drawn in its cell and cannot push the rest of the row off its
    /// columns.
    fn row(&self, index: usize, snapshot: &Snapshot, focused: bool) -> gpui::Div {
        let cell = self.cell.sanitized();
        let (width, height) = (cell.width, cell.height);
        let mut element = div()
            .relative()
            .flex_none()
            .w_full()
            .h(px(height))
            .whitespace_nowrap()
            .debug_selector(move || format!("terminal.row.{index}"));
        let Some(row) = snapshot.rows.get(index) else {
            return element;
        };
        let canvas_color = color_of(palette::BACKGROUND);
        let cursor_color = color_of(palette::CURSOR);
        let cursor = snapshot
            .cursor
            .filter(|cursor| cursor.row == index && cursor.shape != CursorShape::Hidden);
        // A focused block cursor is solid, and the character under it is drawn in
        // the canvas colour so it stays legible.
        let solid_cursor = cursor
            .filter(|cursor| focused && cursor.shape == CursorShape::Block)
            .map(|cursor| cursor.column);
        let x = |column: usize| px(column as f32 * width);

        // Backgrounds: one rectangle per stretch of cells with the same colour.
        let mut column = 0;
        let mut stretch: Option<(usize, usize, gpui::Rgba)> = None;
        let mut backgrounds = Vec::new();
        for rendered in &row.cells {
            let span = rendered.width.max(1);
            let background = cell_background(rendered, canvas_color);
            match (&mut stretch, background) {
                (Some((_, end, color)), Some(next)) if *color == next && *end == column => {
                    *end = column + span;
                }
                (_, next) => {
                    backgrounds.extend(stretch.take());
                    stretch = next.map(|color| (column, column + span, color));
                }
            }
            column += span;
        }
        backgrounds.extend(stretch);
        for (start, end, color) in backgrounds {
            element = element.child(
                div()
                    .absolute()
                    .top_0()
                    .left(x(start))
                    .w(x(end - start))
                    .h(px(height))
                    .bg(color),
            );
        }

        if let Some(cursor) = cursor {
            let left = x(cursor.column);
            let caret = px(CARET_WIDTH);
            let mark = div()
                .absolute()
                .left(left)
                .debug_selector(|| "terminal.cursor".to_owned());
            element = element.child(match cursor.shape {
                CursorShape::Beam => mark.top_0().w(caret).h(px(height)).bg(cursor_color),
                CursorShape::Underline => mark
                    .top(px(height - CARET_WIDTH))
                    .w(px(width))
                    .h(caret)
                    .bg(cursor_color),
                CursorShape::Block if focused => {
                    mark.top_0().w(px(width)).h(px(height)).bg(cursor_color)
                }
                // Unfocused, or asked for: an outline, so the text stays readable
                // and the pane shows it does not have the keyboard.
                _ => mark
                    .top_0()
                    .w(px(width))
                    .h(px(height))
                    .border_1()
                    .border_color(cursor_color),
            });
        }

        // Text and shapes.
        let stroke = (self.font_size / 13.0).round().max(1.0);
        let mut column = 0;
        let mut run: Option<TextRun> = None;
        let mut shapes: Option<ShapeRun> = None;
        let mut pieces: Vec<gpui::Div> = Vec::new();
        for rendered in &row.cells {
            let span = rendered.width.max(1);
            let background = cell_background(rendered, canvas_color).unwrap_or(canvas_color);
            let ink = if solid_cursor == Some(column) {
                canvas_color
            } else {
                ink(
                    color_of(rendered.foreground),
                    background,
                    rendered.attributes.dim,
                )
            };
            let style = TextStyle {
                ink,
                bold: rendered.attributes.bold,
                italic: rendered.attributes.italic,
                underline: rendered.attributes.underline,
                strike: rendered.attributes.strikethrough,
            };
            let mut characters = rendered.text.chars();
            let first = characters.next().unwrap_or(' ');
            let single = characters.next().is_none();
            let drawn = if single { glyphs::shape(first) } else { None };

            if let Some(shape) = drawn {
                flush_text(&mut run, &mut pieces, cell, self.font_size, index);
                let joins = shapes.as_ref().is_some_and(|current| {
                    current.character == first
                        && current.ink == ink
                        && current.end == column
                        && shape.tiles_horizontally()
                });
                if joins {
                    if let Some(current) = shapes.as_mut() {
                        current.end = column + span;
                    }
                } else {
                    flush_shapes(&mut shapes, &mut pieces, cell, stroke, index);
                    shapes = Some(ShapeRun {
                        character: first,
                        shape,
                        ink,
                        start: column,
                        end: column + span,
                    });
                }
            } else {
                flush_shapes(&mut shapes, &mut pieces, cell, stroke, index);
                let blank = rendered.text.trim().is_empty();
                // Plain ASCII shares the grid font's advance, so a run of it can be
                // drawn as one piece; anything else is drawn alone in its cells.
                let plain = single && first.is_ascii() && span == 1;
                let continues = run.as_ref().is_some_and(|current| {
                    current.plain && current.style == style && current.end == column
                });
                if plain && continues {
                    if let Some(current) = run.as_mut() {
                        current.text.push_str(&rendered.text);
                        current.end = column + span;
                    }
                } else if blank && !(style.underline || style.strike) {
                    flush_text(&mut run, &mut pieces, cell, self.font_size, index);
                } else {
                    flush_text(&mut run, &mut pieces, cell, self.font_size, index);
                    run = Some(TextRun {
                        text: rendered.text.clone(),
                        style,
                        plain,
                        start: column,
                        end: column + span,
                    });
                    if !plain {
                        flush_text(&mut run, &mut pieces, cell, self.font_size, index);
                    }
                }
            }
            column += span;
        }
        flush_text(&mut run, &mut pieces, cell, self.font_size, index);
        flush_shapes(&mut shapes, &mut pieces, cell, stroke, index);
        element.children(pieces)
    }

    fn composition_overlay(&self, snapshot: &Snapshot) -> Option<gpui::Div> {
        let marked = self.marked.as_ref()?;
        let cell = self.cell.sanitized();
        let cursor = snapshot.cursor?;
        Some(
            div()
                .absolute()
                .left(px(cursor.column as f32 * cell.width))
                .top(px(cursor.row as f32 * cell.height))
                .px(px(2.0))
                .bg(theme::swatch(shell_palette::SURFACE_SELECTED))
                .border_1()
                .border_color(theme::swatch(shell_palette::ACCENT))
                .text_size(px(self.font_size))
                .line_height(px(cell.height))
                .text_color(theme::swatch(shell_palette::TEXT_PRIMARY))
                .debug_selector(|| "terminal.composition".to_owned())
                .child(SharedString::from(marked.clone())),
        )
    }

    fn mode_button(
        &self,
        metrics: Metrics,
        strings: &crate::app::copy::Strings,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let integrated = self.core.presentation() == TerminalMode::Integrated;
        // An icon that is lit while command blocks are on; the tooltip names the
        // view it is in.
        let label = if integrated {
            strings.terminal_mode_blocks
        } else {
            strings.terminal_mode_classic
        };
        components::icon_only_button(Icon::Layers, metrics)
            // The bar is 36px tall; the button keeps the pointer-target minimum rather
            // than a smaller drawing at a reduced text size (R4).
            .h(metrics.hit_target())
            .w(metrics.hit_target())
            .when(integrated, |button| {
                button.bg(theme::swatch(shell_palette::ACCENT_TINT))
            })
            .id("terminal.mode")
            .debug_selector(|| "terminal.mode".to_owned())
            .tooltip(components::tooltip(label))
            .on_click(cx.listener(move |this, _event, _window, cx| {
                let next = if integrated {
                    TerminalMode::Classic
                } else {
                    TerminalMode::Integrated
                };
                this.set_presentation(next, cx);
            }))
    }

    fn search_button(
        &self,
        metrics: Metrics,
        strings: &crate::app::copy::Strings,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        components::icon_only_button(Icon::Search, metrics)
            .w(metrics.hit_target())
            .h(metrics.hit_target())
            .id("terminal.search")
            .debug_selector(|| "terminal.search".to_owned())
            .tooltip(components::tooltip(SharedString::from(
                Self::with_shortcut(strings.terminal_search, &self.shortcuts.search),
            )))
            .on_click(cx.listener(|this, _event, window, cx| {
                this.toggle_search(window, cx);
            }))
    }

    /// What the shell could not be given at startup, in the pane's language (R14, R15).
    ///
    /// The session reports these once. They are drawn in the bar under the grid,
    /// because a shell running in the environment the window manager passed down, or
    /// in a locale that cannot encode Chinese, is a silent degradation the user has to
    /// be able to see rather than guess at. The wording carries no value of any
    /// variable, so nothing about the environment is disclosed by showing it.
    fn environment_note(&self) -> Option<SharedString> {
        if self.environment_notes.is_empty() {
            return None;
        }
        let strings = self.locale.strings();
        let messages: Vec<&'static str> = self
            .environment_notes
            .iter()
            .map(|note| match note {
                ShellEnvironmentNote::LoginEnvironmentUnavailable => {
                    strings.terminal_environment_unavailable
                }
                ShellEnvironmentNote::LoginEnvironmentUnreadable => {
                    strings.terminal_environment_unreadable
                }
                ShellEnvironmentNote::LocaleReplaced => {
                    strings.terminal_environment_locale_replaced
                }
                ShellEnvironmentNote::LocaleAdded => strings.terminal_environment_locale_added,
            })
            .collect();
        Some(SharedString::from(messages.join(" \u{00B7} ")))
    }

    /// The degradation note the bar is drawing, so a test reads what the user reads.
    #[cfg(test)]
    #[must_use]
    pub fn environment_note_for_test(&self) -> Option<String> {
        self.environment_note().map(|note| note.to_string())
    }

    /// Puts the notes a degraded start would report on this pane (R14, R15).
    #[cfg(test)]
    pub fn set_environment_notes_for_test(
        &mut self,
        notes: Vec<ShellEnvironmentNote>,
        cx: &mut Context<Self>,
    ) {
        self.environment_notes = notes;
        cx.notify();
    }

    fn integration_note(&self) -> Option<&'static str> {
        if self.alternate_screen() || self.core.presentation() != TerminalMode::Integrated {
            return None;
        }
        let strings = self.locale.strings();
        if self.core.integration_failed() {
            Some(strings.terminal_blocks_failed)
        } else if !self.core.shows_command_blocks() {
            Some(strings.terminal_no_blocks)
        } else {
            None
        }
    }

    fn paste_overlay(&self, metrics: Metrics, cx: &mut Context<Self>) -> gpui::Div {
        let strings = self.locale.strings();
        let Some(pending) = &self.pending_paste else {
            return div();
        };
        let (count, preview) = paste_preview(&pending.text, 4);
        let target = pending.target.clone();
        div()
            .absolute()
            .top(px(8.0))
            .left(px(8.0))
            .right(px(8.0))
            .flex()
            .flex_col()
            .gap(metrics.scale(spacing::HAIRLINE))
            .p(metrics.scale(spacing::TIGHT))
            .bg(theme::swatch(shell_palette::SURFACE_RAISED))
            .border_1()
            .border_color(theme::swatch(shell_palette::BORDER_STRONG))
            .rounded(px(theme::radius::MEDIUM))
            .debug_selector(|| "terminal.paste".to_owned())
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_this, _event: &MouseDownEvent, _window, cx| {
                    cx.stop_propagation();
                }),
            )
            .child(
                div()
                    .text_size(metrics.label_size())
                    .text_color(theme::swatch(shell_palette::TEXT_PRIMARY))
                    .child(strings.terminal_paste_title),
            )
            .child(
                div()
                    .debug_selector(|| "terminal.paste.target".to_owned())
                    .text_size(metrics.caption_size())
                    .child(SharedString::from(format!(
                        "{}: {target}",
                        strings.terminal_paste_target
                    ))),
            )
            .child(
                div()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(shell_palette::TEXT_MUTED))
                    .child(SharedString::from(format!(
                        "{count} {}",
                        strings.terminal_paste_lines
                    ))),
            )
            // A saved command that was asked to run says so before it is confirmed: the
            // prompt is otherwise the same one a paste gets, and what follows it differs.
            .when(pending.execute, |column| {
                column.child(
                    div()
                        .debug_selector(|| "terminal.paste.runs".to_owned())
                        .text_size(metrics.caption_size())
                        .text_color(theme::swatch(shell_palette::ACCENT))
                        .child(strings.terminal_paste_runs),
                )
            })
            .child(
                div()
                    .debug_selector(|| "terminal.paste.preview".to_owned())
                    .font_family(monospace_family())
                    .text_size(metrics.caption_size())
                    .child(SharedString::from(preview)),
            )
            .child(
                div()
                    .flex()
                    .gap(metrics.scale(spacing::TIGHT))
                    .child(
                        components::compact_button(
                            strings.terminal_paste_confirm,
                            components::Variant::Primary,
                            metrics,
                        )
                        .id("terminal.paste.confirm")
                        .debug_selector(|| "terminal.paste.confirm".to_owned())
                        .on_click(cx.listener(
                            |this, _event, _window, cx| {
                                this.confirm_paste(cx);
                            },
                        )),
                    )
                    .child(
                        components::compact_button(
                            strings.terminal_paste_cancel,
                            components::Variant::Secondary,
                            metrics,
                        )
                        .id("terminal.paste.cancel")
                        .debug_selector(|| "terminal.paste.cancel".to_owned())
                        .on_click(cx.listener(
                            |this, _event, _window, cx| {
                                this.cancel_paste(cx);
                            },
                        )),
                    ),
            )
    }

    /// The blocks the list shows for the current query, filter and switches.
    ///
    /// A query that cannot be read leaves no rows: the bar says why, so the list
    /// never pretends the terminal holds nothing.
    fn listed_blocks(&self) -> Vec<CommandBlock> {
        self.core
            .search_blocks(&self.search_query, self.block_filter, self.search_options)
            .unwrap_or_default()
    }

    /// The panel under the grid: the find bar and the command blocks (R1).
    ///
    /// The bar and the block list read one query, so a search means the same thing
    /// in both: the numbers come from the real scrollback, the rows from the blocks
    /// whose command or output matches with the same switches.
    fn block_overlay(&self, metrics: Metrics, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let strings = self.locale.strings();
        let blocks = self.listed_blocks();
        let mut panel = div()
            .absolute()
            .bottom(px(SHELL_BAR_HEIGHT))
            .left(px(0.0))
            .right(px(0.0))
            // The bar grows with the text; the panel stops at a height that still
            // leaves the grid readable behind it and scrolls inside itself.
            .max_h(metrics.scale(180.0).clamp(px(180.0), px(260.0)))
            .flex()
            .flex_col()
            .id("terminal.blocks")
            .overflow_y_scroll()
            .p(metrics.scale(spacing::TIGHT))
            .gap(metrics.scale(spacing::HAIRLINE))
            .bg(theme::swatch(shell_palette::SURFACE))
            .border_t_1()
            .border_color(theme::swatch(shell_palette::BORDER))
            .debug_selector(|| "terminal.blocks".to_owned())
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_this, _event: &MouseDownEvent, _window, cx| {
                    cx.stop_propagation();
                }),
            )
            .when(self.search_open, |panel| {
                panel.child(self.search_bar(metrics, cx))
            })
            .when(self.core.shows_command_blocks(), |panel| {
                panel.child(self.block_header(metrics, strings, cx))
            });
        for block in blocks {
            panel = panel.child(self.block_row(&block, metrics, strings, cx));
        }
        panel
    }

    /// The find bar: the query, where it is, and the switches that narrow it.
    ///
    /// Exactly one note is drawn beside the query — position and total, an empty
    /// result, or the reason an expression could not be read — so "nothing matched"
    /// and "this pattern is broken" never look alike, and an empty query says
    /// neither (R1). Each note is a sentence: it may shrink and wrap onto its own
    /// line instead of running past the pane (R4).
    fn search_bar(&self, metrics: Metrics, cx: &mut Context<Self>) -> gpui::Div {
        let strings = self.locale.strings();
        let empty_query = self.search_query.is_empty();
        let query = if empty_query {
            strings.terminal_search.to_owned()
        } else {
            self.search_query.clone()
        };
        // The count, the empty result and the invalid-expression reason are whole
        // sentences. The reason for `a{2,1}` is wider than one pane of a sideways
        // split (354 px at the minimum window). A note that cannot shrink is drawn
        // past that pane's right edge, and the pane's `overflow_hidden` clips the
        // end of the reason where it cannot be read (R4). The line may be narrower
        // than its words and wraps there, the same way the key hint does.
        let note = match self.search_note() {
            SearchNote::Silent => None,
            SearchNote::Position { position, total } => Some(
                div()
                    .min_w(px(0.0))
                    .debug_selector(|| "terminal.search.position".to_owned())
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(shell_palette::TEXT_SECONDARY))
                    .child(SharedString::from(format!("{position} / {total}"))),
            ),
            SearchNote::Empty => Some(
                div()
                    .min_w(px(0.0))
                    .debug_selector(|| "terminal.search.empty".to_owned())
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(shell_palette::TEXT_MUTED))
                    .child(SharedString::from(strings.terminal_search_empty.to_owned())),
            ),
            SearchNote::Invalid(reason) => Some(
                div()
                    .min_w(px(0.0))
                    .debug_selector(|| "terminal.search.error".to_owned())
                    .text_size(metrics.caption_size())
                    .text_color(Tone::Danger.text())
                    .child(SharedString::from(format!(
                        "{}: {reason}",
                        strings.terminal_search_invalid
                    ))),
            ),
        };

        let focused = self.search_focused;
        let accent = theme::swatch(shell_palette::ACCENT);
        // The placeholder is a sentence. A field that shrinks to a few letters and
        // then truncates cuts it through a word ("Search this te"), and the pane
        // has no other place that text is written (R4).
        let caption = metrics.caption_size();
        let proportional =
            crate::app::components::reserved_text_width(strings.terminal_search, caption);
        let mono = px(strings.terminal_search.chars().count() as f32
            * (caption / px(1.0))
            * shellune_terminal::MONOSPACE_ADVANCE_RATIO);
        let query_text_floor = proportional.max(mono);
        let query_field_floor = query_text_floor + metrics.scale(spacing::HAIRLINE) * 2.0 + px(2.0);
        let mut bar = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(metrics.scale(spacing::TIGHT))
            .debug_selector(|| "terminal.search.bar".to_owned())
            // The field the keys are going to says so, and says it twice: an accent
            // border with a soft ring while the bar holds the keyboard, a neutral one
            // once the terminal has it back, and the hint beside it names where the
            // keys go in words as well (R4).
            .child(
                div()
                    .flex()
                    .items_center()
                    .when(empty_query, |field| {
                        field.flex_none().min_w(query_field_floor)
                    })
                    .when(!empty_query, |field| field.flex_1().min_w(px(80.0)))
                    .max_w_full()
                    .rounded(px(radius::SMALL))
                    .border_1()
                    .border_color(if focused {
                        accent
                    } else {
                        theme::swatch(shell_palette::BORDER_STRONG)
                    })
                    .when(focused, |field| {
                        field.shadow(vec![BoxShadow {
                            color: theme::faded(accent, 0.22).into(),
                            offset: point(px(0.0), px(0.0)),
                            blur_radius: px(0.0),
                            spread_radius: px(2.0),
                        }])
                    })
                    .px(metrics.scale(spacing::HAIRLINE))
                    .debug_selector(move || {
                        if focused {
                            "terminal.search.keyboard.bar".to_owned()
                        } else {
                            "terminal.search.keyboard.terminal".to_owned()
                        }
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w(if empty_query {
                                query_text_floor
                            } else {
                                px(0.0)
                            })
                            .when(empty_query, |text| text.whitespace_normal())
                            .when(!empty_query, |text| text.truncate())
                            .debug_selector(|| "terminal.search.query".to_owned())
                            .font_family(monospace_family())
                            .text_size(metrics.caption_size())
                            .text_color(if empty_query {
                                theme::swatch(shell_palette::TEXT_MUTED)
                            } else {
                                theme::swatch(shell_palette::TEXT_PRIMARY)
                            })
                            .child(SharedString::from(query)),
                    ),
            );
        // Which keys do what, or — once the terminal has the keyboard again — the one
        // thing the reader needs to know before typing: their keys go to the shell.
        //
        // The line is allowed to be narrower than its words: in a pane of a split tab
        // (354 px at the minimum window) it is wider than the whole bar, and a line that
        // could not shrink would be drawn past the pane's edge, where the rest of it —
        // including the key that closes the bar — cannot be read or scrolled to (R4).
        // It takes its own line and wraps there instead; the buttons stay whole.
        bar = bar.child(
            div()
                .min_w(px(0.0))
                .debug_selector(move || {
                    if focused {
                        "terminal.search.keys".to_owned()
                    } else {
                        "terminal.search.keys.terminal".to_owned()
                    }
                })
                .text_size(metrics.caption_size())
                .text_color(if focused {
                    theme::swatch(shell_palette::TEXT_MUTED)
                } else {
                    theme::swatch(shell_palette::TEXT_SECONDARY)
                })
                .child(SharedString::from(
                    if focused {
                        strings.terminal_search_keys
                    } else {
                        strings.terminal_search_keyboard_terminal
                    }
                    .to_owned(),
                )),
        );
        if let Some(note) = note {
            bar = bar.child(note);
        }
        bar.child(
            components::compact_button(
                strings.terminal_search_previous,
                components::Variant::Ghost,
                metrics,
            )
            .h(metrics.hit_target())
            .id("terminal.search.previous")
            .debug_selector(|| "terminal.search.previous".to_owned())
            .on_click(cx.listener(|this, _event, _window, cx| {
                this.previous_search_hit(cx);
            })),
        )
        .child(
            components::compact_button(
                strings.terminal_search_next,
                components::Variant::Ghost,
                metrics,
            )
            .h(metrics.hit_target())
            .id("terminal.search.next")
            .debug_selector(|| "terminal.search.next".to_owned())
            .on_click(cx.listener(|this, _event, _window, cx| {
                this.next_search_hit(cx);
            })),
        )
        .child(Self::search_switch(
            strings.terminal_search_case,
            self.search_options.case_sensitive,
            metrics,
            "terminal.search.case",
            Self::toggle_case_sensitive,
            cx,
        ))
        .child(Self::search_switch(
            strings.terminal_search_regex,
            self.search_options.regex,
            metrics,
            "terminal.search.regex",
            Self::toggle_regex,
            cx,
        ))
        .child(
            components::compact_button(
                strings.terminal_search_close,
                components::Variant::Ghost,
                metrics,
            )
            .h(metrics.hit_target())
            .id("terminal.search.close")
            .debug_selector(|| "terminal.search.close".to_owned())
            .on_click(cx.listener(|this, _event, window, cx| {
                this.close_search(window, cx);
            })),
        )
    }

    /// One switch of the find bar.
    ///
    /// The box in the label says which state it is in, so the state is readable as
    /// text; the filled variant only repeats it instead of carrying the answer alone
    /// (R3). The identifier is also the debug selector, so a test clicks the same
    /// control a person does.
    fn search_switch(
        label: &'static str,
        on: bool,
        metrics: Metrics,
        id: &'static str,
        toggle: fn(&mut Self, &mut Context<Self>),
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let box_mark = if on { "[x]" } else { "[ ]" };
        components::compact_button(
            format!("{box_mark} {label}"),
            if on {
                components::Variant::Soft
            } else {
                components::Variant::Ghost
            },
            metrics,
        )
        .h(metrics.hit_target())
        .id(id)
        .debug_selector(move || id.to_owned())
        .tooltip(components::tooltip(SharedString::from(label)))
        .on_click(cx.listener(move |this, _event, _window, cx| toggle(this, cx)))
    }

    /// The block list's own row: how the list is filtered, and the switch that
    /// changes it.
    fn block_header(
        &self,
        metrics: Metrics,
        strings: &crate::app::copy::Strings,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let filter_label = match self.block_filter {
            BlockFilter::All => strings.terminal_filter_all,
            BlockFilter::Running => strings.terminal_filter_running,
            BlockFilter::Unknown => strings.terminal_filter_unknown,
            BlockFilter::Failed => strings.terminal_filter_failed,
            BlockFilter::ExitedZero => strings.terminal_filter_zero,
        };
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(metrics.scale(spacing::TIGHT))
            .debug_selector(|| "terminal.blocks.header".to_owned())
            .child(
                components::compact_button(filter_label, components::Variant::Ghost, metrics)
                    .id("terminal.blocks.filter")
                    .debug_selector(|| "terminal.blocks.filter".to_owned())
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.block_filter = this.block_filter.cycle();
                        cx.notify();
                    })),
            )
    }

    fn block_row(
        &self,
        block: &CommandBlock,
        metrics: Metrics,
        strings: &crate::app::copy::Strings,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let id = block.id;
        let status = block_status_label(block.status, strings);
        let tone = block_tone(block.status);
        let duration = duration_label(block.duration);
        let command = if block.command.is_empty() {
            status.clone()
        } else {
            block.command.clone()
        };
        div()
            .flex()
            .items_center()
            .gap(metrics.scale(spacing::TIGHT))
            .debug_selector(move || format!("terminal.block.{}", id.get()))
            .child(components::chip(status, tone, metrics))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .truncate()
                    .font_family(monospace_family())
                    .text_size(metrics.caption_size())
                    .child(SharedString::from(command)),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(shell_palette::TEXT_MUTED))
                    .child(SharedString::from(duration)),
            )
            .child(
                components::compact_button(
                    strings.terminal_block_copy,
                    components::Variant::Ghost,
                    metrics,
                )
                .id(SharedString::from(format!(
                    "terminal.block.copy.{}",
                    id.get()
                )))
                .on_click(cx.listener(move |this, _event, _window, cx| {
                    this.copy_block(id, cx);
                })),
            )
            .child(
                components::compact_button(
                    strings.terminal_block_rerun,
                    components::Variant::Secondary,
                    metrics,
                )
                .id(SharedString::from(format!(
                    "terminal.block.rerun.{}",
                    id.get()
                )))
                .on_click(cx.listener(move |this, _event, _window, cx| {
                    this.rerun_block(id, cx);
                })),
            )
    }

    fn viewport(&self, window: &mut Window, cx: &mut Context<Self>) -> gpui::Div {
        let snapshot = self.core.snapshot();
        let focused = self.focus.is_focused(window);
        let weak = cx.entity().downgrade();
        let entity = cx.entity();
        let focus = self.focus.clone();
        let listener = move |bounds: Vec<Bounds<Pixels>>, _window: &mut Window, cx: &mut App| {
            if let Some(bounds) = bounds.first() {
                let _ = weak.update(cx, |pane, cx| pane.apply_viewport(*bounds, cx));
            }
        };

        // The grid is laid out in the face its cells were measured in: drawn in the
        // interface font, every column after the first would drift from its cell.
        let mut rows = div()
            .flex()
            .flex_col()
            .absolute()
            .inset_0()
            .font_family(monospace_family())
            .debug_selector(|| "terminal.viewport".to_owned());
        for index in 0..snapshot.rows.len() {
            rows = rows.child(self.row(index, &snapshot, focused));
        }
        if let Some(overlay) = self.composition_overlay(&snapshot) {
            rows = rows.child(overlay);
        }

        div()
            .flex_1()
            .relative()
            .min_h(px(0.0))
            .overflow_hidden()
            .bg(color_of(palette::BACKGROUND))
            .cursor_text()
            // The grid declares its own key context so the application's shortcuts
            // can step aside: inside a terminal Escape belongs to the program.
            .key_context(crate::app::TERMINAL_KEY_CONTEXT)
            .track_focus(&self.focus)
            .on_children_prepainted(listener)
            .child(rows)
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
                cx.listener(|this, event: &MouseUpEvent, _window, cx| {
                    this.on_mouse_up(event, cx);
                }),
            )
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    this.on_middle_down(event, window, cx);
                }),
            )
            .on_mouse_up(
                MouseButton::Middle,
                cx.listener(|this, event: &MouseUpEvent, _window, cx| {
                    this.release_reported(TerminalMouseButton::Middle, event, cx);
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.on_right_down(event, window, cx);
                }),
            )
            .on_mouse_up(
                MouseButton::Right,
                cx.listener(|this, event: &MouseUpEvent, _window, cx| {
                    this.release_reported(TerminalMouseButton::Right, event, cx);
                }),
            )
            .on_drop(
                cx.listener(|this, paths: &gpui::ExternalPaths, window, cx| {
                    this.drop_paths(paths.paths(), window, cx);
                }),
            )
            .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _window, cx| {
                this.on_scroll(event, cx);
            }))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                this.handle_key(event, window, cx);
            }))
            // The clipboard and search shortcuts are actions rather than key
            // branches, so the settings page can rebind or unbind them: an unbound
            // combination falls through to the program below.
            .on_action(cx.listener(|this, _: &TerminalSearch, window, cx| {
                // A full-screen program owns the screen: its contents are not history
                // and must not be searched (R1). The bar refuses to open there itself,
                // so no other entry point can open one either.
                this.toggle_search(window, cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|this, _: &TerminalCopy, _window, cx| {
                this.copy_selection(cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(|this, _: &TerminalPaste, window, cx| {
                this.paste(window, cx);
                cx.stop_propagation();
            }))
    }
}

/// A path as this computer's shell reads it: bare when it is plain, quoted when
/// it holds spaces or characters the shell would act on.
fn quote_local_path(path: &str) -> String {
    if cfg!(target_os = "windows") {
        if path
            .chars()
            .any(|c| c.is_whitespace() || "&()[]{}^=;!'+,`~%$@#".contains(c))
        {
            format!("\"{path}\"")
        } else {
            path.to_owned()
        }
    } else {
        quote_posix_path(path)
    }
}

/// A path as a POSIX shell reads it, in single quotes when it needs them.
#[must_use]
pub fn quote_posix_path(path: &str) -> String {
    let plain = !path.is_empty()
        && path
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-+:,@%=~".contains(c));
    if plain {
        path.to_owned()
    } else {
        format!("'{}'", path.replace('\'', "'\\''"))
    }
}

fn color_of(color: Rgb) -> gpui::Rgba {
    let value = u32::from(color.r) << 16 | u32::from(color.g) << 8 | u32::from(color.b);
    gpui::rgb(theme::terminal_color(value))
}

/// The background a cell paints over the canvas, or `None` when it shows the canvas.
fn cell_background(cell: &RenderedCell, canvas: gpui::Rgba) -> Option<gpui::Rgba> {
    if cell.selected {
        return Some(gpui::rgba(match theme::terminal_appearance() {
            theme::Appearance::Dark => SELECTION_BACKGROUND,
            theme::Appearance::Light => SELECTION_BACKGROUND_LIGHT,
        }));
    }
    let background = color_of(cell.background);
    (background != canvas).then_some(background)
}

/// The least contrast text keeps against its background, and dimmed text.
const TEXT_CONTRAST_FLOOR: f32 = 3.0;
const DIM_CONTRAST_FLOOR: f32 = 2.0;

/// The colour a character is drawn in.
///
/// Dim text is mixed towards its background. Then, like the minimum-contrast
/// option of other terminals, a colour too close to its background is moved away
/// from it until it reads: programs choose colours for one kind of terminal, and a
/// grey meant for a dark one vanishes on a light one. A colour equal to its
/// background is left alone, since a program that hides text on purpose does so.
fn ink(foreground: gpui::Rgba, background: gpui::Rgba, dim: bool) -> gpui::Rgba {
    if foreground == background {
        return foreground;
    }
    let foreground = if dim {
        mix(foreground, background, 0.4)
    } else {
        foreground
    };
    let floor = if dim {
        DIM_CONTRAST_FLOOR
    } else {
        TEXT_CONTRAST_FLOOR
    };
    if contrast(foreground, background) >= floor {
        return foreground;
    }
    // Towards black on a light background, towards white on a dark one.
    let target = if luminance(background) > 0.4 {
        gpui::Rgba {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 1.0,
        }
    } else {
        gpui::Rgba {
            r: 1.0,
            g: 1.0,
            b: 1.0,
            a: 1.0,
        }
    };
    let (mut low, mut high) = (0.0_f32, 1.0_f32);
    for _ in 0..12 {
        let middle = (low + high) / 2.0;
        if contrast(mix(foreground, target, middle), background) >= floor {
            high = middle;
        } else {
            low = middle;
        }
    }
    mix(foreground, target, high)
}

/// `from` moved `amount` of the way to `to`.
fn mix(from: gpui::Rgba, to: gpui::Rgba, amount: f32) -> gpui::Rgba {
    let blend = |a: f32, b: f32| a + (b - a) * amount;
    gpui::Rgba {
        r: blend(from.r, to.r),
        g: blend(from.g, to.g),
        b: blend(from.b, to.b),
        a: from.a,
    }
}

/// WCAG relative luminance.
fn luminance(color: gpui::Rgba) -> f32 {
    let channel = |value: f32| {
        if value <= 0.039_28 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(color.r) + 0.7152 * channel(color.g) + 0.0722 * channel(color.b)
}

/// WCAG contrast ratio, from 1 to 21.
fn contrast(a: gpui::Rgba, b: gpui::Rgba) -> f32 {
    let (a, b) = (luminance(a), luminance(b));
    let (light, dark) = if a > b { (a, b) } else { (b, a) };
    (light + 0.05) / (dark + 0.05)
}

/// How a stretch of text is drawn.
#[derive(Clone, Copy, PartialEq)]
struct TextStyle {
    ink: gpui::Rgba,
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
}

/// Text waiting to be drawn from `start`.
struct TextRun {
    text: String,
    style: TextStyle,
    /// Plain ASCII, which may take more characters; anything else is one cell.
    plain: bool,
    start: usize,
    end: usize,
}

/// Cells of one drawn shape waiting to be drawn, as one piece when they tile.
struct ShapeRun {
    character: char,
    shape: glyphs::Shape,
    ink: gpui::Rgba,
    start: usize,
    end: usize,
}

fn flush_text(
    run: &mut Option<TextRun>,
    pieces: &mut Vec<gpui::Div>,
    cell: CellSize,
    font_size: f32,
    row: usize,
) {
    let Some(run) = run.take() else {
        return;
    };
    let style = run.style;
    let start = run.start;
    let mut piece = div()
        .debug_selector(move || format!("terminal.text.{row}.{start}"))
        .absolute()
        .top_0()
        .left(px(run.start as f32 * cell.width))
        .h(px(cell.height))
        .text_size(px(font_size))
        .line_height(px(cell.height))
        .text_color(style.ink)
        .font_weight(if style.bold {
            FontWeight::BOLD
        } else {
            FontWeight::NORMAL
        });
    if !run.plain {
        // A character from another face is centred in its cells; it may overhang
        // them, as in other terminals, but never moves its neighbours.
        piece = piece
            .w(px((run.end - run.start) as f32 * cell.width))
            .flex()
            .justify_center();
    }
    if style.italic {
        piece = piece.italic();
    }
    if style.underline {
        piece = piece.underline();
    }
    if style.strike {
        piece = piece.line_through();
    }
    pieces.push(piece.child(SharedString::from(run.text)));
}

fn flush_shapes(
    run: &mut Option<ShapeRun>,
    pieces: &mut Vec<gpui::Div>,
    cell: CellSize,
    stroke: f32,
    row: usize,
) {
    let Some(run) = run.take() else {
        return;
    };
    let start = run.start;
    let left = run.start as f32 * cell.width;
    let width = (run.end - run.start) as f32 * cell.width;
    let height = cell.height;
    let ink = run.ink;
    let rect = |x0: f32, y0: f32, x1: f32, y1: f32| {
        let (x0, y0, x1, y1) = (x0.round(), y0.round(), x1.round(), y1.round());
        div()
            .debug_selector(move || format!("terminal.shape.{row}.{start}"))
            .absolute()
            .left(px(x0))
            .top(px(y0))
            .w(px((x1 - x0).max(1.0)))
            .h(px((y1 - y0).max(1.0)))
            .bg(ink)
    };
    match run.shape {
        glyphs::Shape::Fill { rects, alpha } => {
            for part in rects.into_iter().flatten() {
                pieces.push(
                    rect(
                        left + part.left * width,
                        part.top * height,
                        left + part.right * width,
                        part.bottom * height,
                    )
                    .opacity(alpha),
                );
            }
        }
        glyphs::Shape::Lines(arms) => {
            // The centre of the cell; for a joined run, of its first and last cells.
            let centre_x = (left + cell.width / 2.0).round();
            let last_centre_x = (left + width - cell.width / 2.0).round();
            let centre_y = (height / 2.0).round();
            let thickness = |weight: glyphs::Weight| match weight {
                glyphs::Weight::Heavy => stroke * 2.0,
                _ => stroke,
            };
            // A double arm is two lines `gap` either side of the centre. Where
            // double arms meet, each line stops at the line it joins instead of
            // crossing it, as a drawn frame would: the outer lines meet at the
            // outer corner and the inner ones at the inner corner.
            let gap = stroke * 1.5;
            let double = |weight: glyphs::Weight| weight == glyphs::Weight::Double;
            let vertical_double = double(arms.up) || double(arms.down);
            let horizontal_double = double(arms.left) || double(arms.right);
            // Left arm: from the left edge to the centre.
            match arms.left {
                glyphs::Weight::None => {}
                glyphs::Weight::Double => {
                    for (y, joins_double) in [
                        (centre_y - gap, double(arms.up)),
                        (centre_y + gap, double(arms.down)),
                    ] {
                        let end = if joins_double {
                            last_centre_x - gap
                        } else {
                            last_centre_x + gap
                        };
                        pieces.push(rect(
                            left,
                            y - stroke / 2.0,
                            end + stroke / 2.0,
                            y + stroke / 2.0,
                        ));
                    }
                }
                weight => {
                    let t = thickness(weight);
                    let end = if vertical_double {
                        last_centre_x - gap
                    } else {
                        last_centre_x
                    } + t / 2.0;
                    pieces.push(rect(left, centre_y - t / 2.0, end, centre_y + t / 2.0));
                }
            }
            // Right arm: from the centre to the right edge.
            match arms.right {
                glyphs::Weight::None => {}
                glyphs::Weight::Double => {
                    for (y, joins_double) in [
                        (centre_y - gap, double(arms.up)),
                        (centre_y + gap, double(arms.down)),
                    ] {
                        let start = if joins_double {
                            centre_x + gap
                        } else {
                            centre_x - gap
                        };
                        pieces.push(rect(
                            start - stroke / 2.0,
                            y - stroke / 2.0,
                            left + width,
                            y + stroke / 2.0,
                        ));
                    }
                }
                weight => {
                    let t = thickness(weight);
                    let start = if vertical_double {
                        centre_x + gap
                    } else {
                        centre_x
                    } - t / 2.0;
                    pieces.push(rect(
                        start,
                        centre_y - t / 2.0,
                        left + width,
                        centre_y + t / 2.0,
                    ));
                }
            }
            // Up arm: from the top edge to the centre.
            match arms.up {
                glyphs::Weight::None => {}
                glyphs::Weight::Double => {
                    for (x, joins_double) in [
                        (centre_x - gap, double(arms.left)),
                        (centre_x + gap, double(arms.right)),
                    ] {
                        let end = if joins_double {
                            centre_y - gap
                        } else {
                            centre_y + gap
                        };
                        pieces.push(rect(
                            x - stroke / 2.0,
                            0.0,
                            x + stroke / 2.0,
                            end + stroke / 2.0,
                        ));
                    }
                }
                weight => {
                    let t = thickness(weight);
                    let end = if horizontal_double {
                        centre_y - gap
                    } else {
                        centre_y
                    } + t / 2.0;
                    pieces.push(rect(centre_x - t / 2.0, 0.0, centre_x + t / 2.0, end));
                }
            }
            // Down arm: from the centre to the bottom edge.
            match arms.down {
                glyphs::Weight::None => {}
                glyphs::Weight::Double => {
                    for (x, joins_double) in [
                        (centre_x - gap, double(arms.left)),
                        (centre_x + gap, double(arms.right)),
                    ] {
                        let start = if joins_double {
                            centre_y + gap
                        } else {
                            centre_y - gap
                        };
                        pieces.push(rect(
                            x - stroke / 2.0,
                            start - stroke / 2.0,
                            x + stroke / 2.0,
                            height,
                        ));
                    }
                }
                weight => {
                    let t = thickness(weight);
                    let start = if horizontal_double {
                        centre_y + gap
                    } else {
                        centre_y
                    } - t / 2.0;
                    pieces.push(rect(centre_x - t / 2.0, start, centre_x + t / 2.0, height));
                }
            }
        }
        glyphs::Shape::Arc(arc) => {
            let centre_x = (left + cell.width / 2.0).round();
            let centre_y = (height / 2.0).round();
            let half = stroke / 2.0;
            let right_edge = left + width;
            let radius = px((cell.width / 2.0).min(height / 2.0));
            let border = px(stroke);
            let corner = |x0: f32, y0: f32, x1: f32, y1: f32| {
                div()
                    .absolute()
                    .left(px(x0.round()))
                    .top(px(y0.round()))
                    .w(px((x1 - x0).round().max(1.0)))
                    .h(px((y1 - y0).round().max(1.0)))
                    .border_color(ink)
            };
            pieces.push(match arc {
                glyphs::Arc::DownRight => {
                    corner(centre_x - half, centre_y - half, right_edge, height + half)
                        .border_t(border)
                        .border_l(border)
                        .rounded_tl(radius)
                }
                glyphs::Arc::DownLeft => {
                    corner(left - half, centre_y - half, centre_x + half, height + half)
                        .border_t(border)
                        .border_r(border)
                        .rounded_tr(radius)
                }
                glyphs::Arc::UpLeft => corner(left - half, -half, centre_x + half, centre_y + half)
                    .border_b(border)
                    .border_r(border)
                    .rounded_br(radius),
                glyphs::Arc::UpRight => corner(centre_x - half, -half, right_edge, centre_y + half)
                    .border_b(border)
                    .border_l(border)
                    .rounded_bl(radius),
            });
        }
    }
}

impl Render for TerminalPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let metrics = Metrics::default();
        // The advance width comes from the font the window actually resolved, so
        // the grid matches what is drawn even when the platform substitutes a face.
        let _ = self.measure_cell(window);

        let show_blocks =
            !self.alternate_screen() && (self.search_open || self.core.shows_command_blocks());
        let show_paste = self.pending_paste.is_some();
        // The pane's own chrome - the status line, the find bar, the paste prompt,
        // the menu - is drawn in the terminal's palette, so a dark terminal in a
        // light window is dark to its edges. The window's palette comes back after.
        let window_appearance = theme::appearance();
        theme::set_appearance(theme::terminal_appearance());
        let pane = div()
            .flex()
            .flex_col()
            .size_full()
            .min_h(px(0.0))
            .relative()
            .child(
                // A margin inside the frame keeps the first column off the border; the
                // grid itself is measured inside it, so no cell is lost to the margin.
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h(px(0.0))
                    .pl(metrics.scale(spacing::SNUG))
                    .pr(metrics.scale(spacing::HAIRLINE))
                    .py(metrics.scale(spacing::TIGHT))
                    .bg(color_of(palette::BACKGROUND))
                    .child(self.viewport(window, cx)),
            )
            .child(self.bar(metrics, cx))
            .when(show_paste, |column| {
                column.child(self.paste_overlay(metrics, cx))
            })
            .when(show_blocks, |column| {
                column.child(self.block_overlay(metrics, cx))
            })
            .children(self.context_menu(window, cx));
        theme::set_appearance(window_appearance);
        pane
    }
}

fn block_status_label(status: BlockStatus, strings: &crate::app::copy::Strings) -> String {
    match status {
        BlockStatus::Running => strings.terminal_block_running.to_owned(),
        BlockStatus::Unknown => strings.terminal_block_unknown.to_owned(),
        BlockStatus::Exited(code) => format!("{} {code}", strings.terminal_block_exit),
    }
}

fn block_tone(status: BlockStatus) -> Tone {
    match status {
        BlockStatus::Exited(0) => Tone::Positive,
        BlockStatus::Exited(_) => Tone::Danger,
        BlockStatus::Unknown => Tone::Warning,
        BlockStatus::Running => Tone::Neutral,
    }
}

fn duration_label(duration: Option<std::time::Duration>) -> String {
    let Some(duration) = duration else {
        return "\u{2014}".to_owned();
    };
    let millis = duration.as_millis();
    if millis < 1000 {
        format!("{millis} ms")
    } else {
        format!("{:.1} s", millis as f64 / 1000.0)
    }
}

impl EntityInputHandler for TerminalPane {
    fn text_for_range(
        &mut self,
        _range: Range<usize>,
        _adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        // The terminal has no editable text buffer: committed text goes straight to
        // the shell, and the shell decides where it appears.
        None
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        if self.core.has_selection() {
            // A clipboard selection is not an editable selection; reporting it would
            // let an input method replace text the shell already printed.
            return Some(UTF16Selection {
                range: 0..0,
                reversed: false,
            });
        }
        Some(UTF16Selection {
            range: 0..0,
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
        // A commit replaces any composition with its final text, so the pending
        // marker is dropped before the text is sent.
        self.marked = None;
        if self.search_focused && self.search_open && !self.alternate_screen() {
            self.append_search_query(text, cx);
            return;
        }
        // A commit is program input, including one command that ends in a newline.
        // Multi-line and high-risk confirmation belongs to clipboard paste.
        let bytes = encode_text(text);
        self.write(&bytes, cx);
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
        // Composition is shown next to the caret but nothing is sent until the
        // input method commits, which is what makes Chinese input usable.
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
        let cell = self.cell.sanitized();
        let cursor = self.core.snapshot().cursor?;
        Some(Bounds::new(
            gpui::point(
                element_bounds.origin.x + px(cursor.column as f32 * cell.width),
                element_bounds.origin.y + px(cursor.row as f32 * cell.height),
            ),
            gpui::size(px(cell.width), px(cell.height)),
        ))
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

#[cfg(test)]
mod ink_tests {
    use super::{contrast, ink};

    fn rgb(hex: u32) -> gpui::Rgba {
        gpui::rgb(hex)
    }

    #[test]
    fn a_pale_colour_on_a_light_canvas_is_made_legible() {
        // A grey chosen for a dark terminal, on the light one.
        let drawn = ink(rgb(0xe6e6e6), rgb(0xfcfcfd), false);
        assert!(contrast(drawn, rgb(0xfcfcfd)) >= 2.99);
    }

    #[test]
    fn a_dark_colour_on_a_dark_canvas_is_made_legible() {
        let drawn = ink(rgb(0x1a1c22), rgb(0x0b0c10), false);
        assert!(contrast(drawn, rgb(0x0b0c10)) >= 2.99);
    }

    #[test]
    fn readable_colours_are_left_as_they_are() {
        assert_eq!(ink(rgb(0x1f2329), rgb(0xfcfcfd), false), rgb(0x1f2329));
        assert_eq!(ink(rgb(0xe6e9f0), rgb(0x0b0c10), false), rgb(0xe6e9f0));
    }

    #[test]
    fn dim_text_is_dimmer_yet_still_readable() {
        let canvas = rgb(0x0b0c10);
        let regular = ink(rgb(0xe6e9f0), canvas, false);
        let dim = ink(rgb(0xe6e9f0), canvas, true);
        assert!(contrast(dim, canvas) < contrast(regular, canvas));
        assert!(contrast(dim, canvas) >= 1.99);
    }

    #[test]
    fn paths_are_quoted_only_when_the_shell_would_act_on_them() {
        use super::quote_posix_path;
        assert_eq!(quote_posix_path("/home/me/a.txt"), "/home/me/a.txt");
        assert_eq!(
            quote_posix_path("/home/me/my file.txt"),
            "'/home/me/my file.txt'"
        );
        assert_eq!(quote_posix_path("it's"), "'it'\\''s'");
        assert_eq!(quote_posix_path("~/下载/x"), "'~/下载/x'");
    }

    #[test]
    fn text_hidden_on_purpose_stays_hidden() {
        assert_eq!(ink(rgb(0x334455), rgb(0x334455), false), rgb(0x334455));
    }
}
